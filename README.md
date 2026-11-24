# Tap

Tap records terminal sessions as editable timelines and replays them through Ghostty. It captures what a person sends to the PTY, what the program writes back, terminal-generated replies, resizes, split layouts, and Kitty graphics. The same project produces screenshots, high-frame-rate video, and an interactive browser player.

## Implemented

- Exact timestamped **PTY input and output** capture, including terminal-generated query replies.
- Optional input redaction or omission for secrets.
- Concurrent split-pane recording with one PTY and event track per pane.
- Public, stable **`libghostty-vt` 1.0.1** terminal/render state.
- Direct Kitty RGB/RGBA and PNG images, placements, layers, crop, scaling, and animations.
- Portable normalization of Kitty file and temporary-file transports while retaining original PTY bytes.
- Kitty animation driven from project time during screenshots, seeking, and 30–240 fps video export.
- Editable `.tap` JSON: input, output, resize, viewport scroll, layout, visibility, marker, and overlay events.
- PNG screenshots and MP4/WebM/MOV/GIF through raw RGBA frames piped to FFmpeg.
- Charmbracelet VHS syntax through the upstream VHS 0.12.1 lexer/parser, including `Source`, `Env`, `Require`, typing/key delays, modifiers, `Wait`, `Hide`/`Show`, clipboard copy/paste, scrolling, screenshots, and outputs.
- Browser playback with Kitty-enabled `libghostty-vt` WASM images/animations or native pixel frames, seeking, speed control, optional keystroke display, and right-click editing controls.
- Live browser capture backed by tmux, with right-click split creation/focus/close recorded in the PTY stream.
- Live browser capture with Tap-native independent PTYs (`native-live`): right-click split/close with neighbour reflow, per-pane fit-to-window resizes, and UTF-8/IME/paste input.
- Optional macOS screenshots through Ghostty's actual internal CoreText/Metal/IOSurface renderer.

## Build

The complete app/DMG build layout is documented in [`BUILDING.md`](BUILDING.md). Icon source layers and macOS 26/27 treatment guidance are documented in [`docs/icon-design.md`](docs/icon-design.md).

Requirements:

- Bun 1.4.3+
- Go 1.27+
- FFmpeg
- Clang
- `mise` (the build pins Zig 0.17.1)
- Cairo and libpng development libraries
- Git; `pkg-config` on non-Homebrew systems
- tmux for `tap live`
- macOS for `ghostty-screenshot`/`ghostty-video` (the build fetches the pinned Ghostty.app)
- FFmpeg builds must expose the requested encoders (`libx264`, `libvpx-vp9`, `prores_ks`)

On macOS:

```sh
brew install cairo libpng ffmpeg mise
mise x bun@1.4.3 -- bun install
mise x bun@1.4.3 -- bun run build
mise x bun@1.4.3 -- bun run check
```

`build:native` checks out the `vt-1.0.1` tag in `.cache/`, builds its static library, a direct-Kitty browser WASM, the Tap renderer, and POSIX shared-memory helper. `build:vhs` builds a parser sidecar against upstream VHS 0.12.1. Generated binaries live in `native/bin/` and are intentionally not committed.

On macOS, the standard `bun run build` also builds the actual Ghostty Metal renderer; `build:ghostty-full` can rebuild that component independently:

```sh
bun run build:ghostty-full
bun src/cli.ts ghostty-screenshot demo.tap -o ghostty.png
bun src/cli.ts ghostty-video demo.tap -o ghostty.mp4 --fps 10
```

This path dynamically loads an unmodified, signed Ghostty.app's renderer exports and uses Ghostty's official CoreText glyph pipeline, atlas, precompiled Metal shaders, cursor, and IOSurface target. No Ghostty source patches are involved.

The Ghostty.app is pinned to the same commit as Tap's libghostty-vt (`vt-1.0.1`, commit `108cacb17`), so the Metal renderer and Tap's terminal state come from identical Ghostty source. No Ghostty.app release contains that commit, so `bun run build:ghostty-app` (part of `bun run build`) downloads Ghostty's official build of it and refuses it unless it is signed by Ghostty's Developer ID (`24VZTF6M5V`), notarized, and reports that exact commit. The DMG bundles this app plus a provenance receipt; `tap doctor` reports the commit in use. Without the pinned build, Tap falls back to an installed `/Applications/Ghostty.app` and `doctor` flags the mismatch.

Each frame replays the pane's recorded output into a Ghostty surface sized to the pane's exact grid at that moment. The replay runs with echo off, so Ghostty's answers to recorded terminal queries are not drawn, and it starts with a full terminal reset, so the banner macOS `login(1)` prints is not drawn either. `ghostty-screenshot --at <seconds>` renders any moment; `ghostty-video` renders each frame this way and encodes with FFmpeg. Replay is cumulative, so resizes earlier in a session reflow at the frame's grid size.

Building Ghostty.app itself from source is not part of Tap's build: it would need full Xcode for Apple's `metal` shader compiler, and Ghostty's own signed, notarized build of the pinned commit is the same code.

## Watch someone use their terminal

```sh
mise x bun@1.4.3 -- bun src/cli.ts capture -o demo.tap -- zsh
```

Tap enters raw-input mode and records:

1. bytes typed or pasted into the PTY;
2. terminal-generated input such as cursor-position replies;
3. output bytes read from the PTY;
4. terminal resizes.

When capture ends, Tap returns your terminal to a normal shell state even if the recorded program crashed or was killed: it leaves the alternate screen if the program left it on and turns off mouse reporting, focus events, bracketed paste, application cursor keys, modifyOtherKeys and the Kitty keyboard protocol.

Output is replayed through Ghostty at its original time. Shell echo makes ordinary typing appear naturally. The separate input lane powers the optional keystroke overlay and preserves activity that is not echoed.

Input may contain passwords or tokens. Choose a policy at capture time:

```sh
# Default: exact input bytes
bun src/cli.ts capture --input-policy record -o demo.tap -- zsh

# Preserve input timing/provenance, but not bytes
bun src/cli.ts capture --input-policy redact -o demo.tap -- zsh

# Store no keyboard-input events
bun src/cli.ts capture --input-policy omit -o demo.tap -- zsh
```

## Capture splits

```sh
bun src/cli.ts example-spec capture.json
# Edit each pane's command, cwd, environment, dimensions, and normalized layout.
bun src/cli.ts capture-spec capture.json -o split.tap
```

All pane PTYs start on one monotonic project clock. The resulting tracks can be moved, resized, hidden, or composited independently.

For live split creation in the browser:

```sh
bun src/cli.ts live -o live.tap -- zsh
# Open http://127.0.0.1:4378 and right-click the terminal.
```

Tap runs tmux inside one recorded PTY. Right-click actions create vertical/horizontal splits, switch focus, and close panes while the exact tmux input/output stream is recorded. Browser-generated device replies are suppressed in favor of the single stable `libghostty-vt` reply path, so they do not leak as shell text.

For independent Tap-owned PTYs instead of tmux:

```sh
bun src/cli.ts native-live -o native-live.tap -- zsh
# Open http://127.0.0.1:4379 and right-click a pane.
```

The native browser UI creates, focuses, closes, displays, and records independent pane processes through `CaptureSession`. Closing a pane (or exiting its shell) hands its area to the neighbour that shares a full edge; each pane fits its PTY to its on-screen size and records the resize. When the last shell exits, the recording is saved automatically. `bun run test:e2e` drives this flow in Chrome — split, type UTF-8 text, close, save — and checks the resulting tracks.

## VHS tapes

```sh
bun src/cli.ts tape demo.tape -o demo.tap
bun src/cli.ts tape demo.tape -o demo.tap --render-outputs
```

Tap uses Charmbracelet's real parser rather than a lookalike grammar. Tape commands execute against a Ghostty-backed terminal and produce an ordinary editable project. `Screenshot` outputs are rendered at their recorded timestamps. With `--render-outputs`, declared `Output` files are rendered after capture.

Compatibility is command/timing compatibility, not pixel equality with VHS's ttyd/xterm.js/Chrome renderer. Named shells start exactly as VHS starts them — bash by default, without rc files or history, with VHS's `> ` prompt — so recordings never pick up the recorder's own prompt or paths; any other `Set Shell` value runs as a command line (a Tap extension). Key chords send VHS's xterm.js bytes: `Ctrl+C` is `0x03` whatever the letter's case, and `Alt+x` is `ESC x`.

`bun run test:vhs-upstream` parses all 109 upstream example tapes (the sole parser failure is the intentionally invalid fixture) and executes the rest that can run unattended: all 12 command fixtures, the presentation/settings fixtures, the environment, screenshot, decorations, demo, welcome and tmux split tapes, every shell tape whose shell is installed (bash, zsh, ksh, fish, nu, osh, xonsh), and the tapes that drive glow, gum, slides and jqp when those programs are installed — 55 on a machine with all of them. Tapes for programs or shells that are missing are skipped or, for shells, checked to fail with a clear error. Those app tapes run with a scratch `HOME` and `EDITOR=vim`, and the upstream checkout is reset after each, since several edit or save files. `bun run test:vhs-bubbletea` fetches the Bubble Tea examples and drives each real TUI with its upstream tape (35 more), checking that it builds, launches, accepts the tape's keys, and exits back to the shell. Not run: the Ruby cli-ui gem tapes, the `gh` tapes (GitHub login), `meta.tape` (needs VHS itself), `neofetch` (no longer in Homebrew), PowerShell and Windows `cmd`, Bubble Tea's `http` example (fetches an external site), and `publish.tape`, which uploads to vhs.charm.sh and is never run.

Leading-decimal durations, 50ms typing/repeat defaults, global PlaybackSpeed, default waits, late-setting rules, frame directories, text outputs, and impossible-dimension errors follow upstream behavior.

Working examples are included:

```sh
bun src/cli.ts tape examples/kitty-animation.tape -o kitty.tap --render-outputs
bun src/cli.ts capture-spec examples/split-capture.json -o split.tap
```

## VHS presentation controls

Tape projects persist and render `Padding`, `Margin`, `MarginFill`, `BorderRadius`, `WindowBar`/`WindowBarSize`, `WindowTitle`, `Shadow`, `FontFamily`, `FontSize`, `LetterSpacing`, `LineHeight`, `CursorBlink`, `Theme`, and GIF `LoopOffset`. Presentation remains editable in the `.tap` `presentation` object. Named themes are resolved from the upstream VHS theme catalogue with all 16 ANSI palette colors plus background, foreground, and cursor; portable output draws the outer margin, rounded frame, optional window bar, title, padding, and content bounds consistently.

## Screenshots and video

```sh
bun src/cli.ts screenshot demo.tap -o demo.png --at 1.25
bun src/cli.ts render demo.tap -o demo.mp4 --fps 120
bun src/cli.ts render demo.tap -o demo.webm --fps 60
```

Export uses a deterministic fixed-step project clock. It does not screen-capture and does not derive timing from pipe arrival. Every frame is composited to RGBA and sent to FFmpeg.

A tested 120 fps Kitty animation produced changing red/blue frames in a 120/1 H.264 stream. Split-pane capture with ordinary PTY output and a Kitty animation was also rendered and inspected. Interactive playback in headed Chrome on a 120 Hz ProMotion display measured 119.7 WASM animation ticks/s and 117.7 native Kitty frames/s over two seconds, i.e. tracking the display refresh rate. With Chrome's frame limiter/vsync disabled, Kitty-enabled WASM sustained over 4,000 animation ticks/s, leaving headroom for 144/240 Hz displays. Live rates are visible in the player toolbar.

## Browser player/editor

```sh
bun src/cli.ts serve demo.tap
# http://127.0.0.1:4377
```

The player binds to loopback by default and offers two views:

- **Native pixels:** server-rendered stable `libghostty-vt` frames, including Kitty graphics and overlays.
- **WASM cells + Kitty:** low-latency in-browser Ghostty-WASM playback with direct RGB/RGBA/gray Kitty placements and animations. PNG/file/shared-memory packets are normalized to direct RGBA during capture.

Controls include seeking, speed, keystroke visibility, saving, downloading, and a right-click menu for copy, markers, hide/show, horizontal/vertical pane layouts, and terminal resizing.

## Project model

A `.tap` file is versioned, formatted JSON. Each pane has an ordered event track with integer microsecond timestamps:

- `input` — exact bytes plus origin (`keyboard`, `paste`, `terminal-reply`, or `vhs`), optionally redacted;
- `output` — exact PTY bytes plus optional self-contained normalized playback bytes;
- `resize` — terminal rows and columns;
- `scroll` — viewport movement;
- `layout` — normalized compositing bounds;
- `visibility` and `marker`;
- project-level text and image overlays.

Raw byte payloads use base64 so partial UTF-8 and binary protocol data survive round trips. Visual edits do not mutate source bytes.

A post-capture layout resize is always safe. A synthetic PTY resize cannot invent the alternate output a live TUI would have produced after receiving `SIGWINCH`; rerun a VHS source when semantic re-execution is needed.

## Fidelity and security boundaries

The portable deterministic exporter uses public `libghostty-vt` render state and an application-owned Cairo rasterizer. It draws with the JetBrains Mono shipped in the app (registered with CoreText at startup, so nothing needs to be installed), places every glyph on the font's baseline, slants the upright face for italics, and falls back to the bundled Noto Sans Symbols 2, Nerd Font symbols and Noto Emoji, then the system fonts, for characters JetBrains Mono lacks (Braille spinners, icons, CJK). A VHS `FontFamily` that is not installed falls back to JetBrains Mono. On macOS, `ghostty-screenshot` uses the actual internal Ghostty CoreText/Metal renderer and captures its IOSurface directly. That internal API comes from the same pinned commit as libghostty-vt and is isolated behind an optional helper because Ghostty does not provide a stable public offscreen-renderer ABI.

Direct Kitty data is self-contained and supported. Kitty file and temporary-file transports are normalized during capture: Tap retains the exact original PTY bytes, reads only files under the pane working directory, the system temporary directory, or explicit `kittyFileRoots`, and stores a direct replay payload. Temporary files are removed after successful normalization. Shared-memory transfers are normalized and unlinked through `/dev/shm` on Linux and a bounded POSIX `shm_open` helper on macOS/BSD.

Playback never executes recorded output. Clipboard, URL, notification, and filesystem effects remain inert unless explicitly handled by the application.

## Local macOS DMG

See [`RELEASE_NOTES.md`](RELEASE_NOTES.md) for the current release.

Build and verify the self-contained arm64 image locally with:

```sh
bun run package:dmg --dry-run
open dist/Tap-0.1.0-arm64.dmg
```

The mounted image intentionally contains only `Tap.app` and the `Applications` shortcut. Inside `Tap.app` are bundled Bun, FFmpeg, tmux, native helpers, Kitty WASM, fonts, the stock Ghostty runtime, relocated dylibs, SBOMs, source/build receipts, and `THIRDPARTYNOTICES`. `--dry-run` builds and verifies it locally with an ad-hoc signature under the hardened runtime, exactly as `stuffbucket/macos-builder` will sign it; signed, notarized releases come only from the builder (see `BUILDING.md`). After installation the CLI is `/Applications/Tap.app/Contents/Resources/tap/bin/tap`; `… install-cli` links it into `~/.local/bin` (or `--dir`) so `tap` works from any shell, and the first-run quickstart shows that command until it is done; `tap licenses` prints notices and `tap licenses --path` prints their canonical location.

## Development

```sh
bun run build
bun src/cli.ts doctor
bun src/cli.ts quickstart
bun run check
bun run test:e2e
bun run test:vhs-upstream
bun src/cli.ts --help
```

Architecture and future large-recording/container work are documented in [`docs/architecture.md`](docs/architecture.md). Tap is MIT-licensed (see [`LICENSE`](LICENSE)). Third-party licenses for everything bundled in the DMG are in [`THIRDPARTYNOTICES`](THIRDPARTYNOTICES), also printed by `tap licenses`.
