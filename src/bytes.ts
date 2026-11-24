import type { TapEvent } from "./model.ts"

export function encodeBytes(data: Uint8Array): { data: string; encoding: "base64" } {
  return { data: Buffer.from(data).toString("base64"), encoding: "base64" }
}

export function decodeEventBytes(event: Extract<TapEvent, { type: "output" | "input" }>): Uint8Array {
  if (event.encoding === "base64") return Uint8Array.from(Buffer.from(event.data, "base64"))
  return new TextEncoder().encode(event.data)
}

export function decodePlaybackBytes(event: Extract<TapEvent, { type: "output" }>): Uint8Array {
  if (event.playbackData === undefined) return decodeEventBytes(event)
  if (event.playbackEncoding === "base64") return Uint8Array.from(Buffer.from(event.playbackData, "base64"))
  return new TextEncoder().encode(event.playbackData)
}

export function decodeEventText(event: Extract<TapEvent, { type: "output" | "input" }>): string {
  return new TextDecoder().decode(decodeEventBytes(event))
}
