import { init, Terminal, FitAddon } from "/vendor/ghostty-web.js"
await init("/vendor/ghostty-vt.wasm")

const status = document.querySelector("#status")
const host = document.querySelector("#terminal")
const menu = document.querySelector("#menu")
let terminal
let writing = false
const socket = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`)
const send = (value) => { if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(value)) }
const encoder = new TextEncoder()
const toBase64 = (text) => {
  let binary = ""
  for (const byte of encoder.encode(text)) binary += String.fromCharCode(byte)
  return btoa(binary)
}

socket.addEventListener("open", () => { status.textContent = "recording" })
socket.addEventListener("close", () => { if (!status.textContent.startsWith("saved")) status.textContent = "disconnected" })
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data)
  if (message.type === "init") {
    terminal = new Terminal({ cols: message.cols, rows: message.rows, fontSize: 16, cursorBlink: true, theme: { background: "#1a1b26", foreground: "#c0caf5" } })
    const fit = new FitAddon()
    terminal.loadAddon(fit)
    terminal.open(host)
    // Keys, IME, and paste arrive through onData. Replies the browser terminal generates
    // while writing are dropped; the server's libghostty-vt reply path answers queries.
    terminal.onData((data) => { if (!writing) send({ type: "input", data: toBase64(data) }) })
    terminal.onResize(({ cols, rows }) => send({ type: "resize", cols, rows }))
    // ghostty-web cancels plain insertText (character viewer, dictation) without forwarding it.
    host.addEventListener("beforeinput", (event) => {
      if (event.isComposing || !event.data) return
      if (event.inputType === "insertText" || event.inputType === "insertReplacementText") send({ type: "input", data: toBase64(event.data) })
    }, true)
    fit.observeResize()
    requestAnimationFrame(() => fit.fit())
  } else if (message.type === "output") {
    const binary = atob(message.data)
    const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0))
    writing = true
    try { terminal?.write(bytes) } finally { writing = false }
  } else if (message.type === "saved") {
    status.textContent = `saved ${message.output}`
  }
})

host.addEventListener("contextmenu", (event) => {
  event.preventDefault()
  menu.style.display = "block"
  menu.style.left = `${Math.min(event.clientX, innerWidth - 210)}px`
  menu.style.top = `${Math.min(event.clientY, innerHeight - 190)}px`
})
document.addEventListener("pointerdown", (event) => { if (!menu.contains(event.target)) menu.style.display = "none" })
menu.addEventListener("click", (event) => {
  const action = event.target.dataset.control
  if (action) socket.send(JSON.stringify({ type: "control", action }))
  menu.style.display = "none"
})
document.querySelector("#stop").addEventListener("click", async () => {
  status.textContent = "saving…"
  await fetch("/stop", { method: "POST" })
})
