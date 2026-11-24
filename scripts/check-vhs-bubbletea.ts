#!/usr/bin/env bun
// Runs VHS's upstream Bubble Tea tapes against the real Bubble Tea example programs.
// Each tape builds its example, drives it (typing, arrows, Ctrl+C/q to quit), and returns to the
// shell. A tape passes when the build produced no errors, the program launched, and the shell
// prompt came back afterwards — i.e. the program accepted the tape's keys and exited.
//
// The tapes end with a hidden `rm <binary>` typed immediately after the quit key. Its first
// character can be read by the exiting program (the same race exists under VHS, where it is
// masked by browser latency and hidden from the recording), so the cleanup line is not judged
// and the binary is removed here instead.

import { copyFile, readdir, rm } from "node:fs/promises"
import { existsSync } from "node:fs"
import { basename, join, resolve } from "node:path"
import { runTape } from "../src/tape.ts"
import { decodePlaybackBytes } from "../src/bytes.ts"

const root = resolve(import.meta.dir, "..")
const vhs = resolve(root, ".cache/vhs-upstream-0.12.1")
const version = "v1.3.10"
const bubbletea = resolve(root, `.cache/bubbletea-${version}`)
// `http` fetches an external website; it is not run unattended.
const skipped = new Set(["http"])
// The prompt VHS's clean bash prints (see resolveShell in src/tape.ts).
const VHS_BASH_PROMPT = "\x1b[38;2;90;86;224m> "

async function run(command: string[], cwd?: string) {
  const child = Bun.spawn(command, { ...(cwd ? { cwd } : {}), stdin: "ignore", stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text()])
  return { code: await child.exited, stdout, stderr }
}

if (!existsSync(vhs)) throw new Error("run bun run test:vhs-upstream first to fetch VHS")
if (!existsSync(resolve(bubbletea, ".git"))) {
  const clone = await run(["git", "clone", "--depth", "1", "--branch", version, "https://github.com/charmbracelet/bubbletea.git", bubbletea])
  if (clone.code !== 0) throw new Error(clone.stderr)
}
// Warm the module and build caches so each tape's own `go build` finishes promptly.
const warm = await run(["go", "build", "./..."], resolve(bubbletea, "examples"))
if (warm.code !== 0) throw new Error(`go build ./... failed in bubbletea examples:\n${warm.stderr}`)

const only = process.argv.slice(2)
const tapes = (await readdir(resolve(vhs, "examples/bubbletea"))).filter((name) => name.endsWith(".tape")).sort()
const results: { name: string; ok: boolean; reason?: string }[] = []
for (const file of tapes) {
  const name = basename(file, ".tape")
  if (only.length && !only.includes(name)) continue
  if (skipped.has(name)) { results.push({ name, ok: true, reason: "skipped (network)" }); continue }
  const directory = resolve(bubbletea, "examples", name)
  if (!existsSync(directory)) { results.push({ name, ok: false, reason: "no matching Bubble Tea example" }); continue }
  const tape = join(directory, ".tap-vhs-check.tape")
  const binary = (await Bun.file(resolve(vhs, "examples/bubbletea", file)).text()).match(/go build -o (\S+)/)?.[1]
  await copyFile(resolve(vhs, "examples/bubbletea", file), tape)
  try {
    const result = await runTape(tape)
    const output = result.project.panes[0]!.events
      .filter((event) => event.type === "output")
      .map((event) => event.type === "output" ? new TextDecoder().decode(decodePlaybackBytes(event)) : "")
      .join("")
    const failure = (text: string) => text.match(/command not found|No such file or directory|cannot find|go: [^\r\n]+|undefined: [^\r\n]+/)?.[0]
    const launch = binary ? output.indexOf(`./${binary}`) : -1
    const promptAfterExit = launch === -1 ? -1 : output.indexOf(VHS_BASH_PROMPT, launch)
    if (launch === -1) results.push({ name, ok: false, reason: failure(output) ?? "program was never launched" })
    else if (failure(output.slice(0, launch))) results.push({ name, ok: false, reason: `build: ${failure(output.slice(0, launch))}` })
    else if (promptAfterExit === -1) results.push({ name, ok: false, reason: "program did not exit back to the shell" })
    else if (failure(output.slice(launch, promptAfterExit))) results.push({ name, ok: false, reason: `run: ${failure(output.slice(launch, promptAfterExit))}` })
    else results.push({ name, ok: true })
  } catch (error) {
    results.push({ name, ok: false, reason: error instanceof Error ? error.message.split("\n")[0] : String(error) })
  } finally {
    await rm(tape, { force: true })
    if (binary) await rm(join(directory, binary), { force: true })
  }
  const last = results.at(-1)!
  console.log(`${last.ok ? "ok  " : "FAIL"} ${name}${last.reason ? ` — ${last.reason}` : ""}`)
}

const failed = results.filter((result) => !result.ok)
const ran = results.filter((result) => !result.reason?.startsWith("skipped"))
console.log(`Bubble Tea tapes: ${ran.length - failed.length}/${ran.length} passed${results.length > ran.length ? `, ${results.length - ran.length} skipped` : ""}`)
if (failed.length) process.exit(1)
