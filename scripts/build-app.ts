#!/usr/bin/env bun
// Builds dist/Tap.app — Tap's producer for stuffbucket/macos-builder.
//
//   bun scripts/build-app.ts [vX.Y.Z]
//
// Like every macos-builder producer this only BUILDS the app: it never uses a Developer ID,
// never builds a DMG and never notarizes. Binaries it rewrites are ad-hoc signed so the app runs
// as-is; the builder re-signs everything (see .macos-builder/config), and `bun run package:dmg
// --dry-run` mirrors that locally.

import { chmod, copyFile, cp, mkdir, rm, symlink, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import { basename, resolve } from "node:path"
import { pinnedGhosttyApp, pinnedGhosttyDirectory } from "../src/ghostty-app.ts"
import { TAP_VERSION } from "../src/version.ts"
import { readBuilderConfig } from "./macos-builder-config.ts"

const root = resolve(import.meta.dir, "..")
const config = await readBuilderConfig()
const app = resolve(root, config.app_path)
const contents = resolve(app, "Contents")
const resources = resolve(contents, "Resources/tap")
const bin = resolve(resources, "bin")
const lib = resolve(resources, "lib")
const licenses = resolve(resources, "licenses")

const requested = process.argv[2]?.replace(/^v/, "")
if (requested && requested !== TAP_VERSION) throw new Error(`release tag ${process.argv[2]} does not match package.json version ${TAP_VERSION}`)

async function run(command: string[], capture = false): Promise<string> {
  const child = Bun.spawn(command, { stdin: "ignore", stdout: capture ? "pipe" : "inherit", stderr: "inherit" })
  const output = capture ? new Response(child.stdout).text() : Promise.resolve("")
  if ((await child.exited) !== 0) throw new Error(`${command[0]} failed`)
  return (await output).trim()
}

const ffmpegPath = "/opt/homebrew/opt/ffmpeg/bin/ffmpeg"
const tmuxPath = "/opt/homebrew/bin/tmux"
for (const required of [ffmpegPath, tmuxPath, "/opt/homebrew/opt/cairo/lib/libcairo.2.dylib", "/opt/homebrew/opt/libpng/lib/libpng16.16.dylib"]) {
  if (!existsSync(required)) throw new Error(`missing build dependency: ${required} (brew install ffmpeg tmux cairo libpng)`)
}
for (const built of ["native/bin/tap-vt-render", "native/bin/tap-vhs-parse", "native/bin/tap-shm-read", "native/bin/tap-ghostty-offscreen", ".cache/libghostty-vt-kitty-prefix/bin/ghostty-vt.wasm"]) {
  if (!existsSync(resolve(root, built))) throw new Error(`missing ${built}; run bun run build first`)
}

await run([process.execPath, resolve(root, "scripts/generate-notices.ts")])
await rm(app, { recursive: true, force: true })
for (const directory of [bin, lib, licenses, resolve(contents, "MacOS")]) await mkdir(directory, { recursive: true })

for (const path of ["src", "web", "native", "docs", "examples", "assets", "node_modules"]) {
  await cp(resolve(root, path), resolve(resources, path), { recursive: true, filter: (source) => !source.includes("native/bin") })
}
for (const file of ["package.json", "bun.lock", "README.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "THIRDPARTYNOTICES"]) {
  await copyFile(resolve(root, file), resolve(resources, file))
}
await copyFile(resolve(root, "assets/Tap.icns"), resolve(contents, "Resources/Tap.icns"))
await mkdir(resolve(resources, "native/bin"), { recursive: true })
for (const helper of ["tap-vt-render", "tap-vhs-parse", "tap-shm-read", "tap-ghostty-offscreen"]) {
  await copyFile(resolve(root, "native/bin", helper), resolve(resources, "native/bin", helper))
  await chmod(resolve(resources, "native/bin", helper), 0o755)
}
await mkdir(resolve(resources, ".cache/libghostty-vt-kitty-prefix/bin"), { recursive: true })
await copyFile(resolve(root, ".cache/libghostty-vt-kitty-prefix/bin/ghostty-vt.wasm"), resolve(resources, ".cache/libghostty-vt-kitty-prefix/bin/ghostty-vt.wasm"))

const bunPath = await run(["mise", "x", "bun@1.4.3", "--", "which", "bun"], true)
await copyFile(bunPath, resolve(bin, "bun")); await chmod(resolve(bin, "bun"), 0o755)
for (const [source, name] of [[ffmpegPath, "ffmpeg"], [tmuxPath, "tmux"]] as const) {
  await copyFile(source, resolve(bin, name)); await chmod(resolve(bin, name), 0o755)
}

// Ghostty's official build of the exact commit libghostty-vt is pinned to, with its provenance.
await run([process.execPath, resolve(root, "scripts/fetch-ghostty-app.ts")])
await cp(resolve(pinnedGhosttyApp, "Contents"), resolve(resources, "ghostty-stock/Contents"), { recursive: true })
await copyFile(resolve(pinnedGhosttyDirectory, "RECEIPT.json"), resolve(licenses, "Ghostty.app-RECEIPT.json"))
await rm(resolve(resources, "node_modules/.bin"), { recursive: true, force: true })
const sparkle = resolve(resources, "ghostty-stock/Contents/Frameworks/Sparkle.framework")
for (const [name, target] of [
  ["Resources", "Versions/Current/Resources"], ["Autoupdate", "Versions/Current/Autoupdate"],
  ["Updater.app", "Versions/Current/Updater.app"], ["XPCServices", "Versions/Current/XPCServices"],
  ["Sparkle", "Versions/Current/Sparkle"], ["Versions/Current", "B"],
] as const) {
  await rm(resolve(sparkle, name), { recursive: true, force: true })
  await symlink(target, resolve(sparkle, name))
}

// Copy Homebrew dylibs into lib/ and point every load command at them with @loader_path, so the
// app needs no DYLD_* environment (which the hardened runtime would ignore).
const bundled = new Map<string, string>()
interface MachDependency { loadPath: string; sourcePath: string }
async function dependencies(path: string): Promise<MachDependency[]> {
  const output = await run(["otool", "-L", path], true)
  const result: MachDependency[] = []
  for (const loadPath of output.split("\n").slice(1).map((line) => line.trim().split(" ")[0]!).filter(Boolean)) {
    if (loadPath.startsWith("/opt/homebrew/") || loadPath.startsWith("/usr/local/")) result.push({ loadPath, sourcePath: loadPath })
    else if (loadPath.startsWith("@rpath/")) {
      const sourcePath = resolve("/opt/homebrew/lib", basename(loadPath))
      if (existsSync(sourcePath)) result.push({ loadPath, sourcePath })
    }
  }
  return result
}
async function bundleMachO(path: string, isLibrary = false, loaderLib = "@loader_path/../lib"): Promise<void> {
  for (const dependency of await dependencies(path)) {
    const name = basename(dependency.sourcePath)
    let target = bundled.get(dependency.sourcePath)
    if (!target) {
      target = resolve(lib, name)
      bundled.set(dependency.sourcePath, target)
      await copyFile(dependency.sourcePath, target)
      await chmod(target, 0o755)
      await bundleMachO(target, true)
      await run(["install_name_tool", "-id", `@loader_path/${name}`, target])
    }
    await run(["install_name_tool", "-change", dependency.loadPath, isLibrary ? `@loader_path/${name}` : `${loaderLib}/${name}`, path])
  }
}
await bundleMachO(resolve(bin, "ffmpeg"))
await bundleMachO(resolve(bin, "tmux"))
await bundleMachO(resolve(resources, "native/bin/tap-vt-render"), false, "@loader_path/../../lib")

const fonts = resolve(resources, "fonts")
await mkdir(fonts, { recursive: true })
const termlessFonts = resolve(resources, "node_modules/@termless/core/assets/fonts")
if (existsSync(termlessFonts)) await cp(termlessFonts, fonts, { recursive: true })
// prefix="relative" resolves the font directory next to this file, wherever the app is installed.
await writeFile(resolve(resources, "fonts.conf"), `<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "fonts.dtd"><fontconfig><dir prefix="relative">fonts</dir><cachedir>/tmp/tap-font-cache</cachedir></fontconfig>\n`)

const tapLauncher = `#!/bin/zsh\nROOT="\${0:A:h:h}"\nexport TAP_USER_PATH="$PATH"\nexport PATH="$ROOT/bin:$PATH"\nexport FONTCONFIG_FILE="$ROOT/fonts.conf"\nexport TAP_GHOSTTY_STOCK="$ROOT/ghostty-stock/Contents/MacOS/ghostty"\nexport TAP_GHOSTTY_FRAMEWORKS="$ROOT/ghostty-stock/Contents/Frameworks"\nexec "$ROOT/bin/bun" "$ROOT/src/cli.ts" "$@"\n`
await writeFile(resolve(bin, "tap"), tapLauncher); await chmod(resolve(bin, "tap"), 0o755)
await copyFile(resolve(root, "scripts/dmg/launcher.zsh"), resolve(contents, "MacOS/Tap")); await chmod(resolve(contents, "MacOS/Tap"), 0o755)
await copyFile(resolve(root, "scripts/dmg/welcome.applescript"), resolve(resources, "welcome.applescript"))
const plist: Record<string, string | boolean> = {
  CFBundleName: "Tap", CFBundleDisplayName: "Tap", CFBundleIdentifier: config.bundle_id,
  CFBundleVersion: TAP_VERSION, CFBundleShortVersionString: TAP_VERSION, CFBundleExecutable: "Tap",
  CFBundlePackageType: "APPL", CFBundleIconFile: "Tap.icns", LSMinimumSystemVersion: "26.0", LSUIElement: false,
  NSAppleEventsUsageDescription: "Tap opens a Terminal window to run its first-run check and quickstart.",
}
const plistBody = Object.entries(plist).map(([key, value]) => `<key>${key}</key>${typeof value === "boolean" ? `<${value}/>` : `<string>${value}</string>`}`).join("")
await writeFile(resolve(contents, "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>${plistBody}</dict></plist>\n`)

for (const [source, name] of [
  [resolve(root, "LICENSE"), "Tap-MIT.txt"],
  [resolve(root, ".cache/libghostty-vt-src/LICENSE"), "Ghostty-MIT.txt"],
  [resolve(root, ".cache/vhs-upstream-0.12.1/LICENSE"), "VHS-MIT.txt"],
  [resolve(root, "node_modules/@termless/core/LICENSE"), "Termless-MIT.txt"],
  ["/opt/homebrew/opt/libpng/LICENSE", "libpng.txt"],
] as const) if (existsSync(source)) await copyFile(source, resolve(licenses, name))
if (existsSync(resolve(root, "node_modules/@termless/ghostty/dist/ghostty-web-RECEIPT.json"))) await copyFile(resolve(root, "node_modules/@termless/ghostty/dist/ghostty-web-RECEIPT.json"), resolve(licenses, "ghostty-web-RECEIPT.json"))
await copyFile(resolve(root, "THIRDPARTYNOTICES"), resolve(licenses, "THIRDPARTYNOTICES"))
for (const [source, name] of [
  [resolve(root, "node_modules/@napi-rs/canvas-darwin-arm64/sbom.cdx.json"), "canvas-sbom.cdx.json"],
  ["/opt/homebrew/Cellar/ffmpeg/9.0.2_1/sbom.spdx.json", "ffmpeg-sbom.spdx.json"],
  [resolve(root, "native/vhsparse/go.sum"), "vhs-parser-go.sum"],
  [resolve(root, "bun.lock"), "tap-bun.lock"],
] as const) if (existsSync(source)) await copyFile(source, resolve(licenses, name))
const ffmpegBuild = await run([ffmpegPath, "-buildconf"], true)
await writeFile(resolve(licenses, "BUNDLED-COMPONENTS.txt"), `Tap ${TAP_VERSION} arm64\n\nFFmpeg build configuration:\n${ffmpegBuild}\n\nBundled dylibs:\n${[...bundled.keys()].sort().join("\n")}\n`)

// Rewritten binaries lost their signatures; ad-hoc sign every Mach-O (outside Ghostty.app, which
// keeps its own) so the bundle runs. The builder replaces all of these with Developer ID signatures.
for (const path of (await run(["find", resources, "-type", "f", "-not", "-path", "*/ghostty-stock/*"], true)).split("\n").filter(Boolean)) {
  if ((await run(["file", "-b", "--mime-type", path], true)) === "application/x-mach-binary") await run(["codesign", "--force", "--sign", "-", path])
}
console.log(`Built ${app} (Tap ${TAP_VERSION}, ${config.bundle_id})`)
