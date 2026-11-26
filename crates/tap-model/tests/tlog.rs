//! The `.tlog` record log: encoding, damage detection and recovery.

mod common;

use common::TempDir;
use proptest::prelude::*;
use tap_model::event::{
    EventKind, Input, InputOrigin, Output, PaneLayout, Playback, RawEvent, TextEncoding,
};
use tap_model::tlog::{self, Appender, HEADER_LEN, RECORD_PREFIX_LEN, Records, TlogError};

fn ev(at_us: u64, seq: u64, kind: EventKind) -> RawEvent {
    RawEvent { at_us, seq, kind }
}

fn sample() -> Vec<RawEvent> {
    vec![
        ev(0, 0, EventKind::Visibility { visible: false }),
        ev(
            0,
            1,
            EventKind::Output(Output {
                data: b"hello\r\n".to_vec(),
                data_encoding: TextEncoding::Unspecified,
                playback: None,
            }),
        ),
        ev(
            5,
            2,
            EventKind::Output(Output {
                data: vec![0, 255, 27],
                data_encoding: TextEncoding::Base64,
                playback: Some(Playback {
                    data: b"direct".to_vec(),
                    encoding: TextEncoding::Utf8,
                }),
            }),
        ),
        ev(
            5,
            7,
            EventKind::Input(Input {
                data: b"ls\r".to_vec(),
                data_encoding: TextEncoding::Utf8,
                origin: Some(InputOrigin::TerminalReply),
                redacted: Some(false),
            }),
        ),
        ev(
            6,
            8,
            EventKind::Input(Input {
                data: Vec::new(),
                data_encoding: TextEncoding::Base64,
                origin: None,
                redacted: Some(true),
            }),
        ),
        ev(
            7,
            9,
            EventKind::Resize {
                cols: 65535,
                rows: 1,
            },
        ),
        ev(8, 10, EventKind::Scroll { delta: -1_000_000 }),
        ev(
            9,
            11,
            EventKind::Marker {
                label: "✓ marker".into(),
            },
        ),
        ev(
            10,
            12,
            EventKind::Layout(PaneLayout {
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
            }),
        ),
        ev(u64::MAX, u64::MAX, EventKind::Visibility { visible: true }),
    ]
}

#[test]
fn encodes_and_decodes_every_kind() {
    let events = sample();
    let bytes = tlog::encode(&events).unwrap();
    assert_eq!(tlog::read(&bytes).unwrap(), events);
    assert_eq!(&bytes[..8], &tlog::MAGIC);
    let empty = tlog::encode(&[]).unwrap();
    assert_eq!(empty.len(), HEADER_LEN);
    assert_eq!(tlog::read(&empty).unwrap(), []);
}

#[test]
fn byte_layout_is_as_documented() {
    let event = ev(300, 2, EventKind::Resize { cols: 80, rows: 24 });
    let bytes = tlog::encode(std::slice::from_ref(&event)).unwrap();
    // Header: magic, version 1, flags 0, CRC-32C of the first 12 bytes.
    assert_eq!(&bytes[8..12], &[1, 0, 0, 0]);
    assert_eq!(&bytes[12..16], &crc32c::crc32c(&bytes[..12]).to_le_bytes());
    // Record: len, crc, payload = tag 3, flags 0, varint 300, varint 2, varint 80, varint 24.
    let payload = [3, 0, 0xac, 0x02, 2, 80, 24];
    assert_eq!(&bytes[16..20], &(payload.len() as u32).to_le_bytes());
    assert_eq!(&bytes[20..24], &crc32c::crc32c(&payload).to_le_bytes());
    assert_eq!(&bytes[24..], &payload);
}

#[test]
fn writer_rejects_out_of_order_events() {
    let a = ev(
        5,
        1,
        EventKind::Marker {
            label: String::new(),
        },
    );
    for b in [
        ev(
            4,
            2,
            EventKind::Marker {
                label: String::new(),
            },
        ),
        ev(
            5,
            1,
            EventKind::Marker {
                label: String::new(),
            },
        ),
    ] {
        let err = tlog::encode(&[a.clone(), b]).unwrap_err();
        assert!(matches!(err, TlogError::Encode { .. }), "{err}");
    }
}

#[test]
fn every_truncation_is_reported_and_recoverable() {
    let events = sample();
    let bytes = tlog::encode(&events).unwrap();
    let starts: Vec<u64> = Records::new(&bytes)
        .unwrap()
        .map(|r| r.unwrap().0.offset)
        .collect();
    for cut in 0..bytes.len() {
        let torn = &bytes[..cut];
        let recovery = tlog::recover(torn);
        if cut < HEADER_LEN {
            assert!(matches!(tlog::read(torn), Err(TlogError::BadHeader { .. })));
            assert_eq!(recovery.valid_len, 0);
            continue;
        }
        // Records wholly before the cut survive.
        let whole = starts
            .iter()
            .enumerate()
            .filter(|&(i, _)| starts.get(i + 1).map_or(bytes.len() as u64, |&n| n) <= cut as u64)
            .count();
        assert_eq!(recovery.events, events[..whole], "cut at {cut}");
        if starts.contains(&(cut as u64)) {
            // A cut on a record boundary is a valid, shorter log.
            assert_eq!(recovery.damage, None);
            assert_eq!(tlog::read(torn).unwrap(), events[..whole]);
        } else {
            let err = tlog::read(torn).unwrap_err();
            assert_eq!(recovery.damage.as_ref(), Some(&err));
            let TlogError::Truncated {
                index,
                offset,
                available,
                ..
            } = err
            else {
                panic!("cut at {cut}: {err}")
            };
            assert_eq!(index, whole as u64);
            assert_eq!(offset, starts[whole]);
            assert_eq!(available, cut as u64 - offset);
            assert_eq!(recovery.valid_len, offset);
        }
    }
}

#[test]
fn every_bit_flip_is_detected_and_located() {
    let events = sample();
    let bytes = tlog::encode(&events).unwrap();
    let starts: Vec<u64> = Records::new(&bytes)
        .unwrap()
        .map(|r| r.unwrap().0.offset)
        .collect();
    for i in 0..bytes.len() {
        for bit in 0..8 {
            let mut damaged = bytes.clone();
            damaged[i] ^= 1 << bit;
            let err = tlog::read(&damaged).expect_err("a flipped bit must be detected");
            let recovery = tlog::recover(&damaged);
            if i < HEADER_LEN {
                assert!(err.record().is_none(), "{err}");
                assert!(recovery.events.is_empty());
                continue;
            }
            let record = starts.iter().rposition(|&s| s <= i as u64).unwrap();
            let pos = err.record().unwrap_or_else(|| panic!("byte {i}: {err}"));
            assert_eq!(pos.index, record as u64, "byte {i} bit {bit}: {err}");
            assert_eq!(pos.offset, starts[record]);
            assert_eq!(recovery.events, events[..record]);
            assert_eq!(recovery.valid_len, starts[record]);
        }
    }
}

#[test]
fn rejects_wrong_magic_version_and_flags() {
    let mut bytes = tlog::encode(&sample()).unwrap();
    bytes[1] = b'X';
    assert!(
        tlog::read(&bytes)
            .unwrap_err()
            .to_string()
            .contains("magic")
    );

    let mut bytes = tlog::encode(&sample()).unwrap();
    bytes[8] = 2;
    let crc = crc32c::crc32c(&bytes[..12]);
    bytes[12..16].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(
        tlog::read(&bytes).unwrap_err(),
        TlogError::UnsupportedVersion { found: 2 }
    );
}

/// Frames a payload with a correct length and checksum, to test what the
/// decoder does with well-checksummed garbage.
fn framed(payloads: &[&[u8]]) -> Vec<u8> {
    let mut out = tlog::header().to_vec();
    for p in payloads {
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc32c::crc32c(p).to_le_bytes());
        out.extend_from_slice(p);
    }
    out
}

#[test]
fn malformed_payloads_are_reported_not_panicked() {
    let cases: &[(&[u8], &str)] = &[
        (&[9, 0, 0, 0], "unknown event tag 9"),
        (&[3, 0, 0, 0, 80], "runs past the end"),
        (&[3, 0, 0, 0, 80, 24, 0], "unexpected bytes"),
        (&[3, 1, 0, 0, 80, 24], "unknown flag bits"),
        (&[3, 0, 0, 0, 0x80, 0x80, 0x04, 1], "exceeds 65535"),
        (&[1, 3, 0, 0, 0], "unknown text encoding"),
        (
            &[1, 0b1_1000, 0, 0, 0],
            "playback encoding set without playback",
        ),
        (&[2, 0b110_0000, 0, 0, 0], "unknown redaction state"),
        (&[2, 0b1_1000, 0, 0, 0], "unknown input origin"),
        (&[5, 0, 0, 0, 2, 0xc3, 0x28], "not UTF-8"),
        (&[6, 0, 0, 0, 1, 2, 3], "layout runs past"),
        (
            &[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0x0f],
            "runs past the end of the record",
        ),
        (&[7], "shorter than its tag"),
    ];
    for &(payload, expected) in cases {
        let bytes = framed(&[&[7, 1, 0, 0], payload]);
        let err = tlog::read(&bytes).unwrap_err();
        let TlogError::Malformed {
            index: 1,
            offset,
            ref reason,
        } = err
        else {
            panic!("{payload:?}: {err}")
        };
        assert_eq!(offset, (HEADER_LEN + RECORD_PREFIX_LEN + 4) as u64);
        assert!(reason.contains(expected), "{payload:?}: {reason}");
        assert_eq!(tlog::recover(&bytes).events.len(), 1);
    }
    let bytes = framed(&[&[7, 0, 5, 1], &[7, 0, 4, 2]]);
    assert!(matches!(
        tlog::read(&bytes),
        Err(TlogError::OutOfOrder { index: 1, .. })
    ));
    let mut bytes = tlog::header().to_vec();
    bytes.extend_from_slice(&[0; 8]);
    assert!(matches!(
        tlog::read(&bytes),
        Err(TlogError::BadLength {
            index: 0,
            len: 0,
            ..
        })
    ));
    let mut bytes = tlog::header().to_vec();
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    assert!(matches!(
        tlog::read(&bytes),
        Err(TlogError::BadLength { index: 0, .. })
    ));
}

#[test]
fn appender_survives_a_torn_tail() {
    let tmp = TempDir::new("tlog-append");
    let path = tmp.path().join("main.tlog");
    let events = sample();
    {
        let mut log = Appender::create(&path).unwrap();
        for e in &events[..5] {
            log.append(e).unwrap();
        }
        assert!(
            log.append(&events[0]).is_err(),
            "out-of-order append must fail"
        );
        log.sync().unwrap();
    }
    assert!(Appender::create(&path).is_err(), "create must not clobber");
    {
        let mut log = Appender::open(&path).unwrap();
        for e in &events[5..] {
            log.append(e).unwrap();
        }
    }
    assert_eq!(tlog::read_file(&path).unwrap(), events);

    // Simulate a crash mid-append: half of a new record reaches the disk.
    let mut bytes = std::fs::read(&path).unwrap();
    let good_len = bytes.len();
    let mut record = Vec::new();
    tlog::encode_record(
        &ev(u64::MAX, u64::MAX, EventKind::Marker { label: "x".into() }),
        &mut record,
    )
    .unwrap();
    bytes.extend_from_slice(&record[..record.len() / 2]);
    std::fs::write(&path, &bytes).unwrap();

    let message = Appender::open(&path).unwrap_err().to_string();
    assert!(
        message.contains(&format!(
            "record {} at byte {good_len} is truncated",
            events.len()
        )),
        "{message}"
    );
    let (_, damage) = Appender::open_recovering(&path).unwrap();
    assert!(matches!(damage, Some(TlogError::Truncated { .. })));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), good_len as u64);
    assert_eq!(tlog::read_file(&path).unwrap(), events);
}

#[test]
fn write_file_is_atomic_and_leaves_no_temp_files() {
    let tmp = TempDir::new("tlog-write");
    let path = tmp.path().join("p.tlog");
    tlog::write_file(&path, &sample()).unwrap();
    tlog::write_file(&path, &sample()[..2]).unwrap();
    assert_eq!(tlog::read_file(&path).unwrap(), sample()[..2]);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

fn arb_encoding() -> impl Strategy<Value = TextEncoding> {
    prop_oneof![
        Just(TextEncoding::Unspecified),
        Just(TextEncoding::Utf8),
        Just(TextEncoding::Base64)
    ]
}

fn arb_kind() -> impl Strategy<Value = EventKind> {
    let bytes = || prop::collection::vec(any::<u8>(), 0..64);
    let origin = prop_oneof![
        Just(None),
        Just(Some(InputOrigin::Keyboard)),
        Just(Some(InputOrigin::Paste)),
        Just(Some(InputOrigin::TerminalReply)),
        Just(Some(InputOrigin::Vhs)),
        Just(Some(InputOrigin::Automation)),
    ];
    prop_oneof![
        (
            bytes(),
            arb_encoding(),
            prop::option::of((bytes(), arb_encoding()))
        )
            .prop_map(|(data, data_encoding, p)| {
                EventKind::Output(Output {
                    data,
                    data_encoding,
                    playback: p.map(|(data, encoding)| Playback { data, encoding }),
                })
            }),
        (
            bytes(),
            arb_encoding(),
            origin,
            prop::option::of(any::<bool>())
        )
            .prop_map(|(data, data_encoding, origin, redacted)| {
                EventKind::Input(Input {
                    data,
                    data_encoding,
                    origin,
                    redacted,
                })
            }),
        (1..=u16::MAX, 1..=u16::MAX).prop_map(|(cols, rows)| EventKind::Resize { cols, rows }),
        any::<i64>().prop_map(|delta| EventKind::Scroll { delta }),
        ".*".prop_map(|label| EventKind::Marker { label }),
        (any::<f64>(), any::<f64>(), any::<f64>(), any::<f64>())
            .prop_filter("NaN never compares equal", |(a, b, c, d)| ![a, b, c, d]
                .iter()
                .any(|v| v.is_nan()))
            .prop_map(|(x, y, width, height)| EventKind::Layout(PaneLayout {
                x,
                y,
                width,
                height
            })),
        any::<bool>().prop_map(|visible| EventKind::Visibility { visible }),
    ]
}

fn arb_events() -> impl Strategy<Value = Vec<RawEvent>> {
    prop::collection::vec((0..1u64 << 40, 1..1000u64, arb_kind()), 0..40).prop_map(|steps| {
        let (mut at, mut seq) = (0u64, 0u64);
        steps
            .into_iter()
            .enumerate()
            .map(|(i, (dt, dseq, kind))| {
                if i > 0 {
                    at += dt % 3 * dt; // often 0: equal timestamps
                    seq += dseq;
                }
                ev(at, seq, kind)
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn round_trips_arbitrary_events(events in arb_events()) {
        let bytes = tlog::encode(&events).unwrap();
        prop_assert_eq!(tlog::read(&bytes).unwrap(), events);
    }

    #[test]
    fn arbitrary_damage_never_panics_and_recovers_a_prefix(
        events in arb_events(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut bytes = tlog::encode(&events).unwrap();
        for (i, b) in edits {
            let i = i.index(bytes.len());
            bytes[i] = b;
        }
        bytes.truncate(cut.index(bytes.len() + 1));
        let recovery = tlog::recover(&bytes);
        // CRC-32C makes an undetected change vanishingly unlikely, so whatever
        // survives is exactly a prefix of what was written.
        prop_assert!(events.starts_with(&recovery.events));
        prop_assert!(recovery.valid_len <= bytes.len() as u64);
        match tlog::read(&bytes) {
            Ok(read) => prop_assert_eq!(read, recovery.events),
            Err(e) => prop_assert_eq!(Some(e), recovery.damage),
        }
    }

    #[test]
    fn random_bytes_never_panic(mut bytes in prop::collection::vec(any::<u8>(), 0..512), with_header in any::<bool>()) {
        if with_header && bytes.len() >= HEADER_LEN {
            bytes[..HEADER_LEN].copy_from_slice(&tlog::header());
        }
        let _ = tlog::read(&bytes);
        let _ = tlog::recover(&bytes);
    }
}
