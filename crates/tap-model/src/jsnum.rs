//! Serialises `f64` the way JavaScript's `JSON.stringify` does, so integral
//! values are written as `16`, not `16.0`, and documents round-trip through
//! the TypeScript tools unchanged.

use serde::Serializer;

/// Largest integer JavaScript represents exactly (2^53 - 1).
pub(crate) const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

pub(crate) fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    if value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER as f64 {
        serializer.serialize_i64(*value as i64)
    } else {
        serializer.serialize_f64(*value)
    }
}

pub(crate) mod option {
    use serde::Serializer;

    pub(crate) fn serialize<S: Serializer>(
        value: &Option<f64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(v) => super::serialize(v, serializer),
            None => serializer.serialize_none(),
        }
    }
}
