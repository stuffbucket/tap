#!/usr/bin/env bun

import { Command } from "commander"
import { lstat, mkdir, mkdtemp, readFile, rm, symlink, unlink, writeFile } from "node:fs/promises"
import { existsSync, realpathSync } from "node:fs"
import { basename, dirname, join, resolve } from "node:path"
import { homedir, tmpdir } from "node:os"
import { captureProject } from "./capture.ts"
import { readProject, writeProject } from "./project.ts"
import { renderFrameDirectory, renderScreenshot, renderVideo } from "./render.ts"
import { runTape } from "./tape.ts"
import { serveProject } from "./server.ts"
import { startLiveCapture } from "./live.ts"
import { startNativeLiveCapture } from "./native-live.ts"
import { renderGhosttyFrame, uniformFrameSizes } from "./ghostty-full.ts"
import { TAP_VERSION } from "./version.ts"
import { printTranscript } from "./transcript.ts"

/** The CLI launcher inside Tap.app (absent when running from a source checkout). */
const bundledCli = resolve(import.meta.dir, "../bin/tap")
const shellQuote = (value: string) => `'${value.replaceAll("'", `'\\''`)}'`
/** Whether `tap` on the user's own PATH (before the launcher prepends its bin) is this Tap. */
function cliOnPath(): boolean {
  if (!existsSync(bundledCli)) return false
  const target = realpathSync(bundledCli)
  for (const entry of (process.env.TAP_USER_PATH ?? process.env.PATH ?? "").split(":")) {
    const candidate = join(entry, "tap")
    try { if (realpathSync(candidate) === target) return true } catch {}
  }
  return false
}
import { PINNED_GHOSTTY_COMMIT, PINNED_VT_TAG, ghosttyAppCommit, matchesPinnedCommit, resolveGhosttyApp } from "./ghostty-app.ts"
import type { CaptureSpec } from "./model.ts"

process.stdout.on("error", (error: NodeJS.ErrnoException) => {
  if (error.code === "EPIPE") process.exit(0)
  throw error
})

const program = new Command()
  .name("tap")
  .description("Editable Ghostty terminal capture, playback, and media export")
  .version(TAP_VERSION)

program
  .command("capture")
  .description("Capture one interactive PTY into an editable .tap JSON project")
  .option("-o, --output <path>", "project path", "session.tap")
  .option("--cols <number>", "initial columns", "80")
  .option("--rows <number>", "initial rows", "24")
  .option("--font-size <number>", "font size", "16")
  .option("--no-interactive", "do not forward keyboard input")
  .option("--input-policy <policy>", "record, redact, or omit input bytes", "record")
  .option("--no-shell-integration", "do not have zsh mark prompts and commands (OSC 133)")
  .argument("<command...>")
  .allowUnknownOption(true)
  .action(async (command: string[], options: Record<string, string | boolean>) => {
    const inputPolicy = String(options.inputPolicy)
    if (inputPolicy !== "record" && inputPolicy !== "redact" && inputPolicy !== "omit") {
      throw new Error("--input-policy must be record, redact, or omit")
    }
    const spec: CaptureSpec = {
      title: command.join(" "),
      panes: [
        {
          id: "main",
          command,
          cols: Number(options.cols),
          rows: Number(options.rows),
          fontSize: Number(options.fontSize),
        },
      ],
    }
    const project = await captureProject(spec, {
      interactive: options.interactive !== false,
      mirrorOutput: true,
      inputPolicy,
      shellIntegration: options.shellIntegration !== false,
    })
    await writeProject(String(options.output), project)
    process.stderr.write(`\nSaved ${options.output} (${project.durationUs}µs, ${project.panes[0]!.events.length} events)\n`)
  })

program
  .command("capture-spec")
  .description("Capture multiple PTYs concurrently from a JSON split-pane specification")
  .argument("<spec>")
  .option("-o, --output <path>", "project path", "session.tap")
  .action(async (specPath: string, options: { output: string }) => {
    const spec = JSON.parse(await readFile(resolve(specPath), "utf8")) as CaptureSpec
    const project = await captureProject(spec, { interactive: false, mirrorOutput: false })
    await writeProject(options.output, project)
    console.log(`Saved ${options.output} with ${project.panes.length} panes and ${project.durationUs}µs duration`)
  })

program
  .command("tape")
  .description("Execute a VHS-compatible .tape through Ghostty and save an editable project")
  .argument("<tape>")
  .option("-o, --output <path>", "project path", "session.tap")
  .option("--render-outputs", "also render Output paths declared in the tape", false)
  .action(async (tapePath: string, options: { output: string; renderOutputs: boolean }) => {
    const result = await runTape(tapePath)
    await writeProject(options.output, result.project)
    for (const warning of result.warnings) console.warn(`warning: ${warning}`)
    console.log(`Saved ${options.output}`)
    for (const screenshot of result.requestedScreenshots) {
      const target = resolve(dirname(resolve(tapePath)), screenshot.path)
      await renderScreenshot(result.project, target, screenshot.atUs)
      console.log(`Rendered ${target}`)
    }
    if (options.renderOutputs) {
      for (const output of result.requestedOutputs) {
        const target = resolve(dirname(resolve(tapePath)), output)
        if (/[\\/]$/.test(output)) {
          const count = await renderFrameDirectory(result.project, target)
          console.log(`Rendered ${count} PNG frames to ${target}`)
        } else if (/\.(txt|ascii|test)$/i.test(output)) {
          await writeFile(target, `${result.finalText}\n`, "utf8")
          console.log(`Rendered ${target}`)
        } else if (/\.png$/i.test(output)) {
          await renderScreenshot(result.project, target)
          console.log(`Rendered ${target}`)
        } else {
          await renderVideo(result.project, target)
          console.log(`Rendered ${target}`)
        }
      }
    }
  })

program
  .command("render")
  .description("Render a project to MP4, WebM, MOV, or GIF through FFmpeg")
  .argument("<project>")
  .requiredOption("-o, --output <path>", "media output path")
  .option("--fps <number>", "override frames per second")
  .option("--start <seconds>", "start time in seconds")
  .option("--end <seconds>", "end time in seconds")
  .option("--scale <factor>", "output size multiplier; 2 gives Retina-sharp video at twice the canvas size", "1")
  .action(async (projectPath: string, options: { output: string; fps?: string; start?: string; end?: string; scale: string }) => {
    const project = await readProject(projectPath)
    let lastPercent = -1
    await renderVideo(project, options.output, {
      scale: Number(options.scale),
      ...(options.fps ? { fps: Number(options.fps) } : {}),
      ...(options.start ? { startUs: Math.round(Number(options.start) * 1_000_000) } : {}),
      ...(options.end ? { endUs: Math.round(Number(options.end) * 1_000_000) } : {}),
      onProgress(frame, total) {
        const percent = Math.floor((frame / total) * 100)
        if (percent >= lastPercent + 5 || frame === total) {
          process.stderr.write(`\rRendering ${frame}/${total} (${percent}%)`)
          lastPercent = percent
        }
      },
    })
    process.stderr.write(`\nSaved ${options.output}\n`)
  })

program
  .command("ghostty-screenshot")
  .description("Render a captured pane with Ghostty's actual Metal/CoreText renderer (macOS)")
  .argument("<project>")
  .requiredOption("-o, --output <path>", "PNG output path")
  .option("--at <seconds>", "timestamp in seconds (default: end of recording)")
  .option("--pane <id>", "pane to render (default: first pane)")
  .action(async (projectPath: string, options: { output: string; at?: string; pane?: string }) => {
    const project = await readProject(projectPath)
    const pane = options.pane ? project.panes.find((entry) => entry.id === options.pane) : project.panes[0]
    if (!pane) throw new Error(`unknown pane: ${options.pane}`)
    const atUs = options.at === undefined ? project.durationUs : Math.round(Number(options.at) * 1_000_000)
    await renderGhosttyFrame(pane, atUs, options.output)
    console.log(`Saved ${options.output} with Ghostty's Metal renderer`)
  })

program
  .command("ghostty-video")
  .description("Render video frames with Ghostty's actual Metal renderer, then encode with FFmpeg (macOS)")
  .argument("<project>")
  .requiredOption("-o, --output <path>", "video output path")
  .option("--fps <number>", "frames per second", "10")
  .option("--pane <id>", "pane to render (default: first pane)")
  .action(async (projectPath: string, options: { output: string; fps: string; pane?: string }) => {
    const project = await readProject(projectPath)
    const pane = options.pane ? project.panes.find((entry) => entry.id === options.pane) : project.panes[0]
    if (!pane) throw new Error(`unknown pane: ${options.pane}`)
    const fps = Math.max(1, Math.min(30, Number(options.fps)))
    const frameCount = Math.max(1, Math.floor(project.durationUs / (1_000_000 / fps)) + 1)
    const directory = await mkdtemp(join(tmpdir(), "tap-ghostty-video-"))
    try {
      for (let frame = 0; frame < frameCount; frame++) {
        const atUs = Math.round(frame * 1_000_000 / fps)
        await renderGhosttyFrame(pane, atUs, join(directory, `${String(frame + 1).padStart(6, "0")}.png`), { quiet: true })
        process.stderr.write(`\rRendering ${frame + 1}/${frameCount}`)
      }
      process.stderr.write("\n")
      // Frames are sized to the pane's grid; pad odd dimensions for H.264.
      // Frames change size when the recording resizes; pad them to one size rather than letting FFmpeg rescale.
      await uniformFrameSizes(Array.from({ length: frameCount }, (_, frame) => join(directory, `${String(frame + 1).padStart(6, "0")}.png`)))
      const ffmpeg = Bun.spawn(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-framerate", String(fps), "-i", join(directory, "%06d.png"), "-c:v", "libx264", "-pix_fmt", "yuv420p", resolve(options.output)], { stdin: "ignore", stdout: "ignore", stderr: "inherit" })
      if ((await ffmpeg.exited) !== 0) throw new Error("FFmpeg encoding failed")
      console.log(`Saved ${options.output} with ${frameCount} actual Ghostty-rendered frames`)
    } finally {
      await rm(directory, { recursive: true, force: true })
    }
  })

program
  .command("screenshot")
  .description("Render an exact project timestamp to PNG")
  .argument("<project>")
  .requiredOption("-o, --output <path>", "PNG output path")
  .option("--at <seconds>", "timestamp in seconds")
  .option("--scale <factor>", "output size multiplier; 2 gives a Retina-sharp image", "1")
  .action(async (projectPath: string, options: { output: string; at?: string; scale: string }) => {
    const project = await readProject(projectPath)
    await renderScreenshot(
      project,
      options.output,
      options.at === undefined ? project.durationUs : Math.round(Number(options.at) * 1_000_000),
      { scale: Number(options.scale) },
    )
    console.log(`Saved ${options.output}`)
  })

program
  .command("native-live")
  .description("Record browser-controlled Tap-native independent PTY splits")
  .argument("[command...]", "command for each new pane")
  .option("-o, --output <path>", "project path", "native-live.tap")
  .option("--host <hostname>", "listen hostname", "127.0.0.1")
  .option("--port <number>", "listen port", "4379")
  .action(async (command: string[], options: { output: string; host: string; port: string }) => {
    const shell = command.length ? command : [process.env.SHELL || "/bin/sh"]
    const live = await startNativeLiveCapture({ output: options.output, shell, hostname: options.host, port: Number(options.port) })
    console.log(`Tap native live capture: http://${live.hostname}:${live.port}`)
    const stop = () => { void live.stop() }; process.once("SIGINT", stop); process.once("SIGTERM", stop)
    await live.done
    console.log(`Saved ${options.output}`)
  })

program
  .command("live")
  .description("Record a live browser-controlled tmux session with right-click split creation")
  .argument("[command...]", "initial tmux pane command")
  .option("-o, --output <path>", "project path", "live.tap")
  .option("--host <hostname>", "listen hostname", "127.0.0.1")
  .option("--port <number>", "listen port", "4378")
  .action(async (command: string[], options: { output: string; host: string; port: string }) => {
    if (!Bun.which("tmux")) throw new Error("tap live requires tmux on PATH")
    const session = `tap-live-${process.pid}-${Date.now()}`
    const initial = command.length > 0 ? command : [process.env.SHELL || "/bin/sh"]
    const live = await startLiveCapture({
      output: options.output,
      hostname: options.host,
      port: Number(options.port),
      command: ["tmux", "-f", "/dev/null", "new-session", "-s", session, ...initial],
      cleanupCommand: ["tmux", "-f", "/dev/null", "kill-session", "-t", session],
    })
    console.log(`Tap live capture: http://${live.hostname}:${live.port}`)
    console.log("Right-click the terminal to create, focus, or close tmux splits")
    const stop = () => { void live.stop() }
    process.once("SIGINT", stop)
    process.once("SIGTERM", stop)
    const project = await live.done
    console.log(`Saved ${options.output} (${project.durationUs}µs)`)
  })

program
  .command("serve")
  .description("Open a local Ghostty-WASM player with right-click editing controls")
  .argument("<project>")
  .option("--host <hostname>", "listen hostname", "127.0.0.1")
  .option("--port <number>", "listen port", "4377")
  .action(async (projectPath: string, options: { host: string; port: string }) => {
    const server = await serveProject(projectPath, { hostname: options.host, port: Number(options.port) })
    console.log(`Tap player: http://${server.hostname}:${server.port}`)
    console.log("Press Ctrl+C to stop")
    await new Promise<void>((resolvePromise) => {
      const stop = () => {
        void server.stop().finally(resolvePromise)
      }
      process.once("SIGINT", stop)
      process.once("SIGTERM", stop)
    })
  })

program
  .command("transcript")
  .description("Print a recording's transcript, one block per command (runs the Rust tap-index binary)")
  .argument("<recording>", ".tap JSON project or Session.tap package")
  .option("--json", "print the transcript as JSON")
  .action(async (recording: string, options: { json?: boolean }) => {
    const code = await printTranscript(recording, options)
    if (code !== 0) process.exitCode = code
  })

program
  .command("quickstart")
  .description("Show the shortest capture, playback, live-split, and render workflow")
  .action(() => {
    const onPath = cliOnPath()
    if (!onPath && existsSync(bundledCli)) {
      console.log(`Put tap on your PATH first (links it into ~/.local/bin):
   ${shellQuote(bundledCli)} install-cli
`)
    }
    console.log(`Tap quick start

1. Record a terminal:
   tap capture -o demo.tap -- zsh

2. Watch and edit it:
   tap serve demo.tap
   open http://127.0.0.1:4377

3. Export media:
   tap screenshot demo.tap -o demo.png
   tap render demo.tap -o demo.mp4 --fps 120

Live browser capture with tmux-owned splits:
   tap live -o live.tap -- zsh
   open http://127.0.0.1:4378

Live browser capture with Tap-native independent PTYs:
   tap native-live -o native-live.tap -- zsh
   open http://127.0.0.1:4379

VHS tape compatibility:
   tap tape demo.tape -o demo.tap --render-outputs

Diagnostics and notices:
   tap doctor
   tap licenses

Tap does not upload or publish recordings.`)
  })

program
  .command("install-cli")
  .description("Link the tap command into a directory on your PATH (default ~/.local/bin)")
  .option("--dir <path>", "directory to link tap into", join(homedir(), ".local/bin"))
  .action(async (options: { dir: string }) => {
    if (!existsSync(bundledCli)) throw new Error("install-cli links the CLI inside Tap.app; run it from the installed app")
    if (realpathSync(bundledCli).startsWith("/Volumes/")) throw new Error("Tap is running from its disk image; drag Tap to Applications, then run install-cli from there")
    const directory = resolve(options.dir)
    const link = join(directory, "tap")
    await mkdir(directory, { recursive: true })
    const existing = await lstat(link).catch(() => null)
    if (existing && !existing.isSymbolicLink()) throw new Error(`${link} exists and is not a link; remove it or choose --dir`)
    if (existing) await unlink(link)
    await symlink(bundledCli, link)
    console.log(`Linked ${link} -> ${bundledCli}`)
    const pathEntries = (process.env.TAP_USER_PATH ?? process.env.PATH ?? "").split(":").map((entry) => resolve(entry))
    if (!pathEntries.includes(directory)) {
      console.log(`${directory} is not on your PATH. Add it, e.g. for zsh:\n   echo 'export PATH="${directory}:$PATH"' >> ~/.zshrc && exec zsh`)
    }
  })

program
  .command("launch")
  .description("What opening Tap.app does: record a shell in this terminal, then open the recording in the player")
  .option("--dir <path>", "folder for recordings", join(homedir(), "Movies/Tap"))
  .option("--no-open", "do not open the player in a browser afterwards")
  .option("--no-shell-integration", "do not have zsh mark prompts and commands (OSC 133)")
  .action(async (options: { dir: string; open: boolean; shellIntegration: boolean }) => {
    // The first launch checks every bundled component before anything is recorded.
    const support = join(homedir(), "Library/Application Support/Tap")
    const checked = join(support, "doctor-passed")
    if (!existsSync(checked)) {
      const doctor = Bun.spawn([process.execPath, import.meta.path, "doctor"], { stdio: ["ignore", "inherit", "inherit"] })
      if ((await doctor.exited) !== 0) {
        console.error("\nTap can't record until the problems above are fixed.")
        process.exitCode = 1
        return
      }
      await mkdir(support, { recursive: true })
      await writeFile(checked, `${new Date().toISOString()}\n`)
      console.log()
    }

    const now = new Date()
    const pad = (value: number) => String(value).padStart(2, "0")
    const name = `Tap ${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())} at ${pad(now.getHours())}.${pad(now.getMinutes())}.${pad(now.getSeconds())}.tap`
    const directory = resolve(options.dir)
    await mkdir(directory, { recursive: true })
    const output = join(directory, name)
    const shell = process.env.SHELL || "/bin/zsh"

    process.stdout.write(
      `\x1b[1;31m●\x1b[0m \x1b[1mTap is recording this terminal\x1b[0m to ${output}\n` +
      `  Everything you type and see is saved, including passwords. Type \x1b[1mexit\x1b[0m to stop and open the player.\n` +
      `  For sensitive work, record with: tap capture --input-policy redact -o file.tap -- ${basename(shell)}\n\n`,
    )
    const project = await captureProject({
      title: `${basename(shell)} — ${name.replace(/\.tap$/, "")}`,
      panes: [{ id: "main", command: [shell, "-l"], cols: process.stdout.columns || 80, rows: process.stdout.rows || 24, fontSize: 16 }],
    }, { interactive: true, mirrorOutput: true, inputPolicy: "record", shellIntegration: options.shellIntegration })
    await writeProject(output, project)
    console.log(`\n\x1b[1m■\x1b[0m Saved ${output} (${(project.durationUs / 1_000_000).toFixed(1)}s)`)

    const server = await serveProject(output, { hostname: "127.0.0.1", port: 0 })
    const url = `http://${server.hostname}:${server.port}`
    console.log(`  Player: ${url}  — scrub, edit, and export there. Press Ctrl+C to close it.`)
    console.log(`  Later: tap serve ${shellQuote(output)}   ·   tap render ${shellQuote(output)} -o video.mp4`)
    if (options.open) Bun.spawn(["/usr/bin/open", url], { stdio: ["ignore", "ignore", "ignore"] })
    await new Promise<void>((resolvePromise) => {
      const stop = () => { void server.stop().finally(resolvePromise) }
      process.once("SIGINT", stop)
      process.once("SIGTERM", stop)
    })
  })

program
  .command("licenses")
  .description("Print bundled third-party software notices and license information")
  .option("--path", "print the notices file path instead of its contents")
  .action(async (options: { path?: boolean }) => {
    const notices = resolve(import.meta.dir, "../THIRDPARTYNOTICES")
    if (!(await Bun.file(notices).exists())) throw new Error("THIRDPARTYNOTICES is missing from this installation")
    if (options.path) console.log(notices)
    else process.stdout.write(await Bun.file(notices).text())
  })

program
  .command("doctor")
  .description("Check capture, rendering, browser, split, and codec prerequisites")
  .action(async () => {
    const checks: Array<[string, boolean, string]> = []
    const command = async (name: string, args: string[] = []) => {
      const path = Bun.which(name)
      if (!path) return false
      const child = Bun.spawn([path, ...args], { stdin: "ignore", stdout: "ignore", stderr: "ignore" })
      return (await child.exited) === 0
    }
    checks.push(["Bun >= 1.4.3", typeof Bun.version === "string", Bun.version])
    checks.push(["FFmpeg", await command("ffmpeg", ["-version"]), Bun.which("ffmpeg") ?? "missing"])
    checks.push(["tmux (tap live)", await command("tmux", ["-V"]), Bun.which("tmux") ?? "missing"])
    const nativeRenderer = resolve(import.meta.dir, "../native/bin/tap-vt-render")
    const vhsParser = resolve(import.meta.dir, "../native/bin/tap-vhs-parse")
    const kittyWasm = resolve(import.meta.dir, "../.cache/libghostty-vt-kitty-prefix/bin/ghostty-vt.wasm")
    checks.push(["native libghostty-vt renderer", await Bun.file(nativeRenderer).exists(), nativeRenderer])
    checks.push(["VHS parser", await Bun.file(vhsParser).exists(), vhsParser])
    checks.push(["Kitty WASM", await Bun.file(kittyWasm).exists(), kittyWasm])
    if (process.platform === "darwin") {
      const ghostty = resolveGhosttyApp()
      const commit = await ghosttyAppCommit(ghostty.executable)
      const present = await Bun.file(ghostty.executable).exists()
      const matched = matchesPinnedCommit(commit)
      checks.push([
        `Ghostty.app renderer (${PINNED_VT_TAG} commit)`,
        present && matched,
        present
          ? `${ghostty.executable} — commit ${commit ?? "unknown"}${matched ? "" : `; expected ${PINNED_GHOSTTY_COMMIT.slice(0, 9)} (run bun run build:ghostty-app)`}`
          : `missing (run bun run build:ghostty-app)`,
      ])
    }
    let failed = false
    for (const [label, ok, detail] of checks) { console.log(`${ok ? "✓" : "✗"} ${label} — ${detail}`); if (!ok) failed = true }
    if (failed) process.exitCode = 1
  })

program
  .command("inspect")
  .description("Print a concise project summary")
  .argument("<project>")
  .action(async (projectPath: string) => {
    const project = await readProject(projectPath)
    console.log(
      JSON.stringify(
        {
          title: project.title,
          durationSeconds: project.durationUs / 1_000_000,
          canvas: project.canvas,
          panes: project.panes.map((pane) => ({
            id: pane.id,
            title: pane.title,
            size: `${pane.cols}x${pane.rows}`,
            events: pane.events.length,
            eventTypes: Object.fromEntries(
              [...new Set(pane.events.map((event) => event.type))].map((type) => [
                type,
                pane.events.filter((event) => event.type === type).length,
              ]),
            ),
            layout: pane.layout,
          })),
          overlays: project.overlays.length,
          engine: project.metadata.terminalEngine,
        },
        null,
        2,
      ),
    )
  })

program
  .command("example-spec")
  .description("Write a two-pane capture specification")
  .argument("[path]", "output path", "capture.json")
  .action(async (path: string) => {
    const example: CaptureSpec = {
      title: "Two-pane demo",
      canvas: { width: 1280, height: 720, fps: 60, background: "#101014" },
      panes: [
        { id: "left", command: ["/bin/sh", "-lc", "printf '\\033[32mleft pane\\033[0m\\n'; sleep 1"] },
        { id: "right", command: ["/bin/sh", "-lc", "printf '\\033[36mright pane\\033[0m\\n'; sleep 1"] },
      ],
    }
    await writeFile(resolve(path), `${JSON.stringify(example, null, 2)}\n`)
    console.log(`Saved ${path}`)
  })

// Errors surface as one line; set TAP_DEBUG=1 for stack traces.
const report = (error: unknown) => {
  if (process.env.TAP_DEBUG) console.error(error)
  else console.error(`tap: ${error instanceof Error ? error.message : String(error)}`)
}
// Failures outside a command's own promise (signal handlers, stream errors) get the same treatment.
process.on("unhandledRejection", (error) => { report(error); process.exit(1) })
process.on("uncaughtException", (error) => { report(error); process.exit(1) })

program.parseAsync().catch((error: unknown) => {
  report(error)
  process.exitCode = 1
})
