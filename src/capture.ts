import { defaultLayouts, type CaptureSpec, type TapEvent, type TapPane, type TapProject } from "./model.ts"
import { encodeBytes } from "./bytes.ts"
import { NativePaneRenderer, nativeRendererAvailable } from "./native-renderer.ts"
import { KittyNormalizer } from "./kitty-normalizer.ts"
import { TAP_VERSION } from "./version.ts"
import { AltScreenTracker } from "./terminal-modes.ts"
import { QueryFilter } from "./mirror-filter.ts"
import { withShellIntegration, type IntegratedShell } from "./shell-integration.ts"

export interface CaptureOptions {
  interactive?: boolean
  mirrorOutput?: boolean
  /** Input is recorded by default. Redact keeps timing/provenance; omit stores nothing. */
  inputPolicy?: "record" | "redact" | "omit"
  /**
   * Have supported shells mark prompts and commands with OSC 133, so a recording can be split
   * into commands. On by default; the marks don't change what is on screen.
   */
  shellIntegration?: boolean
}

interface PtyHandle {
  write(data: string | Uint8Array): void
  resize(cols: number, rows: number): void
  close(): void
}

interface ParserState {
  queue: Promise<void>
  error: unknown
  pty: PtyHandle | null
  normalizer: KittyNormalizer
}

interface LivePane {
  pane: TapPane
  process: ReturnType<typeof Bun.spawn>
  pty: PtyHandle
  terminal: NativePaneRenderer
  parser: ParserState
}

const DEFAULT_CANVAS = {
  width: 1280,
  height: 720,
  background: "#101014",
  fps: 60,
}

function elapsedUs(start: bigint): number {
  return Number((process.hrtime.bigint() - start) / 1_000n)
}

function sortEvents(events: TapEvent[]): void {
  events.sort((left, right) => left.atUs - right.atUs)
}

function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false
  for (let index = 0; index < left.byteLength; index++) {
    if (left[index] !== right[index]) return false
  }
  return true
}

export async function captureProject(spec: CaptureSpec, options: CaptureOptions = {}): Promise<TapProject> {
  if (spec.panes.length === 0) throw new Error("capture spec needs at least one pane")
  if (!nativeRendererAvailable()) {
    throw new Error("capture requires the stable libghostty-vt backend; run: bun run build:native")
  }
  const layouts = defaultLayouts(spec.panes.length)
  const start = process.hrtime.bigint()
  const live: LivePane[] = []
  // Modes the mirrored program turns on in the user's terminal are restored when capture ends.
  const mirroredScreen = new AltScreenTracker()
  const mirrorFilter = new QueryFilter()
  const integratedShells = new Set<IntegratedShell>()

  try {
    for (const [index, paneSpec] of spec.panes.entries()) {
      if (!paneSpec.command.length) throw new Error(`pane ${paneSpec.id} has no command`)
      const events: TapEvent[] = []
      const cols = paneSpec.cols ?? 80
      const rows = paneSpec.rows ?? 24
      const fontSize = paneSpec.fontSize ?? 16
      const terminal = new NativePaneRenderer(cols, rows, fontSize)
      const pane: TapPane = {
        id: paneSpec.id,
        ...(paneSpec.title ? { title: paneSpec.title } : {}),
        cols,
        rows,
        fontSize,
        layout: paneSpec.layout ?? layouts[index]!,
        events,
      }

      const parser: ParserState = {
        queue: Promise.resolve(),
        error: null,
        pty: null,
        normalizer: new KittyNormalizer({
          cwd: paneSpec.cwd ?? process.cwd(),
          ...(paneSpec.kittyFileRoots ? { allowedRoots: paneSpec.kittyFileRoots } : {}),
        }),
      }
      let env: Record<string, string | undefined> = {
        ...process.env,
        TERM: "xterm-ghostty",
        COLORTERM: "truecolor",
        TERM_PROGRAM: "ghostty",
        TERM_PROGRAM_VERSION: `tap/${TAP_VERSION}`,
        ...paneSpec.env,
      }
      if (options.shellIntegration !== false) {
        const integrated = withShellIntegration(paneSpec.command, env, {
          commandLines: (options.inputPolicy ?? "record") === "record",
        })
        env = integrated.env
        if (integrated.shell) integratedShells.add(integrated.shell)
      }
      const child = Bun.spawn(paneSpec.command, {
        ...(paneSpec.cwd ? { cwd: paneSpec.cwd } : {}),
        env: Object.fromEntries(Object.entries(env).filter((entry): entry is [string, string] => entry[1] !== undefined)),
        terminal: {
          cols,
          rows,
          data: (_terminal, data) => {
            const bytes = Uint8Array.from(data)
            const event: Extract<TapEvent, { type: "output" }> = {
              atUs: elapsedUs(start),
              type: "output",
              ...encodeBytes(bytes),
            }
            events.push(event)
            if (options.mirrorOutput && index === 0) {
              // The user's terminal sees the output without its queries, so only the recording
              // terminal answers them (see mirror-filter.ts).
              process.stdout.write(mirrorFilter.push(bytes))
              mirroredScreen.push(bytes)
            }
            parser.queue = parser.queue
              .then(async () => {
                const playback = await parser.normalizer.push(bytes)
                if (!sameBytes(playback, bytes)) {
                  const encodedPlayback = encodeBytes(playback)
                  event.playbackData = encodedPlayback.data
                  event.playbackEncoding = encodedPlayback.encoding
                }
                const reply = await terminal.write(playback)
                if (reply.byteLength > 0) {
                  parser.pty?.write(reply)
                  events.push({
                    atUs: elapsedUs(start),
                    type: "input",
                    origin: "terminal-reply",
                    ...encodeBytes(reply),
                  })
                }
              })
              .catch((error) => {
                parser.error = error
              })
          },
        },
      })
      const pty = child.terminal as PtyHandle
      parser.pty = pty
      live.push({ pane, process: child, pty, terminal, parser })
    }

    const focused = live[0]!
    const stdin = process.stdin
    const canInteract = options.interactive === true && stdin.isTTY
    const previousRaw = stdin.isRaw

    const onInput = (chunk: Buffer | string) => {
      const bytes = typeof chunk === "string" ? Buffer.from(chunk) : chunk
      const inputPolicy = options.inputPolicy ?? "record"
      if (inputPolicy === "record") {
        focused.pane.events.push({
          atUs: elapsedUs(start),
          type: "input",
          origin: "keyboard",
          ...encodeBytes(bytes),
        })
      } else if (inputPolicy === "redact") {
        focused.pane.events.push({
          atUs: elapsedUs(start),
          type: "input",
          origin: "keyboard",
          data: "",
          encoding: "base64",
          redacted: true,
        })
      }
      focused.pty.write(bytes)
    }

    const onResize = () => {
      const cols = process.stdout.columns
      const rows = process.stdout.rows
      if (!cols || !rows) return
      focused.pty.resize(cols, rows)
      focused.pane.events.push({ atUs: elapsedUs(start), type: "resize", cols, rows })
      focused.parser.queue = focused.parser.queue.then(() => focused.terminal.resize(cols, rows))
    }

    if (canInteract) {
      stdin.setRawMode(true)
      stdin.resume()
      stdin.on("data", onInput)
      process.stdout.on("resize", onResize)
      onResize()
    }

    // If Tap itself is signalled (Ctrl+C, the window closing, kill), hang up the recorded programs as a
    // closing terminal would, so the normal path below still saves the recording and restores the
    // terminal. A program that ignores the hangup is killed after two seconds.
    const stopSignals = ["SIGINT", "SIGTERM", "SIGHUP", "SIGQUIT"] as const
    const onStopSignal = () => {
      for (const { process: child } of live) try { child.kill("SIGHUP") } catch {}
      setTimeout(() => { for (const { process: child } of live) try { child.kill("SIGKILL") } catch {} }, 2_000).unref()
    }
    for (const signal of stopSignals) process.on(signal, onStopSignal)

    try {
      await Promise.all(live.map(({ process: child }) => child.exited))
      await Promise.all(live.map(({ parser }) => parser.queue))
      for (const pane of live) {
        const pending = pane.parser.normalizer.flush()
        if (pending.byteLength > 0) {
          const playback = encodeBytes(pending)
          pane.pane.events.push({
            atUs: elapsedUs(start),
            type: "output",
            data: "",
            encoding: "base64",
            playbackData: playback.data,
            playbackEncoding: playback.encoding,
          })
          await pane.terminal.write(pending)
        }
        if (pane.parser.error) throw pane.parser.error
      }
    } finally {
      for (const signal of stopSignals) process.off(signal, onStopSignal)
      // The terminal may already be gone (window closed); restoring it is best-effort.
      if (options.mirrorOutput && process.stdout.isTTY) try { process.stdout.write(mirroredScreen.restoreSequence()) } catch {}
      if (canInteract) {
        stdin.off("data", onInput)
        process.stdout.off("resize", onResize)
        stdin.setRawMode(previousRaw ?? false)
        stdin.pause()
      }
    }
  } finally {
    await Promise.allSettled(
      live.flatMap((pane) => [
        Promise.resolve().then(() => pane.pty.close()),
        pane.terminal.close(),
      ]),
    )
  }

  const durationUs = elapsedUs(start)
  for (const { pane } of live) sortEvents(pane.events)

  return {
    version: 1,
    title: spec.title ?? "Tap recording",
    createdAt: new Date().toISOString(),
    durationUs,
    canvas: { ...DEFAULT_CANVAS, ...spec.canvas },
    panes: live.map(({ pane }) => pane),
    overlays: [],
    metadata: {
      terminal: "ghostty",
      terminalEngine: "libghostty-vt 1.0.1",
      timebase: "microseconds",
      source: options.interactive ? "interactive-pty" : "pty",
      ...(integratedShells.size ? { shellIntegration: [...integratedShells].sort() } : {}),
    },
  }
}
