import { fileURLToPath } from "node:url"
import { dirname, join, resolve } from "node:path"
import { NativePaneRenderer } from "./native-renderer.ts"
import { KittyNormalizer } from "./kitty-normalizer.ts"
import { encodeBytes } from "./bytes.ts"
import { writeProject } from "./project.ts"
import type { TapEvent, TapProject } from "./model.ts"

interface LivePty {
  write(data: string | Uint8Array): void
  resize(cols: number, rows: number): void
  close(): void
}

interface LiveOptions {
  output: string
  command: string[]
  hostname?: string
  port?: number
  cols?: number
  rows?: number
  cleanupCommand?: string[]
}

export async function startLiveCapture(options: LiveOptions) {
  const cols = options.cols ?? 120
  const rows = options.rows ?? 40
  const started = process.hrtime.bigint()
  const events: TapEvent[] = []
  const clients = new Set<any>()
  const pendingReplies: Array<{ bytes: Uint8Array; timer: ReturnType<typeof setTimeout> }> = []
  const terminal = new NativePaneRenderer(cols, rows, 16)
  const normalizer = new KittyNormalizer()
  let queue: Promise<void> = Promise.resolve()
  let pty: LivePty | null = null
  let finished = false
  let resolveDone!: (project: TapProject) => void
  const done = new Promise<TapProject>((resolvePromise) => { resolveDone = resolvePromise })
  const nowUs = () => Number((process.hrtime.bigint() - started) / 1_000n)
  const broadcast = (message: unknown) => {
    const encoded = JSON.stringify(message)
    for (const client of clients) client.send(encoded)
  }
  const recordInput = (bytes: Uint8Array, origin: "keyboard" | "automation" | "terminal-reply") => {
    events.push({ atUs: nowUs(), type: "input", origin, ...encodeBytes(bytes) })
    pty?.write(bytes)
  }
  const sameBytes = (left: Uint8Array, right: Uint8Array) =>
    left.byteLength === right.byteLength && left.every((value, index) => value === right[index])

  const child = Bun.spawn(options.command, {
    env: { ...process.env, TERM: "xterm-ghostty", COLORTERM: "truecolor", TERM_PROGRAM: "ghostty" },
    terminal: {
      cols,
      rows,
      data: (_terminal, data) => {
        const bytes = Uint8Array.from(data)
        const event: Extract<TapEvent, { type: "output" }> = { atUs: nowUs(), type: "output", ...encodeBytes(bytes) }
        events.push(event)
        queue = queue.then(async () => {
          const playback = await normalizer.push(bytes)
          if (playback.byteLength !== bytes.byteLength || playback.some((value, index) => value !== bytes[index])) {
            const encoded = encodeBytes(playback)
            event.playbackData = encoded.data
            event.playbackEncoding = encoded.encoding
          }
          const reply = await terminal.write(playback)
          if (reply.byteLength) {
            const response = Uint8Array.from(reply)
            if (clients.size === 0) {
              recordInput(response, "terminal-reply")
            } else {
              const pending = {
                bytes: response,
                timer: setTimeout(() => {
                  const index = pendingReplies.indexOf(pending)
                  if (index !== -1) pendingReplies.splice(index, 1)
                  recordInput(response, "terminal-reply")
                }, 75),
              }
              pendingReplies.push(pending)
            }
          }
          if (playback.byteLength) broadcast({ type: "output", data: Buffer.from(playback).toString("base64") })
        })
      },
    },
  })
  pty = child.terminal as LivePty

  const sourceRoot = dirname(fileURLToPath(import.meta.url))
  const webRoot = resolve(sourceRoot, "../web")
  const ghosttyEntry = fileURLToPath(import.meta.resolve("@termless/ghostty"))
  const ghosttyWeb = join(resolve(dirname(ghosttyEntry), ".."), "node_modules/ghostty-web")
  const files = new Map([
    ["/", join(webRoot, "live.html")],
    ["/live.js", join(webRoot, "live.js")],
    ["/vendor/ghostty-web.js", join(ghosttyWeb, "dist/ghostty-web.js")],
    ["/vendor/__vite-browser-external-2447137e.js", join(ghosttyWeb, "dist/__vite-browser-external-2447137e.js")],
    ["/vendor/ghostty-vt.wasm", join(ghosttyWeb, "ghostty-vt.wasm")],
  ])

  let server: ReturnType<typeof Bun.serve>
  const finish = async () => {
    if (finished) return done
    finished = true
    for (const pending of pendingReplies.splice(0)) clearTimeout(pending.timer)
    try { child.kill() } catch {}
    try { pty?.close() } catch {}
    if (options.cleanupCommand?.length) {
      try {
        const cleanup = Bun.spawn(options.cleanupCommand, { stdin: "ignore", stdout: "ignore", stderr: "ignore" })
        await cleanup.exited
      } catch {}
    }
    await queue
    await terminal.close()
    const project: TapProject = {
      version: 1,
      title: "Live tmux capture",
      createdAt: new Date().toISOString(),
      durationUs: nowUs(),
      canvas: { width: 1280, height: 720, background: "#101014", fps: 120 },
      panes: [{ id: "tmux", title: "Live tmux", cols, rows, fontSize: 16, layout: { x: 0, y: 0, width: 1, height: 1 }, events }],
      overlays: [],
      metadata: { terminal: "ghostty", terminalEngine: "libghostty-vt 1.0.1", timebase: "microseconds", source: "live-tmux" },
    }
    await writeProject(options.output, project)
    broadcast({ type: "saved", output: options.output })
    setTimeout(() => server.stop(true), 20)
    resolveDone(project)
    return project
  }

  server = Bun.serve({
    hostname: options.hostname ?? "127.0.0.1",
    port: options.port ?? 4378,
    websocket: {
      open(ws) {
        clients.add(ws)
        ws.send(JSON.stringify({ type: "init", cols, rows }))
        for (const event of events) {
          if (event.type !== "output") continue
          const data = event.playbackData ?? event.data
          ws.send(JSON.stringify({ type: "output", data }))
        }
      },
      close(ws) { clients.delete(ws) },
      message(_ws, raw) {
        try {
          const message = JSON.parse(String(raw))
          if (message.type === "input") {
            const bytes = Uint8Array.from(Buffer.from(message.data, "base64"))
            const replyIndex = pendingReplies.findIndex((pending) => sameBytes(pending.bytes, bytes))
            if (replyIndex === -1) recordInput(bytes, "keyboard")
            else {
              const [pending] = pendingReplies.splice(replyIndex, 1)
              clearTimeout(pending!.timer)
              recordInput(bytes, "terminal-reply")
            }
          } else if (message.type === "resize") {
            const nextCols = Math.max(1, Math.min(65535, Math.trunc(message.cols)))
            const nextRows = Math.max(1, Math.min(65535, Math.trunc(message.rows)))
            pty?.resize(nextCols, nextRows)
            void terminal.resize(nextCols, nextRows)
            events.push({ atUs: nowUs(), type: "resize", cols: nextCols, rows: nextRows })
          } else if (message.type === "control") {
            const controls: Record<string, string> = {
              "split-right": "\x02%", "split-down": "\x02\"", "next-pane": "\x02o", "close-pane": "\x02xy",
            }
            const value = controls[message.action]
            if (value) recordInput(new TextEncoder().encode(value), "automation")
          }
        } catch {}
      },
    },
    fetch(request, bunServer) {
      const url = new URL(request.url)
      if (url.pathname === "/ws" && bunServer.upgrade(request, { data: {} })) return undefined
      if (url.pathname === "/stop" && request.method === "POST") {
        setTimeout(() => void finish(), 0)
        return Response.json({ stopping: true })
      }
      if (url.pathname === "/favicon.ico") return new Response(null, { status: 204 })
      const path = files.get(url.pathname)
      if (!path) return new Response("not found", { status: 404 })
      return new Response(Bun.file(path))
    },
  })
  void child.exited.then(() => finish())

  return {
    hostname: server.hostname,
    port: server.port,
    done,
    stop: finish,
  }
}
