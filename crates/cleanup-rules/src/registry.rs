use crate::lexicon::LexiconFlagRule;
use crate::schema::{Action, Pack};
use crate::typographic::{NormalizeEllipsis, NormalizeEmDash, NormalizeQuotes, StripInvisibles};
use crate::unicode_hidden::{
    FlagBidiControls, NormalizeExoticSpaces, StripBidiOverrides, StripSmuggledChars,
};
use cleanup_core::Rule;

/// The built-in default template pack, embedded at compile time for zero-config first runs.
const CORE_PACK_YAML: &str = include_str!("../../../templates/core.yaml");

/// Parse and validate the embedded built-in pack.
pub fn load_builtin_pack() -> Result<Pack, String> {
    let pack: Pack = serde_norway::from_str(CORE_PACK_YAML)
        .map_err(|e| format!("failed to parse built-in template: {e}"))?;
    pack.validate()?;
    Ok(pack)
}

/// Build the active rule set: built-in typographic transformers (code) plus data-driven
/// lexical/phrasal flag rules constructed from the built-in template pack.
pub fn builtin_rules() -> Vec<Box<dyn Rule>> {
    let mut rules: Vec<Box<dyn Rule>> = vec![
        // Invisible-character hygiene + typographic normalization (built-in code rules).
        Box::new(StripInvisibles),
        Box::new(StripSmuggledChars),
        Box::new(NormalizeExoticSpaces),
        Box::new(StripBidiOverrides),
        Box::new(FlagBidiControls),
        Box::new(NormalizeQuotes),
        Box::new(NormalizeEllipsis),
        Box::new(NormalizeEmDash),
    ];

    if let Ok(pack) = load_builtin_pack() {
        for spec in pack.rules {
            // Scaffold scope: the engine materializes flag-only rules from any term list.
            // Replace/Delete actions and richer match kinds are future work (DESIGN.md §7).
            if spec.action == Action::Flag && !spec.terms.is_empty() {
                rules.push(Box::new(LexiconFlagRule::new(
                    spec.id,
                    spec.level,
                    spec.terms,
                    spec.description,
                )));
            }
        }
    }

    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_pack_parses_and_validates() {
        let pack = load_builtin_pack().expect("built-in pack should parse");
        assert_eq!(pack.schema_version, Pack::CURRENT_SCHEMA);
        assert!(!pack.rules.is_empty());
    }

    #[test]
    fn builtin_rules_includes_typographic_and_lexical() {
        let rules = builtin_rules();
        assert!(rules.iter().any(|r| r.id() == "typo.invisibles.strip"));
        assert!(rules.iter().any(|r| r.id().starts_with("lexical.")));
    }
}
