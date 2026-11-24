export const TAP_VERSION = 1 as const

export interface TapCanvas {
  width: number
  height: number
  background: string
  fps: number
}

export interface PaneLayout {
  /** Normalized coordinates in the 0..1 canvas space. */
  x: number
  y: number
  width: number
  height: number
}

export interface TapPane {
  id: string
  title?: string
  cols: number
  rows: number
  fontSize: number
  layout: PaneLayout
  events: TapEvent[]
}

export type TapEvent =
  | {
      atUs: number
      type: "output"
      data: string
      encoding?: "utf8" | "base64"
      /** Self-contained bytes used for replay when external Kitty media was normalized. */
      playbackData?: string
      playbackEncoding?: "utf8" | "base64"
    }
  | {
      atUs: number
      type: "input"
      data: string
      encoding?: "utf8" | "base64"
      origin?: "keyboard" | "paste" | "terminal-reply" | "vhs" | "automation"
      redacted?: boolean
    }
  | { atUs: number; type: "resize"; cols: number; rows: number }
  | { atUs: number; type: "scroll"; delta: number }
  | { atUs: number; type: "marker"; label: string }
  | { atUs: number; type: "layout"; layout: PaneLayout }
  | { atUs: number; type: "visibility"; visible: boolean }

export type TapOverlay =
  | {
      id: string
      type: "text"
      startUs: number
      endUs: number
      x: number
      y: number
      text: string
      color?: string
      font?: string
    }
  | {
      id: string
      type: "image"
      startUs: number
      endUs: number
      x: number
      y: number
      width: number
      height: number
      source: string
      opacity?: number
    }

export interface TapPresentation {
  padding: number
  margin: number
  marginFill: string
  borderRadius: number
  windowBar: "none" | "rings" | "ringsright" | "colorful" | "colorfulright"
  windowBarSize: number
  windowTitle?: string
  shadow: number
  fontFamily?: string
  letterSpacing?: number
  lineHeight?: number
  cursorBlink: boolean
  loopOffsetPercent: number
  themeName?: string
  theme?: { background: string; foreground: string; cursor?: string; palette?: string[] }
}

export interface TapProject {
  version: typeof TAP_VERSION
  title: string
  createdAt: string
  durationUs: number
  canvas: TapCanvas
  presentation?: TapPresentation
  panes: TapPane[]
  overlays: TapOverlay[]
  metadata: {
    terminal: "ghostty"
    terminalEngine: string
    timebase: "microseconds"
    source?: string
  }
}

export interface CapturePaneSpec {
  id: string
  title?: string
  command: string[]
  cwd?: string
  env?: Record<string, string>
  /** Additional roots allowed for Kitty file/temporary-file transport normalization. */
  kittyFileRoots?: string[]
  cols?: number
  rows?: number
  fontSize?: number
  layout?: PaneLayout
}

export interface CaptureSpec {
  title?: string
  canvas?: Partial<TapCanvas>
  panes: CapturePaneSpec[]
}

function finitePositive(value: number, field: string, maximum = Number.POSITIVE_INFINITY): void {
  if (!Number.isFinite(value) || value <= 0 || value > maximum) {
    throw new Error(`${field} must be a positive finite number no greater than ${maximum}`)
  }
}

function boundedPositiveInteger(value: number, field: string, maximum: number): void {
  if (!Number.isSafeInteger(value) || value <= 0 || value > maximum) {
    throw new Error(`${field} must be an integer from 1 through ${maximum}`)
  }
}

function validLayout(layout: PaneLayout, field: string): void {
  for (const [name, value] of Object.entries(layout)) {
    if (!Number.isFinite(value)) throw new Error(`${field}.${name} must be finite`)
  }
  if (layout.x < 0 || layout.y < 0 || layout.width <= 0 || layout.height <= 0) {
    throw new Error(`${field} must have non-negative origin and positive size`)
  }
  if (layout.x + layout.width > 1.000001 || layout.y + layout.height > 1.000001) {
    throw new Error(`${field} must fit inside the normalized canvas`)
  }
}

export function validateProject(value: unknown): asserts value is TapProject {
  if (!value || typeof value !== "object") throw new Error("project must be an object")
  const project = value as Partial<TapProject>
  if (project.version !== TAP_VERSION) throw new Error(`unsupported tap project version: ${String(project.version)}`)
  if (typeof project.title !== "string") throw new Error("title must be a string")
  const durationUs = project.durationUs
  if (!Number.isSafeInteger(durationUs) || durationUs === undefined || durationUs < 0) {
    throw new Error("durationUs must be a non-negative safe integer")
  }
  boundedPositiveInteger(project.canvas?.width ?? 0, "canvas.width", 8192)
  boundedPositiveInteger(project.canvas?.height ?? 0, "canvas.height", 8192)
  if ((project.canvas?.width ?? 0) * (project.canvas?.height ?? 0) > 64 * 1024 * 1024) {
    throw new Error("canvas exceeds the 64-megapixel ceiling")
  }
  finitePositive(project.canvas?.fps ?? 0, "canvas.fps", 240)
  if (project.presentation) {
    const presentation = project.presentation
    for (const [field, value] of Object.entries({
      padding: presentation.padding, margin: presentation.margin, borderRadius: presentation.borderRadius,
      windowBarSize: presentation.windowBarSize, shadow: presentation.shadow,
    })) {
      if (!Number.isFinite(value) || value < 0 || value > 4096) throw new Error(`presentation.${field} is invalid`)
    }
    if (!Number.isFinite(presentation.loopOffsetPercent) || presentation.loopOffsetPercent < 0 || presentation.loopOffsetPercent > 100) {
      throw new Error("presentation.loopOffsetPercent must be from 0 through 100")
    }
  }
  if (!Array.isArray(project.panes) || project.panes.length === 0) throw new Error("project needs at least one pane")
  const ids = new Set<string>()
  for (const [index, pane] of project.panes.entries()) {
    if (!pane.id || ids.has(pane.id)) throw new Error(`pane ${index} needs a unique id`)
    ids.add(pane.id)
    boundedPositiveInteger(pane.cols, `panes[${index}].cols`, 65_535)
    boundedPositiveInteger(pane.rows, `panes[${index}].rows`, 65_535)
    finitePositive(pane.fontSize, `panes[${index}].fontSize`, 512)
    validLayout(pane.layout, `panes[${index}].layout`)
    let previous = -1
    for (const event of pane.events) {
      if (!Number.isSafeInteger(event.atUs) || event.atUs < previous) {
        throw new Error(`panes[${index}].events must use sorted, non-negative integer microseconds`)
      }
      previous = event.atUs
      if (event.atUs > durationUs) throw new Error(`panes[${index}].event is beyond project duration`)
      if (!["output", "input", "resize", "scroll", "marker", "layout", "visibility"].includes(event.type)) {
        throw new Error(`panes[${index}] has an unknown event type`)
      }
      if (event.type === "layout") validLayout(event.layout, `panes[${index}].events.layout`)
      if (event.type === "resize") {
        boundedPositiveInteger(event.cols, `panes[${index}].event.resize.cols`, 65_535)
        boundedPositiveInteger(event.rows, `panes[${index}].event.resize.rows`, 65_535)
      }
      if (event.type === "scroll" && (!Number.isSafeInteger(event.delta) || Math.abs(event.delta) > 1_000_000)) {
        throw new Error(`panes[${index}].event.scroll.delta must be a bounded integer`)
      }
      if ((event.type === "output" || event.type === "input") && typeof event.data !== "string") {
        throw new Error(`panes[${index}] byte event data must be a string`)
      }
      if (event.type === "output" && event.playbackData !== undefined && typeof event.playbackData !== "string") {
        throw new Error(`panes[${index}] output playbackData must be a string`)
      }
    }
  }
  if (!Array.isArray(project.overlays)) throw new Error("overlays must be an array")
  for (const [index, overlay] of project.overlays.entries()) {
    if (!overlay.id || (overlay.type !== "text" && overlay.type !== "image")) {
      throw new Error(`overlays[${index}] has an invalid id or type`)
    }
    if (!Number.isSafeInteger(overlay.startUs) || !Number.isSafeInteger(overlay.endUs) ||
        overlay.startUs < 0 || overlay.endUs < overlay.startUs || overlay.endUs > durationUs) {
      throw new Error(`overlays[${index}] has an invalid time range`)
    }
  }
}

export function defaultLayouts(count: number): PaneLayout[] {
  if (count <= 0) return []
  if (count === 1) return [{ x: 0, y: 0, width: 1, height: 1 }]
  if (count === 2) {
    return [
      { x: 0, y: 0, width: 0.5, height: 1 },
      { x: 0.5, y: 0, width: 0.5, height: 1 },
    ]
  }
  const cols = Math.ceil(Math.sqrt(count))
  const rows = Math.ceil(count / cols)
  return Array.from({ length: count }, (_, index) => ({
    x: (index % cols) / cols,
    y: Math.floor(index / cols) / rows,
    width: 1 / cols,
    height: 1 / rows,
  }))
}
