// When `tap capture` mirrors a program's output to the user's terminal, the program can switch on
// terminal modes there. If it exits without switching them off (it crashed, was killed, or just
// forgot), the user's shell is left receiving mouse reports and Kitty-encoded keys as typed text.
// So capture always ends by restoring them.

/** Turns off input modes a full-screen program may leave on; each is a no-op when already off. */
export const RESET_INPUT_MODES = [
  "\x1b[?1000l", "\x1b[?1002l", "\x1b[?1003l", "\x1b[?1005l", "\x1b[?1006l", "\x1b[?1015l", "\x1b[?1016l", // mouse reporting
  "\x1b[?1004l", // focus in/out reports
  "\x1b[?2004l", // bracketed paste (shells re-enable it at their next prompt)
  "\x1b[?1l", "\x1b>", // normal cursor keys and keypad
  "\x1b[>4;0m", // xterm modifyOtherKeys off
  "\x1b[<99u", // pop the whole Kitty keyboard protocol stack
  "\x1b[?25h", // show the cursor
].join("")

const ENTER_ALT = ["\x1b[?1049h", "\x1b[?1047h", "\x1b[?47h"]
const LEAVE_ALT = ["\x1b[?1049l", "\x1b[?1047l", "\x1b[?47l"]

/** Follows mirrored output to know whether the program left the alternate screen active. */
export class AltScreenTracker {
  private tail = ""
  active = false

  push(bytes: Uint8Array): void {
    // Keep a few trailing bytes so a sequence split across chunks is still seen.
    const text = this.tail + Buffer.from(bytes).toString("latin1")
    const entered = Math.max(...ENTER_ALT.map((sequence) => text.lastIndexOf(sequence)))
    const left = Math.max(...LEAVE_ALT.map((sequence) => text.lastIndexOf(sequence)))
    if (entered !== left) this.active = entered > left
    this.tail = text.slice(-8)
  }

  /** The bytes that return the user's terminal to a normal shell state. */
  restoreSequence(): string {
    return (this.active ? "\x1b[?1049l" : "") + RESET_INPUT_MODES
  }
}
