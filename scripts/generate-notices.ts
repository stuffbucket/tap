#!/usr/bin/env bun

import { mkdir, readFile, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import { resolve } from "node:path"

const root = resolve(import.meta.dir, "..")
const cache = resolve(root, ".cache/licenses")
await mkdir(cache, { recursive: true })

async function download(name: string, url: string): Promise<string> {
  const path = resolve(cache, name)
  if (!existsSync(path)) {
    const response = await fetch(url)
    if (!response.ok) throw new Error(`license download failed: ${url}`)
    await writeFile(path, await response.text())
  }
  return path
}

const bunLicense = await download("Bun-LICENSE.txt", "https://raw.githubusercontent.com/oven-sh/bun/main/LICENSE.md")
const ofl = await download("SIL-OFL-1.1.txt", "https://raw.githubusercontent.com/JetBrains/JetBrainsMono/master/OFL.txt")
const skia = await download("Skia-LICENSE.txt", "https://raw.githubusercontent.com/google/skia/main/LICENSE")

const components: Array<{ name: string; source: string; license: string; files: string[]; note?: string }> = [
  { name: "Ghostty and libghostty-vt", source: "https://github.com/ghostty-org/ghostty", license: "MIT", files: [resolve(root, ".cache/libghostty-vt-src/LICENSE")] },
  { name: "Charmbracelet VHS", source: "https://github.com/charmbracelet/vhs", license: "MIT", files: [resolve(root, ".cache/vhs-upstream-0.12.1/LICENSE")] },
  { name: "Termless", source: "https://github.com/beorn/termless", license: "MIT", files: [resolve(root, "node_modules/@termless/core/LICENSE")] },
  { name: "ghostty-web", source: "https://github.com/coder/ghostty-web", license: "MIT", files: [resolve(root, "node_modules/@termless/ghostty/node_modules/ghostty-web/LICENSE")] },
  { name: "Commander.js", source: "https://github.com/tj/commander.js", license: "MIT", files: [resolve(root, "node_modules/commander/LICENSE")] },
  { name: "@napi-rs/canvas", source: "https://github.com/Brooooooklyn/canvas", license: "MIT", files: [resolve(root, "node_modules/@napi-rs/canvas/LICENSE"), skia], note: "Includes Google Skia and its third-party components; see the packaged node module for its complete notices." },
  { name: "Bun runtime", source: "https://github.com/oven-sh/bun", license: "MIT", files: [bunLicense] },
  { name: "Cairo", source: "https://cairographics.org", license: "LGPL-2.1-only OR MPL-1.1", files: ["/opt/homebrew/Cellar/cairo/1.18.6/COPYING", "/opt/homebrew/Cellar/cairo/1.18.6/COPYING-LGPL-2.1", "/opt/homebrew/Cellar/cairo/1.18.6/COPYING-MPL-1.1"] },
  { name: "libpng", source: "http://www.libpng.org/pub/png/libpng.html", license: "libpng-2.0", files: ["/opt/homebrew/Cellar/libpng/1.6.59/LICENSE"] },
  { name: "FFmpeg", source: "https://ffmpeg.org", license: "GPL-3.0-or-later for the bundled build", files: ["/opt/homebrew/Cellar/ffmpeg/9.0.2_1/LICENSE.md", "/opt/homebrew/Cellar/ffmpeg/9.0.2_1/COPYING.GPLv3"], note: "Bundled build configuration enables --enable-gpl, --enable-version3, libx264 and libx265. Corresponding source: https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz and the linked codec sources listed by Homebrew's ffmpeg formula." },
  { name: "tmux", source: "https://github.com/tmux/tmux", license: "ISC", files: ["/opt/homebrew/Cellar/tmux/3.8/COPYING"] },
  { name: "JetBrains Mono, Noto fonts", source: "https://github.com/JetBrains/JetBrainsMono", license: "SIL-OFL-1.1", files: [ofl], note: "Font provenance is listed in node_modules/@termless/core/assets/fonts/README.md." },
]

let output = `THIRD-PARTY SOFTWARE NOTICES AND INFORMATION\n\nTap incorporates or redistributes the components below. This file is informational and does not modify their licenses. Complete corresponding license texts follow each entry.\n\n`
for (const component of components) {
  output += `${"=".repeat(78)}\n${component.name}\nSource: ${component.source}\nLicense: ${component.license}\n`
  if (component.note) output += `Notice: ${component.note}\n`
  for (const file of component.files) {
    if (!existsSync(file)) throw new Error(`required license text missing: ${file}`)
    output += `\n--- ${file.split("/").at(-1)} ---\n${await readFile(file, "utf8")}\n`
  }
  output += "\n"
}
output += `${"=".repeat(78)}\nBuild receipts\n\nThe release also carries bun.lock, native/vhsparse/go.sum, and ghostty-web-RECEIPT.json. A machine-readable inventory of bundled Mach-O libraries is generated while packaging.\n`
await writeFile(resolve(root, "THIRDPARTYNOTICES"), output)
console.log(`Generated THIRDPARTYNOTICES with ${components.length} component notices`)
