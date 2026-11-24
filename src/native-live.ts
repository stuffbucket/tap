import { fileURLToPath } from "node:url"
import { dirname, join, resolve } from "node:path"
import { CaptureSession } from "./capture-session.ts"
import { writeProject } from "./project.ts"
import type { PaneLayout } from "./model.ts"

export async function startNativeLiveCapture(options: { output: string; shell: string[]; hostname?: string; port?: number }) {
  const clients = new Set<any>()
  const panes = new Map<string, { id: string; cols: number; rows: number; layout: PaneLayout }>()
  const broadcast = (value: unknown) => { const data = JSON.stringify(value); for (const ws of clients) ws.send(data) }
  let paneCounter = 1
  let stop: () => Promise<void> = async () => {}
  const session = await CaptureSession.start({ id: "pane-1", command: options.shell, cols: 80, rows: 24 }, {
    onOutput: (paneId, bytes) => broadcast({ type: "output", paneId, data: Buffer.from(bytes).toString("base64") }),
    onPaneOpened: (pane) => { panes.set(pane.id, pane); broadcast({ type: "pane-open", pane }) },
    onLayout: (paneId, layout) => { const pane = panes.get(paneId); if (pane) pane.layout = layout; broadcast({ type: "layout", paneId, layout }) },
    onPaneClosed: (paneId) => {
      panes.delete(paneId)
      broadcast({ type: "pane-close", paneId })
      // The last shell exiting ends the recording, like closing the final terminal window.
      if (session.openPaneCount() === 0) setTimeout(() => void stop(), 0)
    },
  })
  let stopped = false
  let resolveDone!: () => void
  const done = new Promise<void>((resolvePromise) => { resolveDone = resolvePromise })
  const webRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../web")
  const ghosttyEntry = fileURLToPath(import.meta.resolve("@termless/ghostty"))
  const ghosttyWeb = join(resolve(dirname(ghosttyEntry), ".."), "node_modules/ghostty-web")
  const files = new Map([
    ["/", join(webRoot, "native-live.html")], ["/native-live.js", join(webRoot, "native-live.js")],
    ["/vendor/ghostty-web.js", join(ghosttyWeb, "dist/ghostty-web.js")],
    ["/vendor/__vite-browser-external-2447137e.js", join(ghosttyWeb, "dist/__vite-browser-external-2447137e.js")],
    ["/vendor/ghostty-vt.wasm", join(ghosttyWeb, "ghostty-vt.wasm")],
  ])
  let server: ReturnType<typeof Bun.serve>
  stop = async () => {
    if (stopped) return
    stopped = true
    const project = await session.stop()
    await writeProject(options.output, project)
    broadcast({ type: "saved", output: options.output })
    setTimeout(() => server.stop(true), 20)
    resolveDone()
  }
  server = Bun.serve({
    hostname: options.hostname ?? "127.0.0.1", port: options.port ?? 4379,
    websocket: {
      open(ws) { clients.add(ws); ws.send(JSON.stringify({ type: "init", panes: [...panes.values()] })) },
      close(ws) { clients.delete(ws) },
      message(ws, raw) {
        void (async () => {
          if (stopped) return
          const message = JSON.parse(String(raw))
          if (message.type === "input" || message.type === "paste") {
            session.focusPane(message.paneId)
            session.input(Uint8Array.from(Buffer.from(message.data, "base64")), message.type === "paste" ? "paste" : "keyboard")
          } else if (message.type === "split") {
            paneCounter++
            await session.splitPane(message.paneId, message.direction, { id: `pane-${paneCounter}`, command: options.shell, cols: 80, rows: 24 })
          } else if (message.type === "resize") session.resizePane(message.paneId, Number(message.cols), Number(message.rows))
          else if (message.type === "focus") session.focusPane(message.paneId)
          else if (message.type === "close") await session.closePane(message.paneId)
        })().catch((error: unknown) => ws.send(JSON.stringify({ type: "error", message: error instanceof Error ? error.message : String(error) })))
      },
    },
    fetch(request, bunServer) {
      const url = new URL(request.url)
      if (url.pathname === "/ws" && bunServer.upgrade(request, { data: {} })) return undefined
      if (url.pathname === "/stop" && request.method === "POST") { setTimeout(() => void stop(), 0); return Response.json({ stopping: true }) }
      if (url.pathname === "/favicon.ico") return new Response(null, { status: 204 })
      const path = files.get(url.pathname); return path ? new Response(Bun.file(path)) : new Response("not found", { status: 404 })
    },
  })
  return { hostname: server.hostname, port: server.port, done, stop }
}
