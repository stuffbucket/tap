import { readFile, realpath, rm, stat } from "node:fs/promises"
import { existsSync } from "node:fs"
import { isAbsolute, relative, resolve, sep } from "node:path"
import { tmpdir } from "node:os"
import { createCanvas, loadImage } from "@napi-rs/canvas"

const START = Uint8Array.from([0x1b, 0x5f, 0x47]) // ESC _ G
const END = Uint8Array.from([0x1b, 0x5c]) // ESC \
const MAX_EXTERNAL_BYTES = 64 * 1024 * 1024
const PAYLOAD_CHUNK = 4096

function concat(chunks: readonly Uint8Array[]): Uint8Array {
  const length = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0)
  const output = new Uint8Array(length)
  let offset = 0
  for (const chunk of chunks) {
    output.set(chunk, offset)
    offset += chunk.byteLength
  }
  return output
}

function find(haystack: Uint8Array, needle: Uint8Array, from = 0): number {
  outer: for (let index = from; index <= haystack.byteLength - needle.byteLength; index++) {
    for (let part = 0; part < needle.byteLength; part++) {
      if (haystack[index + part] !== needle[part]) continue outer
    }
    return index
  }
  return -1
}

function startPrefixLength(bytes: Uint8Array): number {
  const maximum = Math.min(START.byteLength - 1, bytes.byteLength)
  for (let length = maximum; length > 0; length--) {
    let matches = true
    for (let index = 0; index < length; index++) {
      if (bytes[bytes.byteLength - length + index] !== START[index]) {
        matches = false
        break
      }
    }
    if (matches) return length
  }
  return 0
}

function within(path: string, root: string): boolean {
  const child = relative(root, path)
  return child === "" || (!child.startsWith(`..${sep}`) && child !== ".." && !isAbsolute(child))
}

function controlParams(control: string): Map<string, string> {
  const result = new Map<string, string>()
  for (const item of control.split(",")) {
    const equals = item.indexOf("=")
    if (equals > 0) result.set(item.slice(0, equals), item.slice(equals + 1))
  }
  return result
}

function directSequences(control: string, bytes: Uint8Array): Uint8Array {
  const encoded = Buffer.from(bytes).toString("base64")
  const params = control
    .split(",")
    .filter((item) => item.length > 0 && !item.startsWith("t=") && !item.startsWith("m="))
  params.push("t=d")
  const chunks: Uint8Array[] = []
  const count = Math.max(1, Math.ceil(encoded.length / PAYLOAD_CHUNK))
  for (let index = 0; index < count; index++) {
    const payload = encoded.slice(index * PAYLOAD_CHUNK, (index + 1) * PAYLOAD_CHUNK)
    const more = index < count - 1
    const packetControl = index === 0 ? [...params, `m=${more ? 1 : 0}`].join(",") : `m=${more ? 1 : 0}`
    chunks.push(new TextEncoder().encode(`\x1b_G${packetControl};${payload}\x1b\\`))
  }
  return concat(chunks)
}

async function directRenderableSequences(control: string, bytes: Uint8Array): Promise<Uint8Array> {
  const params = controlParams(control)
  if (params.get("f") !== "100") return directSequences(control, bytes)
  try {
    const image = await loadImage(Buffer.from(bytes))
    const canvas = createCanvas(image.width, image.height)
    const context = canvas.getContext("2d")
    context.drawImage(image, 0, 0)
    const rgba = context.getImageData(0, 0, image.width, image.height).data
    const rewritten = control
      .split(",")
      .filter((item) => !item.startsWith("f=") && !item.startsWith("s=") && !item.startsWith("v=") && !item.startsWith("O="))
    rewritten.push("f=32", `s=${image.width}`, `v=${image.height}`)
    return directSequences(rewritten.join(","), Uint8Array.from(rgba))
  } catch {
    return directSequences(control, bytes)
  }
}

async function readPosixSharedMemory(name: string, expectedSize?: number): Promise<Uint8Array | null> {
  if (!/^\/[!-\.0-~]+$/.test(name) || name.slice(1).includes("/")) return null
  const helper = resolve(import.meta.dir, "../native/bin/tap-shm-read")
  if (!existsSync(helper)) return null
  const process = Bun.spawn(
    [helper, name, ...(expectedSize === undefined ? [] : [String(expectedSize)])],
    { stdin: "ignore", stdout: "pipe", stderr: "pipe" },
  )
  const stdout = new Response(process.stdout).arrayBuffer()
  const stderr = new Response(process.stderr).text()
  if ((await process.exited) !== 0) {
    await stderr
    return null
  }
  return new Uint8Array(await stdout)
}

export interface KittyNormalizerOptions {
  cwd?: string
  allowedRoots?: string[]
  maxExternalBytes?: number
}

/**
 * Streaming transport normalizer for Kitty file and temporary-file packets.
 * It preserves the original PTY bytes separately; this returns only the bytes
 * to feed into the replay terminal.
 */
export class KittyNormalizer {
  private pending = new Uint8Array(0)
  private readonly cwd: string
  private readonly roots: string[]
  private readonly maxExternalBytes: number
  private rootsReady: Promise<void> | null = null

  constructor(options: KittyNormalizerOptions = {}) {
    this.cwd = resolve(options.cwd ?? process.cwd())
    this.roots = [
      this.cwd,
      tmpdir(),
      ...(process.platform === "linux" ? ["/dev/shm"] : []),
      ...(options.allowedRoots ?? []),
    ].map((path) => resolve(path))
    this.maxExternalBytes = Math.min(options.maxExternalBytes ?? MAX_EXTERNAL_BYTES, MAX_EXTERNAL_BYTES)
  }

  private async prepareRoots(): Promise<void> {
    if (!this.rootsReady) {
      this.rootsReady = Promise.all(
        this.roots.map(async (root, index) => {
          try {
            this.roots[index] = await realpath(root)
          } catch {
            // A configured root that does not exist simply authorizes nothing.
          }
        }),
      ).then(() => undefined)
    }
    await this.rootsReady
  }

  private async normalizeSequence(sequence: Uint8Array): Promise<Uint8Array> {
    // Strip ESC_G and ESC\\.
    const body = new TextDecoder().decode(sequence.subarray(START.byteLength, sequence.byteLength - END.byteLength))
    const semicolon = body.indexOf(";")
    if (semicolon === -1) return sequence
    const control = body.slice(0, semicolon)
    const params = controlParams(control)
    const medium = params.get("t")
    if (medium !== "f" && medium !== "t" && medium !== "s") {
      if ((medium === undefined || medium === "d") && params.get("f") === "100") {
        const png = Buffer.from(body.slice(semicolon + 1), "base64")
        return directRenderableSequences(control, png)
      }
      return sequence
    }

    let requestedPath: string
    try {
      requestedPath = Buffer.from(body.slice(semicolon + 1), "base64").toString("utf8")
    } catch {
      return sequence
    }
    if (!requestedPath || requestedPath.includes("\0")) return sequence

    if (medium === "s" && process.platform !== "linux") {
      const declaredSize = params.has("S") ? Number(params.get("S")) : undefined
      if (declaredSize !== undefined &&
          (!Number.isSafeInteger(declaredSize) || declaredSize < 0 || declaredSize > this.maxExternalBytes)) {
        return sequence
      }
      const bytes = await readPosixSharedMemory(requestedPath, declaredSize)
      if (!bytes || bytes.byteLength > this.maxExternalBytes) return sequence
      return directRenderableSequences(control, bytes)
    }

    await this.prepareRoots()
    const candidate = medium === "s"
      ? resolve("/dev/shm", requestedPath.replace(/^\/+/, ""))
      : resolve(this.cwd, requestedPath)
    let canonical: string
    try {
      canonical = await realpath(candidate)
    } catch {
      return sequence
    }
    if (!this.roots.some((root) => within(canonical, root))) return sequence

    try {
      const info = await stat(canonical)
      if (!info.isFile() || info.size < 0 || info.size > this.maxExternalBytes) return sequence
      const bytes = await readFile(canonical)
      const normalized = directSequences(control, bytes)
      if (medium === "t" || medium === "s") await rm(canonical, { force: true })
      return normalized
    } catch {
      return sequence
    }
  }

  async push(chunk: Uint8Array): Promise<Uint8Array> {
    let data = this.pending.byteLength > 0 ? concat([this.pending, chunk]) : Uint8Array.from(chunk)
    this.pending = new Uint8Array(0)
    const output: Uint8Array[] = []
    let cursor = 0

    while (cursor < data.byteLength) {
      const start = find(data, START, cursor)
      if (start === -1) {
        const remainder = data.subarray(cursor)
        const retained = startPrefixLength(remainder)
        const visibleLength = remainder.byteLength - retained
        if (visibleLength > 0) output.push(remainder.subarray(0, visibleLength))
        if (retained > 0) this.pending = Uint8Array.from(remainder.subarray(visibleLength))
        break
      }
      if (start > cursor) output.push(data.subarray(cursor, start))
      const end = find(data, END, start + START.byteLength)
      if (end === -1) {
        this.pending = Uint8Array.from(data.subarray(start))
        if (this.pending.byteLength > this.maxExternalBytes * 2) {
          output.push(this.pending)
          this.pending = new Uint8Array(0)
        }
        break
      }
      const sequenceEnd = end + END.byteLength
      output.push(await this.normalizeSequence(data.subarray(start, sequenceEnd)))
      cursor = sequenceEnd
    }

    return concat(output)
  }

  flush(): Uint8Array {
    const pending = this.pending
    this.pending = new Uint8Array(0)
    return pending
  }
}
