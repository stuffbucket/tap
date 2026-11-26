//! Unsigned LEB128 varints and zigzag signed integers.

/// Longest encoding of a `u64`.
pub(crate) const MAX_LEN: usize = 10;

pub(crate) fn put_u64(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Reads a varint at `*pos`, advancing it. Rejects encodings that run past
/// the input, overflow 64 bits, or carry redundant trailing zero groups, so
/// every value has exactly one encoding.
pub(crate) fn get_u64(input: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut value: u64 = 0;
    for i in 0..MAX_LEN {
        let Some(&byte) = input.get(*pos + i) else {
            return Err("varint runs past the end of the record".into());
        };
        let group = u64::from(byte & 0x7f);
        if i == MAX_LEN - 1 && byte > 1 {
            return Err("varint overflows 64 bits".into());
        }
        value |= group << (7 * i);
        if byte & 0x80 == 0 {
            if i > 0 && byte == 0 {
                return Err("varint has a redundant trailing zero group".into());
            }
            *pos += i + 1;
            return Ok(value);
        }
    }
    Err("varint overflows 64 bits".into())
}

pub(crate) fn put_i64(out: &mut Vec<u8>, value: i64) {
    put_u64(out, ((value << 1) ^ (value >> 63)) as u64);
}

pub(crate) fn get_i64(input: &[u8], pos: &mut usize) -> Result<i64, String> {
    let raw = get_u64(input, pos)?;
    Ok(((raw >> 1) as i64) ^ -((raw & 1) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_edge_values() {
        for value in [
            0,
            1,
            127,
            128,
            300,
            u64::from(u32::MAX),
            u64::MAX - 1,
            u64::MAX,
        ] {
            let mut buf = Vec::new();
            put_u64(&mut buf, value);
            let mut pos = 0;
            assert_eq!(get_u64(&buf, &mut pos).unwrap(), value);
            assert_eq!(pos, buf.len());
        }
        for value in [0, 1, -1, 63, -64, i64::MAX, i64::MIN] {
            let mut buf = Vec::new();
            put_i64(&mut buf, value);
            let mut pos = 0;
            assert_eq!(get_i64(&buf, &mut pos).unwrap(), value);
        }
    }

    #[test]
    fn rejects_malformed() {
        let mut pos = 0;
        assert!(get_u64(&[0x80], &mut pos).is_err());
        assert!(get_u64(&[0x80, 0x00], &mut pos).is_err());
        assert!(get_u64(&[0xff; 10], &mut pos).is_err());
        assert!(
            get_u64(
                &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02],
                &mut pos
            )
            .is_err()
        );
    }
}
