# Building Tap

All commands run from the repository root:

```sh
cd tap
```

## Prerequisites

```sh
brew install cairo libpng ffmpeg mise tmux
mise x bun@1.4.3 -- bun install
```

The macOS full-render path also uses the installed `/Applications/Ghostty.app`.

## Complete application build

```sh
mise x bun@1.4.3 -- bun run build
```

This invokes `scripts/build-all.ts`, which runs:

1. `scripts/build-native.ts`
   - stable `libghostty-vt` native library;
   - native terminal renderer;
   - Kitty-enabled browser WASM;
   - POSIX shared-memory helper.
2. `scripts/build-vhs-parser.ts`
   - upstream VHS 0.12.1 parser sidecar.
3. `scripts/build-ghostty-full.ts` on macOS
   - offscreen stock Ghostty.app CoreText/Metal/IOSurface host.

Generated binaries are written under `native/bin/`; generated dependency trees and WASM are under `.cache/`. Both locations are intentionally ignored by Git.

## Checks

```sh
mise x bun@1.4.3 -- bun run check
mise x bun@1.4.3 -- bun run test:e2e
mise x bun@1.4.3 -- bun run test:vhs-upstream
mise x bun@1.4.3 -- bun src/cli.ts doctor
```

## Third-party notices

```sh
mise x bun@1.4.3 -- bun scripts/generate-notices.ts
mise x bun@1.4.3 -- bun src/cli.ts licenses --path
```

The canonical generated notice is `THIRDPARTYNOTICES`.

## Release builds: stuffbucket/macos-builder

Signed release DMGs are built only by the private `stuffbucket/macos-builder`, like winvm and the other clients. Tap declares what it needs in two files:

- `.macos-builder/config` — `app_path = dist/Tap.app`, bundle id `co.stuffbucket.tap`, the builder's `bun-runtime` entitlements, a `dmg` artifact named `Tap-{version}-arm64.dmg`, and `sign_walk = bun-runtime` (Tap.app nests Ghostty.app's Sparkle.framework, a bundle, so every nested bundle and Mach-O is walk-signed rather than listed).
- `.macos-builder/build.sh` — the producer. It installs the `mise.toml` toolchain (Bun, Zig, Go), runs `bun install --frozen-lockfile` and `bun run build`, then `scripts/build-app.ts "$TAG"`, which builds the unsigned `dist/Tap.app` and refuses a tag that does not match `package.json`. FFmpeg, tmux, Cairo and libpng must be installed with Homebrew on the runner; the producer stops with a clear error if they are not.

The builder then signs, seals, packages, notarizes, staples and checksums. Nothing in this repository uses a Developer ID or Apple credentials. The steps still outside this repository — the builder policy, runner packages and release trigger — are in [`docs/macos-builder.md`](docs/macos-builder.md).

`bun.lock` records packages without registry URLs, so the runner resolves them from npmjs while a machine behind a package proxy uses its proxy; a test fails if a registry URL creeps back in.

## Dry run: build and verify the DMG locally

```sh
mise x bun@1.4.3 -- bun run package:dmg --dry-run
```

Before anything is signed, the dry run proves the DMG the builder will produce actually works. It runs the producer, then repeats the builder's steps from `.macos-builder/config` with the ad-hoc identity `-`: every nested bundle and Mach-O is signed deepest-first with the hardened runtime and the `bun-runtime` entitlements, the app is sealed top-level without `--deep`, and `hdiutil` builds a DMG holding `Tap.app` and an `Applications` link. Only notarization and stapling are skipped. Finally `scripts/verify-dmg.ts` mounts the DMG and runs Tap from it.

`scripts/dmg/bun-runtime.entitlements` is a copy of the builder's profile; when a `macos-builder` checkout sits next to this repository (or `MACOS_BUILDER_DIR` points at one), the dry run fails if the copy has drifted. Without `--dry-run`, `package:dmg` refuses to run and points at the builder.

Outputs:

```text
dist/Tap.app                      the producer's app
dist/Tap-0.1.0-arm64.dmg          ad-hoc signed DMG
dist/Tap-0.1.0-arm64.dmg.sha256   checksum, as the builder writes it
dist/Tap-0.1.0-arm64.json         manifest (dryRun: true, notarized: false)
```

## Verify a DMG

```sh
mise x bun@1.4.3 -- bun run verify:dmg [path/to/Tap-X.Y.Z-arm64.dmg]
```

The same checks run on the dry-run DMG and on the builder's signed DMG. They cover:

- checksum and manifest;
- exactly `Tap.app` and `Applications` in the window;
- deep/strict signature, bundle id, and the hardened runtime on Bun, FFmpeg, tmux, the native helpers and Ghostty;
- version, `doctor`, `quickstart`, and `THIRDPARTYNOTICES`;
- the double-click AppleScript compiling;
- PTY capture, a VHS tape, a portable screenshot and an FFmpeg video;
- a Ghostty Metal screenshot from the bundled Ghostty.app, pinned commit and receipt;
- for a Developer ID signed DMG, also a stapled ticket and Gatekeeper approval.

A public release also needs legal review of corresponding-source materials for bundled GPL/LGPL components.
