# Onboarding Tap to stuffbucket/macos-builder

Tap's side is in place: `.macos-builder/config`, the producer `.macos-builder/build.sh`, and a
local mirror of the builder's tail (`bun run package:dmg --dry-run`). Two things live outside this
repository.

## 1. Build policy (macos-builder issue form)

Open a **Build config** issue in `stuffbucket/macos-builder` with these answers, then apply the
`approved` label:

| Form field | Value |
| --- | --- |
| Client repo (owner/name) | `stuffbucket/tap` |
| Bundle identifier | `co.stuffbucket.tap` |
| Allowed entitlements profiles | `bun-runtime` |
| Allowed artifact types | `dmg` |
| Volume / display name | `Tap` |
| App path within the checkout | `dist/Tap.app` |
| Allowed nested-signing map | *(leave empty — Tap uses `sign_walk`)* |
| Notes / justification | Bun/JavaScriptCore CLI that also `dlopen`s Ghostty.app (signed by Ghostty's Developer ID) and loads bundled Homebrew dylibs and Node addons, so it needs `bun-runtime` (JIT, unsigned executable memory, disable-library-validation). Tap.app nests Ghostty.app's Sparkle.framework, a bundle, so it signs with `sign_walk = bun-runtime` rather than a `sign_nested` list. |

The policy the workflow should write to `clients/stuffbucket/tap.policy`:

```text
bundle_id_allowed = co.stuffbucket.tap
entitlements_allowed = bun-runtime
artifact_allowed = dmg
volname_allowed = Tap
app_path_allowed = dist/Tap.app
sign_nested_allowed =
```

`sign_walk` needs no policy key of its own: the builder bounds its profile by
`entitlements_allowed`.

## 2. Runner and trigger

- The runner needs Homebrew `ffmpeg`, `tmux`, `cairo` and `libpng`; `build.sh` stops with a clear
  error naming any that are missing. Bun, Zig and Go come from `mise.toml`.
- The producer downloads Ghostty's official build of the pinned commit from
  `tip.files.ghostty.org`, and Bun and Go modules from their registries.
- Wire the release trigger as for the other clients (`examples/client-dispatch.yml` in
  macos-builder, with the `MACOS_BUILDER_PAT` secret and the app-repoman installation on this repo).

## Before tagging

```sh
bun run package:dmg --dry-run
```

Then tag `vX.Y.Z` matching `package.json`'s version; `build.sh` refuses a mismatched tag. After the
builder uploads the signed DMG, `bun run verify:dmg path/to/Tap-X.Y.Z-arm64.dmg` checks it too,
including the stapled ticket and Gatekeeper approval.
