# Tap 0.1.0

Tap records a terminal session — what you type, what programs print, the terminal's own replies,
resizes, splits and Kitty graphics — as an editable `.tap` timeline, and replays it through Ghostty
for screenshots, high-frame-rate video and an interactive browser player. Nothing is uploaded.

## Download

`Tap-0.1.0-arm64.dmg` (Apple silicon, macOS 26 or later), Developer ID signed and notarized by
Tap's macOS builder, with a matching `.sha256`.

## First run

1. Open the DMG and drag `Tap.app` to Applications.
2. Open Tap. A Terminal window opens that is already recording: everything you type and see is
   saved to `~/Movies/Tap/`. Type `exit` and the recording opens in the browser player, where you
   can scrub, edit and export it. The first launch also runs `tap doctor`, and macOS asks once
   whether Tap may control Terminal.
3. Put `tap` on your PATH:

   ```sh
   /Applications/Tap.app/Contents/Resources/tap/bin/tap install-cli
   ```

## What you can do

```sh
tap capture -o demo.tap -- zsh                   # record a terminal, input and output
tap serve demo.tap                               # browser player/editor at http://127.0.0.1:4377
tap screenshot demo.tap -o demo.png --at 2.5     # any moment as PNG
tap render demo.tap -o demo.mp4 --fps 120        # MP4, WebM, MOV or GIF via FFmpeg
tap ghostty-screenshot demo.tap -o g.png         # rendered by Ghostty's own Metal renderer
tap live -o live.tap -- zsh                      # live capture with right-click tmux splits
tap native-live -o native.tap -- zsh             # live capture with independent PTY splits
tap tape demo.tape -o demo.tap --render-outputs  # run a VHS tape
tap licenses                                     # third-party notices
```

## Highlights

- libghostty-vt 1.0.1 (`vt-1.0.1`) for capture state and portable rendering, Kitty-enabled
  Ghostty WASM for browser playback, and Ghostty's official build of the same commit
  (`108cacb17`) for Metal screenshots and video, rendered at each pane's exact grid.
- Kitty graphics, including animations, normalized from file, temp-file and shared-memory
  transfers; animations play by project time in screenshots, seeking and 30–240 fps export.
- Split capture: concurrent panes from a spec, live tmux splits, or live Tap-native PTY splits
  with reflow on close; per-pane resize, layout, visibility and marker tracks.
- VHS compatibility on Charmbracelet's own parser (0.12.1): VHS's clean shells, xterm.js key
  bytes, the full theme catalogue and presentation controls. 55 upstream tapes plus 35 Bubble Tea
  TUI tapes pass.
- Portable renderer with the bundled JetBrains Mono, correct baselines, italics, and fallback
  fonts for Braille spinners, symbols and CJK.
- Editable, versioned `.tap` JSON with exact base64 PTY bytes; optional input redaction.

## Notes

- Apple silicon only; requires macOS 26.
- Bundles Bun, FFmpeg (GPL build), tmux, Cairo and other libraries; `tap licenses` prints every
  license and the corresponding-source locations.
- Tap is MIT-licensed.
