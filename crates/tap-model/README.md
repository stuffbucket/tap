# tap-model

The first crate of Tap's Rust editor core (see
[`docs/editor-design.md`](../../docs/editor-design.md)): everything about a
recording that needs no terminal emulator.

- `event` — the raw event model, mirroring `TapEvent` in `src/model.ts`, with
  integer microseconds and a per-pane sequence number.
- `package` — the `Session.tap/` directory: `manifest.json`, one `.tlog` per
  pane, `edits.json`.
- `tlog` — the append-safe, checksummed event log.
- `legacy` — import from and export to the single-file `.tap` JSON form.
- `edl` — the edit decision list (`EditOp`), serialised with serde.
- `timemap` — the source → edited time map built from the time-only ops.

Nothing here is published (`publish = false`).

## Checks

The toolchain is pinned in `rust-toolchain.toml` (1.99.0). Run cargo through
mise so no global Rust default is needed:

```sh
mise x rust@1.99.0 -- cargo fmt --check
mise x rust@1.99.0 -- cargo clippy --all-targets -- -D warnings
mise x rust@1.99.0 -- cargo test
```

or all three with `mise x rust@1.99.0 bun@1.4.3 -- bun run check:rust`. `bun run check`
does not run them, so machines without Rust are unaffected.

## The package

```text
Session.tap/
  manifest.json          what the package contains (closed schema, below)
  events/<pane>.tlog     each pane's raw events; written once, never edited
  edits.json             the edit list, presentation and overlays; rewritten on save
  index/                 derived: disposable, rebuildable, never written here
  checkpoints/           derived: disposable, rebuildable, never written here
  thumbs/                derived: disposable, rebuildable, never written here
```

- **Raw events are authoritative.** A package is created once
  (`package::create`): it is assembled in a hidden sibling directory, every
  file written to a temp file, synced and renamed, and the directory renamed
  into place. The event logs are not modified afterwards.
- **Edits are replaced atomically** (`package::save_edits`): temp file, sync,
  rename, directory sync.
- **Derived directories** (`index/`, `checkpoints/`, `thumbs/`) can be deleted
  at any time. `package::open` ignores them; it rejects a `.tlog` in `events/`
  that the manifest does not list.
- **Pane ids are file names**: 1–64 bytes of ASCII letters, digits, `.`, `_`,
  `-`; starting with a letter or digit; not ending in `.`; not a Windows device
  name (`CON`, `NUL`, `COM1`, …); unique ignoring case.

### `manifest.json`

```json
{
  "formatVersion": 1,
  "title": "Split capture",
  "createdAt": "2026-11-26T06:38:41.304Z",
  "durationUs": 1047249,
  "canvas": { "width": 1280, "height": 720, "background": "#08090d", "fps": 120 },
  "panes": [
    { "id": "left", "cols": 60, "rows": 24, "fontSize": 16,
      "layout": { "x": 0, "y": 0, "width": 0.5, "height": 1 } }
  ],
  "metadata": { "terminal": "ghostty", "terminalEngine": "libghostty-vt 1.0.1",
                "timebase": "microseconds", "source": "pty", "shellIntegration": ["zsh"] },
  "engineFingerprint": "libghostty-vt 1.0.1"
}
```

`engineFingerprint` names the engine that recorded the session. It is
informational: derived caches are keyed by the engine that builds them.

### `edits.json`

```json
{
  "formatVersion": 1,
  "edits": [
    { "op": "skip", "from": { "us": 1000000 }, "to": { "byte": { "pane": "left", "offset": 4096 } } },
    { "op": "collapseIdle", "thresholdUs": 1500000, "maxUs": 500000, "enabled": false }
  ],
  "presentation": { "...": "as TapPresentation" },
  "overlays": [ { "...": "as TapOverlay; startUs/endUs are source µs" } ]
}
```

Each op is tagged by `"op"` with camelCase fields; every anchor is a source
coordinate, `{"us": n}` or `{"byte": {"pane": id, "offset": n}}` (an offset into
the pane's concatenated raw `output` bytes). `"enabled": false` keeps an entry
without effect. The full op list is `tap_model::edl::EditOp`.

## `.tlog` byte layout (version 1)

Little-endian throughout. `varint` is unsigned LEB128, at most 10 bytes, with
no redundant trailing zero groups; `zigzag` is a varint of a zigzag-encoded
`i64`. CRC is CRC-32C (Castagnoli).

```text
offset  size  field
0       8     magic        89 54 4C 4F 47 0D 0A 1A   ("\x89TLOG\r\n\x1a")
8       2     version      u16 = 1
10      2     flags        u16 = 0 (reserved; readers reject anything else)
12      4     header_crc   u32 = CRC-32C of bytes 0..12
16      …     records

record:
0       4     len          u32, 1 ..= 64 MiB (the payload length)
4       4     crc          u32 = CRC-32C of the payload
8       len   payload

payload:
        1     tag
        1     flags        (per tag; unused bits must be 0)
        var   at_us        varint, absolute source µs
        var   seq          varint, absolute per-pane sequence number
        …     body         (per tag; must fill the payload exactly)
```

| tag | event | flags | body |
|---|---|---|---|
| 1 | `output` | bits 0–1 data encoding; bit 2 has playback; bits 3–4 playback encoding | `varint n`, `n` bytes; with bit 2, `varint m`, `m` playback bytes |
| 2 | `input` | bits 0–1 data encoding; bits 2–4 origin; bits 5–6 redacted | `varint n`, `n` bytes |
| 3 | `resize` | 0 | `varint cols`, `varint rows` (each ≤ 65535) |
| 4 | `scroll` | 0 | `zigzag delta` |
| 5 | `marker` | 0 | `varint n`, `n` bytes of UTF-8 |
| 6 | `layout` | 0 | `x`, `y`, `width`, `height` as `f64` |
| 7 | `visibility` | bit 0 visible | empty |

Encodings record how the JSON form spelled the bytes, only so export
reproduces it: 0 none, 1 `utf8`, 2 `base64`. Origins: 0 none, 1 `keyboard`,
2 `paste`, 3 `terminal-reply`, 4 `vhs`, 5 `automation`. Redacted: 0 absent,
1 `false`, 2 `true`.

Records are in `(at_us, seq)` order: `at_us` never decreases, `seq` strictly
increases. `at_us` and `seq` are absolute so that any record decodes on its
own; an index can seek straight to a record offset.

**Damage.** An appender writes each framed record with one `write`, so a
crash tears at most the last record. `tlog::read` rejects any damage;
`tlog::recover` returns every record before the first damaged one plus the
error, which names the record index and the byte offset where it starts:
`Truncated` (the file ends inside the record), `Checksum`, `BadLength`,
`Malformed` (checksum good, payload does not decode) or `OutOfOrder`.
Truncating the file to `Recovery::valid_len` repairs it
(`Appender::open_recovering` does exactly that).

## Versioning

- `.tlog` has its own `version` in the header. Readers reject other versions
  and any non-zero header flags. A new event tag, flag bit or body field is a
  new version: version 1 readers reject unknown tags and bits rather than
  skip them.
- `manifest.json` and `edits.json` each carry `formatVersion` (both 1),
  checked before the rest of the document. Their schemas are closed: unknown
  fields are errors, so an older build can never drop a newer build's data by
  re-saving. Adding a field means bumping that document's version.
- The derived directories carry their own keys (engine fingerprint, byte
  program hash) and are not versioned by the package.

## Legacy `.tap` JSON

`legacy::import_json` accepts what `validateProject` accepts, except that
unknown fields are errors and pane ids must be safe file names. Presentation
and overlays move to `edits.json`; the edit list starts empty; events are
numbered `0, 1, 2, …` per pane. `legacy::export_json` gives back the same JSON
value (key order aside) and refuses a session that has edits, since the
legacy form cannot hold them.

## Time map

`TimeMap::build` evaluates trim, skip, collapse-idle, constant speed and hold
exactly (rationals in `u128`, floored once to integer µs). Trims intersect,
cuts take their union, speeds multiply and holds add, so all of these ops
commute with each other in every combination. The precise rules — gap
detection, where a collapsed gap keeps its pause, holds on removed instants,
and the inverse — are in the `timemap` module docs.

## Deferred

- Evaluation of `retimeTyping` (needs typing segments from the index), speed
  ramps, and the byte-changing and render-time ops (`excise`, `replaceText`,
  `redactInput`, `mask`). They parse and round-trip; `TimeMap::build` returns
  `EvalError::Unsupported` listing each one.
- The byte program, segmentation, the index, checkpoints and thumbnails
  (`tap-eval`, `tap-index`).
- Camera and keystroke-HUD lanes in `edits.json`, an undo stack, and Kitty
  media under `assets/`.
- Resynchronising past a damaged record in the middle of a `.tlog`
  (recovery stops at the first damage).
- CI for the Rust workspace.
