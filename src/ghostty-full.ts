import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { decodePlaybackBytes } from "./bytes.ts"
import { resolveGhosttyApp } from "./ghostty-app.ts"
import type { TapPane } from "./model.ts"

export const ghosttyOffscreen = resolve(import.meta.dir, "../native/bin/tap-ghostty-offscreen")

/** The pane's terminal grid at `atUs`: its starting size, updated by any recorded resize events. */
export function gridAt(pane: TapPane, atUs: number): { cols: number; rows: number } {
  let grid = { cols: pane.cols, rows: pane.rows }
  for (const event of pane.events) if (event.type === "resize" && event.atUs <= atUs) grid = { cols: event.cols, rows: event.rows }
  return grid
}

export interface ReplaySegment { cols: number; rows: number; bytes: Uint8Array }

/**
 * The pane's output up to `atUs`, split at every resize: each segment's bytes were written while
 * the terminal had that segment's grid, so replaying them in order (resizing between segments)
 * reproduces the screen, including how each resize reflowed it.
 */
export function replaySegments(pane: TapPane, atUs: number): ReplaySegment[] {
  const segments: { cols: number; rows: number; chunks: Uint8Array[] }[] = [{ cols: pane.cols, rows: pane.rows, chunks: [] }]
  const events = pane.events.filter((event) => event.atUs <= atUs).sort((a, b) => a.atUs - b.atUs)
  for (const event of events) {
    const current = segments.at(-1)!
    if (event.type === "resize") {
      if (event.cols === current.cols && event.rows === current.rows) continue
      if (current.chunks.length === 0 && segments.length > 1) segments.pop() // nothing written at that size
      segments.push({ cols: event.cols, rows: event.rows, chunks: [] })
    } else if (event.type === "output") {
      current.chunks.push(decodePlaybackBytes(event))
    }
  }
  return segments.map(({ cols, rows, chunks }) => ({ cols, rows, bytes: Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))) }))
}

/**
 * Ghostty frames are as large as the pane's grid at that moment, so they change size when the
 * recording resizes. Video needs one size: each frame is placed top-left on a canvas as large as
 * the largest frame (even, for H.264) and filled with the terminal's own background colour, so
 * nothing is rescaled.
 */
export async function uniformFrameSizes(paths: string[]): Promise<{ width: number; height: number }> {
  const { createCanvas, loadImage } = await import("@napi-rs/canvas")
  let width = 2, height = 2
  for (const path of paths) {
    const image = await loadImage(await Bun.file(path).arrayBuffer().then((buffer) => Buffer.from(buffer)))
    width = Math.max(width, image.width)
    height = Math.max(height, image.height)
  }
  width += width % 2
  height += height % 2
  for (const path of paths) {
    const image = await loadImage(Buffer.from(await Bun.file(path).arrayBuffer()))
    if (image.width === width && image.height === height) continue
    const canvas = createCanvas(width, height)
    const ctx = canvas.getContext("2d")
    ctx.drawImage(image, 0, 0)
    // Ghostty's window padding at the bottom-left corner shows the terminal background.
    const [red, green, blue] = ctx.getImageData(0, image.height - 1, 1, 1).data
    ctx.fillStyle = `rgb(${red}, ${green}, ${blue})`
    ctx.fillRect(image.width, 0, width - image.width, height)
    ctx.fillRect(0, image.height, image.width, height - image.height)
    await Bun.write(path, await canvas.encode("png"))
  }
  return { width, height }
}

/**
 * Renders one pane at `atUs` with the real Ghostty.app renderer: the recorded output up to that
 * moment is replayed into a Ghostty surface, resized between segments exactly as it was recorded,
 * then its IOSurface is saved as PNG at the pane's grid at that moment.
 */
export async function renderGhosttyFrame(pane: TapPane, atUs: number, output: string, options: { quiet?: boolean } = {}) {
  if (!(await Bun.file(ghosttyOffscreen).exists())) throw new Error("run: bun run build:ghostty-full")
  const directory = await mkdtemp(join(tmpdir(), "tap-ghostty-replay-"))
  try {
    const lines: string[] = []
    for (const [index, segment] of replaySegments(pane, atUs).entries()) {
      const file = join(directory, `segment-${index}.vt`)
      await writeFile(file, segment.bytes)
      lines.push(`${segment.cols} ${segment.rows} ${file}`)
    }
    const manifest = join(directory, "replay")
    await writeFile(manifest, `${lines.join("\n")}\n`)
    const ghostty = resolveGhosttyApp()
    const child = Bun.spawn([ghosttyOffscreen, resolve(output), manifest, String(pane.fontSize ?? 16)], {
      env: { ...process.env, TAP_GHOSTTY_STOCK: ghostty.executable, TAP_GHOSTTY_FRAMEWORKS: ghostty.frameworks },
      stdin: "ignore", stdout: options.quiet ? "ignore" : "inherit", stderr: options.quiet ? "ignore" : "inherit",
    })
    if ((await child.exited) !== 0) throw new Error("Ghostty full-renderer capture failed")
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}
