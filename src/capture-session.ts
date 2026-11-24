import { NativePaneRenderer } from "./native-renderer.ts"
import { KittyNormalizer } from "./kitty-normalizer.ts"
import { encodeBytes } from "./bytes.ts"
import type { CapturePaneSpec, PaneLayout, TapEvent, TapPane, TapProject } from "./model.ts"

interface SessionPane {
  pane: TapPane
  child: ReturnType<typeof Bun.spawn>
  pty: { write(data: string | Uint8Array): void; resize(cols: number, rows: number): void; close(): void }
  terminal: NativePaneRenderer
  normalizer: KittyNormalizer
  queue: Promise<void>
  closed: boolean
  /** Live geometry; `pane.cols/rows/layout` stay at their recorded starting values. */
  current: { cols: number; rows: number; layout: PaneLayout }
}

export interface CaptureSessionCallbacks {
  onOutput?: (paneId: string, bytes: Uint8Array) => void
  onPaneOpened?: (pane: { id: string; cols: number; rows: number; layout: PaneLayout }) => void
  onPaneClosed?: (paneId: string) => void
  onLayout?: (paneId: string, layout: PaneLayout) => void
}

const EPSILON = 1e-6
const near = (a: number, b: number) => Math.abs(a - b) < EPSILON

/** Returns the rectangle covering both layouts when they share a full edge, otherwise null. */
function mergeAdjacent(a: PaneLayout, b: PaneLayout): PaneLayout | null {
  if (near(a.x, b.x) && near(a.width, b.width) && (near(a.y + a.height, b.y) || near(b.y + b.height, a.y))) {
    return { x: a.x, y: Math.min(a.y, b.y), width: a.width, height: a.height + b.height }
  }
  if (near(a.y, b.y) && near(a.height, b.height) && (near(a.x + a.width, b.x) || near(b.x + b.width, a.x))) {
    return { x: Math.min(a.x, b.x), y: a.y, width: a.width + b.width, height: a.height }
  }
  return null
}

export class CaptureSession {
  private readonly started = process.hrtime.bigint()
  private readonly panes = new Map<string, SessionPane>()
  private focused: string | null = null
  private stopped = false
  private constructor(private readonly callbacks: CaptureSessionCallbacks = {}) {}

  private nowUs() { return Number((process.hrtime.bigint() - this.started) / 1_000n) }

  static async start(initial: CapturePaneSpec, callbacks: CaptureSessionCallbacks = {}): Promise<CaptureSession> {
    const session = new CaptureSession(callbacks)
    await session.spawn(initial, initial.layout ?? { x: 0, y: 0, width: 1, height: 1 }, 0)
    session.focused = initial.id
    return session
  }

  private async spawn(spec: CapturePaneSpec, layout: PaneLayout, visibleAt: number): Promise<string> {
    if (this.panes.has(spec.id)) throw new Error(`duplicate pane id: ${spec.id}`)
    const cols = spec.cols ?? 80, rows = spec.rows ?? 24, fontSize = spec.fontSize ?? 16
    const events: TapEvent[] = visibleAt > 0 ? [{ atUs: 0, type: "visibility", visible: false }, { atUs: visibleAt, type: "visibility", visible: true }] : []
    const pane: TapPane = { id: spec.id, ...(spec.title ? { title: spec.title } : {}), cols, rows, fontSize, layout, events }
    const terminal = new NativePaneRenderer(cols, rows, fontSize)
    const normalizer = new KittyNormalizer({ cwd: spec.cwd ?? process.cwd(), ...(spec.kittyFileRoots ? { allowedRoots: spec.kittyFileRoots } : {}) })
    let record: SessionPane | null = null
    const child = Bun.spawn(spec.command, {
      ...(spec.cwd ? { cwd: spec.cwd } : {}),
      env: { ...process.env, TERM: "xterm-ghostty", COLORTERM: "truecolor", ...spec.env },
      terminal: { cols, rows, data: (_terminal, data) => {
        const bytes = Uint8Array.from(data)
        const event: Extract<TapEvent, { type: "output" }> = { atUs: this.nowUs(), type: "output", ...encodeBytes(bytes) }
        events.push(event)
        if (!record) return
        record.queue = record.queue.then(async () => {
          const playback = await normalizer.push(bytes)
          const encoded = encodeBytes(playback)
          if (Buffer.compare(Buffer.from(bytes), Buffer.from(playback)) !== 0) {
            event.playbackData = encoded.data; event.playbackEncoding = encoded.encoding
          }
          this.callbacks.onOutput?.(spec.id, playback)
          const reply = await terminal.write(playback)
          if (reply.byteLength) {
            events.push({ atUs: this.nowUs(), type: "input", origin: "terminal-reply", ...encodeBytes(reply) })
            record!.pty.write(reply)
          }
        })
      } },
    })
    record = { pane, child, pty: child.terminal as SessionPane["pty"], terminal, normalizer, queue: Promise.resolve(), closed: false, current: { cols, rows, layout } }
    this.panes.set(spec.id, record)
    this.callbacks.onPaneOpened?.({ id: spec.id, cols, rows, layout })
    // A shell that exits on its own (e.g. the user typed `exit`) closes its pane.
    void child.exited.then(() => { if (!this.stopped) void this.closePane(spec.id) })
    return spec.id
  }

  private openPane(id: string): SessionPane {
    const pane = this.panes.get(id)
    if (!pane) throw new Error(`unknown pane: ${id}`)
    if (pane.closed) throw new Error(`pane is closed: ${id}`)
    return pane
  }

  /** Resize a pane's PTY and terminal state; records a `resize` event so playback reflows identically. */
  resizePane(id: string, cols: number, rows: number) {
    const pane = this.openPane(id)
    cols = Math.max(2, Math.min(1000, Math.floor(cols)))
    rows = Math.max(1, Math.min(1000, Math.floor(rows)))
    if (cols === pane.current.cols && rows === pane.current.rows) return
    pane.current.cols = cols; pane.current.rows = rows
    pane.pane.events.push({ atUs: this.nowUs(), type: "resize", cols, rows })
    pane.pty.resize(cols, rows)
    pane.queue = pane.queue.then(() => pane.terminal.resize(cols, rows))
  }

  async splitPane(targetId: string, direction: "horizontal" | "vertical", spec: CapturePaneSpec, ratio = 0.5): Promise<string> {
    if (this.stopped) throw new Error("capture session is stopped")
    const target = this.openPane(targetId)
    ratio = Math.max(0.1, Math.min(0.9, ratio))
    const atUs = this.nowUs()
    const old = target.current.layout
    const first: PaneLayout = direction === "vertical"
      ? { ...old, width: old.width * ratio }
      : { ...old, height: old.height * ratio }
    const second: PaneLayout = direction === "vertical"
      ? { x: old.x + first.width, y: old.y, width: old.width - first.width, height: old.height }
      : { x: old.x, y: old.y + first.height, width: old.width, height: old.height - first.height }
    this.setLayout(target, targetId, first, atUs)
    await this.spawn(spec, second, atUs)
    this.focused = spec.id
    return spec.id
  }

  private setLayout(pane: SessionPane, id: string, layout: PaneLayout, atUs = this.nowUs()) {
    pane.current.layout = layout
    pane.pane.events.push({ atUs, type: "layout", layout })
    this.callbacks.onLayout?.(id, layout)
  }

  focusPane(id: string) { this.openPane(id); this.focused = id }
  openPaneCount() { return [...this.panes.values()].filter((pane) => !pane.closed).length }
  input(bytes: Uint8Array, origin: "keyboard" | "paste" = "keyboard") {
    if (!this.focused) throw new Error("no focused pane")
    const pane = this.openPane(this.focused)
    pane.pane.events.push({ atUs: this.nowUs(), type: "input", origin, ...encodeBytes(bytes) })
    pane.pty.write(bytes)
  }

  /** Hide a pane, end its process, and give its area to an edge-sharing neighbour. The last open pane cannot be closed. */
  async closePane(id: string) {
    const pane = this.panes.get(id)
    if (!pane || pane.closed) return
    const open = [...this.panes.entries()].filter(([, entry]) => !entry.closed)
    if (open.length <= 1 && !this.stopped) {
      // Keep the recording usable: an exiting last shell ends the session rather than leaving an empty stage.
      if (pane.child.exitCode === null) throw new Error("cannot close the last pane")
    }
    pane.closed = true
    const atUs = this.nowUs()
    pane.pane.events.push({ atUs, type: "visibility", visible: false })
    try { pane.child.kill() } catch {}
    try { pane.pty.close() } catch {}
    for (const [otherId, other] of open) {
      if (otherId === id) continue
      const merged = mergeAdjacent(other.current.layout, pane.current.layout)
      if (merged) { this.setLayout(other, otherId, merged, atUs); break }
    }
    if (this.focused === id) this.focused = open.find(([otherId]) => otherId !== id)?.[0] ?? null
    this.callbacks.onPaneClosed?.(id)
  }

  async stop(): Promise<TapProject> {
    this.stopped = true
    const running = [...this.panes.values()]
    for (const pane of running) if (pane.child.exitCode === null) try { pane.child.kill(1) } catch {}
    await Promise.race([
      Promise.allSettled(running.map((pane) => pane.child.exited)),
      Bun.sleep(1_000),
    ])
    for (const pane of running) if (pane.child.exitCode === null) try { pane.child.kill(9) } catch {}
    await Promise.allSettled(running.map((pane) => pane.child.exited))
    await Promise.all([...this.panes.values()].map((pane) => pane.queue))
    await Promise.all([...this.panes.values()].map((pane) => pane.terminal.close()))
    const durationUs = this.nowUs()
    for (const pane of this.panes.values()) pane.pane.events.sort((a, b) => a.atUs - b.atUs)
    return {
      version: 1, title: "Dynamic split capture", createdAt: new Date().toISOString(), durationUs,
      canvas: { width: 1280, height: 720, background: "#101014", fps: 120 },
      panes: [...this.panes.values()].map((entry) => entry.pane), overlays: [],
      metadata: { terminal: "ghostty", terminalEngine: "libghostty-vt 1.0.1", timebase: "microseconds", source: "dynamic-panes" },
    }
  }
}
