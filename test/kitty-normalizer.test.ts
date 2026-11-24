import { afterAll, expect, test } from "bun:test"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { KittyNormalizer } from "../src/kitty-normalizer.ts"

const temporary: string[] = []
afterAll(async () => {
  await Promise.all(temporary.map((path) => rm(path, { recursive: true, force: true })))
})

const png = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
  "base64",
)

function packet(medium: "f" | "t", path: string): Uint8Array {
  return new TextEncoder().encode(`\x1b_Ga=T,f=100,t=${medium},i=7,c=2,r=2,q=2;${Buffer.from(path).toString("base64")}\x1b\\`)
}

test("normalizes direct Kitty PNG into browser-renderable RGBA", async () => {
  const normalizer = new KittyNormalizer()
  const source = new TextEncoder().encode(`\x1b_Ga=T,f=100,i=3,q=2;${png.toString("base64")}\x1b\\`)
  const result = new TextDecoder().decode(await normalizer.push(source))
  expect(result).toContain("f=32")
  expect(result).toContain("s=1")
  expect(result).toContain("v=1")
})

test("normalizes split Kitty file packets into self-contained direct payloads", async () => {
  const directory = await mkdtemp(join(tmpdir(), "tap-kitty-file-"))
  temporary.push(directory)
  const image = join(directory, "pixel.png")
  await writeFile(image, png)
  const normalizer = new KittyNormalizer({ cwd: directory })
  const source = packet("f", image)
  const first = await normalizer.push(source.subarray(0, 2))
  const second = await normalizer.push(source.subarray(2, 17))
  const third = await normalizer.push(source.subarray(17))
  const result = Buffer.concat([Buffer.from(first), Buffer.from(second), Buffer.from(third)]).toString("utf8")
  expect(result).toContain("t=d")
  expect(result).not.toContain(Buffer.from(image).toString("base64"))
  expect(result).toContain(png.toString("base64"))
})

test("temporary-file packets remove the file after successful normalization", async () => {
  const directory = await mkdtemp(join(tmpdir(), "tap-kitty-temp-"))
  temporary.push(directory)
  const image = join(directory, "temporary.png")
  await writeFile(image, png)
  const normalizer = new KittyNormalizer({ cwd: directory })
  const result = await normalizer.push(packet("t", image))
  expect(new TextDecoder().decode(result)).toContain("t=d")
  expect(existsSync(image)).toBe(false)
})
