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

**Scrollback.** Seek returns what a Ghostty window would have shown, so the
terminals behind checkpoints, seek and `replay_from_start` keep Ghostty's
default scrollback: 50,000,000 bytes of history pages, the default
`scrollback-limit-bytes` in `src/config/Config.zig` at the commit
libghostty-vt is built from (`vt::GHOSTTY_SCROLLBACK_BYTES`;
`IndexOptions` can change it). libghostty-vt drops the oldest page of
history whenever a new page would take it over that. The transcript's
replay keeps all history, so a command's output is read whole however long
it is; the mark scan reads no history and keeps Ghostty's default.

With all history kept, every checkpoint held the whole session so far:
memory grew with the square of the session's length, and every seek
decoded all of it.

**Why a restore under a limit can differ.** A snapshot holds every page,
but two things it does not reproduce decide which old rows a limit drops
(`tests/scrollback.rs` reproduces both):

1. libghostty-vt enforces the limit lazily. A column resize reflows history
   into new pages (a wider screen needs more of them) and a page that
   outgrows its pool item moves to a larger allocation, and neither drops
   anything: the next page the screen grows does. The snapshot decoder
   refuses every page of history that would take the restored screen over
   the limit, so a terminal restored while over it comes back with less.
   Every replay the index makes (indexing, seek, `replay_from_start`)
   therefore applies the limit at the end of every event, as libghostty-vt
   would at its next page, and a checkpoint taken inside an event while
   over the limit is kept only if its restore brings back every page.
2. How much room a page has for styles and hyperlinks is not in the
   snapshot. The decoder rebuilds each page's allocation from size hints,
   so a page with a large hyperlink table can come back larger and push the
   oldest page out, and a new page's room depends on what the last few
   pages used, so after a restore pages fill at different rows. With output
   like that, the limit drops old pages at different moments after a seek
   than in a replay.

**What seek guarantees.** `seek(pane, t)` and `replay_from_start(pane, t)`
have the same size, cursor, active screen and title, and:

- when they keep the same number of rows, the same screen text and the
  same VT dump of every row with every formatter extra (styles, modes,
  palette, cursor style, scrolling region, tab stops, character sets);
- otherwise the one that kept fewer rows is the newest part of the other:
  its rows after its oldest line are the other's newest rows (screen text
  and VT dump identical), and its text, soft wraps joined, ends the other's.
  Only its oldest line is exempt, because pruning may have cut that line's
  start and a later resize rewrapped what was left.

The screen and everything but the oldest history are always exact. The
golden tests (`tests/seek.rs`) check this at hundreds of random times, every
event time and every checkpoint time, on the recorded sessions, two panes, a
Kitty animation and a two-minute synthetic session (characters and sequences
split across events, resizes, the alternate screen, clears, a reset, a
600 KiB event). They require exact equality where nothing is pruned, and
also for the synthetic session under a 2 MB limit, which prunes almost all
of it while resizes reflow it; only its variant with a hyperlink on every
line keeps different amounts of history. Re-encoded snapshot bytes match for
the recorded sessions; they are not compared for the synthetic one, because
libghostty-vt's snapshot encoding is not canonical once reflow has laid
pages out differently.

**When checkpoints are taken.** At every segment boundary, and whenever
256 KiB of output or 10 s of source time have passed since the last one,
always with the parser at ground. Checkpoints are keyed by `(engine
fingerprint, pane, stream offset)`; the fingerprint names libghostty-vt's
version, build metadata and optimisation mode, and the scrollback limit.
(The design keys them by a hash of the byte program up to the offset; until
byte-changing edits are evaluated the byte program is the raw stream, so the
offset alone identifies it.) Each records its exact position (event and
bytes into it), so a resize at the same offset is replayed after it.

`seek(pane, source_us)` decodes the nearest checkpoint at or before the
moment, at most the scrollback limit's worth of pages, and replays the
events after it.

Checkpoints are kept in memory. Writing them under
`Session.tap/checkpoints/<fingerprint>/` is not done yet.

## Tests

The fixtures in `tests/fixtures/` are real recordings of `zsh -i` made with
`tap capture` by [`scripts/record-fixture.ts`](../../scripts/record-fixture.ts):
with Tap's shell integration, without it (`--no-shell-integration`), and with
input redacted. Re-record them with `bun scripts/record-fixture.ts` and update
the expectations in `tests/transcript.rs`.

`tests/seek.rs` holds the golden seek tests, and `tests/scrollback.rs` the
two ways a restore differs under a limit.
