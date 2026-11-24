import { init, Terminal, FitAddon } from "/vendor/ghostty-web.js"

await init("/vendor/ghostty-vt.wasm")

const stage = document.querySelector("#stage")
const menu = document.querySelector("#menu")
const status = document.querySelector("#status")
const panes = new Map()
let selected = null

const ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`)
const send = (value) => { if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(value)) }
const encoder = new TextEncoder()
const toBase64 = (text) => {
  let binary = ""
  for (const byte of encoder.encode(text)) binary += String.fromCharCode(byte)
  return btoa(binary)
}
const fromBase64 = (data) => Uint8Array.from(atob(data), (char) => char.charCodeAt(0))

function place(state, layout) {
  Object.assign(state.element.style, {
    left: `${layout.x * 100}%`, top: `${layout.y * 100}%`,
    width: `${layout.width * 100}%`, height: `${layout.height * 100}%`,
  })
  // The layout change resizes the container; the observer refits on the next frame.
}

function select(id) {
  selected = id
  for (const [paneId, state] of panes) state.element.classList.toggle("focused", paneId === id)
}

function add(pane) {
  if (panes.has(pane.id)) return
  const element = document.createElement("section")
  element.className = "pane"
  element.dataset.id = pane.id
  const host = document.createElement("div")
  host.className = "host"
  element.append(host)
  stage.append(element)

  const terminal = new Terminal({ cols: pane.cols, rows: pane.rows, fontSize: 15, cursorBlink: true, theme: { background: "#1a1b26", foreground: "#c0caf5" } })
  const fit = new FitAddon()
  terminal.loadAddon(fit)
  terminal.open(host)
  const state = { element, terminal, fit, writing: false }
  panes.set(pane.id, state)
  place(state, pane.layout)

  // Keys, IME, and paste all arrive through onData. Query replies the browser terminal
  // generates while writing are dropped: the server's libghostty-vt already answered them.
  terminal.onData((data) => {
    if (state.writing) return
    send({ type: data.startsWith("\x1b[200~") ? "paste" : "input", paneId: pane.id, data: toBase64(data) })
  })
  terminal.onResize(({ cols, rows }) => send({ type: "resize", paneId: pane.id, cols, rows }))
  // ghostty-web cancels `beforeinput` without forwarding it, which drops text inserted by the
  // character viewer, dictation, and similar sources. Composition (IME/dead keys) and keydown
  // input are already handled by ghostty-web and never arrive here as plain insertText.
  element.addEventListener("beforeinput", (event) => {
    if (event.isComposing || !event.data) return
    if (event.inputType !== "insertText" && event.inputType !== "insertReplacementText") return
    send({ type: "input", paneId: pane.id, data: toBase64(event.data) })
  }, true)
  fit.observeResize()
  requestAnimationFrame(() => fit.fit())

  element.addEventListener("pointerdown", () => { select(pane.id); send({ type: "focus", paneId: pane.id }) })
  element.addEventListener("contextmenu", (event) => {
    event.preventDefault()
    select(pane.id)
    menu.style.display = "block"
    menu.style.left = `${event.clientX}px`
    menu.style.top = `${event.clientY}px`
  })
  select(pane.id)
  terminal.focus?.()
}

function remove(id) {
  const state = panes.get(id)
  if (!state) return
  state.fit.dispose()
  state.terminal.dispose()
  state.element.remove()
  panes.delete(id)
  if (selected === id) select(panes.keys().next().value ?? null)
}

ws.addEventListener("message", (event) => {
  const message = JSON.parse(event.data)
  switch (message.type) {
    case "init": message.panes.forEach(add); break
    case "pane-open": add(message.pane); break
    case "pane-close": remove(message.paneId); break
    case "layout": { const state = panes.get(message.paneId); if (state) place(state, message.layout); break }
    case "output": {
      const state = panes.get(message.paneId)
      if (!state) break
      state.writing = true
      try { state.terminal.write(fromBase64(message.data)) } finally { state.writing = false }
      break
    }
    case "error": status.textContent = message.message; break
    case "saved": status.textContent = `saved ${message.output}`; break
  }
})
ws.addEventListener("close", () => { if (!status.textContent.startsWith("saved")) status.textContent = "disconnected" })

menu.addEventListener("click", (event) => {
  const action = event.target.dataset.action
  menu.style.display = "none"
  if (!selected || !action) return
  if (action === "vertical" || action === "horizontal") send({ type: "split", paneId: selected, direction: action })
  else send({ type: action, paneId: selected })
})
document.addEventListener("pointerdown", (event) => { if (!menu.contains(event.target)) menu.style.display = "none" })
document.addEventListener("keydown", (event) => { if (event.key === "Escape") menu.style.display = "none" })
document.querySelector("#stop").addEventListener("click", async () => {
  status.textContent = "saving…"
  await fetch("/stop", { method: "POST" })
})
