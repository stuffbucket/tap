import { existsSync } from "node:fs"
import { resolve } from "node:path"

const OP_WRITE = 1
const OP_RESIZE = 2
const OP_FRAME = 3
const OP_RESET = 4
const OP_QUIT = 5
const OP_SCROLL = 6
const OP_TICK = 7

class StreamReader {
  private readonly reader: ReadableStreamDefaultReader<Uint8Array<ArrayBufferLike>>
  private pending: Uint8Array<ArrayBufferLike> = new Uint8Array(0)

  constructor(stream: ReadableStream<Uint8Array<ArrayBufferLike>>) {
    this.reader = stream.getReader()
  }

  async exact(length: number): Promise<Uint8Array<ArrayBufferLike>> {
    const output = new Uint8Array(length)
    let written = 0
    while (written < length) {
      if (this.pending.byteLength > 0) {
        const count = Math.min(this.pending.byteLength, length - written)
        output.set(this.pending.subarray(0, count), written)
        written += count
        this.pending = this.pending.subarray(count)
        continue
      }
      const next = await this.reader.read()
      if (next.done) throw new Error("native Ghostty renderer exited while a response was pending")
      this.pending = next.value
    }
    return output
  }
}

function u32(bytes: Uint8Array<ArrayBufferLike>, offset: number): number {
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(offset, true)
}

function i32(bytes: Uint8Array<ArrayBufferLike>, offset: number): number {
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getInt32(offset, true)
}

export function nativeRendererPath(): string {
  return resolve(import.meta.dir, "../native/bin/tap-vt-render")
}

export function nativeRendererAvailable(): boolean {
  return existsSync(nativeRendererPath())
}

/** JetBrains Mono and symbol fonts shipped with Termless; the renderer registers them at startup. */
export const bundledFontDirectory = resolve(import.meta.dir, "../node_modules/@termless/core/assets/fonts")

export interface NativeRendererStyle {
  background?: string
  foreground?: string
  palette?: string[]
  fontFamily?: string
  lineHeight?: number
  letterSpacing?: number
}

export class NativePaneRenderer {
  private readonly process: ReturnType<typeof Bun.spawn>
  private readonly stdin: Bun.FileSink
  private readonly reader: StreamReader
  private closed = false
  readonly cellWidth: number
  readonly cellHeight: number

  constructor(cols: number, rows: number, fontSize: number, style: NativeRendererStyle = {}) {
    if (!Number.isSafeInteger(cols) || cols <= 0 || cols > 65_535 ||
        !Number.isSafeInteger(rows) || rows <= 0 || rows > 65_535) {
      throw new RangeError("terminal rows and columns must be integers from 1 through 65535")
    }
    if (!Number.isFinite(fontSize) || fontSize <= 0 || fontSize > 512) {
      throw new RangeError("font size must be greater than 0 and no greater than 512")
    }
    const executable = nativeRendererPath()
    if (!existsSync(executable)) {
      throw new Error(`native libghostty-vt renderer is not built; run: bun run build:native`)
    }
    this.cellWidth = Math.max(4, Math.ceil(fontSize * 0.62 + (style.letterSpacing ?? 0)))
    this.cellHeight = Math.max(8, Math.ceil(style.lineHeight ?? fontSize * 1.35))
    this.process = Bun.spawn(
      [
        executable, String(cols), String(rows), String(this.cellWidth), String(this.cellHeight), String(fontSize),
        style.background ?? "#000000", style.foreground ?? "#ffffff",
        style.palette?.join(",") ?? "", style.fontFamily ?? "JetBrains Mono",
      ],
      { stdin: "pipe", stdout: "pipe", stderr: "pipe", env: { ...process.env, TAP_FONT_DIR: bundledFontDirectory } },
    )
    this.stdin = this.process.stdin as Bun.FileSink
    this.reader = new StreamReader(this.process.stdout as ReadableStream<Uint8Array<ArrayBufferLike>>)
  }

  private async request(
    operation: number,
    payload: Uint8Array<ArrayBufferLike> = new Uint8Array(0),
  ): Promise<Uint8Array<ArrayBufferLike>> {
    if (this.closed) throw new Error("native Ghostty renderer is closed")
    const header = new Uint8Array(5)
    const view = new DataView(header.buffer)
    header[0] = operation
    view.setUint32(1, payload.byteLength, true)
    this.stdin.write(header)
    if (payload.byteLength > 0) this.stdin.write(payload)
    this.stdin.flush()

    const responseHeader = await this.reader.exact(8)
    const status = i32(responseHeader, 0)
    const length = u32(responseHeader, 4)
    const response = length > 0 ? await this.reader.exact(length) : new Uint8Array(0)
    if (status !== 0) {
      throw new Error(`libghostty-vt renderer request ${operation} failed with status ${status}`)
    }
    return response
  }

  /** Feed PTY output and return any terminal-generated reply bytes. */
  write(bytes: Uint8Array<ArrayBufferLike>): Promise<Uint8Array<ArrayBufferLike>> {
    return this.request(OP_WRITE, bytes)
  }

  resize(cols: number, rows: number): Promise<void> {
    if (!Number.isSafeInteger(cols) || cols <= 0 || cols > 65_535 ||
        !Number.isSafeInteger(rows) || rows <= 0 || rows > 65_535) {
      throw new RangeError("terminal rows and columns must be integers from 1 through 65535")
    }
    const payload = new Uint8Array(8)
    const view = new DataView(payload.buffer)
    view.setUint32(0, cols, true)
    view.setUint32(4, rows, true)
    return this.request(OP_RESIZE, payload).then(() => undefined)
  }

  scroll(delta: number): Promise<void> {
    if (!Number.isSafeInteger(delta) || Math.abs(delta) > 1_000_000) {
      throw new RangeError("scroll delta must be a bounded integer")
    }
    const payload = new Uint8Array(4)
    new DataView(payload.buffer).setInt32(0, Math.trunc(delta), true)
    return this.request(OP_SCROLL, payload).then(() => undefined)
  }

  advance(timeMs: number): Promise<void> {
    const payload = new Uint8Array(8)
    new DataView(payload.buffer).setBigUint64(0, BigInt(Math.max(0, Math.floor(timeMs))), true)
    return this.request(OP_TICK, payload).then(() => undefined)
  }

  frame(timeMs: number): Promise<Uint8Array<ArrayBufferLike>> {
    const payload = new Uint8Array(8)
    new DataView(payload.buffer).setBigUint64(0, BigInt(Math.max(0, Math.floor(timeMs))), true)
    return this.request(OP_FRAME, payload)
  }

  reset(): Promise<void> {
    return this.request(OP_RESET).then(() => undefined)
  }

  async close(): Promise<void> {
    if (this.closed) return
    try {
      await this.request(OP_QUIT)
    } finally {
      this.closed = true
      this.stdin.end()
      await this.process.exited
    }
  }
}
