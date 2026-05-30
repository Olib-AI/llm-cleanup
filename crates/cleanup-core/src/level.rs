use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Cleanup aggressiveness. Ordering matters: `Light < Standard < Aggressive`, so a rule
/// whose minimum level is `min` is active whenever `current >= min`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupLevel {
    /// Invisible/zero-width character hygiene only. Safe to run on anything.
    Light,
    /// + visible-punctuation normalization and safe phrase handling.
    #[default]
    Standard,
    /// + stylistic flags and opt-in lexical/typographic rewriting.
    Aggressive,
}

impl CleanupLevel {
    /// True if a rule whose minimum level is `min` should run at `self`.
    pub fn includes(self, min: CleanupLevel) -> bool {
        self >= min
    }
}

impl fmt::Display for CleanupLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CleanupLevel::Light => "light",
            CleanupLevel::Standard => "standard",
            CleanupLevel::Aggressive => "aggressive",
        })
    }
}

impl FromStr for CleanupLevel {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "light" | "l" => Ok(CleanupLevel::Light),
            "standard" | "std" | "s" => Ok(CleanupLevel::Standard),
            "aggressive" | "aggr" | "a" => Ok(CleanupLevel::Aggressive),
            other => Err(format!("unknown cleanup level: {other}")),
        }
    }
}
