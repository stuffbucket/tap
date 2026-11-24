#!/usr/bin/env bun
// Mounts a Tap DMG and runs Tap from it.
//
//   bun scripts/verify-dmg.ts [path/to/Tap-X.Y.Z-arm64.dmg]
//
// Works on the dry-run DMG (ad-hoc, hardened runtime) and on the signed DMG the macOS builder
// produces; for a Developer ID signed DMG it also requires a stapled ticket and Gatekeeper approval.

import { mkdir, mkdtemp, readFile, readdir, rm } from "node:fs/promises"
import { existsSync } from "node:fs"
import { basename, join, resolve } from "node:path"
import { tmpdir } from "node:os"
import { PINNED_GHOSTTY_COMMIT, PINNED_VT_TAG, ghosttyAppCommit, matchesPinnedCommit } from "../src/ghostty-app.ts"
import { TAP_VERSION } from "../src/version.ts"
import { dmgName, readBuilderConfig } from "./macos-builder-config.ts"

const root = resolve(import.meta.dir, "..")
const config = await readBuilderConfig()
const dmg = resolve(process.argv[2] ?? resolve(root, "dist", dmgName(config, TAP_VERSION)))
if (!existsSync(dmg)) throw new Error(`no DMG at ${dmg}; run bun run package:dmg --dry-run`)
const sha256 = new Bun.CryptoHasher("sha256").update(await Bun.file(dmg).arrayBuffer()).digest("hex")

// Checksums written next to the DMG (by the dry run or the builder) must match it.
const checksumFile = `${dmg}.sha256`
if (existsSync(checksumFile) && !(await readFile(checksumFile, "utf8")).startsWith(sha256)) throw new Error(`${basename(checksumFile)} does not match the DMG`)
const manifestPath = dmg.replace(/\.dmg$/, ".json")
if (existsSync(manifestPath)) {
  const manifest = JSON.parse(await readFile(manifestPath, "utf8")) as { sha256: string; size: number; version: string }
  if (manifest.sha256 !== sha256 || manifest.size !== Bun.file(dmg).size || manifest.version !== TAP_VERSION) throw new Error("release manifest does not match the DMG")
}

const work = await mkdtemp(join(tmpdir(), "tap-dmg-verify-"))
const mount = join(work, "Tap")
await mkdir(mount)

async function run(command: string[], capture = false): Promise<string> {
  const child = Bun.spawn(command, { stdin: "ignore", stdout: capture ? "pipe" : "inherit", stderr: capture ? "pipe" : "inherit" })
  const [stdout, stderr] = capture ? await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text()]) : ["", ""]
  if ((await child.exited) !== 0) throw new Error(`${command.join(" ")} failed\n${stderr}`)
  return (stdout + stderr).trim()
}
const signatureOf = (path: string) => run(["codesign", "-dv", "--verbose=2", path], true)

try {
  await run(["hdiutil", "verify", dmg])
  const developerId = /Authority=Developer ID Application/.test(await signatureOf(dmg))
  if (developerId) {
    await run(["xcrun", "stapler", "validate", dmg])
    await run(["spctl", "--assess", "--type", "open", "--context", "context:primary-signature", "-vv", dmg])
  }
  await run(["hdiutil", "attach", "-nobrowse", "-readonly", "-mountpoint", mount, dmg])
  const visibleRootItems = (await readdir(mount)).filter((name) => !name.startsWith(".")).sort()
  if (JSON.stringify(visibleRootItems) !== JSON.stringify(["Applications", "Tap.app"])) {
    throw new Error(`unexpected visible DMG root items: ${visibleRootItems.join(", ")}`)
  }
  const app = join(mount, "Tap.app")
  const resources = join(app, "Contents/Resources/tap")
  const tap = join(resources, "bin/tap")

  await run(["codesign", "--verify", "--deep", "--strict", app])
  if (developerId) await run(["spctl", "--assess", "--type", "execute", "-vv", app])
  const appSignature = await signatureOf(app)
  if (!appSignature.includes(`Identifier=${config.bundle_id}`)) throw new Error(`Tap.app is not signed as ${config.bundle_id}`)
  // Notarization requires the hardened runtime; check it is on, so the smoke tests run under it.
  for (const executable of ["bin/bun", "bin/ffmpeg", "bin/tmux", "native/bin/tap-ghostty-offscreen", "native/bin/tap-vt-render", "native/bin/tap-vhs-parse", "ghostty-stock/Contents/MacOS/ghostty"]) {
    if (!/flags=0x\w+\([^)]*runtime/.test(await signatureOf(join(resources, executable)))) throw new Error(`${executable} is not signed with the hardened runtime`)
  }
  const plist = await readFile(join(app, "Contents/Info.plist"), "utf8")
  if (!plist.includes(`<string>${TAP_VERSION}</string>`) || !plist.includes(`<string>${config.bundle_id}</string>`)) throw new Error("Info.plist version or bundle id is wrong")
  const fontsConf = await readFile(join(resources, "fonts.conf"), "utf8")
  if (!fontsConf.includes('prefix="relative"')) throw new Error("fonts.conf must locate fonts relative to the installed app")

  // The double-click path: the launcher hands a compilable AppleScript to osascript.
  await run(["osacompile", "-o", join(work, "welcome.scpt"), join(resources, "welcome.applescript")])

  const version = await run([tap, "--version"], true)
  if (version !== TAP_VERSION) throw new Error(`unexpected version: ${version}`)
  const doctor = await run([tap, "doctor"], true)
  if ((doctor.match(/^✓/gm) ?? []).length !== 7) throw new Error(`doctor failed:\n${doctor}`)
  const quickstart = await run([tap, "quickstart"], true)
  if (!quickstart.includes("tap capture") || !quickstart.includes("Tap does not upload")) throw new Error("quickstart is incomplete")
  // install-cli links the CLI onto PATH; the linked command must run Tap from the app.
  const cliDir = join(work, "bin")
  await run([tap, "install-cli", "--dir", cliDir], true)
  if ((await run([join(cliDir, "tap"), "--version"], true)) !== TAP_VERSION) throw new Error("the installed tap link does not run Tap")
  const noticeText = await readFile(await run([tap, "licenses", "--path"], true), "utf8")
  if (!noticeText.includes("THIRD-PARTY SOFTWARE NOTICES") || !noticeText.includes("FFmpeg")) throw new Error("notices are incomplete")

  const project = join(work, "smoke.tap")
  const outputs = [project, join(work, "smoke.png"), join(work, "smoke.mp4"), join(work, "ghostty.png"), join(work, "tape.tap")]
  await run([tap, "capture", "--no-interactive", "-o", project, "--", "/bin/sh", "-lc", "printf 'dmg-verify\\n'"])
  await run([tap, "screenshot", project, "-o", outputs[1]!])
  await run([tap, "render", project, "-o", outputs[2]!, "--fps", "30"])
  await run([tap, "ghostty-screenshot", project, "-o", outputs[3]!])
  const tape = join(work, "smoke.tape")
  await Bun.write(tape, 'Type "echo tape-ok"\nEnter\nSleep 200ms\n')
  await run([tap, "tape", tape, "-o", outputs[4]!])
  for (const path of outputs) if (!(await Bun.file(path).exists())) throw new Error(`missing output: ${path}`)

  if (!matchesPinnedCommit(await ghosttyAppCommit(join(resources, "ghostty-stock/Contents/MacOS/ghostty")))) throw new Error(`bundled Ghostty.app is not commit ${PINNED_GHOSTTY_COMMIT}`)
  if (!existsSync(join(resources, "licenses/Ghostty.app-RECEIPT.json"))) throw new Error("Ghostty.app receipt is missing")

  console.log(`DMG verification passed (${developerId ? "Developer ID, notarized" : "ad-hoc dry run"}): ${basename(dmg)} — signature, hardened runtime, doctor, notices, capture, VHS tape, portable render, FFmpeg, Ghostty.app ${PINNED_GHOSTTY_COMMIT.slice(0, 9)} (${PINNED_VT_TAG})`)
} finally {
  await run(["hdiutil", "detach", "-force", mount]).catch(() => undefined)
  await rm(work, { recursive: true, force: true })
}
