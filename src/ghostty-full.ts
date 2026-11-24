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

/**
 * Renders one pane at `atUs` with the real Ghostty.app renderer: every output byte recorded up to
 * that moment is replayed into a Ghostty surface sized to the pane's grid, then its IOSurface is
 * saved as PNG. Replay is cumulative, so earlier mid-session resizes reflow at the final size.
 */
export async function renderGhosttyFrame(pane: TapPane, atUs: number, output: string, options: { quiet?: boolean } = {}) {
  if (!(await Bun.file(ghosttyOffscreen).exists())) throw new Error("run: bun run build:ghostty-full")
  const chunks = pane.events
    .filter((event) => event.type === "output" && event.atUs <= atUs)
    .map((event) => event.type === "output" ? decodePlaybackBytes(event) : new Uint8Array(0))
  const directory = await mkdtemp(join(tmpdir(), "tap-ghostty-replay-"))
  try {
    const replay = join(directory, "output.vt")
    await writeFile(replay, Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))))
    const { cols, rows } = gridAt(pane, atUs)
    const ghostty = resolveGhosttyApp()
    const child = Bun.spawn([ghosttyOffscreen, resolve(output), String(cols), String(rows), replay, String(pane.fontSize ?? 16)], {
      env: { ...process.env, TAP_GHOSTTY_STOCK: ghostty.executable, TAP_GHOSTTY_FRAMEWORKS: ghostty.frameworks },
      stdin: "ignore", stdout: options.quiet ? "ignore" : "inherit", stderr: options.quiet ? "ignore" : "inherit",
    })
    if ((await child.exited) !== 0) throw new Error("Ghostty full-renderer capture failed")
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}
