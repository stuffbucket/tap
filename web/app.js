import { init, Terminal } from "/vendor/ghostty-web.js"
import { createKittyWasmFactory } from "/kitty-wasm.js"

await init("/vendor/ghostty-vt.wasm")
const project = await fetch("/project.json").then((response) => {
  if (!response.ok) throw new Error(`project load failed: ${response.status}`)
  return response.json()
})
const capabilities = await fetch("/capabilities").then((response) => response.json())
const kittyFactory = capabilities.kittyWasm ? await createKittyWasmFactory() : null

const stage = document.querySelector("#stage")
const wrap = document.querySelector("#stage-wrap")
const playButton = document.querySelector("#play")
const seek = document.querySelector("#seek")
const time = document.querySelector("#time")
const fpsLabel = document.querySelector("#fps")
const speed = document.querySelector("#speed")
const rendererButton = document.querySelector("#renderer")
const nativeFrame = document.querySelector("#native-frame")
const inputButton = document.querySelector("#show-input")
const keystrokes = document.querySelector("#keystrokes")
const menu = document.querySelector("#menu")
const toast = document.querySelector("#toast")
stage.style.width = `${project.canvas.width}px`
stage.style.height = `${project.canvas.height}px`
stage.style.background = project.canvas.background

const paneStates = project.panes.map((pane) => {
  const element = document.createElement("article")
  element.className = "pane"
  element.dataset.pane = pane.id
  const host = document.createElement("div")
  host.className = "pane-host"
  element.append(host)
  stage.append(element)
  const terminal = new Terminal({
    cols: pane.cols,
    rows: pane.rows,
    fontSize: pane.fontSize,
    fontFamily: project.presentation?.fontFamily,
    cursorBlink: project.presentation?.cursorBlink ?? false,
    theme: project.presentation?.theme ?? { background: "#1a1b26", foreground: "#c0caf5" },
  })
  terminal.open(host)
  const kitty = kittyFactory ? kittyFactory(pane.cols, pane.rows, host) : null
  return { pane, terminal, kitty, element, index: 0, visible: true, layout: pane.layout }
})

let currentUs = 0
let playing = false
let anchorNow = 0
let anchorUs = 0
let selected = paneStates[0]
let showInputs = false
let inputTimer = 0
let nativePixels = Boolean(capabilities.nativeFrames)
let frameRequest = false
let pendingFrameUs = null
let frameObjectUrl = null
rendererButton.disabled = !capabilities.nativeFrames
rendererButton.textContent = nativePixels ? "Native pixels" : "WASM cells"
nativeFrame.style.display = nativePixels ? "block" : "none"
const metrics = {
  startedAt: performance.now(),
  animationTicks: 0,
  nativeFrames: 0,
  animationFps: 0,
  nativeFps: 0,
}
window.__tapMetrics = metrics
setInterval(() => {
  const elapsed = Math.max(0.001, (performance.now() - metrics.startedAt) / 1000)
  metrics.animationFps = metrics.animationTicks / elapsed
  metrics.nativeFps = metrics.nativeFrames / elapsed
  fpsLabel.textContent = `${metrics.animationFps.toFixed(0)} wasm · ${metrics.nativeFps.toFixed(0)} native`
}, 250)

function decode(event) {
  const data = event.type === "output" && event.playbackData !== undefined ? event.playbackData : event.data
  const encoding = event.type === "output" && event.playbackData !== undefined ? event.playbackEncoding : event.encoding
  if (encoding !== "base64") return data
  const binary = atob(data)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index)
  return bytes
}

function displayInput(event) {
  if (!showInputs) return
  clearTimeout(inputTimer)
  if (event.redacted) {
    keystrokes.textContent = "[redacted input]"
  } else {
    const value = decode(event)
    const bytes = typeof value === "string" ? new TextEncoder().encode(value) : value
    const labels = []
    let printable = ""
    const flush = () => {
      if (printable) labels.push(printable)
      printable = ""
    }
    for (const byte of bytes) {
      if (byte === 13 || byte === 10) { flush(); labels.push("Enter") }
      else if (byte === 9) { flush(); labels.push("Tab") }
      else if (byte === 27) { flush(); labels.push("Esc") }
      else if (byte === 127 || byte === 8) { flush(); labels.push("Backspace") }
      else if (byte > 0 && byte < 27) { flush(); labels.push(`Ctrl+${String.fromCharCode(byte + 64)}`) }
      else if (byte >= 32) printable += String.fromCharCode(byte)
    }
    flush()
    keystrokes.textContent = labels.join(" · ") || "[input]"
  }
  keystrokes.style.opacity = "1"
  inputTimer = setTimeout(() => (keystrokes.style.opacity = "0"), 850)
}

function place(state) {
  const { x, y, width, height } = state.layout
  Object.assign(state.element.style, {
    left: `${x * 100}%`,
    top: `${y * 100}%`,
    width: `${width * 100}%`,
    height: `${height * 100}%`,
    display: state.visible ? "block" : "none",
  })
}

function apply(state, event) {
  switch (event.type) {
    case "output": {
      const bytes = decode(event)
      state.terminal.write(bytes)
      state.kitty?.write(bytes)
      break
    }
    case "input":
      displayInput(event)
      break
    case "resize":
      state.terminal.resize(event.cols, event.rows)
      state.kitty?.resize(event.cols, event.rows)
      break
    case "scroll":
      state.terminal.scrollLines?.(event.delta)
      break
    case "layout":
      state.layout = event.layout
      place(state)
      break
    case "visibility":
      state.visible = event.visible
      place(state)
      break
  }
}

function reset() {
  for (const state of paneStates) {
    state.terminal.reset()
    state.terminal.resize(state.pane.cols, state.pane.rows)
    state.kitty?.reset()
    state.index = 0
    state.visible = true
    state.layout = state.pane.layout
    place(state)
  }
}

async function updateNativeFrame(atUs) {
  if (!nativePixels) return
  if (frameRequest) {
    pendingFrameUs = atUs
    return
  }
  frameRequest = true
  try {
    const response = await fetch(`/frame.png?atUs=${Math.round(atUs)}`)
    if (!response.ok) throw new Error(await response.text())
    const nextUrl = URL.createObjectURL(await response.blob())
    const previous = frameObjectUrl
    frameObjectUrl = nextUrl
    nativeFrame.src = nextUrl
    metrics.nativeFrames++
    if (previous) URL.revokeObjectURL(previous)
  } catch (error) {
    console.error(error)
  } finally {
    frameRequest = false
    if (pendingFrameUs !== null) {
      const next = pendingFrameUs
      pendingFrameUs = null
      void updateNativeFrame(next)
    }
  }
}

function seekTo(targetUs) {
  targetUs = Math.max(0, Math.min(project.durationUs, Math.round(targetUs)))
  if (targetUs < currentUs) reset()
  currentUs = targetUs
  for (const state of paneStates) {
    while (state.index < state.pane.events.length && state.pane.events[state.index].atUs <= currentUs) {
      apply(state, state.pane.events[state.index++])
    }
    state.kitty?.render(currentUs / 1000)
  }
  seek.value = String(project.durationUs ? Math.round((currentUs / project.durationUs) * 1000) : 0)
  time.textContent = `${(currentUs / 1e6).toFixed(3)} / ${(project.durationUs / 1e6).toFixed(3)}`
  void updateNativeFrame(currentUs)
}

function playbackSpeed() {
  return Number.parseFloat(speed.value)
}

function setPlaying(next) {
  const wasPlaying = playing
  playing = next
  if (playing && !wasPlaying) {
    metrics.startedAt = performance.now()
    metrics.animationTicks = 0
    metrics.nativeFrames = 0
  }
  playButton.textContent = playing ? "Pause" : "Play"
  anchorNow = performance.now()
  anchorUs = currentUs
  if (playing) requestAnimationFrame(tick)
}

function tick(now) {
  if (!playing) return
  metrics.animationTicks++
  const target = anchorUs + (now - anchorNow) * 1000 * playbackSpeed()
  seekTo(target)
  if (currentUs >= project.durationUs) setPlaying(false)
  else requestAnimationFrame(tick)
}

function fit() {
  const scale = Math.min((wrap.clientWidth - 32) / project.canvas.width, (wrap.clientHeight - 32) / project.canvas.height)
  stage.style.transform = `scale(${Math.max(0.05, scale)})`
}

function message(text) {
  toast.textContent = text
  toast.style.opacity = "1"
  clearTimeout(message.timer)
  message.timer = setTimeout(() => (toast.style.opacity = "0"), 1200)
}

function download() {
  const blob = new Blob([`${JSON.stringify(project, null, 2)}\n`], { type: "application/json" })
  const link = document.createElement("a")
  link.href = URL.createObjectURL(blob)
  link.download = `${project.title || "session"}.tap`
  link.click()
  setTimeout(() => URL.revokeObjectURL(link.href), 0)
}

playButton.addEventListener("click", () => setPlaying(!playing))
seek.addEventListener("input", () => {
  setPlaying(false)
  seekTo((Number(seek.value) / 1000) * project.durationUs)
})
speed.addEventListener("change", () => {
  if (playing) setPlaying(true)
})
rendererButton.addEventListener("click", () => {
  if (!capabilities.nativeFrames) return
  nativePixels = !nativePixels
  rendererButton.textContent = nativePixels ? "Native pixels" : "WASM cells"
  nativeFrame.style.display = nativePixels ? "block" : "none"
  if (nativePixels) void updateNativeFrame(currentUs)
})
inputButton.addEventListener("click", () => {
  showInputs = !showInputs
  inputButton.textContent = showInputs ? "Keys on" : "Keys off"
  if (!showInputs) keystrokes.style.opacity = "0"
})
window.addEventListener("resize", fit)

function switchToEditableCells() {
  nativePixels = false
  rendererButton.textContent = "WASM cells"
  nativeFrame.style.display = "none"
}

stage.addEventListener("contextmenu", (event) => {
  event.preventDefault()
  const target = event.target.closest(".pane")
  selected = paneStates.find((state) => state.element === target) ?? paneStates[0]
  menu.style.display = "block"
  menu.style.left = `${Math.min(event.clientX, innerWidth - 210)}px`
  menu.style.top = `${Math.min(event.clientY, innerHeight - 340)}px`
})
document.addEventListener("pointerdown", (event) => {
  if (!menu.contains(event.target)) menu.style.display = "none"
})

menu.addEventListener("click", async (event) => {
  const action = event.target.dataset.action
  menu.style.display = "none"
  if (action === "play") setPlaying(!playing)
  if (action === "copy") {
    const text = selected.terminal.getSelection?.() || selected.pane.title || selected.pane.id
    await navigator.clipboard.writeText(text)
    message("Copied")
  }
  if (action === "marker") {
    selected.pane.events.push({ atUs: currentUs, type: "marker", label: `Marker at ${(currentUs / 1e6).toFixed(3)}s` })
    selected.pane.events.sort((a, b) => a.atUs - b.atUs)
    message("Marker added")
  }
  if (action === "toggle") {
    selected.visible = !selected.visible
    selected.pane.events.push({ atUs: currentUs, type: "visibility", visible: selected.visible })
    selected.pane.events.sort((a, b) => a.atUs - b.atUs)
    place(selected)
    switchToEditableCells()
  }
  if (action === "tile-horizontal" || action === "tile-vertical") {
    const horizontal = action === "tile-horizontal"
    paneStates.forEach((state, index) => {
      state.layout = horizontal
        ? { x: index / paneStates.length, y: 0, width: 1 / paneStates.length, height: 1 }
        : { x: 0, y: index / paneStates.length, width: 1, height: 1 / paneStates.length }
      state.pane.events.push({ atUs: currentUs, type: "layout", layout: state.layout })
      state.pane.events.sort((a, b) => a.atUs - b.atUs)
      place(state)
    })
    switchToEditableCells()
    message(horizontal ? "Horizontal layout added" : "Vertical layout added")
  }
  if (action === "resize") {
    const value = prompt("Terminal size (columns x rows)", `${selected.terminal.cols}x${selected.terminal.rows}`)
    const match = /^(\d+)\s*x\s*(\d+)$/i.exec(value || "")
    if (match) {
      const cols = Number(match[1])
      const rows = Number(match[2])
      if (cols > 0 && rows > 0 && cols <= 1000 && rows <= 1000) {
        selected.terminal.resize(cols, rows)
        selected.pane.events.push({ atUs: currentUs, type: "resize", cols, rows })
        selected.pane.events.sort((a, b) => a.atUs - b.atUs)
        switchToEditableCells()
        message(`Resized to ${cols}×${rows}`)
      }
    }
  }
  if (action === "download") download()
})

document.querySelector("#download").addEventListener("click", download)
document.querySelector("#save").addEventListener("click", async () => {
  const response = await fetch("/project.json", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(project),
  })
  if (!response.ok) throw new Error(await response.text())
  message("Saved")
})

reset()
seekTo(0)
fit()
