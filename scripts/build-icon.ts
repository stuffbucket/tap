#!/usr/bin/env bun

import { mkdir, readFile, rm, writeFile } from "node:fs/promises"
import { resolve } from "node:path"
import { Resvg } from "@resvg/resvg-js"

const root = resolve(import.meta.dir, "..")
const svg = await readFile(resolve(root, "assets/icon.svg"), "utf8")
const iconset = resolve(root, ".cache/Tap.iconset")
await rm(iconset, { recursive: true, force: true })
await mkdir(iconset, { recursive: true })

const entries: Array<[string, number]> = [
  ["icon_16x16.png", 16], ["icon_16x16@2x.png", 32],
  ["icon_32x32.png", 32], ["icon_32x32@2x.png", 64],
  ["icon_128x128.png", 128], ["icon_128x128@2x.png", 256],
  ["icon_256x256.png", 256], ["icon_256x256@2x.png", 512],
  ["icon_512x512.png", 512], ["icon_512x512@2x.png", 1024],
]
for (const [name, size] of entries) {
  const png = new Resvg(svg, { fitTo: { mode: "width", value: size } }).render().asPng()
  await writeFile(resolve(iconset, name), png)
  if (size === 1024) await writeFile(resolve(root, "assets/icon.png"), png)
}
const child = Bun.spawn(["iconutil", "-c", "icns", iconset, "-o", resolve(root, "assets/Tap.icns")], {
  stdin: "ignore", stdout: "inherit", stderr: "inherit",
})
if ((await child.exited) !== 0) throw new Error("iconutil failed")
console.log("Built assets/icon.png and assets/Tap.icns")
