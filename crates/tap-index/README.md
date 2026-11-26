# tap-index

Tap's recording index (see
[`docs/editor-design.md`](../../docs/editor-design.md), "The recording
model"): where each command starts and ends in a recording, the transcript
of what was typed and printed, and checkpoints that make seeking constant
time. It builds on [`tap-model`](../tap-model/README.md) for the recording
itself.

Nothing here is published (`publish = false`).

## Building

The crate links libghostty-vt through
[libghostty-rs](https://github.com/Uzaaft/libghostty-rs) (MIT), pinned by
commit to the revision that builds Ghostty `vt-1.0.1` (commit `108cacb17`),
the same libghostty-vt the rest of Tap uses. Its build script clones Ghostty
at that commit and builds it with Zig, so the first build needs network
access and Zig 0.17.1 (pinned in `mise.toml`):

```sh
mise x rust@1.99.0 zig@0.17.1 -- cargo build --release -p tap-index
mise x rust@1.99.0 zig@0.17.1 bun@1.4.3 -- bun run check:rust
```

Only [`src/vt.rs`](src/vt.rs) uses libghostty-rs. It exposes the dozen
operations the index needs (write until ground, screen and cursor state,
pinned cells, plain-text formatting, snapshots), so the engine can later be
swapped for `stuffbucket/libghostty-render`'s Rust binding without touching
the rest of the crate.

The `tap-index` binary is not part of Tap.app: the macOS release builder has
no Rust toolchain yet, and `scripts/build-all.ts` and `scripts/build-app.ts`
do not build it. `tap transcript` runs it from `target/release` or
`target/debug` of a source checkout and says how to build it when it is
missing.

## Command line

```sh
tap transcript session.tap            # via the Bun CLI
tap-index transcript session.tap      # text: one block per command
tap-index transcript --json Session.tap/
tap-index index session.tap           # marks, chunks, inputs and segments as JSON
```

A recording is a `Session.tap/` package directory or a single-file `.tap`
JSON project.

## API

```rust
let session = tap_index::open(path)?;              // package or .tap JSON
let index = tap_index::SessionIndex::build(&session)?;
let transcript = index.transcript();               // Transcript { panes: [PaneTranscript { blocks }] }
let pane = index.pane(&pane_id)?;                  // PaneIndex { chunks, marks, inputs, segments }
let term = index.seek(&pane_id, source_us)?;       // vt::Terminal at that moment
term.screen_text(); term.cursor();
```

## How it works

Each pane's output events are concatenated, in event order, into one byte
stream (an event's `playbackData` when it has one, its `data` otherwise).
Every offset in the index is into that stream. Resize events are applied in
order; input events never reach the terminal, and nothing is written to a
PTY, so the terminal's replies to queries are dropped.

### Marks

The stream is fed through libghostty-vt one escape sequence at a time: plain
text up to the next ESC in one write, then that sequence with
`vt_write_until` until the parser is back at ground (stopping early where an
OSC starts). Everything recorded is attributed to the sequence that caused
it, and each mark records the offset of the sequence's first byte (its ESC)
and the offset just past it.

| Mark | How it is found |
|---|---|
| OSC 133 `P`/`A`, `B`, `C` (with `cmdline_url`), `D` (with status) | libghostty-vt's semantic prompt callback |
| RIS (`ESC c`) | libghostty-vt's reset callback |
| Alternate screen enter and exit | the terminal's active screen, read at every ground stop |
| Clear (ED 2, ED 3) | the bytes of the completed sequence (no state or callback reflects it) |
| Title (OSC 0/2) | the title callback, for naming TUI spans |

### Segments

Top-level segments tile the stream. Each records `boundarySource`, so the
editor can show how sure it is: `osc133`, `enter-key`, `alt-screen`, `clear`,
`idle`, or `start` for the start of a pane.

- **With OSC 133 marks**, a command segment runs from one primary prompt mark
  to the next. A prompt drawn again with no command and no keys in between is
  a redraw, not a new segment. Output before the first prompt is an `output`
  segment; a prompt where no command was started is a `prompt` segment.
- **Without marks**, an Enter typed in the input lane ends a command: a
  carriage return or line feed that is not a terminal reply, not inside a
  bracketed paste and not typed while the alternate screen is active, and
  that the terminal answers with a line feed within a second. A command's
  prompt starts at the last output event before its first key; its output
  starts just past that line feed and ends where the next prompt starts.
- **Inside** a command's output (and inside `output` segments),
  alternate-screen spans are `tui` segments, and the rest is split into
  `paragraph` segments at clears, full resets and idle gaps. Idle gaps of 2 s
  or more anywhere in a segment are `idle` segments (covering no bytes).
- **Ids** are FNV-1a hashes of the pane, the segment kind and the start
  offset (plus the start time for idle gaps), so they survive re-indexing.

zsh prints its partial-line marker (`PROMPT_SP`) before running its prompt
hooks, so with every OSC 133 integration it lands at the end of the previous
command's output. After a TUI it shows up as a paragraph of its own.

### Transcript

A second replay stops at each segment's boundaries:

- **Prompt**: the screen rows from the prompt mark through the cursor, read
  just before the first key is typed.
- **Command**: the OSC 133;C command line; else a small line editor over the
  keys typed at the prompt (printable keys, Backspace, ^U, ^W, bracketed
  paste). History recall, cursor movement, completion and other control keys
  mark it `approximate`; redacted keystrokes show as `•••`.
- **Output**: libghostty-vt's formatter, plain text with soft wraps joined and
  trailing blanks trimmed, as on screen when the command ended. With OSC 133
  it is `select_output` at the cell where output started; otherwise the
  cells from there to the cursor. Output that was cleared before the command
  ended is empty, as are full-screen programs, which are listed by program
  name (from the command line, else the title they set) and duration.
- **Exit code** from OSC 133;D, and **durations**: idle (prompt to first
  key), typing (first key to the command starting) and running (until it
  ended).

### Checkpoints and seek

The second replay also snapshots the terminal (libghostty-vt's snapshot
encoder) at every segment boundary and whenever 256 KiB of output or 10 s of
source time have passed since the last snapshot, always with the parser at
ground. Checkpoints are keyed by `(engine fingerprint, pane, stream offset)`;
the fingerprint names libghostty-vt's version, build metadata and
optimisation mode. (The design keys them by a hash of the byte program up to
the offset; until byte-changing edits are evaluated the byte program is the
raw stream, so the offset alone identifies it.) Each records its exact position (event and bytes into it),
so a resize at the same offset is replayed after it.

The index's terminals keep all scrollback. libghostty-vt prunes history a
page at a time, and a terminal restored from a snapshot lays its pages out
differently, so with any limit a seek and a replay would drop different rows.
The cost is memory: each checkpoint holds the whole history up to it.

`seek(pane, source_us)` decodes the nearest checkpoint at or before the
moment and replays the events after it. The golden tests check that this
gives the same screen text (scrollback included), VT dump of the screen and
scrollback with every formatter extra (styles, modes, palette, cursor style,
scrolling region, tab stops, character sets), cursor, size, active screen and
title as replaying from the start, at hundreds of random and boundary times.
Re-encoded snapshot bytes match too for the recorded sessions; they are not
compared for the synthetic one with many resizes, because libghostty-vt's
snapshot encoding is not canonical once reflow has laid pages out
differently.

Checkpoints are kept in memory. Writing them under
`Session.tap/checkpoints/<fingerprint>/` is not done yet.

## Tests

The fixtures in `tests/fixtures/` are real recordings of `zsh -i` made with
`tap capture` by [`scripts/record-fixture.ts`](../../scripts/record-fixture.ts):
with Tap's shell integration, without it (`--no-shell-integration`), and with
input redacted. Re-record them with `bun scripts/record-fixture.ts` and update
the expectations in `tests/transcript.rs`.
