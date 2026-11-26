# tap-eval

Tap's edit evaluator (see
[`docs/editor-design.md`](../../docs/editor-design.md), "Edits: a
non-destructive EDL"): it turns an edit decision list into what each pane
plays, when, and what to cover. It builds on
[`tap-model`](../tap-model/README.md) for the recording, the edit list and
the time map, and on [`tap-index`](../tap-index/README.md) for marks,
segments, printable runs, replays and the terminal engine (only tap-index's
`vt.rs` touches libghostty-vt).

Nothing here is published (`publish = false`).

## Why a crate of its own

tap-index has to *consume* an edited recording (its index, checkpoints,
seek and transcript are built over the byte program), so the program type
(`tap_index::PaneProgram`) lives there and tap-index cannot depend on the
evaluator. Deciding what the edit list means needs the index of the raw
recording (marks, segments, runs) and replays, so the evaluator depends on
tap-index. The design's module layout already names `tap-eval` as the one
implementation of EDL evaluation.

## Command line

```sh
mise x rust@1.99.0 zig@0.17.1 -- cargo build --release -p tap-eval
tap-eval preview session.tap [--edits edits.json] [--json]   # what each edit did + the edited transcript
tap-eval apply session.tap --edits edits.json --out Edited.tap/
tap-eval plan-delete session.tap main 104 623   # excise, or skip and why
tap-eval redact session.tap sk-live-4f9a2b      # planned edits + what remains
```

`--edits` takes an `edits.json` document or a JSON array of ops; without it
the recording's own list is used. Disabled entries are skipped by evaluation
and kept exactly as given by every save and export, `apply` included. `apply` writes a package when edits remain
(time edits, masks, the skips excisions leave), since the `.tap` JSON form
holds no edits. `preview` with no edits prints exactly `tap-index
transcript`'s output after a duration line; `tap transcript` is unchanged.

## API

```rust
let session = tap_index::open(path)?;
let raw = tap_index::SessionIndex::build(&session)?;          // the unedited index
let evaluation = tap_eval::evaluate(&session, &raw)?;        // or evaluate_entries(.., &entries)
evaluation.programs;   // BTreeMap<PaneId, PaneProgram>: splices over each raw stream
evaluation.time_map;   // tap_model::TimeMap, including excisions' cuts
evaluation.outcomes;   // one Outcome per entry: Excised, Refused, Replaced, NeedsMask, …
evaluation.masks;      // MaskRegion: cells to paint, per span of source time
let edited = evaluation.index(&session, IndexOptions::default())?;  // transcript, seek
tap_eval::plan_delete(&session, &raw, &seg_id)?;            // DeletePlan::Excise | Skip { reasons }
tap_eval::redact::plan(&session, &raw, "secret")?;          // edits + report
tap_eval::bake::bake(&session, &raw, &entries, &evaluation); // the edited session, for export
```

`evaluate` fails only with `EvalError::Unsupported` (typing retiming and
speed ramps, each listed), `Invalid` (an op out of range or naming no pane),
a time-map error, or an engine error. Everything else is an `Outcome`.

## The byte program

A pane's program is an ordered list of splices over its raw stream, each
removing a non-empty source range and playing other bytes in its place
(`tap-index`'s README, "Byte programs", has the mapping and event rules).
Raw events are never changed. Excisions splice in their fixups, attached to
the first byte after the range; replacements splice in their text, attached
to the last byte they replace. Withheld and dropped input events are part of
the program too.

## Excise

An excision removes a prompt-to-prompt range "as if it never happened". It
is exact only when all of these hold; `Outcome::Refused` lists every one
that fails, and the edit then plays as a skip (its time cut, its bytes
applied), which is always exact. Either way the range's time is cut.

1. **Prompt marks**: it starts at a primary OSC 133 prompt mark (`P` or
   `A`) and ends at one or at the end of the pane's output.
2. **Ground**: the parser is at ground just before both ends (checked by
   replaying to them).
3. **Primary screen**: not on the alternate screen at the start, and none
   entered inside.
4. **Kitty images**: no image transmitted inside (`a=t`/`a=T` with `i=` or
   `I=`) is used after it (any later graphics command naming it, or a
   Unicode placeholder), and nothing inside deletes images if graphics are
   used after it.
5. **No absolute positioning after it before a clear**: without the
   command's rows the cursor is higher than it was, so CUP, HVP, VPA,
   DECSTBM, DECSLRM, DECRC, SCORC, DECOM or DECALN on the primary screen
   would land elsewhere. The scan runs from the end of the range to the
   first *clear*: ED 2 next to a CUP/HVP with nothing printed between (as
   `clear` writes it), or RIS. Positioning while the alternate screen is
   active does not count. (This is stricter than "the next block": any later
   block before a clear would be wrong too.)
6. **Readable state**: see below.
7. **Fixups exist and work**: see below.
8. **No overlap** with an earlier excision.
9. **Recorded bytes are what plays**: the range crosses no output event
   whose `data` differs from its `playbackData` (a Kitty file transfer
   normalised at capture). Byte coordinates count the playback stream, so
   splicing there would leave the recorded bytes saying something else.
   ReplaceText (`NeedsMask::PlaybackDiffers`) and mask anchors are refused
   there too.

**State carried across the splice.** The state the original reaches at the
end of the range is diffed against the state the edited program has where
the splice plays (after earlier splices, and after any resize inside the
range), and the difference is written as fixup bytes:

| State | Read with | Fixup |
|---|---|---|
| Every mode libghostty-vt recognises (ANSI and DEC private) | `mode` getter | `CSI [?] n h/l` |
| Palette (256) | palette getters | `OSC 4` / `OSC 104` |
| OSC 10–12 colours | colour getters | `OSC 10–12` / `OSC 110–112` |
| Kitty keyboard flags (top of stack) | getter | `CSI = flags ; 1 u` |
| Title | getter | `OSC 2` |
| Working directory | getter | `OSC 7` |
| Pointer shape | getter | `OSC 22` |
| Cursor shape and blink | render state | `DECSCUSR` |
| Character sets (G0–G3, GL, GR) | formatter extra | designations, `SI`/`SO`/`LS2`/`LS3`/`LS1R`/`LS2R`/`LS3R` |
| Scrolling region | formatter extra | `DECSTBM` (+ `DECSLRM`) |
| Tab stops | formatter extra | `TBC 3`, `HTS` per stop |
| modifyOtherKeys | formatter extra | `CSI > 4 m` + the formatter's |
| SGR, hyperlink and protection new text gets | formatter extras | `SGR 0` / `OSC 8` / `DECSCA` + the formatter's |

Restoring the scrolling region or tab stops moves the cursor, so a CUP puts
it back. The fixups are then played and the state read again; any
difference refuses the excision (`FixupMismatch`) and so does state with no
sequence to restore it (`Unfixable`): 132-column mode (DECCOLM clears the
screen), a character set back to UTF-8 (only a reset does that), a title
or working directory with control characters, an unknown pointer shape.

**What cannot be read** (`Unreadable`), and when it refuses: the cursor
saved by DECSC/SCOSC/mode 1048 or reset by DECSTR, when something after the
range restores it before saving again; private modes saved by XTSAVE, when
XTRESTORE follows; the Kitty keyboard stack below its top, when the range
pushes and pops unevenly; XTSHIFTESCAPE; DECSASD. libghostty-vt keeps no
title stack (XTWINOPS 22/23 do nothing), so that needs nothing.

`plan_delete` is the transcript's Delete: excise, else skip with the reasons.

## ReplaceText

A replacement rewrites only printable bytes, in place:

- The range must be plain text by the runs index: it starts and ends on
  character boundaries inside printable runs, and runs inside it are joined
  only by SGR sequences, which stay (`NeedsMask::NotPlainText`, or
  `CursorAddressed` when something else, such as a CUP, separates them).
- The replacement must take the same cells (libghostty-vt's codepoint
  widths): split one group per original character where widths allow, so
  text typed key by key is rewritten key by key; else one group per piece
  between SGR sequences; else `WidthMismatch`. Control characters are
  `NotPrintable`.
- The edited program is refolded, and the cells the range drew, read right
  after it is drawn (plain text, wraps joined, trailing blanks trimmed),
  must equal `expect`; else `ExpectMismatch` with what they read.

An excision covering the range supersedes it; of two overlapping
replacements the earlier one (in source order) wins.

**Search.** `search::find_text` finds a string in the runs' text (plain,
rewritable occurrences) and with libghostty-vt's search over the screen and
scrollback just before each clear, reset, screen switch and resize, at each
segment's end and at the end of the pane, tracing each screen match's first
cell to the byte that drew it (a mask anchor).

## RedactInput and masks

`RedactInput` withholds the bytes of the pane's input events whose sequence
numbers are in range, keeping their time and origin, marked `redacted`; an
excision drops the input events typed at or during it. Exports (`bake`)
carry neither, and the index's line editor shows withheld keys as `•••`.

A `Mask` stays render-time. Its anchor must start a printed character (a
replaced one maps to its replacement); `resolve` pins that cell when it is
drawn and follows it, giving rectangles of cells (screen rows, scrollback
included, as seek's terminal counts them) for each span of source time,
empty while the other screen is active, ending when the cell is gone.

## Redaction

`redact::plan` hides a string everywhere: bullets of the same width where it
is plain text, a mask where that is refused or it was drawn in pieces, and
`RedactInput` for the input events that typed it. Its report lists what
remains: bytes still in the output (masked, or inside a sequence such as
the command line in OSC 133;C, which the transcript's command text still
shows), input events still holding it, and screen matches no byte could be
traced to.

## Tests

- `tests/excise.rs`: every command of the zsh fixtures is excised or
  refused as expected; each excision's end state (screen and scrollback
  text, VT dump with every extra, cursor, size, screen, title, all of the
  state above) equals a reference built by splicing the recording's bytes
  by hand, and equals the original's state with the command's lines absent;
  refusal lists are checked exactly, including synthetic sessions for each
  unreadable state and an end inside a sequence.
- `tests/replace.rs`: rewrites, SGR kept inside, width mismatch,
  cursor-addressed text, a wrong `expect`.
- `tests/redact.rs`: the plan, the mask's spans, and an export holding the
  secret only where the report says.
- `tests/eval.rs`: unedited sessions (no edits, or time-only edits) index,
  transcribe, checkpoint and seek byte for byte as before; seek over edited
  programs equals a replay from the start at every event and checkpoint;
  150 random edit lists evaluate without a panic and their programs map
  source ↔ program bytes both ways.
- `tests/cli.rs`: the binary end to end.

## Not done yet

- Typing retiming and speed ramps.
- Excision is checked over the raw stream: an absolute positioning that a
  later edit removes still refuses an earlier excision.
- Relative cursor moves near the top of the screen (CUU past row 0) clamp
  differently once rows are gone; they are not checked.
- A screen match whose cells moved since they were drawn (reflow, insert or
  delete line, scrolling regions) cannot be traced to a byte, so it is
  reported, not masked.
- `redact::plan` cannot rewrite text inside sequences, such as OSC 133;C
  command lines; it reports them.
- Fixups are computed with a replay per excision, so excising many commands
  of a long session is slow.
