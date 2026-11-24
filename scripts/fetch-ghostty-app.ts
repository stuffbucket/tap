#!/usr/bin/env bun
// Fetches Ghostty's official build of the commit Tap's libghostty-vt is pinned to, and refuses it
// unless it is signed by Ghostty's Developer ID, notarized, and built from exactly that commit.

import { mkdir, mkdtemp, rm, rmdir, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import {
  GHOSTTY_TEAM_ID, PINNED_GHOSTTY_COMMIT, PINNED_VT_TAG, ghosttyAppCommit, matchesPinnedCommit,
  pinnedGhosttyApp, pinnedGhosttyDirectory, pinnedGhosttyDmgUrl,
} from "../src/ghostty-app.ts"

async function run(command: string[]): Promise<{ code: number; output: string }> {
  const child = Bun.spawn(command, { stdin: "ignore", stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text()])
  return { code: await child.exited, output: stdout + stderr }
}

async function verify(app: string) {
  const signature = await run(["codesign", "--verify", "--deep", "--strict", app])
  if (signature.code !== 0) throw new Error(`Ghostty.app signature is invalid:\n${signature.output}`)
  const details = await run(["codesign", "-dv", "--verbose=2", app])
  if (!details.output.includes(`TeamIdentifier=${GHOSTTY_TEAM_ID}`)) throw new Error(`Ghostty.app is not signed by Ghostty's Developer ID (${GHOSTTY_TEAM_ID})`)
  const gatekeeper = await run(["spctl", "--assess", "--type", "execute", "-vv", app])
  if (gatekeeper.code !== 0 || !gatekeeper.output.includes("Notarized Developer ID")) throw new Error(`Ghostty.app is not notarized:\n${gatekeeper.output}`)
  const commit = await ghosttyAppCommit(join(app, "Contents/MacOS/ghostty"))
  if (!matchesPinnedCommit(commit)) throw new Error(`Ghostty.app was built from ${commit ?? "an unknown commit"}, not ${PINNED_GHOSTTY_COMMIT}`)
  return commit!
}

if (process.platform !== "darwin") throw new Error("Ghostty.app is macOS-only")
if (existsSync(pinnedGhosttyApp)) {
  const commit = await verify(pinnedGhosttyApp)
  console.log(`Pinned Ghostty.app ${commit} (${PINNED_VT_TAG}) already present: ${pinnedGhosttyApp}`)
  process.exit(0)
}

await mkdir(pinnedGhosttyDirectory, { recursive: true })
const dmg = join(pinnedGhosttyDirectory, "Ghostty.dmg")
const response = await fetch(pinnedGhosttyDmgUrl)
if (!response.ok) throw new Error(`download failed (${response.status}): ${pinnedGhosttyDmgUrl}`)
const bytes = new Uint8Array(await response.arrayBuffer())
await Bun.write(dmg, bytes)
const sha256 = new Bun.CryptoHasher("sha256").update(bytes).digest("hex")

const mount = await mkdtemp(join(tmpdir(), "tap-ghostty-app-"))
try {
  const attach = await run(["hdiutil", "attach", "-nobrowse", "-readonly", "-mountpoint", mount, dmg])
  if (attach.code !== 0) throw new Error(attach.output)
  const copy = await run(["ditto", join(mount, "Ghostty.app"), pinnedGhosttyApp])
  if (copy.code !== 0) throw new Error(copy.output)
} finally {
  await run(["hdiutil", "detach", mount])
  await rmdir(mount).catch(() => undefined)
}

try {
  const commit = await verify(pinnedGhosttyApp)
  await writeFile(join(pinnedGhosttyDirectory, "RECEIPT.json"), `${JSON.stringify({
    component: "Ghostty.app", commit: PINNED_GHOSTTY_COMMIT, appCommit: commit, libghosttyVt: PINNED_VT_TAG,
    url: pinnedGhosttyDmgUrl, sha256, teamIdentifier: GHOSTTY_TEAM_ID, notarized: true, fetchedAt: new Date().toISOString(),
  }, null, 2)}\n`)
  console.log(`Fetched pinned Ghostty.app ${commit} (${PINNED_VT_TAG}), DMG sha256 ${sha256}`)
} catch (error) {
  await rm(pinnedGhosttyApp, { recursive: true, force: true })
  throw error
}
