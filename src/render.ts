import { createCanvas, loadImage, type Canvas } from "@napi-rs/canvas"
import { basename, extname, resolve } from "node:path"
import { mkdir, writeFile } from "node:fs/promises"
import { decodePlaybackBytes } from "./bytes.ts"
import { NativePaneRenderer, cellMetrics, nativeRendererAvailable } from "./native-renderer.ts"
import type { PaneLayout, TapPane, TapProject } from "./model.ts"

interface PaneRenderState {
  eventIndex: number
  cols: number
  rows: number
  layout: PaneLayout
  png: Uint8Array<ArrayBufferLike> | null
  visible: boolean
  native: NativePaneRenderer
}

/** Output scale: 2 renders at twice the project canvas size, with text drawn at twice the size. */
export interface ScaleOptions {
  scale?: number
}

export interface RenderOptions extends ScaleOptions {
  fps?: number
  startUs?: number
  endUs?: number
  onProgress?: (frame: number, total: number) => void
}

function pixelLayout(layout: PaneLayout, width: number, height: number, originX = 0, originY = 0) {
  const x = originX + Math.round(layout.x * width)
  const y = originY + Math.round(layout.y * height)
  const right = originX + Math.round((layout.x + layout.width) * width)
  const bottom = originY + Math.round((layout.y + layout.height) * height)
  return { x, y, width: Math.max(1, right - x), height: Math.max(1, bottom - y) }
}

async function updatePaneAt(pane: TapPane, state: PaneRenderState, atUs: number): Promise<void> {
  while (state.eventIndex < pane.events.length) {
    const event = pane.events[state.eventIndex]!
    if (event.atUs > atUs) break
    state.eventIndex++
    switch (event.type) {
      case "output": {
        const bytes = decodePlaybackBytes(event)
        await state.native.write(bytes)
        await state.native.advance(event.atUs / 1_000)
        break
      }
      case "resize":
        state.cols = event.cols
        state.rows = event.rows
        await state.native.resize(event.cols, event.rows)
        break
      case "scroll":
        await state.native.scroll(event.delta)
        break
      case "layout":
        state.layout = event.layout
        break
      case "visibility":
        state.visible = event.visible
        break
      case "input":
      case "marker":
        break
    }
  }
}

async function drawOverlays(canvas: Canvas, project: TapProject, atUs: number): Promise<void> {
  const ctx = canvas.getContext("2d")
  for (const overlay of project.overlays) {
    if (atUs < overlay.startUs || atUs >= overlay.endUs) continue
    if (overlay.type === "text") {
      ctx.save()
      ctx.fillStyle = overlay.color ?? "#ffffff"
      ctx.font = overlay.font ?? "24px sans-serif"
      ctx.textBaseline = "top"
      ctx.fillText(overlay.text, overlay.x, overlay.y)
      ctx.restore()
    } else {
      const image = await loadImage(resolve(overlay.source))
      ctx.save()
      ctx.globalAlpha = overlay.opacity ?? 1
      ctx.drawImage(image, overlay.x, overlay.y, overlay.width, overlay.height)
      ctx.restore()
    }
  }
}

/** Presentation metrics in output pixels: the project's values multiplied by the output scale. */
function scaledPresentation(project: TapProject, scale: number) {
  const p = project.presentation
  if (!p) return null
  return { ...p, margin: p.margin * scale, padding: p.padding * scale, borderRadius: p.borderRadius * scale, shadow: p.shadow * scale, windowBarSize: p.windowBarSize * scale }
}

/** Inset for recordings without presentation settings, so text never touches the frame edge. */
const DEFAULT_INSET = 12

/** Where terminal content goes inside a canvas of the given size, after margin, bar and padding. */
function contentRect(project: TapProject, width: number, height: number, scale: number) {
  const p = scaledPresentation(project, scale)
  if (!p) {
    const inset = Math.round(DEFAULT_INSET * scale)
    return { x: inset, y: inset, width: Math.max(1, width - inset * 2), height: Math.max(1, height - inset * 2) }
  }
  const bar = p.windowBar === "none" ? 0 : p.windowBarSize
  const frameWidth = Math.max(1, width - p.margin * 2)
  const frameHeight = Math.max(1, height - p.margin * 2)
  return {
    x: p.margin + p.padding,
    y: p.margin + p.padding + bar,
    width: Math.max(1, frameWidth - p.padding * 2),
    height: Math.max(1, frameHeight - p.padding * 2 - bar),
  }
}

function presentationFrame(canvas: Canvas, project: TapProject, scale: number) {
  const ctx = canvas.getContext("2d")
  const p = scaledPresentation(project, scale)
  if (!p) return contentRect(project, canvas.width, canvas.height, scale)
  ctx.fillStyle = p.marginFill
  ctx.fillRect(0, 0, canvas.width, canvas.height)
  const frameX = p.margin, frameY = p.margin
  const frameWidth = Math.max(1, canvas.width - p.margin * 2)
  const frameHeight = Math.max(1, canvas.height - p.margin * 2)
  ctx.save()
  ctx.shadowColor = p.shadow > 0 ? "rgba(0,0,0,.45)" : "transparent"
  ctx.shadowBlur = p.shadow
  ctx.beginPath()
  ctx.roundRect(frameX, frameY, frameWidth, frameHeight, p.borderRadius)
  ctx.fillStyle = p.theme?.background ?? "#1a1b26"
  ctx.fill()
  ctx.restore()
  if (p.windowBar !== "none" && p.windowBarSize > 0) {
    ctx.save()
    ctx.beginPath()
    ctx.roundRect(frameX, frameY, frameWidth, frameHeight, p.borderRadius)
    ctx.clip()
    ctx.fillStyle = p.theme?.background ?? "#1a1b26"
    ctx.fillRect(frameX, frameY, frameWidth, p.windowBarSize)
    const right = p.windowBar.endsWith("right")
    const colors = p.windowBar.startsWith("colorful") ? ["#ff5f57", "#febc2e", "#28c840"] : ["#778099", "#778099", "#778099"]
    for (let index = 0; index < 3; index++) {
      ctx.beginPath(); ctx.fillStyle = colors[index]!
      ctx.arc(right ? frameX + frameWidth - (16 + index * 18) * scale : frameX + (16 + index * 18) * scale, frameY + p.windowBarSize / 2, 6 * scale, 0, Math.PI * 2); ctx.fill()
    }
    if (p.windowTitle) { ctx.fillStyle = p.theme?.foreground ?? "#c0caf5"; ctx.font = `${12 * scale}px sans-serif`; ctx.textAlign = "center"; ctx.fillText(p.windowTitle, frameX + frameWidth / 2, frameY + p.windowBarSize / 2 + 4 * scale) }
    ctx.restore()
  }
  return contentRect(project, canvas.width, canvas.height, scale)
}

/**
 * The largest grid a pane shows during the recording. Sizes replaced at the same instant (several
 * resizes applied together, e.g. from editing) are never on screen and are ignored.
 */
export function maxGrid(pane: TapPane): { cols: number; rows: number } {
  const resizes = pane.events.filter((event) => event.type === "resize").sort((a, b) => a.atUs - b.atUs)
  let cols = pane.cols, rows = pane.rows
  let current = { atUs: 0, cols: pane.cols, rows: pane.rows }
  for (const event of resizes) {
    if (event.type !== "resize") continue
    if (event.atUs > current.atUs) { cols = Math.max(cols, current.cols); rows = Math.max(rows, current.rows) }
    current = { atUs: event.atUs, cols: event.cols, rows: event.rows }
  }
  return { cols: Math.max(cols, current.cols), rows: Math.max(rows, current.rows) }
}

/**
 * The font size at which a pane's largest grid exactly fits its area, so text is drawn at its
 * final pixel size instead of a bitmap being stretched into place.
 */
export function fittedFontSize(pane: TapPane, area: { width: number; height: number }, style: { lineHeight?: number; letterSpacing?: number } = {}): number {
  const grid = maxGrid(pane)
  const base = cellMetrics(pane.fontSize, style)
  let fontSize = pane.fontSize * Math.min(area.width / (grid.cols * base.width), area.height / (grid.rows * base.height))
  for (let attempt = 0; attempt < 100; attempt++) {
    const cell = cellMetrics(fontSize, { ...style, letterSpacing: (style.letterSpacing ?? 0) * fontSize / pane.fontSize })
    if (cell.width * grid.cols <= area.width && cell.height * grid.rows <= area.height) break
    fontSize *= 0.99
  }
  return Math.max(4, fontSize)
}

export class ProjectRenderer {
  readonly project: TapProject
  private readonly states = new Map<string, PaneRenderState>()
  private lastAtUs = -1
  readonly scale: number
  /** Output size in pixels: the project canvas multiplied by the scale. */
  readonly width: number
  readonly height: number

  constructor(project: TapProject, options: ScaleOptions = {}) {
    this.project = project
    this.scale = Math.max(0.1, Math.min(8, options.scale ?? 1))
    this.width = Math.max(2, Math.round(project.canvas.width * this.scale))
    this.height = Math.max(2, Math.round(project.canvas.height * this.scale))
    this.createStates()
  }

  private createStates(): void {
    if (!nativeRendererAvailable()) {
      throw new Error("rendering requires the stable libghostty-vt backend; run: bun run build:native")
    }
    const content = contentRect(this.project, this.width, this.height, this.scale)
    const presentation = this.project.presentation
    for (const pane of this.project.panes) {
      const area = pixelLayout(pane.layout, content.width, content.height)
      const style = {
        ...(presentation?.lineHeight !== undefined ? { lineHeight: presentation.lineHeight } : {}),
        ...(presentation?.letterSpacing !== undefined ? { letterSpacing: presentation.letterSpacing } : {}),
      }
      const fontSize = fittedFontSize(pane, area, style)
      const fit = fontSize / pane.fontSize
      this.states.set(pane.id, {
        eventIndex: 0,
        cols: pane.cols,
        rows: pane.rows,
        layout: pane.layout,
        png: null,
        visible: true,
        native: new NativePaneRenderer(pane.cols, pane.rows, fontSize, {
          ...(this.project.presentation?.theme?.background ? { background: this.project.presentation.theme.background } : {}),
          ...(this.project.presentation?.theme?.foreground ? { foreground: this.project.presentation.theme.foreground } : {}),
          ...(this.project.presentation?.theme?.palette ? { palette: this.project.presentation.theme.palette } : {}),
          ...(this.project.presentation?.fontFamily ? { fontFamily: this.project.presentation.fontFamily } : {}),
          ...(this.project.presentation?.lineHeight !== undefined ? { lineHeight: this.project.presentation.lineHeight } : {}),
          ...(this.project.presentation?.letterSpacing !== undefined ? { letterSpacing: this.project.presentation.letterSpacing * fit } : {}),
        }),
      })
    }
    this.lastAtUs = -1
  }

  private async rewind(): Promise<void> {
    await Promise.all([...this.states.values()].map((state) => state.native.close()))
    this.states.clear()
    this.createStates()
  }

  async frame(atUs: number): Promise<Canvas> {
    if (atUs < this.lastAtUs) await this.rewind()
    this.lastAtUs = atUs

    const canvas = createCanvas(this.width, this.height)
    const ctx = canvas.getContext("2d")
    ctx.fillStyle = this.project.canvas.background
    ctx.fillRect(0, 0, canvas.width, canvas.height)
    const content = presentationFrame(canvas, this.project, this.scale)

    for (const pane of this.project.panes) {
      const state = this.states.get(pane.id)!
      await updatePaneAt(pane, state, atUs)
      const destination = pixelLayout(state.layout, content.width, content.height, content.x, content.y)
      if (!state.visible) continue
      // Native libghostty-vt advances Kitty animations from project time on
      // every sampled frame, even when no new PTY bytes arrived.
      state.png = await state.native.frame(atUs / 1_000)
      if (!state.png) continue
      // Drawn 1:1 from the pane's top-left, like a terminal window: the font was sized so the
      // largest grid fits, so nothing is resampled; a later layout change clips rather than stretches.
      const image = await loadImage(Buffer.from(state.png))
      ctx.save()
      ctx.beginPath()
      ctx.rect(destination.x, destination.y, destination.width, destination.height)
      ctx.clip()
      ctx.drawImage(image, destination.x, destination.y)
      ctx.restore()
    }

    await drawOverlays(canvas, this.project, atUs)
    return canvas
  }

  async close(): Promise<void> {
    await Promise.all([...this.states.values()].map((state) => state.native.close()))
  }
}

export async function renderScreenshot(project: TapProject, output: string, atUs = project.durationUs, options: ScaleOptions = {}): Promise<void> {
  const renderer = new ProjectRenderer(project, options)
  try {
    const canvas = await renderer.frame(Math.max(0, Math.min(atUs, project.durationUs)))
    await writeFile(resolve(output), await canvas.encode("png"))
  } finally {
    await renderer.close()
  }
}

export async function renderFrameDirectory(project: TapProject, output: string, fps = project.canvas.fps, options: ScaleOptions = {}): Promise<number> {
  const directory = resolve(output)
  await mkdir(directory, { recursive: true })
  const frameDurationUs = 1_000_000 / fps
  const totalFrames = Math.max(1, Math.floor(project.durationUs / frameDurationUs) + 1)
  const renderer = new ProjectRenderer(project, options)
  try {
    for (let frame = 0; frame < totalFrames; frame++) {
      const atUs = Math.min(project.durationUs, Math.round(frame * frameDurationUs))
      const canvas = await renderer.frame(atUs)
      const name = `${String(frame + 1).padStart(6, "0")}.png`
      await writeFile(resolve(directory, name), await canvas.encode("png"))
    }
  } finally {
    await renderer.close()
  }
  return totalFrames
}

/** Containers `renderVideo` writes; the extension picks the codec (see ffmpegCodecArgs). */
export const VIDEO_FORMATS = [".mp4", ".m4v", ".mkv", ".mov", ".webm", ".gif"]

function ffmpegCodecArgs(output: string, width: number, height: number): string[] {
  const pad = width % 2 !== 0 || height % 2 !== 0
    ? ["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]
    : []
  switch (extname(output).toLowerCase()) {
    case ".webm":
      return [...pad, "-c:v", "libvpx-vp9", "-crf", "28", "-b:v", "0", "-pix_fmt", "yuv420p"]
    case ".mov":
      return [...pad, "-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le"]
    case ".gif":
      return ["-vf", "split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse"]
    default:
      return [...pad, "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p"]
  }
}

export async function renderVideo(project: TapProject, output: string, options: RenderOptions = {}): Promise<void> {
  const format = extname(output).toLowerCase()
  if (!VIDEO_FORMATS.includes(format)) {
    throw new Error(`unsupported video format "${format || basename(output)}"; use ${VIDEO_FORMATS.join(", ")}`)
  }
  const fps = options.fps ?? project.canvas.fps
  if (!Number.isFinite(fps) || fps <= 0 || fps > 240) throw new Error("fps must be between 0 and 240")
  const startUs = Math.max(0, options.startUs ?? 0)
  const endUs = Math.min(project.durationUs, options.endUs ?? project.durationUs)
  if (endUs < startUs) throw new Error("render end must not precede start")
  const frameDurationUs = 1_000_000 / fps
  const totalFrames = Math.max(1, Math.floor((endUs - startUs) / frameDurationUs) + 1)
  const target = resolve(output)
  const renderer = new ProjectRenderer(project, options)
  const args = [
    "-hide_banner",
    "-loglevel",
    "error",
    "-y",
    "-f",
    "rawvideo",
    "-pixel_format",
    "rgba",
    "-video_size",
    `${renderer.width}x${renderer.height}`,
    "-framerate",
    String(fps),
    "-i",
    "pipe:0",
    "-an",
    ...ffmpegCodecArgs(target, renderer.width, renderer.height),
    target,
  ]
  const ffmpeg = Bun.spawn(["ffmpeg", ...args], { stdin: "pipe", stdout: "ignore", stderr: "pipe" })
  const loopOffsetFrames = extname(output).toLowerCase() === ".gif"
    ? Math.round(totalFrames * (project.presentation?.loopOffsetPercent ?? 0) / 100)
    : 0

  const ffmpegFailure = async (): Promise<Error> => {
    const exitCode = await ffmpeg.exited
    const message = (await new Response(ffmpeg.stderr).text()).trim()
    return new Error(`ffmpeg exited with ${exitCode}${message ? `: ${message}` : ""}`)
  }

  try {
    for (let frame = 0; frame < totalFrames; frame++) {
      const sample = (frame + loopOffsetFrames) % totalFrames
      const atUs = Math.min(endUs, Math.round(startUs + sample * frameDurationUs))
      const canvas = await renderer.frame(atUs)
      const pixels = canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height).data
      // If FFmpeg has stopped, the write fails with EPIPE; report FFmpeg's own error instead.
      if (ffmpeg.exitCode !== null) throw await ffmpegFailure()
      try {
        ffmpeg.stdin.write(pixels)
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === "EPIPE") throw await ffmpegFailure()
        throw error
      }
      options.onProgress?.(frame + 1, totalFrames)
    }
    await ffmpeg.stdin.end()
    const exitCode = await ffmpeg.exited
    if (exitCode !== 0) throw await ffmpegFailure()
  } catch (error) {
    ffmpeg.kill()
    throw error
  } finally {
    await renderer.close()
  }
}
