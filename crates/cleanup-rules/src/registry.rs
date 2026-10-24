use crate::lexicon::LexiconFlagRule;
use crate::markers::{
    CitationBracketRule, ReasoningBlockRule, ReasoningTraceRule, agent_trailers, chat_tokens,
    chat_tokens_strip, citation_artifacts, model_identifiers,
};
use crate::schema::{Action, Pack};
use crate::style::{DecorativeEmojiOpener, EmojiDensity, MarkdownResidue, RepeatedPunctuation};
use crate::typographic::{
    NormalizeEllipsis, NormalizeEmDash, NormalizeEnDash, NormalizeFullwidthForms,
    NormalizeHyphens, NormalizeQuotes, StripInvisibles,
};
use crate::unicode_hidden::{
    FlagBidiControls, FlagConfusables, NormalizeExoticSpaces, StripBidiOverrides, StripFillers,
    StripSmuggledChars,
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

/// Build the active rule set:
///
/// 1. character-level transformers (invisible hygiene, typography, Unicode smuggling),
/// 2. exact-string signature detectors (chat tokens, citation artifacts, agent trailers,
///    reasoning traces),
/// 3. formatting tells (decorative emoji, raw Markdown, repeated punctuation),
/// 4. data-driven lexical/phrasal flag rules constructed from the built-in template pack.
///
/// Order matters only for conflict resolution: when two rules propose overlapping edits on
/// the same span, the earlier rule wins (`cleanup_core::pipeline`). Character rules run first
/// so that deleting an invisible character never blocks a punctuation normalization.
pub fn builtin_rules() -> Vec<Box<dyn Rule>> {
    let mut rules: Vec<Box<dyn Rule>> = vec![
        // Invisible-character hygiene + typographic normalization (built-in code rules).
        Box::new(StripInvisibles),
        Box::new(StripSmuggledChars),
        Box::new(StripFillers),
        Box::new(NormalizeExoticSpaces),
        Box::new(StripBidiOverrides),
        Box::new(FlagBidiControls),
        Box::new(FlagConfusables),
        Box::new(NormalizeQuotes),
        Box::new(NormalizeEllipsis),
        Box::new(NormalizeHyphens),
        Box::new(NormalizeEnDash),
        Box::new(NormalizeEmDash),
        Box::new(NormalizeFullwidthForms),
        // Machine signatures (exact strings, no word-boundary heuristics).
        Box::new(chat_tokens()),
        Box::new(citation_artifacts()),
        Box::new(CitationBracketRule),
        Box::new(agent_trailers()),
        Box::new(ReasoningBlockRule),
        Box::new(ReasoningTraceRule),
        Box::new(chat_tokens_strip()),
        Box::new(model_identifiers()),
        // Formatting / layout tells.
        Box::new(DecorativeEmojiOpener),
        Box::new(EmojiDensity),
        Box::new(MarkdownResidue),
        Box::new(RepeatedPunctuation),
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
    use crate::catalog;

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

    #[test]
    fn rule_ids_are_unique() {
        let rules = builtin_rules();
        let mut ids: Vec<&str> = rules.iter().map(|r| r.id()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate rule id in the built-in registry");
    }

    #[test]
    fn every_level_has_rules() {
        let rules = builtin_rules();
        for level in [
            cleanup_core::CleanupLevel::Light,
            cleanup_core::CleanupLevel::Standard,
            cleanup_core::CleanupLevel::Aggressive,
        ] {
            assert!(
                rules.iter().any(|r| r.min_level() == level),
                "no rule is anchored at level {level}"
            );
        }
    }

    /// The catalogue is user-facing documentation; it must not name a rule that does not
    /// exist. This is the guard against "we documented it but never implemented it".
    #[test]
    fn catalog_references_known_rules() {
        let rules = builtin_rules();
        let ids: Vec<String> = rules.iter().map(|r| r.id().to_string()).collect();
        for family in catalog::coverage() {
            for rule in family.rules.split(',') {
                let rule = rule.trim();
                if rule.is_empty() {
                    continue;
                }
                assert!(
                    ids.iter().any(|id| id == rule),
                    "catalogue entry {:?} references unknown rule {:?}",
                    family.id,
                    rule
                );
            }
        }
    }
}
