import {
  backend,
  createTerminal,
  keyToAnsi,
  parseKey,
  parseTape as parseTermlessTape,
} from "@termless/core"
import { TAP_VERSION } from "./version.ts"
import { readFile } from "node:fs/promises"
import { basename, dirname, resolve } from "node:path"
import { existsSync } from "node:fs"
import { encodeBytes } from "./bytes.ts"
import type { TapEvent, TapProject } from "./model.ts"

interface VhsCommand {
  type: string
  options: string
  args: string
  source: string
}

interface VhsParseResult {
  commands: VhsCommand[]
  errors: string[]
}

export interface TapeRunResult {
  project: TapProject
  requestedOutputs: string[]
  requestedScreenshots: Array<{ path: string; atUs: number }>
  finalText: string
  warnings: string[]
}

const KEY_NAMES: Record<string, string> = {
  Enter: "Enter",
  Backspace: "Backspace",
  Tab: "Tab",
  Space: "Space",
  Up: "ArrowUp",
  Down: "ArrowDown",
  Left: "ArrowLeft",
  Right: "ArrowRight",
  Escape: "Escape",
  Delete: "Delete",
  Insert: "Insert",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Home: "Home",
  End: "End",
}

function elapsedUs(start: bigint): number {
  return Number((process.hrtime.bigint() - start) / 1_000n)
}

function parserPath(): string {
  return resolve(import.meta.dir, "../native/bin/tap-vhs-parse")
}

async function parseWithUpstream(path: string): Promise<VhsParseResult> {
  const executable = parserPath()
  if (!existsSync(executable)) return parseWithTermless(path)
  const process = Bun.spawn([executable, resolve(path)], { stdin: "ignore", stdout: "pipe", stderr: "pipe" })
  const stdout = new Response(process.stdout).text()
  const stderr = new Response(process.stderr).text()
  const exitCode = await process.exited
  const text = await stdout
  let parsed: VhsParseResult
  try {
    parsed = JSON.parse(text) as VhsParseResult
  } catch {
    throw new Error(`VHS parser returned invalid JSON: ${(await stderr).trim()}`)
  }
  if (exitCode !== 0 || parsed.errors.length > 0) {
    throw new Error(`VHS tape has parse errors:\n${parsed.errors.join("\n") || (await stderr).trim()}`)
  }
  return parsed
}

async function parseWithTermless(path: string): Promise<VhsParseResult> {
  const tape = parseTermlessTape(await readFile(resolve(path), "utf8"))
  return {
    errors: [],
    commands: tape.commands.map((command) => {
      switch (command.type) {
        case "output": return { type: "Output", options: "", args: command.path, source: "" }
        case "set": return { type: "Set", options: command.key, args: command.value, source: "" }
        case "type": return { type: "Type", options: command.speed === undefined ? "" : `${command.speed}ms`, args: command.text, source: "" }
        case "key": return { type: command.key, options: "", args: String(command.count ?? 1), source: "" }
        case "ctrl": return { type: "Ctrl", options: "", args: command.key, source: "" }
        case "alt": return { type: "Alt", options: "", args: command.key, source: "" }
        case "sleep": return { type: "Sleep", options: "", args: `${command.ms}ms`, source: "" }
        case "screenshot": return { type: "Screenshot", options: "", args: command.path ?? "", source: "" }
        case "expect": return { type: "Wait", options: command.timeout === undefined ? "" : `${command.timeout}ms`, args: `Screen ${command.text}`, source: "" }
        case "hide": return { type: "Hide", options: "", args: "", source: "" }
        case "show": return { type: "Show", options: "", args: "", source: "" }
        case "source": return { type: "Source", options: "", args: command.path, source: "" }
        case "require": return { type: "Require", options: "", args: command.program, source: "" }
      }
    }),
  }
}

// VHS's shell table (vhs 0.12.1 shell.go): every named shell starts without user rc files or
// history and with the same purple "> " prompt, so recordings are reproducible and don't leak
// the recorder's personal prompt, paths, or history.
const VHS_SHELLS: Record<string, { command: string[]; env?: Record<string, string> }> = {
  bash: {
    command: ["bash", "--noprofile", "--norc", "--login", "+o", "history"],
    env: { PS1: "\\[\\e[38;2;90;86;224m\\]> \\[\\e[0m\\]", BASH_SILENCE_DEPRECATION_WARNING: "1" },
  },
  zsh: { command: ["zsh", "--histnostore", "--no-rcs"], env: { PROMPT: "%F{#5B56E0}> %F{reset_color}" } },
  fish: {
    command: ["fish", "--login", "--no-config", "--private", "-C", "function fish_greeting; end",
      "-C", `function fish_prompt; set_color 5B56E0; echo -n "> "; set_color normal; end`],
  },
  powershell: {
    command: ["powershell", "-NoLogo", "-NoExit", "-NoProfile", "-Command",
      "Set-PSReadLineOption -HistorySaveStyle SaveNothing; function prompt { Write-Host '>' -NoNewLine -ForegroundColor Blue; return ' ' }"],
  },
  pwsh: {
    command: ["pwsh", "-Login", "-NoLogo", "-NoExit", "-NoProfile", "-Command",
      "Set-PSReadLineOption -HistorySaveStyle SaveNothing; Function prompt { Write-Host -ForegroundColor Blue -NoNewLine '>'; return ' ' }"],
  },
  cmd: { command: ["cmd.exe", "/k", "prompt=^> "] },
  nu: { command: ["nu", "--execute", "$env.PROMPT_COMMAND = {'\x1b[;38;2;91;86;224m>\x1b[m '}; $env.PROMPT_COMMAND_RIGHT = {''}"] },
  osh: { command: ["osh", "--norc"], env: { PS1: "\\[\\e[38;2;90;86;224m\\]> \\[\\e[0m\\]" } },
  xonsh: { command: ["xonsh", "--no-rc", "-D", "PROMPT=\x1b[;38;2;91;86;224m>\x1b[m "] },
}

/**
 * Resolves `Set Shell`. VHS names (and the bash default) use VHS's clean configuration.
 * As a Tap extension, any other value is run as a command line, e.g. `Set Shell "/bin/sh"`.
 */
export function resolveShell(value: string | undefined): { command: string[]; env: Record<string, string> } {
  const unquoted = (value?.trim() || "bash").replace(/^(?:"([\s\S]*)"|'([\s\S]*)')$/, "$1$2")
  const known = VHS_SHELLS[unquoted]
  if (known) return { command: known.command, env: known.env ?? {} }
  return { command: unquoted.split(/\s+/).filter(Boolean), env: {} }
}

function inputBytes(terminal: ReturnType<typeof createTerminal>, key: string): Uint8Array {
  const descriptor = parseKey(key)
  const encoded = terminal.backend.encodeKey(descriptor)
  return encoded.length ? encoded : new TextEncoder().encode(keyToAnsi(descriptor))
}

/**
 * VHS drives xterm.js, which sends legacy bytes for character keys: Ctrl+letter is a C0 control
 * byte whatever its case (VHS's `Ctrl+C` holds Shift for the capital), and Alt prefixes ESC.
 * Returns null for named keys such as Left or Enter, which the Ghostty encoder already handles.
 */
export function vhsCharacterChord(modifiers: { ctrl?: boolean; alt?: boolean }, key: string): Uint8Array | null {
  if ([...key].length !== 1) return null
  let text = key
  if (modifiers.ctrl) {
    if (key === "?") text = "\x7f"
    else if (key === " ") text = "\x00"
    else {
      const code = key.toUpperCase().charCodeAt(0)
      if (code < 0x40 || code > 0x5f) return null
      text = String.fromCharCode(code & 0x1f)
    }
  }
  return new TextEncoder().encode(modifiers.alt ? `\x1b${text}` : text)
}

async function delay(ms: number): Promise<void> {
  if (ms > 0) await Bun.sleep(ms)
}

function duration(value: string | undefined, fallback: number): number {
  if (!value) return fallback
  const match = /^((?:\d+(?:\.\d*)?|\.\d+))(ms|s|m)?$/.exec(value.trim())
  if (!match) return fallback
  const amount = Number.parseFloat(match[1]!)
  if (match[2] === "ms") return amount
  if (match[2] === "m") return amount * 60_000
  // VHS treats bare durations as seconds.
  return amount * 1_000
}

function positiveInt(value: string | undefined, fallback: number): number {
  const parsed = Number.parseInt(value ?? "", 10)
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback
}

interface VhsTheme {
  name?: string
  background: string
  foreground: string
  cursor?: string
  black: string
  red: string
  green: string
  yellow: string
  blue: string
  magenta: string
  cyan: string
  white: string
  brightBlack: string
  brightRed: string
  brightGreen: string
  brightYellow: string
  brightBlue: string
  brightMagenta: string
  brightCyan: string
  brightWhite: string
}

let vhsThemes: Promise<VhsTheme[]> | null = null
async function resolveVhsTheme(value: string | undefined): Promise<VhsTheme | undefined> {
  if (!value) return undefined
  if (value.trim().startsWith("{")) {
    try { return JSON.parse(value) as VhsTheme } catch { return undefined }
  }
  vhsThemes ??= readFile(resolve(import.meta.dir, "../assets/vhs-themes.json"), "utf8").then((source) => JSON.parse(source) as VhsTheme[])
  return (await vhsThemes).find((theme) => theme.name?.toLowerCase() === value.toLowerCase())
}

function splitWait(value: string): { scope: "Line" | "Screen"; pattern: string } {
  const space = value.indexOf(" ")
  const scope = (space === -1 ? value : value.slice(0, space)) === "Screen" ? "Screen" : "Line"
  return { scope, pattern: space === -1 ? "" : value.slice(space + 1) }
}

export async function runTape(path: string): Promise<TapeRunResult> {
  const parsed = await parseWithUpstream(path)
  const settings: Record<string, string> = {}
  const environment: Record<string, string> = {}
  let inHeader = true
  for (const command of parsed.commands) {
    if (command.type === "Env") environment[command.options] = command.args
    if (command.type === "Set" && (inHeader || command.options === "Shell")) {
      settings[command.options] = command.args
    }
    if (!["Set", "Env", "Output", "Require"].includes(command.type)) inHeader = false
  }

  const initialCols = positiveInt(settings.Columns, 80)
  const initialRows = positiveInt(settings.Rows, 24)
  const cols = initialCols
  const rows = initialRows
  const fontSize = positiveInt(settings.FontSize, 16)
  const canvasWidth = positiveInt(settings.Width, 1280)
  const canvasHeight = positiveInt(settings.Height, 720)
  const padding = Number.parseInt(settings.Padding ?? "0", 10) || 0
  const windowBarSize = Number.parseInt(settings.WindowBarSize ?? "0", 10) || 0
  const theme = await resolveVhsTheme(settings.Theme)
  if (settings.Theme && !theme) throw new Error(`Unknown VHS theme: ${settings.Theme}`)
  const themePalette = theme ? [
    theme.black, theme.red, theme.green, theme.yellow, theme.blue, theme.magenta, theme.cyan, theme.white,
    theme.brightBlack, theme.brightRed, theme.brightGreen, theme.brightYellow,
    theme.brightBlue, theme.brightMagenta, theme.brightCyan, theme.brightWhite,
  ] : []
  const windowBar = (settings.WindowBar ?? "none").toLowerCase() as "none" | "rings" | "ringsright" | "colorful" | "colorfulright"
  if (settings.Width && settings.Columns) throw new Error("VHS tape cannot set both Width and Columns")
  if (settings.Height && settings.Rows) throw new Error("VHS tape cannot set both Height and Rows")
  if (padding < 0 || windowBarSize < 0 || padding * 2 >= canvasWidth || padding * 2 + windowBarSize >= canvasHeight) {
    throw new Error("VHS frame dimensions are too small for the configured padding/window bar")
  }
  const playbackSpeed = Number.parseFloat(settings.PlaybackSpeed ?? "1") || 1
  let defaultTypingSpeed = duration(settings.TypingSpeed, 50)
  const requestedOutputs = parsed.commands.filter((command) => command.type === "Output").map((command) => command.args)
  const requestedScreenshots: Array<{ path: string; atUs: number }> = []
  const warnings: string[] = []
  const events: TapEvent[] = []
  const start = process.hrtime.bigint()
  const terminalBackend = await backend("ghostty")
  const terminal = createTerminal({
    backend: terminalBackend,
    cols,
    rows,
    onAfterWrite(data) {
      events.push({ atUs: elapsedUs(start), type: "output", ...encodeBytes(data) })
    },
  })

  const send = (bytes: Uint8Array, origin: "vhs" | "paste" = "vhs") => {
    events.push({ atUs: elapsedUs(start), type: "input", origin, ...encodeBytes(bytes) })
    terminal.type(new TextDecoder().decode(bytes))
  }

  let clipboard = ""
  let finalText = ""
  let actionsStarted = false
  try {
    for (const command of parsed.commands) {
      if (command.type === "Require" && !Bun.which(command.args)) {
        throw new Error(`VHS Require failed: ${command.args} was not found on PATH`)
      }
    }

    const shell = resolveShell(settings.Shell)
    if (!Bun.which(shell.command[0]!)) throw new Error(`VHS shell not found on PATH: ${shell.command[0]}`)
    await terminal.spawn(shell.command, {
      env: {
        TERM: "xterm-ghostty",
        COLORTERM: "truecolor",
        TERM_PROGRAM: "ghostty",
        TERM_PROGRAM_VERSION: `tap/${TAP_VERSION}`,
        ...shell.env,
        ...environment,
      },
      cwd: dirname(resolve(path)),
    })
    await delay(100)

    for (const command of parsed.commands) {
      if (!terminal.alive && !["Output", "Screenshot", "Hide", "Show", "Set"].includes(command.type)) {
        warnings.push(`Skipped ${command.type}: shell already exited`)
        continue
      }
      if (!["Set", "Env", "Output", "Require"].includes(command.type)) actionsStarted = true
      switch (command.type) {
        case "Type": {
          const speed = duration(command.options, defaultTypingSpeed)
          for (const character of command.args) {
            send(new TextEncoder().encode(character))
            await delay(speed)
          }
          break
        }
        case "Ctrl": {
          const keys = command.args.split(/\s+/).filter(Boolean)
          const last = keys.at(-1) ?? ""
          const legacy = vhsCharacterChord({ ctrl: true, alt: keys.includes("Alt") }, last)
          send(legacy ?? inputBytes(terminal, ["Ctrl", ...keys].join("+")))
          break
        }
        case "Alt": {
          // VHS holds Alt while typing each rune of the argument; Enter/Tab are keywords.
          const runes = [...command.args]
          if (command.args === "Enter" || command.args === "Tab" || runes.length === 0) send(inputBytes(terminal, `Alt+${command.args}`))
          else for (const rune of runes) send(vhsCharacterChord({ alt: true }, rune)!)
          break
        }
        case "Shift":
          send(inputBytes(terminal, `Shift+${command.args}`))
          break
        case "Sleep":
          await delay(duration(command.args, 0))
          break
        case "Wait": {
          const wait = splitWait(command.args)
          const pattern = wait.pattern || settings.WaitPattern || ">$"
          const expression = new RegExp(pattern)
          const timeout = duration(command.options || settings.WaitTimeout, 15_000)
          const deadline = Date.now() + timeout
          const read = () => {
            if (wait.scope === "Screen") return terminal.getText()
            const cursor = terminal.getCursor()
            return terminal.row(cursor.row).getText()
          }
          while (!expression.test(read()) && Date.now() < deadline) await delay(25)
          if (!expression.test(read())) throw new Error(`VHS Wait timed out after ${timeout}ms: /${pattern}/`)
          break
        }
        case "Hide":
          events.push({ atUs: elapsedUs(start), type: "visibility", visible: false })
          break
        case "Show":
          events.push({ atUs: elapsedUs(start), type: "visibility", visible: true })
          break
        case "Screenshot": {
          const atUs = elapsedUs(start)
          const screenshotPath = command.args || `screenshot-${requestedScreenshots.length + 1}.png`
          events.push({ atUs, type: "marker", label: `Screenshot: ${screenshotPath}` })
          requestedScreenshots.push({ path: screenshotPath, atUs })
          break
        }
        case "Set":
          if (command.options === "TypingSpeed") {
            defaultTypingSpeed = duration(command.args, defaultTypingSpeed)
          } else if (actionsStarted) {
            warnings.push(`Ignored late Set ${command.options}, matching VHS semantics`)
          }
          break
        case "Copy":
          clipboard = command.args
          break
        case "Paste":
          send(new TextEncoder().encode(clipboard), "paste")
          break
        case "ScrollUp":
        case "ScrollDown": {
          const count = positiveInt(command.args, 1)
          const delta = command.type === "ScrollUp" ? 1 : -1
          const keyDelay = duration(command.options, 50)
          for (let index = 0; index < count; index++) {
            terminal.backend.scrollViewport(delta)
            events.push({ atUs: elapsedUs(start), type: "scroll", delta })
            await delay(keyDelay)
          }
          break
        }
        case "Output":
        case "Require":
        case "Env":
          break
        case "Source":
          warnings.push(`Source was not expanded by the active parser: ${command.args}`)
          break
        case "Illegal":
          throw new Error("VHS parser produced an illegal command")
        default: {
          const key = KEY_NAMES[command.type]
          if (!key) {
            warnings.push(`Unsupported VHS command: ${command.type}`)
            break
          }
          const count = positiveInt(command.args, 1)
          const keyDelay = duration(command.options, 50)
          for (let index = 0; index < count; index++) {
            send(inputBytes(terminal, key))
            await delay(keyDelay)
          }
        }
      }
    }
    await delay(100)
    finalText = terminal.getText()
  } finally {
    await terminal.close()
  }

  events.sort((left, right) => left.atUs - right.atUs)
  let durationUs = elapsedUs(start)
  if (playbackSpeed !== 1) {
    for (const event of events) event.atUs = Math.round(event.atUs / playbackSpeed)
    for (const screenshot of requestedScreenshots) screenshot.atUs = Math.round(screenshot.atUs / playbackSpeed)
    durationUs = Math.round(durationUs / playbackSpeed)
  }
  return {
    project: {
      version: 1,
      title: basename(path),
      createdAt: new Date().toISOString(),
      durationUs,
      canvas: {
        width: canvasWidth,
        height: canvasHeight,
        background: settings.MarginFill || "#101014",
        fps: positiveInt(settings.Framerate, 60),
      },
      presentation: {
        padding,
        margin: Number.parseInt(settings.Margin ?? "0", 10) || 0,
        marginFill: settings.MarginFill || "#101014",
        borderRadius: Number.parseInt(settings.BorderRadius ?? "0", 10) || 0,
        windowBar,
        windowBarSize,
        ...(settings.WindowTitle ? { windowTitle: settings.WindowTitle } : {}),
        shadow: Number.parseInt(settings.Shadow ?? "0", 10) || 0,
        ...(settings.FontFamily ? { fontFamily: settings.FontFamily } : {}),
        ...(settings.LetterSpacing ? { letterSpacing: Number.parseFloat(settings.LetterSpacing) } : {}),
        ...(settings.LineHeight ? { lineHeight: Number.parseFloat(settings.LineHeight) } : {}),
        cursorBlink: settings.CursorBlink?.toLowerCase() === "true",
        loopOffsetPercent: Number.parseFloat((settings.LoopOffset ?? "0").replace("%", "")) || 0,
        ...(settings.Theme ? { themeName: settings.Theme } : {}),
        ...(theme?.background && theme?.foreground ? {
          theme: {
            background: theme.background,
            foreground: theme.foreground,
            ...(theme.cursor ? { cursor: theme.cursor } : {}),
            palette: themePalette,
          },
        } : {}),
      },
      panes: [
        {
          id: "main",
          title: basename(path),
          cols: initialCols,
          rows: initialRows,
          fontSize,
          layout: { x: 0, y: 0, width: 1, height: 1 },
          events,
        },
      ],
      overlays: [],
      metadata: {
        terminal: "ghostty",
        terminalEngine: "libghostty-vt 1.0.1 native renderer; ghostty-web live capture",
        timebase: "microseconds",
        source: "vhs-tape",
      },
    },
    requestedOutputs,
    requestedScreenshots,
    finalText,
    warnings,
  }
}
