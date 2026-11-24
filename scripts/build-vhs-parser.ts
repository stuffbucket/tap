#!/usr/bin/env bun

import { mkdir } from "node:fs/promises"
import { resolve } from "node:path"

const root = resolve(import.meta.dir, "..")
const source = resolve(root, "native/vhsparse")
const output = resolve(root, "native/bin/tap-vhs-parse")
await mkdir(resolve(root, "native/bin"), { recursive: true })
const process = Bun.spawn(["go", "build", "-trimpath", "-o", output, "."], {
  cwd: source,
  stdin: "ignore",
  stdout: "inherit",
  stderr: "inherit",
})
const exitCode = await process.exited
if (exitCode !== 0) throw new Error(`go build exited with ${exitCode}`)
console.log(`Built ${output} with the upstream Charmbracelet VHS parser`)
