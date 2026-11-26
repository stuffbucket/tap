import { expect, test } from "bun:test"
import { AltScreenTracker, RESET_INPUT_MODES } from "../src/terminal-modes.ts"

const bytes = (text: string) => new TextEncoder().encode(text)

test("capture restores input modes a recorded program left on", () => {
  for (const mode of ["\x1b[?1003l", "\x1b[?1006l", "\x1b[?2004l", "\x1b[<99u", "\x1b[?25h"]) expect(RESET_INPUT_MODES).toContain(mode)
  const tracker = new AltScreenTracker()
  tracker.push(bytes("hello\x1b[?1049h\x1b[?1003h\x1b[>1u"))
  expect(tracker.active).toBe(true)
  expect(tracker.restoreSequence().startsWith("\x1b[?1049l")).toBe(true)
  tracker.push(bytes("bye\x1b[?1049l"))
  expect(tracker.active).toBe(false)
  expect(tracker.restoreSequence()).toBe(RESET_INPUT_MODES)
})

test("the alternate screen is tracked across chunk boundaries", () => {
  const tracker = new AltScreenTracker()
  tracker.push(bytes("abc\x1b[?10"))
  tracker.push(bytes("49hdef"))
  expect(tracker.active).toBe(true)
})
