import { fileURLToPath } from "node:url"
import { existsSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { readProject, writeProject } from "./project.ts"
import { ProjectRenderer } from "./render.ts"
import { nativeRendererAvailable } from "./native-renderer.ts"
import { validateProject } from "./model.ts"

export interface ServeOptions {
  hostname?: string
  port?: number
}

function contentType(path: string): string {
  if (path.endsWith(".html")) return "text/html; charset=utf-8"
  if (path.endsWith(".js")) return "text/javascript; charset=utf-8"
  if (path.endsWith(".wasm")) return "application/wasm"
  if (path.endsWith(".json")) return "application/json; charset=utf-8"
  return "application/octet-stream"
}

export async function serveProject(projectPath: string, options: ServeOptions = {}) {
  const target = resolve(projectPath)
  let project = await readProject(target)
  let renderer = nativeRendererAvailable() ? new ProjectRenderer(project) : null
  let operationQueue: Promise<void> = Promise.resolve()
  const serialized = <T>(operation: () => Promise<T>): Promise<T> => {
    const result = operationQueue.then(operation, operation)
    operationQueue = result.then(() => undefined, () => undefined)
    return result
  }
  const sourceRoot = dirname(fileURLToPath(import.meta.url))
  const webRoot = resolve(sourceRoot, "../web")
  const ghosttyEntry = fileURLToPath(import.meta.resolve("@termless/ghostty"))
  const ghosttyPackage = resolve(dirname(ghosttyEntry), "..")
  const ghosttyWeb = join(ghosttyPackage, "node_modules/ghostty-web")

  const files = new Map<string, string>([
    ["/", join(webRoot, "index.html")],
    ["/app.js", join(webRoot, "app.js")],
    ["/kitty-wasm.js", join(webRoot, "kitty-wasm.js")],
    ["/vendor/ghostty-web.js", join(ghosttyWeb, "dist/ghostty-web.js")],
    ["/vendor/__vite-browser-external-2447137e.js", join(ghosttyWeb, "dist/__vite-browser-external-2447137e.js")],
    ["/vendor/ghostty-vt.wasm", join(ghosttyWeb, "ghostty-vt.wasm")],
    ["/vendor/ghostty-vt-kitty.wasm", resolve(sourceRoot, "../.cache/libghostty-vt-kitty-prefix/bin/ghostty-vt.wasm")],
  ])

  const server = Bun.serve({
    hostname: options.hostname ?? "127.0.0.1",
    port: options.port ?? 4377,
    async fetch(request) {
      const url = new URL(request.url)
      if (url.pathname === "/favicon.ico") return new Response(null, { status: 204 })
      if (url.pathname === "/capabilities") {
        return Response.json({
          nativeFrames: renderer !== null,
          wasmPlayback: true,
          kittyGraphics: renderer !== null,
          kittyWasm: existsSync(files.get("/vendor/ghostty-vt-kitty.wasm")!),
        })
      }
      if (url.pathname === "/frame.png") {
        const requested = Number(url.searchParams.get("atUs") ?? "0")
        try {
          const png = await serialized(async () => {
            if (!renderer) throw new Error("native renderer is not built")
            const atUs = Math.max(
              0,
              Math.min(project.durationUs, Number.isFinite(requested) ? Math.round(requested) : 0),
            )
            const canvas = await renderer.frame(atUs)
            return canvas.encode("png")
          })
          return new Response(Buffer.from(png), {
            headers: { "content-type": "image/png", "cache-control": "no-store" },
          })
        } catch (error) {
          return new Response(error instanceof Error ? error.message : String(error), { status: 501 })
        }
      }
      if (url.pathname === "/project.json") {
        if (request.method === "GET") {
          return Response.json(await readProject(target), { headers: { "cache-control": "no-store" } })
        }
        if (request.method === "PUT") {
          try {
            const value: unknown = await request.json()
            validateProject(value)
            await serialized(async () => {
              await writeProject(target, value)
              await renderer?.close()
              project = value
              renderer = nativeRendererAvailable() ? new ProjectRenderer(project) : null
            })
            return Response.json({ ok: true })
          } catch (error) {
            return new Response(error instanceof Error ? error.message : String(error), { status: 400 })
          }
        }
        return new Response("method not allowed", { status: 405 })
      }
      const path = files.get(url.pathname)
      if (!path) return new Response("not found", { status: 404 })
      const file = Bun.file(path)
      if (!(await file.exists())) return new Response("asset missing", { status: 500 })
      return new Response(file, { headers: { "content-type": contentType(path) } })
    },
  })

  return {
    hostname: server.hostname,
    port: server.port,
    async stop(): Promise<void> {
      server.stop(true)
      await serialized(async () => {
        await renderer?.close()
        renderer = null
      })
    },
  }
}
