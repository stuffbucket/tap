import { expect, test } from "bun:test"
import { gridAt } from "../src/ghostty-full.ts"
import { PINNED_GHOSTTY_COMMIT, matchesPinnedCommit } from "../src/ghostty-app.ts"
import type { TapPane } from "../src/model.ts"

test("Ghostty replay uses the pane's grid at the requested time", () => {
  const pane: TapPane = {
    id: "p", cols: 80, rows: 24, fontSize: 16, layout: { x: 0, y: 0, width: 1, height: 1 },
    events: [
      { atUs: 1_000, type: "resize", cols: 100, rows: 30 },
      { atUs: 5_000, type: "resize", cols: 60, rows: 20 },
    ],
  }
  expect(gridAt(pane, 0)).toEqual({ cols: 80, rows: 24 })
  expect(gridAt(pane, 1_000)).toEqual({ cols: 100, rows: 30 })
  expect(gridAt(pane, 9_000)).toEqual({ cols: 60, rows: 20 })
})

test("only Ghostty builds of the pinned libghostty-vt commit match", () => {
  expect(matchesPinnedCommit(PINNED_GHOSTTY_COMMIT.slice(0, 9))).toBe(true)
  expect(matchesPinnedCommit("83fb95624")).toBe(false) // v1.4.0
  expect(matchesPinnedCommit(null)).toBe(false)
})
