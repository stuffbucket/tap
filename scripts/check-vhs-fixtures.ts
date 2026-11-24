#!/usr/bin/env bun

import { mkdir, mkdtemp, readdir, rm } from "node:fs/promises"
import { existsSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, relative, resolve } from "node:path"
import { runTape } from "../src/tape.ts"

const root = resolve(import.meta.dir, "..")
const checkout = resolve(root, ".cache/vhs-upstream-0.12.1")
const parser = resolve(root, "native/bin/tap-vhs-parse")

async function run(command: string[], cwd?: string): Promise<{ code: number; stdout: string; stderr: string }> {
  const process = Bun.spawn(command, { ...(cwd ? { cwd } : {}), stdin: "ignore", stdout: "pipe", stderr: "pipe" })
  const stdout = new Response(process.stdout).text()
  const stderr = new Response(process.stderr).text()
  return { code: await process.exited, stdout: await stdout, stderr: await stderr }
}

async function tapes(directory: string): Promise<string[]> {
  const result: string[] = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) result.push(...await tapes(path))
    else if (entry.name.endsWith(".tape")) result.push(path)
  }
  return result
}

if (!existsSync(parser)) throw new Error("run bun run build:vhs first")
await mkdir(resolve(root, ".cache"), { recursive: true })
if (!existsSync(resolve(checkout, ".git"))) {
  await rm(checkout, { recursive: true, force: true })
  const clone = await run([
    "git", "clone", "--depth", "1", "--branch", "v0.12.1",
    "https://github.com/charmbracelet/vhs.git", checkout,
  ])
  if (clone.code !== 0) throw new Error(clone.stderr)
}

const files = await tapes(resolve(checkout, "examples"))
let parsed = 0
for (const path of files) {
  const result = await run([parser, path])
  const name = relative(checkout, path)
  const expectedFailure = name === "examples/errors/parser.tape"
  if ((result.code === 0) === expectedFailure) {
    throw new Error(`${name}: unexpected parser exit ${result.code}\n${result.stderr}${result.stdout}`)
  }
  parsed++
}

const commandFixtures = (await tapes(resolve(checkout, "examples/commands")))
  .map((path) => relative(checkout, path))
  .sort()
const safeSettings = [
  "columns", "height", "rows", "rows-columns", "set-border-radius", "set-cursor-blink",
  "set-font-family", "set-font-size-10", "set-font-size-20", "set-font-size-40",
  "set-letter-spacing", "set-line-height", "set-loop-offset", "set-margin", "set-padding",
  "set-theme", "set-theme-name", "set-typing-speed", "set-window-bar", "width",
].map((name) => `examples/settings/${name}.tape`)
const safe = [
  ...commandFixtures,
  ...safeSettings,
  "examples/env/env.tape",
  "examples/screenshot.tape",
  "examples/decorations/decorations.tape",
  "examples/demo.tape",
  // Only types text; the tape that uploads (examples/publish/publish.tape) is never run.
  "examples/publish/cassette.tape",
  "examples/settings/set-shell-bash.tape",
  "examples/settings/set-shell-zsh.tape",
  "examples/settings/set-shell-custom.tape",
]
for (const name of safe) await runTape(resolve(checkout, name))

// split.tape starts its own tmux server on the private `-L test` socket; never touch one we didn't start.
const tmuxSocket = ["tmux", "-L", "test"]
if ((await run([...tmuxSocket, "has-session"])).code === 0) throw new Error("a `tmux -L test` server is already running; refusing to run split.tape")
try {
  const split = await runTape(resolve(checkout, "examples/split/split.tape"))
  const sent = split.project.panes[0]!.events
    .filter((event) => event.type === "input")
    .map((event) => event.type === "input" ? Buffer.from(event.data, "base64").toString("latin1") : "")
  // The tmux prefix must reach tmux as a C0 byte so `o` switches panes rather than being typed.
  if (sent.indexOf("\x02") === -1 || sent[sent.indexOf("\x02") + 1] !== "o") throw new Error("split.tape: Ctrl+B was not sent as the tmux prefix")
} finally {
  await run([...tmuxSocket, "kill-server"])
}
safe.push("examples/split/split.tape")

// Tapes that drive optional programs run only when the program is installed. They get a scratch HOME
// (so first-run configs and history never land in the real home, and the tape's own `~/` paths
// resolve predictably), vim as the editor, and a reset checkout afterwards: several tapes edit or
// save files next to themselves.
const scratchHome = await mkdtemp(join(tmpdir(), "tap-vhs-home-"))
await mkdir(join(scratchHome, "src/gum"), { recursive: true })
await Bun.write(join(scratchHome, "src/gum/README.md"), Bun.file(resolve(checkout, "examples/gum/README.md"))) // gum/pager.tape reads ~/src/gum/README.md
async function runOptional(tape: string): Promise<void> {
  const saved = { HOME: process.env.HOME, EDITOR: process.env.EDITOR, VISUAL: process.env.VISUAL }
  Object.assign(process.env, { HOME: scratchHome, EDITOR: "vim", VISUAL: "vim" })
  try {
    const result = await runTape(tape)
    const output = result.project.panes[0]!.events
      .filter((event) => event.type === "output")
      .map((event) => event.type === "output" ? Buffer.from(event.data, "base64").toString("utf8") : "")
      .join("")
    const problem = output.match(/command not found|No such file or directory/)?.[0]
    if (problem) throw new Error(`${relative(checkout, tape)}: ${problem}`)
  } finally {
    for (const [key, value] of Object.entries(saved)) value === undefined ? delete process.env[key] : process.env[key] = value
    await run(["git", "checkout", "--", "."], checkout)
    await run(["git", "clean", "-fdq", "examples"], checkout)
  }
}

// Tapes for shells that are not installed must fail with a clear error rather than run another shell.
const otherShells: Record<string, string> = { fish: "fish", nu: "nu", osh: "osh", pwsh: "pwsh", xonsh: "xonsh", cmd: "cmd.exe" }
let shellsChecked = 0
for (const [name, program] of Object.entries(otherShells)) {
  const tape = resolve(checkout, `examples/settings/set-shell-${name}.tape`)
  if (Bun.which(program)) { await runOptional(tape); safe.push(relative(checkout, tape)); continue }
  try {
    await runTape(tape)
    throw new Error(`set-shell-${name}.tape ran although ${program} is not installed`)
  } catch (error) {
    if (!String(error).includes(`VHS shell not found on PATH: ${program}`)) throw error
  }
  shellsChecked++
}

const optionalTapes: [tape: string, program: string][] = [
  ["examples/fixtures/all.tape", "fish"],
  ["examples/gum/file.tape", "gum"], ["examples/gum/pager.tape", "gum"], ["examples/gum/table.tape", "gum"],
  ["examples/welcome.tape", "gum"],
  ["examples/glow/glow-simple.tape", "glow"], ["examples/glow/glow.tape", "glow"], ["examples/glow/glow-edit.tape", "glow"],
  ["examples/slides/slides.tape", "slides"],
  ["examples/jqp/jqp.tape", "jqp"], // fetches sample JSON from dummyjson.com
  ["examples/neofetch/neofetch.tape", "neofetch"],
]
const skipped: string[] = []
for (const [tape, program] of optionalTapes) {
  if (!Bun.which(program)) { skipped.push(`${tape} (needs ${program})`); continue }
  await runOptional(resolve(checkout, tape))
  safe.push(tape)
}
await rm(scratchHome, { recursive: true, force: true })

let dimensionsRejected = false
try {
  await runTape(resolve(checkout, "examples/errors/dimensions.tape"))
} catch {
  dimensionsRejected = true
}
if (!dimensionsRejected) throw new Error("examples/errors/dimensions.tape should be rejected")

console.log(`VHS upstream compatibility: ${parsed} tapes parsed, ${safe.length} tapes executed, ${shellsChecked} missing-shell tapes rejected as expected`)
if (skipped.length) console.log(`Skipped (program not installed): ${skipped.join(", ")}`)
