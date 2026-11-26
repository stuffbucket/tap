import { afterAll, beforeAll, expect, test } from "bun:test"
import { chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { BUILD_TAP_INDEX, tapIndexBinary } from "../src/transcript.ts"

let root = ""

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), "tap-transcript-"))
})

afterAll(async () => {
  await rm(root, { recursive: true, force: true })
})

const binaryName = process.platform === "win32" ? "tap-index.exe" : "tap-index"

async function fakeBinary(dir: string): Promise<string> {
  await mkdir(dir, { recursive: true })
  const path = join(dir, binaryName)
  await writeFile(path, "#!/bin/sh\n")
  await chmod(path, 0o755)
  return path
}

test("explains how to build tap-index when it is missing", () => {
  expect(() => tapIndexBinary(join(root, "empty"), {})).toThrow(BUILD_TAP_INDEX)
})

test("prefers a release build, then debug, and honours CARGO_TARGET_DIR", async () => {
  const checkout = join(root, "checkout")
  const debug = await fakeBinary(join(checkout, "target/debug"))
  expect(tapIndexBinary(checkout, {})).toBe(debug)
  const release = await fakeBinary(join(checkout, "target/release"))
  expect(tapIndexBinary(checkout, {})).toBe(release)
  const elsewhere = await fakeBinary(join(root, "cargo-target/debug"))
  expect(tapIndexBinary(checkout, { CARGO_TARGET_DIR: join(root, "cargo-target") })).toBe(elsewhere)
})
