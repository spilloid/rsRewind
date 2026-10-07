//! Which installation a piece of history came from.
//!
//! Every store has a random 128-bit source id (`docs/design/distributed.md` §3.5), written as 32
//! lowercase hex characters. Query results carry `Option<SourceId>`: `None` is this machine's own
//! history, `Some` a replica imported from another rsRewind. The id is identity only; a source's
//! label (usually a host name) is display text and is never used for attribution.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// A source id. `Copy`, totally ordered (byte order, which is also the order of its hex text),
/// so it can sit inside a [`crate::TimelineCursor`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId([u8; 16]);

impl SourceId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Exactly 32 lowercase hex characters, or `None`. Strict on purpose: the id is also a
    /// directory name under `sources/`, and two spellings of one id would be two lanes.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.as_bytes();
        if digits.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (byte, [high, low]) in bytes.iter_mut().zip(digits.as_chunks::<2>().0) {
            *byte = (hex_value(*high)? << 4) | hex_value(*low)?;
        }
        Some(Self(bytes))
    }

    /// The first 8 hex characters, for compact display next to a label.
    pub fn short(&self) -> String {
        self.to_string().chars().take(8).collect()
    }
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SourceId({self})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a source id is exactly 32 lowercase hex characters")]
pub struct InvalidSourceId;

impl FromStr for SourceId {
    type Err = InvalidSourceId;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text).ok_or(InvalidSourceId)
    }
}

impl Serialize for SourceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SourceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| serde::de::Error::custom(InvalidSourceId))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "00112233445566778899aabbccddeeff";

    #[test]
    fn round_trips_through_text_and_json() -> Result<(), Box<dyn std::error::Error>> {
        let id = SourceId::parse(HEX).ok_or("parse")?;
        assert_eq!(id.to_string(), HEX);
        assert_eq!(id.short(), "00112233");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, format!("\"{HEX}\""));
        assert_eq!(serde_json::from_str::<SourceId>(&json)?, id);
        Ok(())
    }

    #[test]
    fn only_the_canonical_spelling_parses() {
        for bad in [
            "",
            "0011",
            "00112233445566778899AABBCCDDEEFF",
            "00112233445566778899aabbccddeef",
            "00112233445566778899aabbccddeeff0",
            "0011223344556677889-aabbccddeeff",
            "../../../../../../../../../etc/x",
            "g0112233445566778899aabbccddeeff",
        ] {
            assert_eq!(SourceId::parse(bad), None, "{bad}");
        }
        assert!(serde_json::from_str::<SourceId>("\"../x\"").is_err());
    }

    #[test]
    fn order_matches_the_hex_text() {
        let mut ids: Vec<SourceId> = ["ff", "0a", "a0", "00"]
            .iter()
            .filter_map(|p| SourceId::parse(&format!("{p}{}", &HEX[2..])))
            .collect();
        ids.sort();
        let text: Vec<String> = ids.iter().map(|i| i.to_string()[..2].to_owned()).collect();
        assert_eq!(text, ["00", "0a", "a0", "ff"]);
    }
}
