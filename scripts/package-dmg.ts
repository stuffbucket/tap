#!/usr/bin/env bun
// Builds and verifies Tap's DMG locally, mirroring stuffbucket/macos-builder — without signing.
//
//   bun run package:dmg --dry-run
//
// Release builds are signed, notarized and stapled only by the macOS builder, from
// .macos-builder/config and .macos-builder/build.sh. The dry run proves that DMG will work first:
//
//   1. the producer builds dist/Tap.app (scripts/build-app.ts, as .macos-builder/build.sh runs it);
//   2. the builder's tail runs with the ad-hoc identity "-": every nested bundle and Mach-O is
//      signed deepest-first with the hardened runtime and the config's sign_walk profile, the app
//      is sealed top-level without --deep, and the DMG holds Tap.app plus an Applications link;
//   3. notarization and stapling are skipped (they need the builder's Apple credentials);
//   4. scripts/verify-dmg.ts mounts the DMG and runs Tap from it under the hardened runtime.

import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import { tmpdir } from "node:os"
import { basename, join, resolve } from "node:path"
import { TAP_VERSION } from "../src/version.ts"
import { dmgName, readBuilderConfig } from "./macos-builder-config.ts"

const root = resolve(import.meta.dir, "..")
const dryRun = process.argv.includes("--dry-run")
if (!dryRun) {
  console.error(
    "Signed release DMGs are built by stuffbucket/macos-builder from .macos-builder/config;\n" +
    "Developer ID signing, notarization and stapling never run locally.\n" +
    "To build and verify the same DMG with an ad-hoc identity: bun run package:dmg --dry-run",
  )
  process.exit(2)
}

async function run(command: string[], capture = false): Promise<string> {
  const child = Bun.spawn(command, { stdin: "ignore", stdout: capture ? "pipe" : "inherit", stderr: "inherit" })
  const output = capture ? new Response(child.stdout).text() : Promise.resolve("")
  if ((await child.exited) !== 0) throw new Error(`${command.slice(0, 2).join(" ")} failed`)
  return (await output).trim()
}

const config = await readBuilderConfig()
if (!config.artifact.split(",").map((value) => value.trim()).includes("dmg")) throw new Error(".macos-builder/config does not request a dmg artifact")
if (config.sign_nested) throw new Error("the dry run mirrors sign_walk only; .macos-builder/config uses sign_nested")

// Entitlement profiles are the builder's, referenced by name. Tap vendors the ones it uses; when a
// builder checkout is available, refuse to dry-run against a copy that has drifted from it.
const profile = (name: string) => {
  const path = resolve(root, `scripts/dmg/${name}.entitlements`)
  if (!existsSync(path)) throw new Error(`no vendored copy of the builder's "${name}" entitlements at ${path}`)
  return path
}
const builderCheckout = process.env.MACOS_BUILDER_DIR ?? resolve(root, "../macos-builder")
for (const name of new Set([config.entitlements, config.sign_walk].filter(Boolean) as string[])) {
  const upstream = resolve(builderCheckout, `entitlements/${name}.entitlements`)
  if (existsSync(upstream) && (await readFile(upstream, "utf8")) !== (await readFile(profile(name), "utf8"))) {
    throw new Error(`scripts/dmg/${name}.entitlements differs from ${upstream}; copy the builder's profile`)
  }
}

console.log("== 1. producer: build Tap.app")
await run([process.execPath, resolve(root, "scripts/build-app.ts"), `v${TAP_VERSION}`])
const app = resolve(root, config.app_path)

const identity = "-"
const work = await mkdtemp(join(tmpdir(), "tap-package-"))
try {
  if (config.sign_walk) {
    console.log(`== 2. sign_walk (${config.sign_walk}, ad-hoc, deepest-first)`)
    const targets: string[] = []
    targets.push(...(await run(["find", app, "-mindepth", "1", "(", "-name", "*.app", "-o", "-name", "*.framework", ")", "-type", "d", "-print"], true)).split("\n").filter(Boolean))
    for (const path of (await run(["find", app, "-mindepth", "1", "-type", "f", "(", "-perm", "-111", "-o", "-name", "*.dylib", "-o", "-name", "*.so", "-o", "-name", "*.node", ")", "-print"], true)).split("\n").filter(Boolean)) {
      if ((await run(["file", "--brief", "--mime-type", path], true)) === "application/x-mach-binary") targets.push(path)
    }
    const ordered = targets.filter((path) => !path.includes("/Versions/Current/")).sort((a, b) => b.split("/").length - a.split("/").length)
    console.log(`walk: ${ordered.length} nested targets`)
    for (const target of ordered) await run(["codesign", "--force", "--options", "runtime", "--entitlements", profile(config.sign_walk), "--sign", identity, target])
  }

  console.log(`== 2. top-level seal (${config.entitlements}, no --deep)`)
  await run(["codesign", "--force", "--options", "runtime", "--identifier", config.bundle_id, "--entitlements", profile(config.entitlements), "--sign", identity, app])
  await run(["codesign", "--verify", "--strict", "--verbose=2", app])
  if (config.sign_walk) await run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", app])

  const name = dmgName(config, TAP_VERSION)
  const dist = resolve(root, "dist")
  const dmg = resolve(dist, name)
  console.log(`== 3. dmg ${name} (notarization and stapling skipped: dry run)`)
  const stage = join(work, "dmg-stage")
  await mkdir(stage, { recursive: true })
  await cp(app, join(stage, basename(app)), { recursive: true, verbatimSymlinks: true })
  await symlink("/Applications", join(stage, "Applications"))
  await rm(dmg, { force: true })
  await run(["hdiutil", "create", "-volname", config.volname, "-srcfolder", stage, "-ov", "-format", "UDZO", dmg])
  await run(["codesign", "--force", "--sign", identity, dmg])

  const sha256 = new Bun.CryptoHasher("sha256").update(await Bun.file(dmg).arrayBuffer()).digest("hex")
  await writeFile(`${dmg}.sha256`, `${sha256}  ${name}\n`)
  await writeFile(resolve(dist, name.replace(/\.dmg$/, ".json")), `${JSON.stringify({
    name: "Tap", version: TAP_VERSION, file: name, architecture: "arm64", minimumMacOS: "26.0",
    bundleId: config.bundle_id, size: Bun.file(dmg).size, sha256,
    signing: "ad-hoc", hardenedRuntime: true, notarized: false, dryRun: true,
  }, null, 2)}\n`)

  console.log("== 4. verify the DMG")
  await run([process.execPath, resolve(root, "scripts/verify-dmg.ts"), dmg])
  console.log(`Dry run passed: ${dmg}\nSHA-256 ${sha256}\nRelease: tag v${TAP_VERSION} and let stuffbucket/macos-builder sign, notarize and staple.`)
} finally {
  await rm(work, { recursive: true, force: true })
}
