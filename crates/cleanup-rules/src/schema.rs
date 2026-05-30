use cleanup_core::CleanupLevel;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A template pack: a versioned, shareable set of detection rules. Authored in YAML
/// (primary) or JSON (interchange); both deserialize to this one model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pack {
    pub schema_version: u32,
    pub pack: PackMeta,
    #[serde(default)]
    pub rules: Vec<RuleSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackMeta {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub license: Option<String>,
}

/// One declarative rule. Unknown YAML keys (e.g. `severity`, `references`) are intentionally
/// ignored for forward-compatibility during the scaffold; a future build adds strict
/// `deny_unknown_fields` validation with line numbers (DESIGN.md §7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSpec {
    pub id: String,
    #[serde(default)]
    pub description: String,
    pub category: Category,
    #[serde(default)]
    pub level: CleanupLevel,
    #[serde(default)]
    pub action: Action,
    /// Term/phrase list for lexical & phrasal rules.
    #[serde(default)]
    pub terms: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Typographic,
    Lexical,
    Phrasal,
    Stylistic,
    Structural,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Detect and report only — never auto-edit (the honest default).
    #[default]
    Flag,
    Replace,
    Delete,
}

impl Pack {
    pub const CURRENT_SCHEMA: u32 = 1;

    /// Minimal semantic validation. Real builds add schema-validation with locations.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::CURRENT_SCHEMA {
            return Err(format!(
                "unsupported schema_version {} (this build supports {})",
                self.schema_version,
                Self::CURRENT_SCHEMA
            ));
        }
        let mut seen = HashSet::new();
        for r in &self.rules {
            if !seen.insert(r.id.as_str()) {
                return Err(format!("duplicate rule id: {}", r.id));
            }
        }
        Ok(())
    }
}
