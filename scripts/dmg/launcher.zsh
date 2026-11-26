#!/bin/zsh
# Tap.app's executable. With arguments it is the CLI; opened from Finder, it opens a Terminal
# window that is recording (`tap launch`).
ROOT="${0:A:h:h}/Resources/tap"
args=("${@:#-psn_*}") # older macOS passes a process serial number when launching from Finder
if (( ${#args} > 0 )); then exec "$ROOT/bin/tap" "${args[@]}"; fi
exec /usr/bin/osascript "$ROOT/welcome.applescript" "$ROOT/bin/tap"
