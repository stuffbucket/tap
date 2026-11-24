import { expect, test } from "bun:test"
import { createHash } from "node:crypto"
import { createCanvas, loadImage } from "@napi-rs/canvas"
import { NativePaneRenderer, nativeRendererAvailable } from "../src/native-renderer.ts"

const encoder = new TextEncoder()

function digest(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex")
}

test("libghostty-vt renders and advances Kitty graphics animations", async () => {
  if (!nativeRendererAvailable()) return
  const renderer = new NativePaneRenderer(20, 10, 16)
  try {
    const reply = await renderer.write(encoder.encode("\x1b[6n"))
    expect(new TextDecoder().decode(reply)).toBe("\x1b[1;1R")
    await renderer.write(
      encoder.encode(
        "\x1b_Ga=T,f=100,i=1,c=2,r=2,q=2;" +
          "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==" +
          "\x1b\\",
      ),
    )
    const pngFrame = await renderer.frame(0)
    const image = await loadImage(Buffer.from(pngFrame))
    const canvas = createCanvas(image.width, image.height)
    const context = canvas.getContext("2d")
    context.drawImage(image, 0, 0)
    const redPixel = context.getImageData(5, 5, 1, 1).data
    expect(redPixel[0]).toBeGreaterThan(240)
    expect(redPixel[1]).toBeLessThan(16)
    await renderer.write(
      encoder.encode(
        "\x1b_Ga=T,f=32,i=2,s=1,v=1,c=5,r=3,q=2;/wAA/w==\x1b\\" +
          "\x1b_Ga=f,f=32,i=2,s=1,v=1,z=40,q=2;AAD//w==\x1b\\" +
          "\x1b_Ga=a,i=2,r=1,z=30,q=2\x1b\\" +
          "\x1b_Ga=a,i=2,s=3,q=2\x1b\\",
      ),
    )
    await renderer.advance(0)
    const first = await renderer.frame(0)
    const second = await renderer.frame(40)
    const sequentialAt100 = await renderer.frame(100)
    expect(Array.from(first.subarray(0, 8))).toEqual([137, 80, 78, 71, 13, 10, 26, 10])
    expect(digest(first)).not.toBe(digest(second))

    const seekRenderer = new NativePaneRenderer(20, 10, 16)
    try {
      await seekRenderer.write(
        encoder.encode(
          "\x1b_Ga=T,f=100,i=1,c=2,r=2,q=2;" +
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==" +
            "\x1b\\",
        ),
      )
      await seekRenderer.write(
        encoder.encode(
          "\x1b_Ga=T,f=32,i=2,s=1,v=1,c=5,r=3,q=2;/wAA/w==\x1b\\" +
            "\x1b_Ga=f,f=32,i=2,s=1,v=1,z=40,q=2;AAD//w==\x1b\\" +
            "\x1b_Ga=a,i=2,r=1,z=30,q=2\x1b\\" +
            "\x1b_Ga=a,i=2,s=3,q=2\x1b\\",
        ),
      )
      await seekRenderer.advance(0)
      const directAt100 = await seekRenderer.frame(100)
      expect(digest(directAt100)).toBe(digest(sequentialAt100))
    } finally {
      await seekRenderer.close()
    }
  } finally {
    await renderer.close()
  }
})
