//! `.tlog`: a pane's raw events as an append-safe, checksummed record log.
//!
//! # Byte layout (format version 1)
//!
//! All integers are little-endian. `varint` is unsigned LEB128 (at most 10
//! bytes, no redundant trailing zero groups); `zigzag` is a varint holding a
//! zigzag-encoded `i64`.
//!
//! ```text
//! file    = header record*
//! header  = magic[8] version:u16 flags:u16 header_crc:u32          (16 bytes)
//!   magic      = 89 54 4C 4F 47 0D 0A 1A   ("\x89TLOG\r\n\x1a")
//!   version    = 1
//!   flags      = 0 (reserved; readers reject other values)
//!   header_crc = CRC-32C of the first 12 bytes
//! record  = len:u32 crc:u32 payload[len]
//!   len        = 1 ..= MAX_PAYLOAD_LEN
//!   crc        = CRC-32C (Castagnoli) of payload
//! payload = tag:u8 flags:u8 at_us:varint seq:varint body
//! ```
//!
//! | tag | event        | flags                                                  | body                                   |
//! |-----|--------------|--------------------------------------------------------|----------------------------------------|
//! | 1   | `output`     | bits 0–1 data encoding, bit 2 has playback, bits 3–4 playback encoding | `varint n, data[n]` then, with bit 2, `varint m, playback[m]` |
//! | 2   | `input`      | bits 0–1 data encoding, bits 2–4 origin, bits 5–6 redacted | `varint n, data[n]`                |
//! | 3   | `resize`     | 0                                                      | `varint cols, varint rows`             |
//! | 4   | `scroll`     | 0                                                      | `zigzag delta`                         |
//! | 5   | `marker`     | 0                                                      | `varint n, utf8[n]`                    |
//! | 6   | `layout`     | 0                                                      | `x, y, width, height` as `f64` LE      |
//! | 7   | `visibility` | bit 0 visible                                          | empty                                  |
//!
//! Encodings: 0 unspecified, 1 utf8, 2 base64. Origins: 0 none, 1 keyboard,
//! 2 paste, 3 terminal-reply, 4 vhs, 5 automation. Redacted: 0 absent,
//! 1 false, 2 true. Unused flag bits must be zero and the body must fill the
//! payload exactly.
//!
//! `at_us` and `seq` are absolute, not deltas, so every record decodes on its
//! own: an index can point at any record offset, and losing one record never
//! changes the meaning of the others. Records are in `(at_us, seq)` order;
//! `at_us` never decreases and `seq` strictly increases.
//!
//! # Damage
//!
//! Appending writes each framed record with a single write, so a crash leaves
//! at most one torn record at the end. [`read`] rejects any damage; [`recover`]
//! returns every record before the first damaged one along with the precise
//! error (record index and byte offset).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error as ThisError;

use crate::event::{
    EventKind, Input, InputOrigin, Output, PaneLayout, Playback, RawEvent, TextEncoding,
};
use crate::{Error, fsutil, varint};

/// The file's first eight bytes.
pub const MAGIC: [u8; 8] = *b"\x89TLOG\r\n\x1a";
/// The format version this build reads and writes.
pub const FORMAT_VERSION: u16 = 1;
/// Size of the file header in bytes.
pub const HEADER_LEN: usize = 16;
/// Size of a record's `len` + `crc` prefix in bytes.
pub const RECORD_PREFIX_LEN: usize = 8;
/// Largest payload a record may carry (64 MiB). A larger length is treated
/// as corruption rather than an allocation request.
pub const MAX_PAYLOAD_LEN: u32 = 64 << 20;

const TAG_OUTPUT: u8 = 1;
const TAG_INPUT: u8 = 2;
const TAG_RESIZE: u8 = 3;
const TAG_SCROLL: u8 = 4;
const TAG_MARKER: u8 = 5;
const TAG_LAYOUT: u8 = 6;
const TAG_VISIBILITY: u8 = 7;

/// Where a record starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordPos {
    /// Zero-based record number.
    pub index: u64,
    /// Byte offset of the record's `len` field from the start of the file.
    pub offset: u64,
}

/// What is wrong with an event log, and where.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum TlogError {
    /// The file does not start with a valid header.
    #[error("not a Tap event log: {reason}")]
    BadHeader {
        /// What is wrong with it.
        reason: String,
    },
    /// The header is valid but from a newer (or unknown) format version.
    #[error(
        "event log format version {found} is not supported (this build reads version {FORMAT_VERSION})"
    )]
    UnsupportedVersion {
        /// The version in the file.
        found: u16,
    },
    /// The file ends inside a record: a torn write at the tail.
    #[error(
        "record {index} at byte {offset} is truncated: it needs {needed} bytes but only {available} remain"
    )]
    Truncated {
        /// Record number.
        index: u64,
        /// Byte offset of the record.
        offset: u64,
        /// Bytes the record needs (prefix plus payload, as far as known).
        needed: u64,
        /// Bytes left in the file from `offset`.
        available: u64,
    },
    /// A record's payload does not match its checksum.
    #[error(
        "record {index} at byte {offset} is corrupt: checksum {computed:#010x} does not match the stored {stored:#010x}"
    )]
    Checksum {
        /// Record number.
        index: u64,
        /// Byte offset of the record.
        offset: u64,
        /// The checksum in the file.
        stored: u32,
        /// The checksum of the bytes present.
        computed: u32,
    },
    /// A record's length is 0 or larger than [`MAX_PAYLOAD_LEN`].
    #[error(
        "record {index} at byte {offset} is corrupt: payload length {len} is outside 1..={MAX_PAYLOAD_LEN}"
    )]
    BadLength {
        /// Record number.
        index: u64,
        /// Byte offset of the record.
        offset: u64,
        /// The stored length.
        len: u32,
    },
    /// The checksum matches but the payload does not decode.
    #[error("record {index} at byte {offset} is malformed: {reason}")]
    Malformed {
        /// Record number.
        index: u64,
        /// Byte offset of the record.
        offset: u64,
        /// What is wrong with it.
        reason: String,
    },
    /// The record decodes but breaks `(at_us, seq)` order.
    #[error("record {index} at byte {offset} is out of order: {reason}")]
    OutOfOrder {
        /// Record number.
        index: u64,
        /// Byte offset of the record.
        offset: u64,
        /// Which ordering rule it breaks.
        reason: String,
    },
    /// An event cannot be written (for example, it is too large).
    #[error("cannot encode event seq {seq}: {reason}")]
    Encode {
        /// The event's sequence number.
        seq: u64,
        /// Why.
        reason: String,
    },
}

impl TlogError {
    /// The damaged record's position, for errors that concern one record.
    pub fn record(&self) -> Option<RecordPos> {
        match *self {
            Self::Truncated { index, offset, .. }
            | Self::Checksum { index, offset, .. }
            | Self::BadLength { index, offset, .. }
            | Self::Malformed { index, offset, .. }
            | Self::OutOfOrder { index, offset, .. } => Some(RecordPos { index, offset }),
            Self::BadHeader { .. } | Self::UnsupportedVersion { .. } | Self::Encode { .. } => None,
        }
    }
}

/// The file header for this format version.
pub fn header() -> [u8; HEADER_LEN] {
    let mut out = [0u8; HEADER_LEN];
    out[..8].copy_from_slice(&MAGIC);
    out[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    out[10..12].copy_from_slice(&0u16.to_le_bytes());
    let crc = crc32c::crc32c(&out[..12]);
    out[12..16].copy_from_slice(&crc.to_le_bytes());
    out
}

fn check_header(bytes: &[u8]) -> Result<(), TlogError> {
    if bytes.len() < HEADER_LEN {
        return Err(TlogError::BadHeader {
            reason: format!(
                "file is {} bytes; the header alone is {HEADER_LEN}",
                bytes.len()
            ),
        });
    }
    if bytes[..8] != MAGIC {
        return Err(TlogError::BadHeader {
            reason: "magic bytes do not match".into(),
        });
    }
    let stored = u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes"));
    let computed = crc32c::crc32c(&bytes[..12]);
    if stored != computed {
        return Err(TlogError::BadHeader {
            reason: format!(
                "header checksum {computed:#010x} does not match the stored {stored:#010x}"
            ),
        });
    }
    let version = u16::from_le_bytes([bytes[8], bytes[9]]);
    if version != FORMAT_VERSION {
        return Err(TlogError::UnsupportedVersion { found: version });
    }
    let flags = u16::from_le_bytes([bytes[10], bytes[11]]);
    if flags != 0 {
        return Err(TlogError::BadHeader {
            reason: format!("unknown header flags {flags:#06x}"),
        });
    }
    Ok(())
}

fn encoding_bits(encoding: TextEncoding) -> u8 {
    match encoding {
        TextEncoding::Unspecified => 0,
        TextEncoding::Utf8 => 1,
        TextEncoding::Base64 => 2,
    }
}

fn encoding_from_bits(bits: u8) -> Result<TextEncoding, String> {
    match bits {
        0 => Ok(TextEncoding::Unspecified),
        1 => Ok(TextEncoding::Utf8),
        2 => Ok(TextEncoding::Base64),
        _ => Err(format!("unknown text encoding {bits}")),
    }
}

fn origin_bits(origin: Option<InputOrigin>) -> u8 {
    match origin {
        None => 0,
        Some(InputOrigin::Keyboard) => 1,
        Some(InputOrigin::Paste) => 2,
        Some(InputOrigin::TerminalReply) => 3,
        Some(InputOrigin::Vhs) => 4,
        Some(InputOrigin::Automation) => 5,
    }
}

fn origin_from_bits(bits: u8) -> Result<Option<InputOrigin>, String> {
    Ok(match bits {
        0 => None,
        1 => Some(InputOrigin::Keyboard),
        2 => Some(InputOrigin::Paste),
        3 => Some(InputOrigin::TerminalReply),
        4 => Some(InputOrigin::Vhs),
        5 => Some(InputOrigin::Automation),
        _ => return Err(format!("unknown input origin {bits}")),
    })
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    varint::put_u64(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn get_bytes<'a>(input: &'a [u8], pos: &mut usize) -> Result<&'a [u8], String> {
    let len = varint::get_u64(input, pos)?;
    let end = usize::try_from(len)
        .ok()
        .and_then(|len| pos.checked_add(len))
        .filter(|&end| end <= input.len())
        .ok_or_else(|| format!("byte string of {len} bytes runs past the end of the record"))?;
    let bytes = &input[*pos..end];
    *pos = end;
    Ok(bytes)
}

/// Encodes one event as a record payload (no `len`/`crc` prefix).
pub fn encode_payload(event: &RawEvent) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    let (tag, flags) = match &event.kind {
        EventKind::Output(o) => {
            let mut flags = encoding_bits(o.data_encoding);
            if let Some(p) = &o.playback {
                flags |= 1 << 2 | encoding_bits(p.encoding) << 3;
            }
            (TAG_OUTPUT, flags)
        }
        EventKind::Input(i) => {
            let redacted = match i.redacted {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            };
            (
                TAG_INPUT,
                encoding_bits(i.data_encoding) | origin_bits(i.origin) << 2 | redacted << 5,
            )
        }
        EventKind::Resize { .. } => (TAG_RESIZE, 0),
        EventKind::Scroll { .. } => (TAG_SCROLL, 0),
        EventKind::Marker { .. } => (TAG_MARKER, 0),
        EventKind::Layout(_) => (TAG_LAYOUT, 0),
        EventKind::Visibility { visible } => (TAG_VISIBILITY, u8::from(*visible)),
    };
    out.push(tag);
    out.push(flags);
    varint::put_u64(&mut out, event.at_us);
    varint::put_u64(&mut out, event.seq);
    match &event.kind {
        EventKind::Output(o) => {
            put_bytes(&mut out, &o.data);
            if let Some(p) = &o.playback {
                put_bytes(&mut out, &p.data);
            }
        }
        EventKind::Input(i) => put_bytes(&mut out, &i.data),
        EventKind::Resize { cols, rows } => {
            varint::put_u64(&mut out, u64::from(*cols));
            varint::put_u64(&mut out, u64::from(*rows));
        }
        EventKind::Scroll { delta } => varint::put_i64(&mut out, *delta),
        EventKind::Marker { label } => put_bytes(&mut out, label.as_bytes()),
        EventKind::Layout(l) => {
            for v in [l.x, l.y, l.width, l.height] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        EventKind::Visibility { .. } => {}
    }
    out
}

/// Decodes a record payload. The error says what is wrong, without position.
pub fn decode_payload(payload: &[u8]) -> Result<RawEvent, String> {
    let [tag, flags, ..] = *payload else {
        return Err("payload is shorter than its tag and flags".into());
    };
    let mut pos = 2;
    let at_us = varint::get_u64(payload, &mut pos)?;
    let seq = varint::get_u64(payload, &mut pos)?;
    let check_flags = |allowed: u8| {
        if flags & !allowed == 0 {
            Ok(())
        } else {
            Err(format!(
                "unknown flag bits {:#04x} for tag {tag}",
                flags & !allowed
            ))
        }
    };
    let kind = match tag {
        TAG_OUTPUT => {
            check_flags(0b1_1111)?;
            let data = get_bytes(payload, &mut pos)?.to_vec();
            let playback = if flags & 1 << 2 != 0 {
                Some(Playback {
                    data: get_bytes(payload, &mut pos)?.to_vec(),
                    encoding: encoding_from_bits(flags >> 3 & 3)?,
                })
            } else if flags >> 3 & 3 != 0 {
                return Err("playback encoding set without playback bytes".into());
            } else {
                None
            };
            EventKind::Output(Output {
                data,
                data_encoding: encoding_from_bits(flags & 3)?,
                playback,
            })
        }
        TAG_INPUT => {
            check_flags(0b111_1111)?;
            let redacted = match flags >> 5 & 3 {
                0 => None,
                1 => Some(false),
                2 => Some(true),
                bits => return Err(format!("unknown redaction state {bits}")),
            };
            EventKind::Input(Input {
                data: get_bytes(payload, &mut pos)?.to_vec(),
                data_encoding: encoding_from_bits(flags & 3)?,
                origin: origin_from_bits(flags >> 2 & 7)?,
                redacted,
            })
        }
        TAG_RESIZE => {
            check_flags(0)?;
            let mut dim = |name: &str| -> Result<u16, String> {
                let v = varint::get_u64(payload, &mut pos)?;
                u16::try_from(v).map_err(|_| format!("resize {name} {v} exceeds {}", u16::MAX))
            };
            let cols = dim("cols")?;
            let rows = dim("rows")?;
            EventKind::Resize { cols, rows }
        }
        TAG_SCROLL => {
            check_flags(0)?;
            EventKind::Scroll {
                delta: varint::get_i64(payload, &mut pos)?,
            }
        }
        TAG_MARKER => {
            check_flags(0)?;
            let bytes = get_bytes(payload, &mut pos)?;
            let label = std::str::from_utf8(bytes)
                .map_err(|e| format!("marker label is not UTF-8: {e}"))?;
            EventKind::Marker {
                label: label.to_owned(),
            }
        }
        TAG_LAYOUT => {
            check_flags(0)?;
            let mut values = [0f64; 4];
            for v in &mut values {
                let bytes = payload
                    .get(pos..pos + 8)
                    .ok_or("layout runs past the end of the record")?;
                *v = f64::from_le_bytes(bytes.try_into().expect("8 bytes"));
                pos += 8;
            }
            let [x, y, width, height] = values;
            EventKind::Layout(PaneLayout {
                x,
                y,
                width,
                height,
            })
        }
        TAG_VISIBILITY => {
            check_flags(1)?;
            EventKind::Visibility {
                visible: flags & 1 != 0,
            }
        }
        _ => return Err(format!("unknown event tag {tag}")),
    };
    if pos != payload.len() {
        return Err(format!(
            "{} unexpected bytes after the event",
            payload.len() - pos
        ));
    }
    Ok(RawEvent { at_us, seq, kind })
}

/// Appends one framed record (`len`, `crc`, payload) to `out`.
pub fn encode_record(event: &RawEvent, out: &mut Vec<u8>) -> Result<(), TlogError> {
    let payload = encode_payload(event);
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|&len| len <= MAX_PAYLOAD_LEN)
        .ok_or_else(|| TlogError::Encode {
            seq: event.seq,
            reason: format!(
                "record of {} bytes exceeds the {MAX_PAYLOAD_LEN}-byte limit",
                payload.len()
            ),
        })?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&crc32c::crc32c(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(())
}

fn check_order(previous: Option<(u64, u64)>, event: &RawEvent) -> Result<(), String> {
    if let Some((at_us, seq)) = previous {
        if event.at_us < at_us {
            return Err(format!(
                "time {}µs is before the previous record's {at_us}µs",
                event.at_us
            ));
        }
        if event.seq <= seq {
            return Err(format!(
                "seq {} does not follow the previous record's {seq}",
                event.seq
            ));
        }
    }
    Ok(())
}

/// Encodes a whole log (header and records) in memory.
pub fn encode(events: &[RawEvent]) -> Result<Vec<u8>, TlogError> {
    let mut out = header().to_vec();
    let mut previous = None;
    for event in events {
        check_order(previous, event).map_err(|reason| TlogError::Encode {
            seq: event.seq,
            reason,
        })?;
        encode_record(event, &mut out)?;
        previous = Some((event.at_us, event.seq));
    }
    Ok(out)
}

/// Iterates over a log's records, checking each one. After the first error
/// it yields nothing more.
#[derive(Debug)]
pub struct Records<'a> {
    bytes: &'a [u8],
    pos: usize,
    index: u64,
    previous: Option<(u64, u64)>,
    done: bool,
}

impl<'a> Records<'a> {
    /// Checks the header and positions the iterator at the first record.
    pub fn new(bytes: &'a [u8]) -> Result<Self, TlogError> {
        check_header(bytes)?;
        Ok(Self {
            bytes,
            pos: HEADER_LEN,
            index: 0,
            previous: None,
            done: false,
        })
    }

    /// Byte offset just past the last record yielded successfully.
    pub fn valid_len(&self) -> u64 {
        self.pos as u64
    }

    fn next_record(&mut self) -> Result<(RecordPos, RawEvent), TlogError> {
        let index = self.index;
        let offset = self.pos as u64;
        let rest = &self.bytes[self.pos..];
        let truncated = |needed: u64| TlogError::Truncated {
            index,
            offset,
            needed,
            available: rest.len() as u64,
        };
        if rest.len() < RECORD_PREFIX_LEN {
            return Err(truncated(RECORD_PREFIX_LEN as u64));
        }
        let len = u32::from_le_bytes(rest[..4].try_into().expect("4 bytes"));
        let stored = u32::from_le_bytes(rest[4..8].try_into().expect("4 bytes"));
        if len == 0 || len > MAX_PAYLOAD_LEN {
            return Err(TlogError::BadLength { index, offset, len });
        }
        let total = RECORD_PREFIX_LEN + len as usize;
        if rest.len() < total {
            return Err(truncated(total as u64));
        }
        let payload = &rest[RECORD_PREFIX_LEN..total];
        let computed = crc32c::crc32c(payload);
        if computed != stored {
            return Err(TlogError::Checksum {
                index,
                offset,
                stored,
                computed,
            });
        }
        let event = decode_payload(payload).map_err(|reason| TlogError::Malformed {
            index,
            offset,
            reason,
        })?;
        check_order(self.previous, &event).map_err(|reason| TlogError::OutOfOrder {
            index,
            offset,
            reason,
        })?;
        self.previous = Some((event.at_us, event.seq));
        self.pos += total;
        self.index += 1;
        Ok((RecordPos { index, offset }, event))
    }
}

impl Iterator for Records<'_> {
    type Item = Result<(RecordPos, RawEvent), TlogError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.pos == self.bytes.len() {
            return None;
        }
        let item = self.next_record();
        self.done = item.is_err();
        Some(item)
    }
}

/// Decodes a whole log, failing on any damage.
pub fn read(bytes: &[u8]) -> Result<Vec<RawEvent>, TlogError> {
    Records::new(bytes)?.map(|r| r.map(|(_, e)| e)).collect()
}

/// The undamaged prefix of a log.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovery {
    /// Every record before the first damaged one.
    pub events: Vec<RawEvent>,
    /// Length of the undamaged prefix in bytes (header plus good records);
    /// truncating the file to this length repairs it. Zero when the header
    /// itself is damaged.
    pub valid_len: u64,
    /// The first damage found, if any.
    pub damage: Option<TlogError>,
}

/// Decodes as much of a log as is intact.
pub fn recover(bytes: &[u8]) -> Recovery {
    let mut records = match Records::new(bytes) {
        Ok(records) => records,
        Err(damage) => {
            return Recovery {
                events: Vec::new(),
                valid_len: 0,
                damage: Some(damage),
            };
        }
    };
    let mut events = Vec::new();
    let mut damage = None;
    for item in records.by_ref() {
        match item {
            Ok((_, event)) => events.push(event),
            Err(e) => damage = Some(e),
        }
    }
    Recovery {
        events,
        valid_len: records.valid_len(),
        damage,
    }
}

/// Reads a log file, failing on any damage.
pub fn read_file(path: &Path) -> Result<Vec<RawEvent>, Error> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    read(&bytes).map_err(|e| Error::tlog(path, e))
}

/// Writes a whole log atomically: a temp file in the same directory, synced
/// and renamed over `path`.
pub fn write_file(path: &Path, events: &[RawEvent]) -> Result<(), Error> {
    let bytes = encode(events).map_err(|e| Error::tlog(path, e))?;
    fsutil::write_atomic(path, &bytes)
}

/// Appends records to a log that is being recorded.
///
/// Each record is written with one `write` call, so a crash tears at most the
/// last record; [`Appender::open_recovering`] cuts such a tail off.
#[derive(Debug)]
pub struct Appender {
    path: PathBuf,
    file: File,
    previous: Option<(u64, u64)>,
    buffer: Vec<u8>,
    failed: bool,
}

impl Appender {
    /// Creates a new, empty log. Fails if `path` exists.
    pub fn create(path: &Path) -> Result<Self, Error> {
        let mut file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)
            .map_err(|e| Error::io(path, e))?;
        file.write_all(&header()).map_err(|e| Error::io(path, e))?;
        file.sync_all().map_err(|e| Error::io(path, e))?;
        fsutil::sync_parent(path)?;
        Ok(Self {
            path: path.to_owned(),
            file,
            previous: None,
            buffer: Vec::new(),
            failed: false,
        })
    }

    /// Opens an existing log for appending. Fails if it is damaged in any way.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let events = read_file(path)?;
        Self::open_after(path, events.last())
    }

    /// Opens an existing log for appending, first truncating any damaged tail
    /// (for example, a record torn by a crash). Returns the damage that was
    /// cut off. Fails if the header itself is damaged.
    pub fn open_recovering(path: &Path) -> Result<(Self, Option<TlogError>), Error> {
        let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
        let recovery = recover(&bytes);
        if recovery.valid_len == 0 {
            let damage = recovery
                .damage
                .expect("an empty prefix means the header is damaged");
            return Err(Error::tlog(path, damage));
        }
        if recovery.damage.is_some() {
            let file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|e| Error::io(path, e))?;
            file.set_len(recovery.valid_len)
                .map_err(|e| Error::io(path, e))?;
            file.sync_all().map_err(|e| Error::io(path, e))?;
        }
        Ok((
            Self::open_after(path, recovery.events.last())?,
            recovery.damage,
        ))
    }

    fn open_after(path: &Path, last: Option<&RawEvent>) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| Error::io(path, e))?;
        Ok(Self {
            path: path.to_owned(),
            file,
            previous: last.map(|e| (e.at_us, e.seq)),
            buffer: Vec::new(),
            failed: false,
        })
    }

    /// Appends one event. It must follow the last one in `(at_us, seq)` order.
    ///
    /// After a failed write the file may end in a torn record, so the
    /// appender refuses further appends; reopen it with
    /// [`Appender::open_recovering`].
    pub fn append(&mut self, event: &RawEvent) -> Result<(), Error> {
        if self.failed {
            return Err(Error::io(
                &self.path,
                std::io::Error::other("an earlier append failed; reopen the log to recover it"),
            ));
        }
        check_order(self.previous, event).map_err(|reason| {
            Error::tlog(
                &self.path,
                TlogError::Encode {
                    seq: event.seq,
                    reason,
                },
            )
        })?;
        self.buffer.clear();
        encode_record(event, &mut self.buffer).map_err(|e| Error::tlog(&self.path, e))?;
        if let Err(e) = self.file.write_all(&self.buffer) {
            self.failed = true;
            return Err(Error::io(&self.path, e));
        }
        self.previous = Some((event.at_us, event.seq));
        Ok(())
    }

    /// Flushes appended records to disk.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.file.sync_data().map_err(|e| Error::io(&self.path, e))
    }
}
