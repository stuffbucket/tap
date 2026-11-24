#!/usr/bin/env bun

import { chmod, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import { resolve } from "node:path"

const ROOT = resolve(import.meta.dir, "..")
const CACHE = resolve(ROOT, ".cache")
const SOURCE = resolve(CACHE, "libghostty-vt-src")
const PREFIX = resolve(CACHE, "libghostty-vt-prefix")
const OUTPUT = resolve(ROOT, "native/bin/tap-vt-render")
const SHM_OUTPUT = resolve(ROOT, "native/bin/tap-shm-read")
const WASM_SOURCE = resolve(CACHE, "libghostty-vt-kitty-src")
const WASM_PREFIX = resolve(CACHE, "libghostty-vt-kitty-prefix")
const VERSION = "vt-1.0.1"

async function run(command: string[], options: { cwd?: string; capture?: boolean } = {}): Promise<string> {
  const process = Bun.spawn(command, {
    ...(options.cwd ? { cwd: options.cwd } : {}),
    stdin: "ignore",
    stdout: options.capture ? "pipe" : "inherit",
    stderr: "inherit",
  })
  const stdout = options.capture ? new Response(process.stdout).text() : Promise.resolve("")
  const exitCode = await process.exited
  if (exitCode !== 0) throw new Error(`${command[0]} exited with ${exitCode}`)
  return (await stdout).trim()
}

async function prefix(name: string): Promise<string | null> {
  try {
    return await run(["brew", "--prefix", name], { capture: true })
  } catch {
    return null
  }
}

await mkdir(CACHE, { recursive: true })
if (!existsSync(resolve(SOURCE, ".git"))) {
  await rm(SOURCE, { recursive: true, force: true })
  await run([
    "git",
    "clone",
    "--depth",
    "1",
    "--branch",
    VERSION,
    "https://github.com/ghostty-org/ghostty.git",
    SOURCE,
  ])
}

if (!existsSync(resolve(WASM_SOURCE, ".git"))) {
  await rm(WASM_SOURCE, { recursive: true, force: true })
  await run(["git", "clone", "--shared", SOURCE, WASM_SOURCE])
  const optionsPath = resolve(WASM_SOURCE, "src/terminal/build_options.zig")
  const imagePath = resolve(WASM_SOURCE, "src/terminal/kitty/graphics_image.zig")
  const options = (await readFile(optionsPath, "utf8")).replace(
    "        if (target.os.tag == .freestanding) return false;\n        return self.features.kitty_graphics;",
    "        _ = target;\n        return self.features.kitty_graphics;",
  )
  const images = (await readFile(imagePath, "utf8")).replace(
    "        switch (t.medium) {\n            .direct => unreachable, // handled above",
    "        if (comptime builtin.target.os.tag == .freestanding) {\n            return error.UnsupportedMedium;\n        } else switch (t.medium) {\n            .direct => unreachable, // handled above",
  )
  await writeFile(optionsPath, options)
  await writeFile(imagePath, images)
}

await rm(PREFIX, { recursive: true, force: true })
await run(
  [
    "mise",
    "x",
    "zig@0.17.1",
    "--",
    "zig",
    "build",
    "-Demit-lib-vt",
    "-Doptimize=ReleaseFast",
    "--prefix",
    PREFIX,
  ],
  { cwd: SOURCE },
)

await rm(WASM_PREFIX, { recursive: true, force: true })
await run(
  [
    "mise", "x", "zig@0.17.1", "--", "zig", "build",
    "-Demit-lib-vt", "-Dtarget=wasm32-freestanding", "-Doptimize=ReleaseSmall",
    "--prefix", WASM_PREFIX,
  ],
  { cwd: WASM_SOURCE },
)

const cairo = await prefix("cairo")
const libpng = await prefix("libpng")
let nativeFlags: string[]
if (cairo && libpng) {
  nativeFlags = [
    `-I${resolve(cairo, "include/cairo")}`,
    `-I${resolve(libpng, "include")}`,
    `-L${resolve(cairo, "lib")}`,
    "-lcairo",
    `-L${resolve(libpng, "lib")}`,
    "-lpng",
    // CoreText resolves font families and loads the bundled fonts (see load_faces in renderer.c).
    "-framework", "CoreText", "-framework", "CoreFoundation", "-framework", "CoreGraphics",
  ]
} else {
  const cflags = (await run(["pkg-config", "--cflags", "cairo", "libpng"], { capture: true })).split(/\s+/)
  const libs = (await run(["pkg-config", "--libs", "cairo", "libpng"], { capture: true })).split(/\s+/)
  nativeFlags = [...cflags, ...libs]
}

await mkdir(resolve(ROOT, "native/bin"), { recursive: true })
await run([
  "clang",
  "-std=c17",
  "-O2",
  "-Wall",
  "-Wextra",
  "-Werror",
  `-I${resolve(PREFIX, "include")}`,
  resolve(ROOT, "native/renderer.c"),
  resolve(PREFIX, "lib/libghostty-vt.a"),
  ...nativeFlags,
  "-lm",
  "-o",
  OUTPUT,
])
await chmod(OUTPUT, 0o755)
await run([
  "clang",
  "-std=c17",
  "-O2",
  "-Wall",
  "-Wextra",
  "-Werror",
  resolve(ROOT, "native/shm_read.c"),
  ...(process.platform === "linux" ? ["-lrt"] : []),
  "-o",
  SHM_OUTPUT,
])
await chmod(SHM_OUTPUT, 0o755)
const size = (await stat(OUTPUT)).size
console.log(`Built ${OUTPUT} (${(size / 1024 / 1024).toFixed(1)} MiB) against libghostty-vt 1.0.1`)
console.log(`Built ${SHM_OUTPUT} for bounded POSIX shared-memory capture`)
console.log(`Built ${resolve(WASM_PREFIX, "bin/ghostty-vt.wasm")} with direct Kitty graphics for browser playback`)
