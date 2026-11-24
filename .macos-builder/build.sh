#!/usr/bin/env bash
set -euo pipefail

# Tap's producer for stuffbucket/macos-builder. It builds the unsigned Tap.app at the config's
# app_path: the Bun runtime and CLI, the native libghostty-vt renderer and helpers, the VHS
# parser, FFmpeg and tmux with their dylibs, and Ghostty's official build of the pinned
# libghostty-vt commit. The builder then signs every nested bundle and Mach-O (sign_walk), seals
# the app, packages the DMG, notarizes, staples and checksums.
#
# The producer runs with the signing keychain locked and SIGN_IDENTITY="-".
# Bun, Zig and Go come from mise.toml; FFmpeg, tmux, Cairo and libpng from Homebrew.
#
# Builder-supplied env consumed: TAG, ARCH.

export PATH="$HOME/.local/bin:/opt/homebrew/bin:$PATH"

VERSION="${TAG#v}"
MISE_VERSION="v2026.9.14"
MISE="$HOME/.local/bin/mise"

echo "Producing Tap.app for ${TAG} (version ${VERSION}, ${ARCH:-arm64})"

missing=()
for formula in ffmpeg tmux cairo libpng; do
  [ -d "/opt/homebrew/opt/${formula}" ] || missing+=("$formula")
done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "::error::missing Homebrew formulae on the runner: ${missing[*]} (brew install ${missing[*]})" >&2
  exit 1
fi

mkdir -p "$HOME/.local/bin"
if [ ! -x "$MISE" ] || [ "$("$MISE" --version | cut -d' ' -f1)" != "${MISE_VERSION#v}" ]; then
  curl -fsSL https://mise.run | MISE_VERSION="$MISE_VERSION" MISE_INSTALL_PATH="$MISE" sh
fi
"$MISE" trust -q mise.toml
"$MISE" install
"$MISE" exec -- bun install --frozen-lockfile

# Native renderer, VHS parser, pinned Ghostty.app and its offscreen host, then the bundle.
# build-app.ts refuses a tag that does not match package.json's version.
"$MISE" exec -- bun run build
"$MISE" exec -- bun scripts/build-app.ts "$TAG"
