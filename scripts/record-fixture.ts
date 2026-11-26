#!/usr/bin/env bun
// Records the zsh sessions crates/tap-index tests against.
//
// Each fixture is a real `tap capture` of an interactive `zsh -i`: this script
// starts the CLI in a PTY, as a person would in a terminal, and types into it
// key by key. Tap records the keystrokes from its own stdin and the shell's
// output from the recorded PTY exactly as it does for a person, so the
// fixtures exercise the same capture path (shell integration, input policy,
// the resize at start) that real recordings go through.
//
// The shell runs with an isolated HOME whose .zshrc sets a fixed prompt, no
// history file and a TERM every machine has, so the recordings depend only on
// zsh and the few standard tools the session runs.
//
//   bun scripts/record-fixture.ts            # writes every fixture
//   bun scripts/record-fixture.ts osc133     # writes one
//
// Fixtures are committed; re-record only when the shell integration or the
// capture format changes, and update the expectations in
// crates/tap-index/tests/ to match.

import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"

const ROOT = resolve(import.meta.dir, "..")
const FIXTURES = join(ROOT, "crates/tap-index/tests/fixtures")
const PROMPT = "tap$ "
const COLS = 80
const ROWS = 24

/** One thing the person at the keyboard does. */
type Step =
  | { type: string; enter?: boolean } // types the text key by key, then Enter unless enter is false
  | { keys: string; prompt?: boolean } // sends one keystroke; waits for the next prompt if prompt is set
  | { wait: number } // pauses, in milliseconds
  | { waitFor: string } // waits until output since the last keystroke contains this text

interface Fixture {
  name: string
  description: string
  args: string[]
  steps: Step[]
}

const ENTER = "\r"
const UP = "\x1b[A"
const BACKSPACE = "\x7f"
const CTRL_U = "\x15"

/** Commands every fixture runs: success, failure, UTF-8, multi-line, idle, a pager, clear. */
const SESSION: Step[] = [
  { type: "echo hellp", enter: false },
  { keys: BACKSPACE },
  { type: "o world" },
  { type: "cat /nonexistent-tap-fixture" },
  { type: "echo 'héllo wörld ✓ 日本語'" },
  { type: "printf 'one\\ntwo\\nthree\\n'" },
  { wait: 2_300 },
  { type: "echo start; sleep 2.5; echo done" },
  { type: "less notes.txt" },
  { waitFor: "third line of notes" },
  { wait: 600 },
  { keys: ENTER },
  { wait: 400 },
  { keys: "q", prompt: true },
  { type: "echo before clear; clear" },
  { type: "echo after clear" },
]

const FIXTURE_LIST: Fixture[] = [
  {
    name: "osc133",
    description: "zsh with Tap's shell integration, input recorded",
    args: [],
    steps: SESSION,
  },
  {
    name: "no-integration",
    description: "zsh without shell integration (--no-shell-integration), input recorded",
    args: ["--no-shell-integration"],
    steps: [
      ...SESSION,
      // History recall and a line kill, so the reconstructed command text is
      // approximate for one command and exact for the other.
      { keys: UP },
      { wait: 300 },
      { keys: ENTER, prompt: true },
      { type: "garbage", enter: false },
      { keys: CTRL_U },
      { type: "echo killed" },
    ],
  },
  {
    name: "redacted",
    description: "zsh with shell integration, input redacted (no command lines in the marks)",
    args: ["--input-policy", "redact"],
    steps: [{ type: "echo secret-value" }, { type: "false" }],
  },
]

const ZSHRC = `# Written by scripts/record-fixture.ts
export TERM=xterm-256color
export LANG=en_US.UTF-8
unset HISTFILE
PS1='${PROMPT}'
RPROMPT=''
`

const NOTES = `first line of notes
second line of notes
third line of notes
`

async function record(fixture: Fixture): Promise<void> {
  const home = await mkdtemp(join(tmpdir(), `tap-fixture-${fixture.name}-`))
  try {
    await writeFile(join(home, ".zshrc"), ZSHRC)
    await writeFile(join(home, "notes.txt"), NOTES)
    const output = join(home, "session.tap")
    let screen = ""
    const child = Bun.spawn(
      ["bun", join(ROOT, "src/cli.ts"), "capture", "-o", output, "--cols", String(COLS), "--rows", String(ROWS), ...fixture.args, "--", "zsh", "-i"],
      {
        cwd: home,
        env: {
          PATH: process.env.PATH ?? "/usr/bin:/bin",
          HOME: home,
          LANG: "en_US.UTF-8",
          TERM: "xterm-256color",
        },
        terminal: { cols: COLS, rows: ROWS, data: (_terminal, data) => { screen += new TextDecoder().decode(data) } },
      },
    )
    const prompts = () => screen.split(PROMPT).length - 1
    const waitFor = async (predicate: () => boolean, what: string) => {
      for (let waited = 0; !predicate(); waited += 10) {
        if (waited > 15_000) throw new Error(`${fixture.name}: timed out waiting for ${what}; output so far: ${JSON.stringify(screen.slice(-2_000))}`)
        await Bun.sleep(10)
      }
    }
    // Where the output stood when the last key was sent, so waits look only at what followed it.
    let sentAt = 0
    const send = (bytes: string) => {
      sentAt = screen.length
      child.terminal!.write(bytes)
    }

    await waitFor(() => prompts() > 0, "the first prompt")
    await Bun.sleep(500)
    for (const step of fixture.steps) {
      if ("wait" in step) {
        await Bun.sleep(step.wait)
      } else if ("waitFor" in step) {
        await waitFor(() => screen.indexOf(step.waitFor, sentAt) >= 0, JSON.stringify(step.waitFor))
      } else if ("keys" in step) {
        const before = prompts()
        send(step.keys)
        if (step.prompt) {
          await waitFor(() => prompts() > before, "the prompt after a keystroke")
          await Bun.sleep(700)
        } else {
          await Bun.sleep(150)
        }
      } else {
        for (const char of step.type) {
          send(char)
          await Bun.sleep(45)
        }
        if (step.enter === false) continue
        const before = prompts()
        await Bun.sleep(250)
        send(ENTER)
        // A pager takes over the screen; the steps that follow wait for it and quit it.
        if (step.type.startsWith("less ")) continue
        await waitFor(() => prompts() > before, `the prompt after ${JSON.stringify(step.type)}`)
        await Bun.sleep(700)
      }
    }
    for (const char of "exit") {
      send(char)
      await Bun.sleep(45)
    }
    await Bun.sleep(250)
    send(ENTER)
    const code = await child.exited
    if (code !== 0) throw new Error(`${fixture.name}: tap capture exited with ${code}: ${screen.slice(-2_000)}`)
    const project = JSON.parse(await Bun.file(output).text()) as { title: string }
    project.title = `tap-index fixture: ${fixture.description}`
    const target = join(FIXTURES, `${fixture.name}.tap`)
    await Bun.write(target, `${JSON.stringify(project, null, 2)}\n`)
    console.log(`Wrote ${target}`)
  } finally {
    await rm(home, { recursive: true, force: true })
  }
}

const only = process.argv.slice(2)
for (const fixture of FIXTURE_LIST) {
  if (only.length && !only.includes(fixture.name)) continue
  await record(fixture)
}
