import { afterAll, describe, expect, test } from "bun:test"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { captureProject } from "../src/capture.ts"
import { renderScreenshot } from "../src/render.ts"

const temporary: string[] = []
afterAll(async () => {
  await Promise.all(temporary.map((path) => rm(path, { recursive: true, force: true })))
})

describe("capture and render", () => {
  test("captures two PTYs and renders their composed Ghostty frame", async () => {
    const directory = await mkdtemp(join(tmpdir(), "tap-test-"))
    temporary.push(directory)
    const project = await captureProject({
      title: "integration",
      canvas: { width: 640, height: 360, fps: 30, background: "#101014" },
      panes: [
        { id: "left", command: ["/bin/sh", "-lc", "printf '\\033[31mleft\\033[0m\\n'"] },
        { id: "right", command: ["/bin/sh", "-lc", "printf '\\033[36mright\\033[0m\\n'"] },
      ],
    })
    expect(project.panes).toHaveLength(2)
    expect(project.panes.every((pane) => pane.events.some((event) => event.type === "output"))).toBe(true)

    const output = join(directory, "frame.png")
    await renderScreenshot(project, output)
    const png = await readFile(output)
    expect(Array.from(png.subarray(0, 8))).toEqual([137, 80, 78, 71, 13, 10, 26, 10])
  }, 20_000)

  test("normalizes Kitty file transport before the source asset disappears", async () => {
    const directory = await mkdtemp(join(tmpdir(), "tap-kitty-capture-"))
    temporary.push(directory)
    const imagePath = join(directory, "source.png")
    await writeFile(
      imagePath,
      Buffer.from(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
        "base64",
      ),
    )
    const sequence = `\x1b_Ga=T,f=100,t=f,i=9,c=4,r=3,q=2;${Buffer.from(imagePath).toString("base64")}\x1b\\`
    const project = await captureProject({
      title: "external Kitty media",
      canvas: { width: 320, height: 180, fps: 60, background: "#000000" },
      panes: [{
        id: "graphics",
        cwd: directory,
        cols: 20,
        rows: 10,
        command: ["/usr/bin/printf", "%s", sequence],
      }],
    })
    const outputEvent = project.panes[0]!.events.find((event) => event.type === "output")
    expect(outputEvent?.type === "output" && outputEvent.playbackData).toBeTruthy()
    await rm(imagePath, { force: true })
    const output = join(directory, "portable.png")
    await renderScreenshot(project, output)
    const png = await readFile(output)
    expect(png.byteLength).toBeGreaterThan(100)
  }, 20_000)
})
