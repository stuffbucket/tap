import { createCanvas, loadImage, type Canvas } from "@napi-rs/canvas"
import { extname, resolve } from "node:path"
import { mkdir, writeFile } from "node:fs/promises"
import { decodePlaybackBytes } from "./bytes.ts"
import { NativePaneRenderer, nativeRendererAvailable } from "./native-renderer.ts"
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

export interface RenderOptions {
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

function presentationFrame(canvas: Canvas, project: TapProject) {
  const ctx = canvas.getContext("2d")
  const p = project.presentation
  if (!p) return { x: 0, y: 0, width: canvas.width, height: canvas.height }
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
      ctx.arc(right ? frameX + frameWidth - 16 - index * 18 : frameX + 16 + index * 18, frameY + p.windowBarSize / 2, 6, 0, Math.PI * 2); ctx.fill()
    }
    if (p.windowTitle) { ctx.fillStyle = p.theme?.foreground ?? "#c0caf5"; ctx.font = "12px sans-serif"; ctx.textAlign = "center"; ctx.fillText(p.windowTitle, frameX + frameWidth / 2, frameY + p.windowBarSize / 2 + 4) }
    ctx.restore()
  }
  return {
    x: frameX + p.padding,
    y: frameY + p.padding + (p.windowBar === "none" ? 0 : p.windowBarSize),
    width: Math.max(1, frameWidth - p.padding * 2),
    height: Math.max(1, frameHeight - p.padding * 2 - (p.windowBar === "none" ? 0 : p.windowBarSize)),
  }
}

export class ProjectRenderer {
  readonly project: TapProject
  private readonly states = new Map<string, PaneRenderState>()
  private lastAtUs = -1

  constructor(project: TapProject) {
    this.project = project
    this.createStates()
  }

  private createStates(): void {
    if (!nativeRendererAvailable()) {
      throw new Error("rendering requires the stable libghostty-vt backend; run: bun run build:native")
    }
    for (const pane of this.project.panes) {
      this.states.set(pane.id, {
        eventIndex: 0,
        cols: pane.cols,
        rows: pane.rows,
        layout: pane.layout,
        png: null,
        visible: true,
        native: new NativePaneRenderer(pane.cols, pane.rows, pane.fontSize, {
          ...(this.project.presentation?.theme?.background ? { background: this.project.presentation.theme.background } : {}),
          ...(this.project.presentation?.theme?.foreground ? { foreground: this.project.presentation.theme.foreground } : {}),
          ...(this.project.presentation?.theme?.palette ? { palette: this.project.presentation.theme.palette } : {}),
          ...(this.project.presentation?.fontFamily ? { fontFamily: this.project.presentation.fontFamily } : {}),
          ...(this.project.presentation?.lineHeight !== undefined ? { lineHeight: this.project.presentation.lineHeight } : {}),
          ...(this.project.presentation?.letterSpacing !== undefined ? { letterSpacing: this.project.presentation.letterSpacing } : {}),
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

    const canvas = createCanvas(this.project.canvas.width, this.project.canvas.height)
    const ctx = canvas.getContext("2d")
    ctx.fillStyle = this.project.canvas.background
    ctx.fillRect(0, 0, canvas.width, canvas.height)
    const content = presentationFrame(canvas, this.project)

    for (const pane of this.project.panes) {
      const state = this.states.get(pane.id)!
      await updatePaneAt(pane, state, atUs)
      const destination = pixelLayout(state.layout, content.width, content.height, content.x, content.y)
      if (!state.visible) continue
      // Native libghostty-vt advances Kitty animations from project time on
      // every sampled frame, even when no new PTY bytes arrived.
      state.png = await state.native.frame(atUs / 1_000)
      if (!state.png) continue
      const image = await loadImage(Buffer.from(state.png))
      ctx.drawImage(image, destination.x, destination.y, destination.width, destination.height)
    }

    await drawOverlays(canvas, this.project, atUs)
    return canvas
  }

  async close(): Promise<void> {
    await Promise.all([...this.states.values()].map((state) => state.native.close()))
  }
}

export async function renderScreenshot(project: TapProject, output: string, atUs = project.durationUs): Promise<void> {
  const renderer = new ProjectRenderer(project)
  try {
    const canvas = await renderer.frame(Math.max(0, Math.min(atUs, project.durationUs)))
    await writeFile(resolve(output), await canvas.encode("png"))
  } finally {
    await renderer.close()
  }
}

export async function renderFrameDirectory(project: TapProject, output: string, fps = project.canvas.fps): Promise<number> {
  const directory = resolve(output)
  await mkdir(directory, { recursive: true })
  const frameDurationUs = 1_000_000 / fps
  const totalFrames = Math.max(1, Math.floor(project.durationUs / frameDurationUs) + 1)
  const renderer = new ProjectRenderer(project)
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
  const fps = options.fps ?? project.canvas.fps
  if (!Number.isFinite(fps) || fps <= 0 || fps > 240) throw new Error("fps must be between 0 and 240")
  const startUs = Math.max(0, options.startUs ?? 0)
  const endUs = Math.min(project.durationUs, options.endUs ?? project.durationUs)
  if (endUs < startUs) throw new Error("render end must not precede start")
  const frameDurationUs = 1_000_000 / fps
  const totalFrames = Math.max(1, Math.floor((endUs - startUs) / frameDurationUs) + 1)
  const target = resolve(output)
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
    `${project.canvas.width}x${project.canvas.height}`,
    "-framerate",
    String(fps),
    "-i",
    "pipe:0",
    "-an",
    ...ffmpegCodecArgs(target, project.canvas.width, project.canvas.height),
    target,
  ]
  const ffmpeg = Bun.spawn(["ffmpeg", ...args], { stdin: "pipe", stdout: "ignore", stderr: "pipe" })
  const renderer = new ProjectRenderer(project)
  const loopOffsetFrames = extname(output).toLowerCase() === ".gif"
    ? Math.round(totalFrames * (project.presentation?.loopOffsetPercent ?? 0) / 100)
    : 0

  try {
    for (let frame = 0; frame < totalFrames; frame++) {
      const sample = (frame + loopOffsetFrames) % totalFrames
      const atUs = Math.min(endUs, Math.round(startUs + sample * frameDurationUs))
      const canvas = await renderer.frame(atUs)
      const pixels = canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height).data
      ffmpeg.stdin.write(pixels)
      options.onProgress?.(frame + 1, totalFrames)
    }
    await ffmpeg.stdin.end()
    const exitCode = await ffmpeg.exited
    if (exitCode !== 0) {
      const error = await new Response(ffmpeg.stderr).text()
      throw new Error(`ffmpeg exited with ${exitCode}: ${error.trim()}`)
    }
  } catch (error) {
    ffmpeg.kill()
    throw error
  } finally {
    await renderer.close()
  }
}
