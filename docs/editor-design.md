# Tap editor: design direction

Status: proposal (2026-11-24). Implemented so far: OSC 133 shell integration at capture; `crates/tap-model` (the `Session.tap/` package, legacy import and export, the EDL and the time map for time-only ops; its [README](../crates/tap-model/README.md) specifies the on-disk format); `crates/tap-index` (marks, segmentation, the transcript, byte programs, printable runs, in-memory checkpoints and seek, and `tap transcript`; its [README](../crates/tap-index/README.md) describes the rules); and `crates/tap-eval` (evaluation of the whole EDL: excise with state fixups, replace text, redact input, mask regions, the secret redaction planner, export of edited sessions, and the `tap-eval` tool; its [README](../crates/tap-eval/README.md) describes the rules). Not yet: typing retiming and speed ramps, thumbnails, rendering, the foreground-process-group lane, and checkpoints on disk. It builds on [`architecture.md`](architecture.md); code references are to the tree at `3418d49`. Requirements: macOS first, with Linux and Windows to follow.

## What we are building

Tap.app should open into an editor that does for terminal recordings what Screen Studio does for screen recordings, using what only a terminal recording gives us. We have the semantic byte stream, not pixels, so we can re-typeset, re-theme, re-time, redact and re-frame a session after the fact. It is never upscaled and never blurry.

The product has three parts:

1. **An automatic first cut that already looks good.** Screen Studio's polish comes from defaults, not tools. Opening a recording should already have long idle time collapsed, typing at an even pace, a sensible look, and a camera that follows the work. Editing is mostly taking things away.
2. **A transcript you edit like a document.** One block per command, Descript-style: delete a block and the video closes up around it. No existing tool maps a command-level transcript onto a timeline. Asciinema editors work on frames or with regular expressions. This is the feature that sets Tap apart.
3. **A timeline with real lanes.** Commands, filmstrip, speed and cuts, camera, keystrokes, redactions and markers, all tied to one playhead shared by the preview, the transcript and the timeline.

## Where we are today

| Area | Today | Consequence |
|---|---|---|
| Editor | Browser page, right-click menu, `<input range>` seek, `prompt()` for resize (`web/app.js`, `web/index.html`) | No timeline, transcript, undo, overlay/presentation UI or export button |
| Edits | Push events into `pane.events` and re-sort (`web/app.js:370-410`) | Destructive; raw capture and edits are mixed together |
| Seek | Forward-incremental; backward kills the renderers and replays from 0 (`src/render.ts:239-246`, `web/app.js:218-226`) | Scrubbing cost grows with session length |
| Native renderer | Global-state process; FRAME returns **PNG** only (`native/renderer.c:804`) | Each frame round-trips PNG encode, pipe, decode and composite |
| Ghostty.app path | New process per frame, bytes fed through a shell and PTY with file polling and a 0.15 s sleep (`native/ghostty_offscreen.m:289-329`) | Fine for exact export; unusable for preview or thumbnails |
| Two previews | ghostty-web is a Canvas 2D renderer; the WASM view skips chrome and overlays | Preview does not match export |
| App shell | `Contents/MacOS/Tap` is a zsh script that opens Terminal.app, then the browser (`scripts/build-app.ts:130`) | No native document, menus or window; macOS-only by construction |

The libghostty-vt we already pin (`vt-1.0.1`) has everything the editor needs, and we use none of it yet:

- **Snapshots**: `ghostty_snapshot_encode*` and `ghostty_snapshot_decoder_*` (`include/ghostty/vt/snapshot.h:418-763`) capture complete terminal state, including unfinished parser input and Kitty images. The format is versioned apart from the ABI, so caches must be keyed by `ghostty_build_info`.
- **Exact byte stops**: `ghostty_terminal_vt_write_until` (`terminal.h:3307`) stops at ground state or at the end of an OSC. That gives each semantic mark an exact byte offset.
- **OSC 133 semantic prompts**: a callback per mark (`GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT`, `terminal.h:2576`) with kind, exit code and command line. Cells and rows are tagged prompt, input or output, and `ghostty_terminal_select_output` (`selection.h:898`) selects a command's output.
- **Text extraction and search**: `ghostty_formatter_*` (`formatter.h`) and `ghostty_search_*` (`search.h`).
- **Tracked grid refs**: `ghostty_terminal_grid_ref_track` (`terminal.h:3639`) keeps a pin that follows a cell through scrolling and reflow. Masks need this.
- **Key encoding** (`key.h`), for recording inside Tap.app.

## Decision: Rust + GPUI, built on herdr-app's terminal renderer

### Options considered

| | Linux + Windows | Preview ceiling | Native feel | Terminal renderer available | Main risk |
|---|---|---|---|---|---|
| **Rust + GPUI** (Zed's UI framework) | Yes: Metal (macOS), wgpu/Vulkan (Linux), DirectX 11 (Windows) | 120 fps. Cap measured 17.5 % CPU on GPUI vs 76.6 % on Tauri | Good. Text input and keyboard navigation need care; gpui-kit covers much of it | **Yes**: herdr-app paints libghostty-vt on GPUI on all three platforms | Framework churn: forks and patches |
| Swift: AppKit + Metal | No; would need a second UI | 120 fps, zero-copy IOSurface | Best on macOS | No; must be written | Mac-only |
| Electron (Screen Studio's choice) | Yes | ~60 fps; native-frame import is experimental | Fair | ghostty-web (Canvas 2D, not Ghostty's output) | Frame bridge; memory |
| Tauri v2 | Yes | Weak: every frame is copied GPU→CPU→WebSocket→GPU | | | Cap abandoned it for this reason |

Linux and Windows decide it. Swift was the fastest path to a polished Mac-only editor, but it would mean writing a second UI later. Two production apps now ship on GPUI:

- **[Cap](https://cap.so/blog/rebuilding-cap-in-gpui)**, a Screen Studio-class recorder and editor. It moved from Tauri to GPUI in October 2026.
- **[herdr-app](https://github.com/herdrdev/herdr-app)** (Apache-2.0), a GPUI terminal app on libghostty-vt that ships for macOS, Linux and Windows.

Between them, they have already solved the two hardest parts of this editor: the editor shell and a cross-platform Ghostty terminal view.

Tap's core should **not** be Ghostty's own renderer (`src/renderer`, `src/font`, ~46k lines of Zig at `vt-1.0.1`). Its back ends are Metal and OpenGL only (`src/renderer/Metal.zig`, `OpenGL.zig`), and Ghostty does not target Windows, so pulling it out would not get us there. The current `ghostty_offscreen.m` stays as an optional macOS-only "Ghostty-identical" export.

### What herdr-app gives us

Checked at `herdrdev/herdr-app@a2e8e3f` (2026-11-24):

| Piece | Where | What it is |
|---|---|---|
| libghostty-vt build | `crates/ghostty-sys/build.rs` (96 lines) | Zig build of `-Demit-lib-vt` for six targets (macOS, Linux and Windows, x64 and arm64), bindgen over `ghostty/vt.h` |
| Safe wrapper | `src/vt.rs` (~3.0k lines) | Terminal, theme, frame extraction (cells, cursor), Kitty placements and BGRA image data, selection, links, search, scrollbar |
| Terminal element | `src/terminal.rs` `paint_grid` and helpers (~lines 1830–2390 of 5.1k) | Paints a frame with GPUI's text system: shaped runs, `paint_glyph`, `paint_quad` for backgrounds, underlines (single, double, dotted, dashed, curly), strikethrough, cursor shapes, fallback fonts |
| Box drawing | `src/sprites.rs` (516 lines) | Pixel-exact block, box and Powerline glyphs |
| Offscreen frames | `vendor/gpui-pre` (patched): `Window::render_to_image` behind `render-to-image` | Renders a window's scene to an RGBA image without presenting it; herdr enables it in production builds |
| Platform shells | `vendor/gpui-pre-{macos,linux,windows,wgpu}`, `vendor/gpui-kit` | GPUI platform back ends and a component kit (shell, inputs) |

Gaps to close:

- **Ghostty pin.** herdr-app pins Ghostty `291fd050b` (2026-10-14), which is **590 commits before `vt-1.0.1`**. `vt.rs` must be ported to the stable 1.0 ABI. That is worthwhile on its own, because 1.0 is the ABI that comes with a stability promise.
- **Zig version.** herdr requires Zig 0.16; Tap pins 0.17.1.
- **Coupling.** `paint_grid` lives inside an app-specific 5k-line file that also handles selection gestures, links, drag paths and mods. It has to be lifted into a self-contained element.
- **Offscreen rendering is tied to a window.** `render_to_image` needs a window that has drawn at its current size and scale (`vendor/gpui-pre/src/window.rs:2789`). Export at an arbitrary resolution needs a headless window/context path. GPUI has `headless_app_context` for tests to build from. This is the main technical risk; the fallback is Cap's approach, a separate wgpu compositor whose output GPUI displays.
- **Fidelity.** herdr's own colour, cursor and decoration rules diverge from Ghostty's. libghostty-render therefore ports Ghostty's logic and keeps only herdr's toolkit painting. What remains different is glyph rasterisation by the host's font stack: CoreText on macOS, DirectWrite on Windows, and the Linux font stack.

### `stuffbucket/libghostty-render`

The renderer is its own library, in a separate repository: `stuffbucket/libghostty-render`. The repository is local only for now. Its `docs/DESIGN.md` and `docs/PLAN.md` are the authority. In summary:

- **Core.** A Zig library with a C ABI, written in libghostty-vt's style. It turns a libghostty-vt render state into a deterministic *frame plan*: cell metrics, background spans, text runs to shape, decorations, cursor, highlights, Kitty image order and A8 sprite masks.
  - The logic is ported from Ghostty's own MIT renderer and font code, against the public vt API only, so output matches Ghostty.app apart from glyph rasterisation.
  - Naming follows libghostty's scheme with its own root word, so it can never collide with an official library: `<gtr/render.h>`, `gtr_<area>_<verb>`, `Gtr<Type>`, tags `render-X.Y.Z`.
  - Licensed MIT, like libghostty-vt.
- **Bindings.** Rust lives under `bindings/rust/`: a `-sys` crate, a safe wrapper and a GPUI element derived from herdr-app. That binding is Apache-2.0, because of herdr-app and GPUI. Swift, Qt and wasm bindings can follow.
- **No upstream replacement yet.** As of 2026-11-24 nothing official replaces it. libghostty ships only VT, and its stated renderer plan is surface-based GPU rendering. `vt-1.0.2` has identical headers.
- **Planned retirement.** When libghostty ships a renderer: separate namespace, Ghostty config-key option names, and bindings that hide the backend. Then deprecate or wrap.
- **Nothing published.** Nothing goes to crates.io or any other registry until the owner decides.

### Process and module layout

```text
Tap (Rust, GPUI app)
├─ UI: preview, timeline, transcript, inspector (GPUI + gpui-kit)
├─ tap-model      package format, EDL, undo stack, project validation
├─ tap-eval       EDL → byte program + time map (the only implementation)
├─ tap-index      segmentation, marks, runs, checkpoints, thumbnail cache
├─ tap-compose    one scene builder: background, padding, corners, shadow, window bar,
│                 camera, overlays, keystroke HUD. Used by preview, thumbnails and export
├─ tap-export     offscreen frames → FFmpeg (motion blur by sub-frame accumulation)
├─ tap-capture    PTY capture (portable-pty: ConPTY on Windows), shell integration, splits
└─ libghostty-render (C core + Rust binding + GPUI element; separate repository)

tap CLI (existing Bun)  during migration; drives the Rust binaries for render/export.
                        VHS stays here until scheduling moves to Rust.
ghostty-exact (existing ObjC helper)  optional macOS-only Ghostty-identical export
```

Two rules keep this from getting worse than what we have:

- **One scene builder for preview, thumbnails and export.** Today there are two previews, neither matches export, and the timing and layout maths is copied between `src/render.ts:103-118` and `web/app.js:126-132`. The EDL evaluator and compositor exist exactly once, in Rust. Camera zoom re-lays out the terminal at a larger cell size instead of scaling pixels, so it stays sharp.
- **Bun is retired gradually, not all at once.** Capture moves first, because Windows needs ConPTY and herdr already uses portable-pty. Render and export move next, and the Cairo path in `src/render.ts` and `native/renderer.c` is deleted once Rust export matches it. VHS moves last.

## The recording model

### Package format

This replaces the single JSON file with the layout `architecture.md` already plans:

```text
Session.tap/
  manifest.json            format version, engine fingerprint, panes, canvas
  events/<pane>.tlog       immutable raw events (chunked, checksummed); never edited
  edits.json               the edit decision list (EDL), presentation, camera, overlays
  index/<pane>.idx         derived: chunks, marks, runs, inputs, segments (rebuildable)
  checkpoints/<fp>/...     derived: terminal snapshots (rebuildable, disposable)
  thumbs/...               derived: thumbnail tiles (rebuildable, disposable)
```

- Autosave writes `edits.json` atomically.
- Raw events are authoritative and never change. Everything under `index/`, `checkpoints/` and `thumbs/` can be deleted and rebuilt.
- Old `.tap` JSON imports by splitting into `events` and an `edits.json` with an empty edit list (presentation and overlays move there).
- `crates/tap-model/README.md` gives the exact byte layout of `.tlog` and the versioning rules.
- On macOS the package is registered as a document bundle; on Linux and Windows it is a plain directory.

### Segmentation: turning bytes into commands

Each segment records where its boundary came from, so the UI can show confidence.

1. **OSC 133 marks (preferred).** At capture, Tap injects a minimal shell shim using the same techniques Ghostty and VS Code use:
   - zsh: a `ZDOTDIR` shim that restores the user's own `ZDOTDIR`;
   - bash: rcfile/`ENV` (macOS `/bin/bash` 3.2 cannot be injected);
   - fish ≥ 4: emits 133 natively;
   - PowerShell: a prompt-function wrapper, as VS Code does.

   The shim emits **only** 133 marks: no title, cursor-shape or ssh features. It uses `133;P`, not `A`, because `A` forces a fresh line and changes the screen (`Terminal.zig:2784-2905`). It stays out if the stream already carries 133, which happens because we set `TERM_PROGRAM=ghostty` (`src/capture.ts:105`). It drops `cmdline_url` unless `inputPolicy` is `record`, otherwise redacted input would leak through output.
2. **Foreground process group** (`tcgetpgrp` on the PTY master on Unix; the console process list on Windows), sampled into a `job` lane. Works with any shell and needs no changes to the shell.
3. **Enter in the input lane**, ignored on the alternate screen or inside bracketed paste, then checked against a prompt fingerprint.
4. **Alternate-screen enter and exit** → a TUI span.
5. **Clear and RIS** → hard visual boundary.
6. **Idle gaps of 2 s or more** → soft "paragraph" breaks inside long output.

VHS recordings write exact `marker` events from the scheduler (`src/tape.ts`).

### Transcript

The tree is session → command block → {output paragraphs, TUI spans, idle gaps}. A command block shows:

- **Prompt**: prompt-tagged cells, plus cwd from OSC 7.
- **Command text**, from the first available source:
  1. the 133 command line;
  2. input-tagged cells when output starts;
  3. a line-editor simulation over the input lane, marked *approximate* when history recall makes it uncertain. Redacted input shows as `•••`.
- **Output**: formatter text of `select_output` at block end, so progress bars show their final state. An `animated` badge marks output that was redrawn in place.
- **Exit badge** (red when non-zero, as Warp does). Duration shown as *source* → *edited*.
- **TUI spans** (vim, htop, less) show a thumbnail strip, program name and duration instead of text. The final screen is available as read-only text.

Idle gaps between blocks are editable objects, like Descript's word gaps: drag to shorten, or right-click → *Collapse*.

### Edits: a non-destructive EDL

Every op is anchored to **source coordinates** (pane plus byte offset, or source µs), never to edited time. Byte offsets count the pane's playback stream (`playbackData` where capture normalised Kitty file media, `data` otherwise), the bytes that are replayed and indexed; an edit that would splice bytes whose recorded and playback forms differ is refused. Ops therefore commute, and re-segmenting does not break them.

```rust
enum EditOp {
    // time-only: always exact, never change the bytes, never invalidate checkpoints
    Trim         { r#in: Src, out: Src },
    Skip         { from: Src, to: Src },                  // jump cut: bytes apply instantly
    CollapseIdle { range: Option<R>, threshold_us: u64, max_us: u64 },
    Speed        { range: R, rate: Rate },                // constant or ramp
    RetimeTyping { segment: SegId, cadence_us: u64, jitter_seed: Option<u64> },
    Hold         { at: Src, duration_us: u64 },
    // byte-changing: produce a new byte program; verified on apply
    Excise       { segment: SegId },                      // the command never happened
    ReplaceText  { pane: PaneId, bytes: Range<u64>, text: String, expect: String },
    RedactInput  { pane: PaneId, events: Range<usize> },
    // render-time: never touch bytes
    Mask         { pane: PaneId, anchor: TrackedRef, cells: u32, style: MaskStyle },
}
```

Evaluation produces two things:

- **Byte program**: raw bytes plus excisions, replacements and splice fixups.
- **Time map**: a monotone, piecewise-linear map from source time to edited time. Kitty animation ticks and overlays go through the same map; today they tick on raw time (`src/render.ts:46`).

**Cut has two meanings, and the UI names the one it uses:**

- **Skip (jump cut)**: always exact. The skipped bytes apply instantly, but their output stays in scrollback.
- **Excise (never happened)**: allowed only at clean boundaries:
  - prompt mark to prompt mark (or the end of the output);
  - parser at ground;
  - primary screen;
  - no Kitty image transmitted inside and used later;
  - nothing after it positions the cursor absolutely on the primary screen before a clear;
  - no change to state libghostty-vt cannot read back that matters later (a saved cursor or saved modes restored later, an unbalanced Kitty keyboard stack).

  State the block left changed (modes, Kitty keyboard flags, palette and dynamic colours, title, cwd, character sets, scrolling region, tab stops, cursor shape, pen style) is diffed and written back as fixup bytes at the splice, and the state is read again to check them. If any condition fails, the edit becomes a skip and the result lists every reason. Deleting a block in the transcript tries excise first (`tap_eval::plan_delete`).

**Editing text inside output** is applied in this order:

1. Find the text via grid search, then map cells back to byte ranges through the index.
2. If the replacement is the same width and the text was drawn as a plain run, rewrite only the printable bytes and keep the SGR between them. Then refold and check that the formatter text equals `expect`.
3. Otherwise fall back to a render-time mask anchored to a tracked grid ref. This works for TUIs, but the secret stays in the raw bytes, and *Share .tap* warns about it.

For secrets, the input keystrokes are also redacted.

### Seek, checkpoints and thumbnails

**Checkpoints**:
- Taken at every segment boundary and every 256 KiB or 10 s, always at ground.
- Keyed by `(engine fingerprint, pane, hash of byte program up to that offset)`.
- Time-only edits never change the byte program, so retiming never invalidates a checkpoint or a thumbnail.

**Live terminals**: three per pane (playhead, scrub-ahead, thumbnail worker), plus an LRU of decoded snapshots in memory and on disk. A seek is one decode plus at most one checkpoint interval of folding. In practice that is constant time.

**Filmstrip**: at low zoom, terminal frames all look alike, which is why neither Screen Studio nor Cap draws one. Tap uses levels of detail instead:

| Zoom | Lane shows |
|---|---|
| Whole session | Command chips (coloured by exit) + an output-activity heat strip + a 1-pixel-per-cell minimap drawn from cell colours |
| Mid | Keyframe thumbnails: each command's first frame and its *result* frame (last frame before the next prompt); TUI enter, middle and exit |
| Close | Uniform tiles at power-of-two spacing, so zooming reuses tiles; a tile is reused when nothing changed between samples |

- Tile key: `(engine fp, byte-program prefix hash, source byte offset, presentation hash, size)`.
- Tiles render off the UI thread through the offscreen renderer at 1× and are then downscaled, because rendering terminals at small sizes breaks font fitting.
- A lower-resolution tile is shown immediately while the exact one renders.
- Hovering the timeline scrubs the preview, Final Cut skimming-style.

### The index (transcript ↔ timeline ↔ bytes)

Each pane has sorted arrays: `chunks` (event → byte range), `marks`, `runs` (printable runs with tracked start and end pins), `inputs` (keystroke → echo bytes) and `segments`. Lookups are binary searches. Runs and masks use an interval tree. The full chain is:

transcript char → cell → run → byte → source µs → edited µs

A cut region maps to the instant of the cut. The same index drives transcript selection ↔ playhead sync, search, redaction and snapping.

## The editor

```text
┌───────────────────────────────────────────────────────────────────────────────┐
│ ● ● ●  Session — 4:12 → 1:38          [Auto-polish ✓]  [Look ▾]  [Export]     │
├──────────────────────┬─────────────────────────────────────┬──────────────────┤
│ TRANSCRIPT           │                                     │ INSPECTOR        │
│ ~/tap  ⏎ 0:02        │         ┌───────────────────┐       │ (context: block, │
│ $ bun run build  ✓   │         │ ● ● ●             │       │  zoom region,    │
│   ▸ 214 lines        │         │ $ bun run build   │       │  range, project) │
│ ·· idle 41s → 1.0s ··│         │ ...               │       │                  │
│ $ git push       ✗ 1 │         └───────────────────┘       │ Background       │
│   ! rejected ...     │      background · padding · shadow  │ Padding / Inset  │
│ $ vim model.ts  TUI  │                                     │ Corners / Shadow │
│   [▮▮▮▮] 1:12        │         ◀◀  ▶  ▶▶   1:02.4 / 1:38   │ Font / Theme     │
├──────────────────────┴─────────────────────────────────────┴──────────────────┤
│ Commands  [build ✓][  push ✗ ][ vim ▮▮▮▮▮▮▮▮ ]                                  │
│ Frames    ▯▯▯ ▯▯▯▯▯ ▯▯▯▯▯▯▯▯▯▯▯▯                                                │
│ Speed/Cut ━━━━━━✂━━━━━━━━ 2× ━━━━━━━━━━━━━                                       │
│ Camera        ▭▭▭▭ 1.6×           ▭▭▭▭▭▭ follow                                 │
│ Keys      ⌘K  ⌃C            :wq                                                │
│ Redact                ███                                                      │
│                    ▲ playhead                                                  │
└───────────────────────────────────────────────────────────────────────────────┘
```

### Interaction rules

- **One playhead everywhere.** Selecting in the transcript selects the time range on the timeline and moves the preview. Scrubbing highlights the current block and line. Selection is a source-coordinate range shared by all three views.
- **Transcript editing follows Descript.**
  - **Delete**: excise, else skip; ripples.
  - **Ignore**: strikethrough; skipped on playback but still visible.
  - **Restore**: a small marker at each cut that you click to undo it (Screen Studio's scissors bubble).
  - Gaps are draggable objects.
- **Timeline**:
  - Click an empty part of a lane to create a region; drag edges to resize; right-click → Disable / Remove. Disable is always offered next to Delete.
  - Snapping to command boundaries, markers, the playhead and cuts; `S` toggles it.
  - `C` cuts at the playhead.
  - Ripple by default; `Alt`-drag lifts, leaving a gap.
- **Keyboard**:
  - `J`/`K`/`L` shuttle;
  - `,`/`.` step one output event (our "frame");
  - `[`/`]` previous/next command;
  - `Cmd/Ctrl+K` command palette for every setting and action;
  - full undo and redo from the app's own command stack, one entry per gesture (GPUI has no system undo manager).
- **Animation**: every transition (camera, layout, aspect change) runs on a spring. Motion blur is velocity-based sub-frame accumulation in export, and a cheaper approximation in preview.

### Auto-polish: the automatic first cut

These are on by default when a recording opens, and each one is a visible, toggleable entry in the EDL:

- collapse idle over 1.5 s to 0.5 s; keep the pause before Enter;
- even typing pace with small seeded jitter, anchored to echo output so it stays in sync;
- 2× through output floods and spinner waits (separate speeds for typing, waiting and flood);
- trim the shell start-up and the final `exit`;
- camera follows the cursor or changed region while typing, frames the command's output when it lands, and backs out during scroll floods. The windows are padded and merged as in Cap, with typing taking the place of clicks;
- a default Look preset (background, padding, corners, shadow, window bar).

### Compositing (Inspector → Look)

- Background: gradient, wallpaper or image.
- Padding, inset, corner radius, shadow (angle, distance, softness, colour).
- Window bar styles (existing `windowBar` values).
- Font family and size, line height, theme; the theme can be swapped after recording.
- Aspect presets (16:9, 1:1, 9:16). These **re-render the grid** for line-oriented output and letterbox TUIs.
- Keystroke HUD (filters plain typing, like Screen Studio's shortcut lane).
- Text and image overlays, in canvas-normalised coordinates (today they are absolute pixels and do not scale with `--scale`, `src/render.ts:71-90`).
- Presets save and share the whole Look.

### Recording from Tap.app

herdr-app is already a live terminal app on the same stack, so in-app recording becomes early work instead of the last phase:

- A Tap recording window is a live `TerminalElement` over a `tap-capture` PTY.
- The same window offers split and close, which removes the tmux and Terminal.app/AppleScript paths.
- On exit, the window turns into the editor for that document.

This is also what makes Windows possible: there is no Terminal.app there.

## Plan

Each phase ships something usable and leaves the codebase in a coherent state.

| Phase | Delivers | Key work |
|---|---|---|
| **0: Core** | Instant seek and a transcript, testable headless | **libghostty-render** (its own PR series). **In Tap:** OSC 133 shell integration at capture; `Session.tap/` package format and import of old `.tap` files; `tap-model` (EDL) and `tap-eval` (byte program and time map); `tap-index` (segmentation, checkpoints) and `tap transcript`. Golden tests: snapshot-seek equals replay-from-0. |
| **1: Editor shell** | Tap.app opens documents on macOS | GPUI app; preview through `tap-compose`; timeline with Commands lane, heat strip and playhead; transcript; Look inspector; `tap render` → Rust export |
| **2: Editing + in-app recording** | Record and edit without leaving Tap | Trim, skip, excise, collapse idle, speed, retime typing; ripple and lift; undo; restore markers; Auto-polish. Recording window on `tap-capture` with shell integration and splits. |
| **3: Compositing** | Screen Studio parity where it matters | Camera lane plus auto-follow; springs and motion blur; aspect re-flow; keystroke HUD; redaction (replace, else mask); LOD filmstrip; presets; command palette |
| **4: Linux + Windows** | Same app on all three | ConPTY capture, PowerShell integration, packaging (AppImage/deb, MSI); retire Bun render path and `native/renderer.c` |

Linux and Windows builds should run in CI from phase 0. herdr-app shows the toolchain works, and building on all three early keeps macOS-only assumptions out of the code.

## Decisions

1. **Rust + GPUI** (`gpui-pre`/`gpui-kit`, as herdr-app uses), because Linux and Windows are wanted.
2. **The renderer** is `stuffbucket/libghostty-render`: a MIT Zig/C-ABI core ported from Ghostty, with an Apache-2.0 Rust/GPUI binding derived from herdr-app. It is credited to herdr, Ghostty and Zed, and nothing is published yet.
3. **Fidelity:** "very close to Ghostty" is the target for preview and default export. Ghostty's logic is ported; only glyph rasterisation differs. The macOS-only `ghostty-exact` export stays as a pixel-exact option.
4. **Shell integration** is on by default at capture, with `--no-shell-integration` to opt out. The shim emits only OSC 133 marks using `133;P`, stays out when the stream already carries 133, and drops `cmdline_url` unless `inputPolicy` is `record`.
