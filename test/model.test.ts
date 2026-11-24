import { describe, expect, test } from "bun:test"
import { defaultLayouts, validateProject, type TapProject } from "../src/model.ts"

describe("project model", () => {
  test("creates deterministic split layouts", () => {
    expect(defaultLayouts(2)).toEqual([
      { x: 0, y: 0, width: 0.5, height: 1 },
      { x: 0.5, y: 0, width: 0.5, height: 1 },
    ])
  })

  test("accepts an editable minimal project", () => {
    const project: TapProject = {
      version: 1,
      title: "demo",
      createdAt: new Date(0).toISOString(),
      durationUs: 1,
      canvas: { width: 1280, height: 720, background: "#101014", fps: 60 },
      panes: [
        {
          id: "main",
          cols: 80,
          rows: 24,
          fontSize: 16,
          layout: { x: 0, y: 0, width: 1, height: 1 },
          events: [{ atUs: 0, type: "output", data: "hello" }],
        },
      ],
      overlays: [],
      metadata: {
        terminal: "ghostty",
        terminalEngine: "libghostty-vt/ghostty-web",
        timebase: "microseconds",
      },
    }
    expect(() => validateProject(project)).not.toThrow()
  })

  test("rejects native geometry overflow and invalid scroll deltas", () => {
    const project: TapProject = {
      version: 1,
      title: "invalid",
      createdAt: new Date(0).toISOString(),
      durationUs: 1,
      canvas: { width: 1280, height: 720, background: "#101014", fps: 60 },
      panes: [{
        id: "main",
        cols: 65_536,
        rows: 24,
        fontSize: 16,
        layout: { x: 0, y: 0, width: 1, height: 1 },
        events: [{ atUs: 0, type: "scroll", delta: 0.5 }],
      }],
      overlays: [],
      metadata: { terminal: "ghostty", terminalEngine: "libghostty-vt 1.0.1", timebase: "microseconds" },
    }
    expect(() => validateProject(project)).toThrow()
  })
})
