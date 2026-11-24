#!/usr/bin/env bun

import { chmod, mkdir, readFile, rm, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import { resolve } from "node:path"
import { resolveGhosttyApp } from "../src/ghostty-app.ts"

const root = resolve(import.meta.dir, "..")
const base = resolve(root, ".cache/libghostty-vt-src")
const source = resolve(root, ".cache/ghostty-full-src")
const prefix = resolve(root, ".cache/ghostty-full-prefix")
const output = resolve(root, "native/bin/tap-ghostty-offscreen")

async function run(command: string[], cwd?: string) {
  const process = Bun.spawn(command, { ...(cwd ? { cwd } : {}), stdin: "ignore", stdout: "inherit", stderr: "inherit" })
  if ((await process.exited) !== 0) throw new Error(`${command[0]} failed`)
}

if (process.platform !== "darwin") throw new Error("full Ghostty IOSurface renderer is macOS-only")
if (!existsSync(resolve(base, ".git"))) throw new Error("run bun run build:native first")
// The host dlopens Ghostty.app at run time (see src/ghostty-app.ts), so it only needs headers here.
const stockGhostty = resolveGhosttyApp().executable
if (existsSync(stockGhostty)) {
  await run([
    "clang", "-fobjc-arc", "-fblocks", "-O2",
    `-I${resolve(base, "include")}`,
    resolve(root, "native/ghostty_offscreen.m"),
    "-o", output,
    "-framework", "AppKit", "-framework", "ImageIO", "-framework", "IOSurface",
    "-framework", "Carbon", "-framework", "Metal", "-framework", "QuartzCore", "-framework", "CoreVideo",
  ])
  await chmod(output, 0o755)
  console.log(`Built stock Ghostty.app IOSurface host: ${output}`)
  process.exit(0)
}
if (!existsSync(resolve(source, ".git"))) {
  await rm(source, { recursive: true, force: true })
  await run(["git", "clone", "--shared", base, source])
  const buildPath = resolve(source, "build.zig")
  const depsPath = resolve(source, "src/build/SharedDeps.zig")
  const shadersPath = resolve(source, "src/renderer/metal/shaders.zig")
  let build = await readFile(buildPath, "utf8")
  build = build.replace(
    `        // We shouldn't have this guard but we don't currently\n        // build on macOS this way ironically so we need to fix that.\n        if (!config.target.result.os.tag.isDarwin()) {\n            lib_shared.installHeader(); // Only need one header\n            if (config.target.result.os.tag == .windows) {\n                lib_shared.install("ghostty-internal.dll");\n                lib_static.install("ghostty-internal-static.lib");\n            } else {\n                lib_shared.install("ghostty-internal.so");\n                lib_static.install("ghostty-internal.a");\n            }\n        }`,
    `        lib_shared.installHeader();\n        if (config.target.result.os.tag.isDarwin()) {\n            lib_static.install("libghostty-internal.a");\n        } else if (config.target.result.os.tag == .windows) {\n            lib_shared.install("ghostty-internal.dll");\n            lib_static.install("ghostty-internal-static.lib");\n        } else {\n            lib_shared.install("ghostty-internal.so");\n            lib_static.install("libghostty-internal.a");\n        }`,
  )
  let deps = await readFile(depsPath, "utf8")
  deps = deps.replace(/if \(step\.rootModuleTarget\(\)\.os\.tag\.isDarwin\(\)\) \{[\s\S]*?\n    \}/,
    `if (step.rootModuleTarget().os.tag.isDarwin()) {\n        try @import("apple_sdk").addPaths(b, step);\n    }`)
  let shaders = await readFile(shadersPath, "utf8")
  shaders = shaders.replace(
    /const data = try macos\.dispatch\.Data\.create\([\s\S]*?try checkError\(err\);/,
    `const source = try macos.foundation.String.createWithBytes(\n        @embedFile("..\/shaders\/shaders.metal"), .utf8, false,\n    );\n    defer source.release();\n    var err: ?*anyopaque = null;\n    const library = device.msgSend(\n        objc.Object,\n        objc.sel("newLibraryWithSource:options:error:"),\n        .{ source, @as(?*anyopaque, null), &err },\n    );\n    try checkError(err);`,
  )
  await writeFile(buildPath, build)
  await writeFile(depsPath, deps)
  await writeFile(shadersPath, shaders)
}

await rm(prefix, { recursive: true, force: true })
await mkdir(prefix, { recursive: true })
await run([
  "mise", "x", "zig@0.17.1", "--", "zig", "build",
  "--prefix", prefix,
  "-Demit-xcframework=false", "-Demit-macos-app=false", "-Demit-exe=false",
  "-Demit-docs=false", "-Demit-themes=false", "-Demit-terminfo=false", "-Demit-termcap=false",
  "-Doptimize=ReleaseFast", "-Dsentry=false",
], source)
await run([
  "clang", "-fobjc-arc", "-fblocks", "-O2",
  `-I${resolve(prefix, "include")}`,
  resolve(root, "native/ghostty_offscreen.m"), resolve(prefix, "lib/libghostty-internal.a"),
  "-o", output,
  "-framework", "AppKit", "-framework", "ImageIO", "-framework", "IOSurface",
  "-framework", "Carbon", "-framework", "Metal", "-framework", "QuartzCore",
  "-framework", "CoreVideo", "-lstdc++",
])
await chmod(output, 0o755)
console.log(`Built actual Ghostty Metal/IOSurface renderer: ${output}`)
