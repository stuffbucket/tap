#!/usr/bin/env bun

import { chromium } from "playwright"
import { mkdtemp, rm } from "node:fs/promises"
import { existsSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"
import { serveProject } from "../src/server.ts"
import { startLiveCapture } from "../src/live.ts"
import { startNativeLiveCapture } from "../src/native-live.ts"
import { renderScreenshot } from "../src/render.ts"
import { readProject, writeProject } from "../src/project.ts"
import { decodePlaybackBytes, encodeBytes } from "../src/bytes.ts"
import type { TapPane, TapProject } from "../src/model.ts"

const directory = await mkdtemp(join(tmpdir(), "tap-e2e-"))
const animation =
  "\x1b_Ga=T,f=32,i=2,s=1,v=1,c=8,r=4,q=2;/wAA/w==\x1b\\" +
  "\x1b_Ga=f,f=32,i=2,s=1,v=1,z=40,q=2;AAD//w==\x1b\\" +
  "\x1b_Ga=a,i=2,r=1,z=30,q=2\x1b\\" +
  "\x1b_Ga=a,i=2,s=3,q=2\x1b\\"
const project: TapProject = {
  version: 1,
  title: "E2E Kitty",
  createdAt: new Date().toISOString(),
  durationUs: 1_000_000,
  canvas: { width: 640, height: 360, background: "#000000", fps: 120 },
  panes: [{
    id: "main", cols: 40, rows: 18, fontSize: 16,
    layout: { x: 0, y: 0, width: 1, height: 1 },
    events: [{ atUs: 0, type: "output", ...encodeBytes(new TextEncoder().encode(animation)) }],
  }],
  overlays: [],
  metadata: { terminal: "ghostty", terminalEngine: "libghostty-vt 1.0.1", timebase: "microseconds" },
}
const projectPath = join(directory, "kitty.tap")
await writeProject(projectPath, project)

const executablePath = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
const browser = await chromium.launch({
  headless: true,
  ...(existsSync(executablePath) ? { executablePath } : {}),
})
try {
  const player = await serveProject(projectPath, { port: 0 })
  try {
    const page = await browser.newPage({ viewport: { width: 900, height: 600 } })
    const errors: string[] = []
    page.on("pageerror", (error) => errors.push(error.message))
    await page.goto(`http://127.0.0.1:${player.port}`, { waitUntil: "networkidle" })
    await page.getByRole("button", { name: "Native pixels" }).click()
    await page.getByRole("button", { name: "Play" }).click()
    await page.waitForTimeout(150)
    const wasmKittyVisible = await page.locator("canvas.kitty-wasm-layer").evaluate((canvas: HTMLCanvasElement) => {
      const data = canvas.getContext("2d")!.getImageData(5, 5, 1, 1).data
      return data[3] > 0 && (data[0] > 0 || data[2] > 0)
    })
    if (!wasmKittyVisible) throw new Error("Kitty WASM canvas did not render image pixels")
    if (errors.length) throw new Error(errors.join("\n"))
    await page.close()
  } finally {
    await player.stop()
  }

  const liveOutput = join(directory, "live.tap")
  const sessionName = `tap-e2e-${process.pid}`
  const live = await startLiveCapture({
    output: liveOutput,
    port: 0,
    command: ["/usr/bin/env", "LANG=en_US.UTF-8", "tmux", "-u", "-f", "/dev/null", "new-session", "-s", sessionName, "/bin/sh"],
    cleanupCommand: ["tmux", "-f", "/dev/null", "kill-session", "-t", sessionName],
  })
  const page = await browser.newPage({ viewport: { width: 1000, height: 700 } })
  await page.goto(`http://127.0.0.1:${live.port}`, { waitUntil: "networkidle" })
  await page.locator("#terminal").click({ button: "right" })
  await page.getByRole("button", { name: "Split right" }).click()
  await page.waitForTimeout(250)
  await page.locator("#terminal").click()
  await page.keyboard.type("echo clean-e2e héllo ✓")
  await page.keyboard.press("Enter")
  await page.waitForTimeout(250)
  await page.getByRole("button", { name: "Stop & save" }).click()
  const captured = await live.done
  await page.close()
  const output = captured.panes[0]!.events
    .filter((event) => event.type === "output")
    .map((event) => event.type === "output" ? new TextDecoder().decode(decodePlaybackBytes(event)) : "")
    .join("")
  if (!output.includes("clean-e2e")) throw new Error("live split command output was not captured")
  if (/62;22c|ghostty 1\.3\.0|2026;2\$y/.test(output)) throw new Error("terminal reply bytes leaked into shell output")
  if (!captured.panes[0]!.events.some((event) => event.type === "input" && event.origin === "automation")) {
    throw new Error("live split control was not recorded")
  }
  const typed = (pane: TapPane) => pane.events
    .filter((event) => event.type === "input" && event.origin !== "terminal-reply")
    .map((event) => event.type === "input" ? Buffer.from(event.data, "base64").toString("utf8") : "")
    .join("")
  if (!typed(captured.panes[0]!).includes("echo clean-e2e héllo ✓\r")) throw new Error("live capture lost non-ASCII keyboard input")
  if (!captured.panes[0]!.events.some((event) => event.type === "resize")) throw new Error("live capture did not fit the terminal to the page")

  // Browser-driven Tap-native panes: independent PTYs created, typed into, and closed from the right-click menu.
  const nativeOutput = join(directory, "native-live.tap")
  const nativeLive = await startNativeLiveCapture({ output: nativeOutput, shell: ["/usr/bin/env", "LANG=en_US.UTF-8", "/bin/sh"], port: 0 })
  const nativePage = await browser.newPage({ viewport: { width: 1000, height: 700 } })
  await nativePage.goto(`http://127.0.0.1:${nativeLive.port}`, { waitUntil: "networkidle" })
  const paneMenu = async (id: string, action: string) => {
    await nativePage.locator(`.pane[data-id="${id}"]`).click({ button: "right" })
    await nativePage.getByRole("button", { name: action }).click()
  }
  await nativePage.waitForSelector('.pane[data-id="pane-1"]')
  await paneMenu("pane-1", "Split right")
  await nativePage.waitForSelector('.pane[data-id="pane-2"]')
  await nativePage.locator('.pane[data-id="pane-2"]').click()
  await nativePage.keyboard.type("echo native-browser ✓")
  await nativePage.keyboard.press("Enter")
  await nativePage.waitForTimeout(250)
  await paneMenu("pane-2", "Close pane")
  await nativePage.waitForFunction(() => document.querySelectorAll(".pane").length === 1)
  if (await nativePage.$eval('.pane[data-id="pane-1"]', (node) => (node as HTMLElement).style.width) !== "100%") {
    throw new Error("closing a native pane did not return its area to the neighbour")
  }
  await nativePage.getByRole("button", { name: "Stop & save" }).click()
  await nativeLive.done
  await nativePage.close()
  const nativeSplit = await readProject(nativeOutput)
  if (nativeSplit.panes.length !== 2) throw new Error("browser did not create an independent Tap-native pane")
  const [nativeOne, nativeTwo] = nativeSplit.panes as [TapPane, TapPane]
  if (!typed(nativeTwo).includes("echo native-browser ✓\r")) throw new Error("native pane lost typed input")
  if (!nativeTwo.events.some((event) => event.type === "visibility" && !event.visible)) throw new Error("native pane close was not recorded")
  if (!nativeOne.events.some((event) => event.type === "layout" && event.layout.width === 1)) throw new Error("native pane reflow was not recorded")
  if (!nativeOne.events.some((event) => event.type === "resize")) throw new Error("native pane did not fit to the page")
  const nativeSplitPng = join(directory, "native-split.png")
  await renderScreenshot(nativeSplit, nativeSplitPng)
  if (!(await Bun.file(nativeSplitPng).exists())) throw new Error("Tap-native split did not render")

  // `tap capture` inside a real terminal (tmux) that answers queries: a program that asks the
  // terminal what it is and reads the answer must get exactly one answer. The outer terminal's
  // duplicate used to arrive as typed input and swallow the next command.
  const queryTape = join(directory, "query.tap")
  const tmuxSocket = `tap-e2e-query-${process.pid}`
  const tmux = (...args: string[]) => Bun.spawnSync(["tmux", "-L", tmuxSocket, "-f", "/dev/null", ...args], { stdout: "pipe", stderr: "pipe" })
  try {
    tmux("new-session", "-d", "-s", "q", "-x", "100", "-y", "30", `${process.execPath} ${join(import.meta.dir, "../src/cli.ts")} capture -o ${queryTape} -- /bin/zsh -f`)
    await Bun.sleep(2500)
    tmux("send-keys", "-t", "q", "stty -echo; printf '\\033[c'; read -rs -k 1 a; while [ \"$a\" != c ]; do read -rs -k 1 a; done; stty echo; sleep 1; echo got-one-reply", "Enter")
    await Bun.sleep(3000)
    tmux("send-keys", "-t", "q", "exit", "Enter")
    for (let attempt = 0; attempt < 50 && !(await Bun.file(queryTape).exists()); attempt++) await Bun.sleep(100)
    const screen = new TextDecoder().decode(tmux("capture-pane", "-p", "-t", "q").stdout)
    if (screen.includes("command not found")) throw new Error(`the outer terminal's query reply leaked into the shell:\n${screen}`)
    if (!(await Bun.file(queryTape).exists())) throw new Error(`capture did not finish:\n${screen}`)
  } finally {
    tmux("kill-server")
  }

  console.log("E2E passed: Kitty WASM pixels, clean tmux split, UTF-8 input, browser-driven Tap-native split/close, terminal queries answered once, saved input/output timeline")
} finally {
  await browser.close()
  await rm(directory, { recursive: true, force: true })
}
