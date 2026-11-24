# Tap 0.1.0 — Local arm64 Trial

## Artifact

```text
dist/Tap-0.1.0-arm64.dmg
SHA-256: e0ab4303b0886a054e227d85697328f5afb67cd57904e09fdd0fac740cd5c164
```

The DMG shows exactly two visible items: `Tap.app` and the `Applications` shortcut.

## First run

1. Open the DMG.
2. Drag `Tap.app` to Applications.
3. Double-click Tap. It opens Terminal with `doctor` and `quickstart` (macOS asks once whether Tap may control Terminal).

Installed CLI:

```sh
/Applications/Tap.app/Contents/Resources/tap/bin/tap
```

Put it on your PATH (links it into `~/.local/bin`):

```sh
/Applications/Tap.app/Contents/Resources/tap/bin/tap install-cli
```

## Core workflows

```sh
# Record keyboard input, terminal replies, output, resize and graphics
Tap.app/Contents/Resources/tap/bin/tap capture -o demo.tap -- zsh

# Browser playback/editor
Tap.app/Contents/Resources/tap/bin/tap serve demo.tap

# Live browser capture with right-click splits (tmux-owned panes)
Tap.app/Contents/Resources/tap/bin/tap live -o live.tap -- zsh

# Live browser capture with Tap-native independent PTYs
Tap.app/Contents/Resources/tap/bin/tap native-live -o native-live.tap -- zsh

# Screenshots and video
Tap.app/Contents/Resources/tap/bin/tap screenshot demo.tap -o demo.png
Tap.app/Contents/Resources/tap/bin/tap render demo.tap -o demo.mp4 --fps 120

# VHS tapes
Tap.app/Contents/Resources/tap/bin/tap tape demo.tape -o demo.tap --render-outputs

# Licenses
Tap.app/Contents/Resources/tap/bin/tap licenses
```

## Included

- Stable `libghostty-vt` 1.0.1 capture/render state.
- Static and animated Kitty graphics, including normalized file/temp/shared-memory transfers.
- Kitty-enabled browser WASM.
- Portable Cairo compositor and the real Ghostty.app Metal/IOSurface renderer, using Ghostty's signed, notarized build of the same commit as libghostty-vt (108cacb17, vt-1.0.1), rendered at each pane's exact grid.
- FFmpeg, tmux, native helpers, browser assets, fonts and dylib closure.
- Upstream VHS 0.12.1 parser and command compatibility: clean VHS shells, xterm.js key bytes, 55 upstream tapes (including glow, gum, slides, jqp, and the fish/nu/osh/xonsh shells) plus 35 Bubble Tea TUI tapes executed.
- Editable `.tap` JSON with presentation controls, splits, layouts, overlays and input redaction.
- MIT project license and canonical `THIRDPARTYNOTICES` available through `tap licenses`.

## Verification

`bun run verify:dmg` verifies:

- DMG checksum and two-item visible layout.
- Deep/strict app signature.
- Doctor and quickstart.
- Third-party notices.
- PTY capture.
- Portable PNG/video rendering.
- Bundled FFmpeg.
- Ghostty Metal screenshot, and that the bundled Ghostty.app is the pinned commit with its provenance receipt.

The source suite currently passes 26 tests and 75 assertions; `bun run test:e2e` drives both live-capture pages in Chrome.

## Trial caveats

- arm64 only.
- Requires macOS 26.0 because the local native toolchain produced that deployment target.
- Dry-run build: ad-hoc signed with the hardened runtime and the builder's `bun-runtime` entitlements. Developer ID signing and notarization will come from `stuffbucket/macos-builder` (`.macos-builder/config`).
- Intended for local evaluation, not public redistribution.
- Public distribution requires notarization and final legal review of bundled GPL/LGPL corresponding-source materials.
- Tap has no upload or `vhs.charm.sh` publishing path.
