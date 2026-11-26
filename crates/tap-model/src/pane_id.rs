//! Pane identifiers, which double as file names inside a package.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Longest pane id accepted, in bytes.
pub const MAX_PANE_ID_LEN: usize = 64;

/// A pane identifier that is safe to use as a file name on every platform.
///
/// A pane's raw events live in `events/<id>.tlog`, so ids are restricted to
/// names no file system will reinterpret:
///
/// - 1 to [`MAX_PANE_ID_LEN`] bytes of ASCII letters, digits, `.`, `_` and
///   `-`;
/// - starting with a letter or digit and not ending with `.` (Windows drops
///   trailing dots);
/// - not a Windows device name (`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`,
///   `LPT0`–`LPT9`), in any case and with or without an extension.
///
/// Ids are compared exactly, but a session rejects two ids that differ only
/// in case, because macOS and Windows file systems would store them in the
/// same file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(String);

impl PaneId {
    /// Validates `id` and wraps it.
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        validate(&id)?;
        Ok(Self(id))
    }

    /// The id as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name of this pane's event log inside `events/`.
    pub fn tlog_file_name(&self) -> String {
        format!("{}.tlog", self.0)
    }
}

fn validate(id: &str) -> Result<(), String> {
    let quoted = || format!("pane id {id:?}");
    if id.is_empty() {
        return Err("pane id must not be empty".into());
    }
    if id.len() > MAX_PANE_ID_LEN {
        return Err(format!(
            "{} is longer than {MAX_PANE_ID_LEN} bytes",
            quoted()
        ));
    }
    if let Some(bad) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "{} contains {bad:?}; pane ids may only use ASCII letters, digits, '.', '_' and '-'",
            quoted()
        ));
    }
    if !id.as_bytes()[0].is_ascii_alphanumeric() {
        return Err(format!("{} must start with a letter or digit", quoted()));
    }
    if id.ends_with('.') {
        return Err(format!("{} must not end with '.'", quoted()));
    }
    let stem = id.split('.').next().unwrap_or(id).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(format!("{} is a reserved device name on Windows", quoted()));
    }
    Ok(())
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PaneId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for PaneId {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for PaneId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PaneId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::new(id).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::PaneId;

    #[test]
    fn accepts_ordinary_ids() {
        for id in [
            "main", "left", "right", "pane-2", "a.b_c", "0", "con1", "COM10",
        ] {
            assert!(PaneId::new(id).is_ok(), "{id}");
        }
    }

    #[test]
    fn rejects_unsafe_ids() {
        for id in [
            "", ".", "..", ".hidden", "-x", "a/b", "a\\b", "a b", "trail.", "CON", "nul.txt",
            "com1", "Lpt9.log", "é", "a:b",
        ] {
            assert!(PaneId::new(id).is_err(), "{id:?} should be rejected");
        }
        assert!(PaneId::new("x".repeat(65)).is_err());
        assert!(PaneId::new("x".repeat(64)).is_ok());
    }
}
