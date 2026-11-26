// `tap capture` shows the recorded program's output in the user's own terminal. Programs ask their
// terminal questions (what are you, where is the cursor, what colour is the background...). Tap's
// recording terminal (libghostty-vt) already answers each one. If the user's terminal also saw the
// question it would answer too, and its answer would arrive on stdin and be forwarded to the
// program as if typed — shells then run it ("command not found: 1;2c"). So the mirrored copy of the
// output has its queries removed; the recording keeps every byte.

const ESC = 0x1b
const BEL = 0x07

/** CSI window-operation codes that ask for a report (CSI Ps t); others resize, move or set titles. */
const WINDOW_REPORTS = new Set(["11", "13", "14", "15", "16", "18", "19", "20", "21"])

/** Whether a complete escape sequence (starting with ESC) asks the terminal to reply. */
export function isTerminalQuery(sequence: string): boolean {
  const kind = sequence[1]
  if (kind === "Z") return true // DECID: identify terminal
  if (kind === "[") {
    const body = sequence.slice(2)
    const final = body[body.length - 1]!
    const prefix = /^[?>=<]/.test(body) ? body[0]! : ""
    const intermediates = body.slice(prefix.length, -1).replace(/[0-9;:]/g, "")
    const params = body.slice(prefix.length, -1).replace(/[^0-9;:]/g, "")
    if (final === "c" && !intermediates) return true // DA1, DA2, DA3
    if (final === "n" && prefix !== ">" && !intermediates) return true // DSR, CPR, colour-scheme report
    if (final === "p" && intermediates === "$") return true // DECRQM
    if (final === "q" && prefix === ">" && !intermediates) return true // XTVERSION
    if (final === "u" && prefix === "?") return true // Kitty keyboard flags
    if (final === "S" && prefix === "?") return true // XTSMGRAPHICS
    if (final === "t" && !prefix && !intermediates) return WINDOW_REPORTS.has(params.split(";")[0] ?? "")
    return false
  }
  if (kind === "]") {
    // OSC colour (4, 10–19, 5, ...) and clipboard (52) reads end their parameters with "?".
    const body = sequence.slice(2).replace(/(\x07|\x1b\\)$/, "")
    return body.split(";").at(-1) === "?"
  }
  if (kind === "P") {
    const body = sequence.slice(2)
    return body.startsWith("$q") || body.startsWith("+q") // DECRQSS, XTGETTCAP
  }
  if (kind === "_") {
    // Kitty graphics: APC G <key=value,...>[;payload]. a=q asks whether images are supported.
    const body = sequence.slice(2)
    if (!body.startsWith("G")) return false
    const control = body.slice(1).split(";")[0] ?? ""
    return control.split(",").includes("a=q")
  }
  return false
}

type State = "ground" | "escape" | "csi" | "string"

/** Streams output and removes terminal queries, holding back any sequence split across chunks. */
export class QueryFilter {
  private state: State = "ground"
  private pending: number[] = []

  push(input: Uint8Array): Uint8Array {
    const out: Uint8Array[] = []
    let runStart = -1
    const flushRun = (end: number) => {
      if (runStart >= 0) {
        out.push(input.subarray(runStart, end))
        runStart = -1
      }
    }
    const finish = () => {
      const sequence = Buffer.from(this.pending)
      if (!isTerminalQuery(sequence.toString("latin1"))) out.push(sequence)
      this.pending = []
      this.state = "ground"
    }

    for (let index = 0; index < input.length; index++) {
      const byte = input[index]!
      switch (this.state) {
        case "ground":
          if (byte === ESC) {
            flushRun(index)
            this.pending = [byte]
            this.state = "escape"
          } else if (runStart < 0) {
            runStart = index
          }
          break
        case "escape":
          this.pending.push(byte)
          if (byte === 0x5b) this.state = "csi" // [
          else if (byte === 0x5d || byte === 0x50 || byte === 0x5f || byte === 0x5e || byte === 0x58) this.state = "string" // ] P _ ^ X
          else if (byte >= 0x20 && byte <= 0x2f) { /* intermediate, e.g. ESC ( B */ }
          else finish()
          break
        case "csi":
          this.pending.push(byte)
          if (byte >= 0x40 && byte <= 0x7e) finish()
          else if (byte === ESC) { // malformed: restart at this ESC
            this.pending.pop()
            out.push(Uint8Array.from(this.pending))
            this.pending = [byte]
            this.state = "escape"
          }
          break
        case "string": {
          this.pending.push(byte)
          const length = this.pending.length
          const osc = this.pending[1] === 0x5d
          if ((osc && byte === BEL) || (byte === 0x5c && this.pending[length - 2] === ESC)) finish()
          break
        }
      }
    }
    flushRun(input.length)
    return Buffer.concat(out)
  }
}
