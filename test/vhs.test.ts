import { afterAll, expect, test } from "bun:test"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { resolveShell, runTape, vhsCharacterChord } from "../src/tape.ts"

const temporary: string[] = []
afterAll(async () => {
  await Promise.all(temporary.map((path) => rm(path, { recursive: true, force: true })))
})

test("upstream VHS parser executes Source, Env, Wait, Screenshot, and Output", async () => {
  if (!existsSync(resolve(import.meta.dir, "../native/bin/tap-vhs-parse"))) return
  const directory = await mkdtemp(join(tmpdir(), "tap-vhs-test-"))
  temporary.push(directory)
  await writeFile(join(directory, "child.tape"), 'Type "echo $TAP_VALUE"\nEnter\n')
  await writeFile(
    join(directory, "main.tape"),
    [
      'Set Shell "/bin/sh"',
      "Set Columns 40",
      "Set Rows 10",
      'Env TAP_VALUE "from-source"',
      'Source "child.tape"',
      "Wait+Screen@2s /from-source/",
      'Screenshot "frame.png"',
      'Output "movie.mp4"',
      "",
    ].join("\n"),
  )

  const result = await runTape(join(directory, "main.tape"))
  expect(result.requestedOutputs).toEqual(["movie.mp4"])
  expect(result.requestedScreenshots.map((entry) => entry.path)).toEqual(["frame.png"])
  expect(result.project.panes[0]!.events.some((event) => event.type === "input" && event.origin === "vhs")).toBe(true)
  expect(result.project.panes[0]!.events.some((event) => event.type === "output")).toBe(true)
}, 20_000)

test("late VHS resize is ignored like upstream VHS", async () => {
  if (!existsSync(resolve(import.meta.dir, "../native/bin/tap-vhs-parse"))) return
  const directory = await mkdtemp(join(tmpdir(), "tap-vhs-resize-"))
  temporary.push(directory)
  const tape = join(directory, "resize.tape")
  await writeFile(
    tape,
    [
      'Set Shell "/bin/sh"',
      "Set Columns 80",
      "Set Rows 24",
      'Type "printf before"',
      "Enter",
      "Set Columns 40",
      'Type "printf after"',
      "Enter",
      "Sleep 20ms",
      "",
    ].join("\n"),
  )
  const result = await runTape(tape)
  const pane = result.project.panes[0]!
  expect(pane.cols).toBe(80)
  expect(pane.events.some((event) => event.type === "resize")).toBe(false)
  expect(result.warnings.some((warning) => warning.includes("Ignored late Set Columns"))).toBe(true)
}, 20_000)

test("VHS leading-decimal and default repeat delays match upstream timing", async () => {
  if (!existsSync(resolve(import.meta.dir, "../native/bin/tap-vhs-parse"))) return
  const directory = await mkdtemp(join(tmpdir(), "tap-vhs-timing-"))
  temporary.push(directory)
  const tape = join(directory, "timing.tape")
  await writeFile(
    tape,
    [
      'Set Shell "/bin/sh"',
      "Enter@.1 2",
      "Backspace 3",
      "Sleep .1s",
      "",
    ].join("\n"),
  )
  const result = await runTape(tape)
  const inputs = result.project.panes[0]!.events.filter((event) => event.type === "input")
  expect(inputs.length).toBeGreaterThanOrEqual(5)
  expect(inputs[1]!.atUs - inputs[0]!.atUs).toBeGreaterThanOrEqual(90_000)
  expect(inputs[3]!.atUs - inputs[2]!.atUs).toBeGreaterThanOrEqual(45_000)
}, 20_000)

test("VHS presentation settings persist into the editable project", async () => {
  if (!existsSync(resolve(import.meta.dir, "../native/bin/tap-vhs-parse"))) return
  const directory = await mkdtemp(join(tmpdir(), "tap-vhs-presentation-"))
  temporary.push(directory)
  const tape = join(directory, "presentation.tape")
  await writeFile(tape, [
    'Set Shell "/bin/sh"', "Set Width 400", "Set Height 240", "Set Padding 20", "Set Margin 12",
    'Set MarginFill "#112233"', "Set BorderRadius 14", "Set WindowBar Colorful", "Set WindowBarSize 30",
    'Set FontFamily "JetBrains Mono"', 'Set Theme "Dracula"', "Set CursorBlink true", "Set LoopOffset 25%", 'Type "printf styled"', "Enter", "",
  ].join("\n"))
  const result = await runTape(tape)
  expect(result.project.presentation).toMatchObject({
    padding: 20, margin: 12, marginFill: "#112233", borderRadius: 14,
    windowBar: "colorful", windowBarSize: 30, fontFamily: "JetBrains Mono",
    cursorBlink: true, loopOffsetPercent: 25, themeName: "Dracula",
  })
  expect(result.project.presentation?.theme?.palette).toHaveLength(16)
})

test("VHS rejects impossible frame padding", async () => {
  if (!existsSync(resolve(import.meta.dir, "../native/bin/tap-vhs-parse"))) return
  const directory = await mkdtemp(join(tmpdir(), "tap-vhs-dimensions-"))
  temporary.push(directory)
  const tape = join(directory, "dimensions.tape")
  await writeFile(tape, 'Set Height 100\nSet Width 100\nSet Padding 100\nType "bad"\n')
  await expect(runTape(tape)).rejects.toThrow("frame dimensions")
})

test("Set Shell follows VHS: clean named shells, bash by default, arbitrary commands as a Tap extension", () => {
  expect(resolveShell(undefined).command).toEqual(["bash", "--noprofile", "--norc", "--login", "+o", "history"])
  expect(resolveShell(undefined).env.PS1).toContain("> ")
  expect(resolveShell('"zsh"').command).toEqual(["zsh", "--histnostore", "--no-rcs"])
  expect(resolveShell("/bin/sh -i")).toEqual({ command: ["/bin/sh", "-i"], env: {} })
})

test("tapes without Set Shell get a clean bash, not the recorder's own shell configuration", async () => {
  if (!existsSync(resolve(import.meta.dir, "../native/bin/tap-vhs-parse"))) return
  const directory = await mkdtemp(join(tmpdir(), "tap-vhs-shell-"))
  temporary.push(directory)
  const tape = join(directory, "default-shell.tape")
  await writeFile(tape, 'Type "echo rc=${BASH_VERSION:+bash}"\nEnter\nSleep 300ms\n')
  const result = await runTape(tape)
  const output = result.project.panes[0]!.events
    .filter((event) => event.type === "output")
    .map((event) => event.type === "output" ? Buffer.from(event.data, "base64").toString("utf8") : "")
    .join("")
  expect(output).toContain("rc=bash")
  expect(output).toContain("\x1b[38;2;90;86;224m> ")
})

test("a missing VHS shell fails clearly", async () => {
  if (!existsSync(resolve(import.meta.dir, "../native/bin/tap-vhs-parse"))) return
  const directory = await mkdtemp(join(tmpdir(), "tap-vhs-missing-shell-"))
  temporary.push(directory)
  const tape = join(directory, "missing.tape")
  await writeFile(tape, 'Set Shell "tap-no-such-shell"\nType "x"\n')
  await expect(runTape(tape)).rejects.toThrow("VHS shell not found on PATH: tap-no-such-shell")
})

test("character key chords use VHS/xterm.js legacy bytes", () => {
  const bytes = (modifiers: { ctrl?: boolean; alt?: boolean }, key: string) => {
    const value = vhsCharacterChord(modifiers, key)
    return value && Buffer.from(value).toString("latin1")
  }
  // Tapes conventionally write capitals (`Ctrl+C`); VHS sends the control byte regardless of case.
  expect(bytes({ ctrl: true }, "C")).toBe("\x03")
  expect(bytes({ ctrl: true }, "c")).toBe("\x03")
  expect(bytes({ ctrl: true }, "B")).toBe("\x02")
  expect(bytes({ ctrl: true }, "[")).toBe("\x1b")
  expect(bytes({ ctrl: true }, "@")).toBe("\x00")
  expect(bytes({ ctrl: true }, "?")).toBe("\x7f")
  expect(bytes({ ctrl: true, alt: true }, "b")).toBe("\x1b\x02")
  expect(bytes({ alt: true }, "b")).toBe("\x1bb")
  expect(bytes({ alt: true }, ".")).toBe("\x1b.")
  expect(bytes({ ctrl: true }, "Left")).toBeNull()
})

test("tape Ctrl and Alt commands record the legacy bytes", async () => {
  if (!existsSync(resolve(import.meta.dir, "../native/bin/tap-vhs-parse"))) return
  const directory = await mkdtemp(join(tmpdir(), "tap-vhs-keys-"))
  temporary.push(directory)
  const tape = join(directory, "keys.tape")
  await writeFile(tape, 'Set Shell "/bin/cat"\nCtrl+B\nCtrl+C\nAlt+b\nCtrl+Left\n')
  const result = await runTape(tape)
  const sent = result.project.panes[0]!.events
    .filter((event) => event.type === "input" && event.origin === "vhs")
    .map((event) => event.type === "input" ? Buffer.from(event.data, "base64").toString("latin1") : "")
  expect(sent).toEqual(["\x02", "\x03", "\x1bb", "\x1b[1;5D"])
})
