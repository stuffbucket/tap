import { expect, test } from "bun:test"
import { CaptureSession } from "../src/capture-session.ts"

test("creates a Tap-native pane during an active capture session", async () => {
  const session = await CaptureSession.start({ id: "one", command: ["/bin/sh", "-lc", "printf one; sleep 1"] })
  await Bun.sleep(20)
  await session.splitPane("one", "vertical", { id: "two", command: ["/bin/sh", "-lc", "printf two; sleep 1"] })
  session.focusPane("two")
  session.input(new TextEncoder().encode("echo focused\r"))
  await Bun.sleep(50)
  const project = await session.stop()
  expect(project.panes).toHaveLength(2)
  expect(project.panes[0]!.events.some((event) => event.type === "layout")).toBe(true)
  expect(project.panes[1]!.events.some((event) => event.type === "visibility" && !event.visible)).toBe(true)
  expect(project.panes[1]!.events.some((event) => event.type === "input" && event.origin === "keyboard")).toBe(true)
})

test("splits from live geometry, records resizes, and reflows closed panes", async () => {
  const shell = ["/bin/sh", "-c", "sleep 5"]
  const layouts: Record<string, unknown> = {}
  const closed: string[] = []
  const session = await CaptureSession.start({ id: "a", command: shell }, {
    onLayout: (id, layout) => { layouts[id] = layout },
    onPaneClosed: (id) => closed.push(id),
  })
  await session.splitPane("a", "vertical", { id: "b", command: shell })
  // Splitting "a" again must divide its current half, not its original full width.
  await session.splitPane("a", "horizontal", { id: "c", command: shell })
  expect(layouts.a).toEqual({ x: 0, y: 0, width: 0.5, height: 0.5 })

  session.resizePane("b", 100, 30)
  session.resizePane("b", 100, 30) // unchanged size is not re-recorded

  await session.closePane("c")
  expect(closed).toEqual(["c"])
  expect(layouts.a).toEqual({ x: 0, y: 0, width: 0.5, height: 1 })
  expect(() => session.focusPane("c")).toThrow("closed")

  await session.closePane("b")
  expect(layouts.a).toEqual({ x: 0, y: 0, width: 1, height: 1 })
  await expect(session.closePane("a")).rejects.toThrow("last pane")
  expect(session.openPaneCount()).toBe(1)

  const project = await session.stop()
  const b = project.panes.find((pane) => pane.id === "b")!
  expect(b.events.filter((event) => event.type === "resize")).toEqual([{ atUs: expect.any(Number), type: "resize", cols: 100, rows: 30 }])
})

test("a shell that exits on its own closes its pane", async () => {
  const closed: string[] = []
  const session = await CaptureSession.start({ id: "a", command: ["/bin/sh", "-c", "sleep 5"] }, { onPaneClosed: (id) => closed.push(id) })
  await session.splitPane("a", "vertical", { id: "b", command: ["/bin/sh", "-c", "exit 0"] })
  for (let i = 0; i < 50 && !closed.length; i++) await Bun.sleep(20)
  expect(closed).toEqual(["b"])
  expect(session.openPaneCount()).toBe(1)
  await session.stop()
})
