use crate::lexicon::LexiconFlagRule;
use crate::markers::{
    AgentTrailerLineRule, AiTrackingUrlFlagRule, AiTrackingUrlStripRule, C2paTextWrapperRule,
    CitationBracketRule, CitationBracketStripRule, ClaudePromptFingerprintRule,
    LegacyCitationStripRule, ReasoningBlockRule, ReasoningTraceRule, SpecialTokenPatternRule,
    agent_trailers, c2pa_structured_manifest, c2pa_structured_manifest_strip, chat_tokens,
    chat_tokens_strip, chat_ui_disclaimers, chat_ui_disclaimers_strip, chat_ui_residue,
    citation_artifacts, gemini_citations, gemini_citations_strip, model_identifiers,
    private_use_citations, private_use_citations_strip, tool_calls, turn_citations,
    turn_citations_strip,
};
use crate::repair::{DecodeHtmlEntities, FlagHtmlTags, RepairMojibake};
use crate::schema::Pack;
use crate::style::{
    DecorativeEmojiOpener, DetokenizedSpacing, EmojiDensity, InlineHeaderList, MarkdownResidue,
    PlaceholderResidue, RepeatedPunctuation, TitleCaseHeading,
};
use crate::typographic::{
    ExpandLigatures, NormalizeCjkPunctuation, NormalizeDetokenizer, NormalizeEllipsis,
    NormalizeEmDash, NormalizeEnDash, NormalizeFullwidthForms, NormalizeHyphens, NormalizeQuotes,
    StripInvisibles,
};
use crate::unicode_hidden::{
    FlagBidiControls, FlagCombiningRuns, FlagConfusables, FlagMathStyledText, FlagPrivateUse,
    FlagWhitespaceStego, FlagZeroWidthJoiners, NormalizeExoticSpaces, NormalizeMathStyledText,
    StripAnsiEscapes, StripBidiOverrides, StripControlChars, StripFillers, StripPrivateUseMarkers,
    StripSmuggledChars, StripWhitespaceStego, StripZeroWidthJoiners,
};
use crate::watermark::WatermarkDisruptRule;
use cleanup_core::Rule;

/// The built-in default template pack, embedded at compile time for zero-config first runs.
const CORE_PACK_YAML: &str = include_str!("../../../templates/core.yaml");

/// Parse and validate a YAML or JSON rule pack from text.
pub fn load_pack_str(content: &str) -> Result<Pack, String> {
    let trimmed = content.trim_start();
    let pack: Pack = if trimmed.starts_with('{') {
        serde_json::from_str(content).map_err(|e| format!("failed to parse JSON pack: {e}"))?
    } else {
        serde_norway::from_str(content).map_err(|e| format!("failed to parse YAML pack: {e}"))?
    };
    pack.validate()?;
    Ok(pack)
}

/// Parse and validate the embedded built-in pack.
pub fn load_builtin_pack() -> Result<Pack, String> {
    load_pack_str(CORE_PACK_YAML)
}

/// Materialize executable rules from a validated [`Pack`] (`flag`, `replace`, and `delete`).
pub fn rules_from_pack(pack: &Pack) -> Vec<Box<dyn Rule>> {
    pack.rules
        .iter()
        .cloned()
        .filter_map(LexiconFlagRule::from_spec)
        .map(|r| Box::new(r) as Box<dyn Rule>)
        .collect()
}

/// Build the active built-in rule set plus any user-supplied packs (`--pack`).
pub fn rules_with_packs(extra_packs: &[Pack]) -> Vec<Box<dyn Rule>> {
    let mut rules = builtin_rules();
    for pack in extra_packs {
        rules.extend(rules_from_pack(pack));
    }
    rules
}

/// Build the active rule set:
///
/// 1. whole-frame C2PA unstructured-text manifest decoding & encoding repairs,
/// 2. character-level transformers (invisible hygiene, typography, Unicode smuggling,
///    whitespace steganography, Mathematical Alphanumeric normalization),
/// 3. exact-string signature detectors (chat tokens, citation artifacts, Gemini grounding,
///    C2PA structured-text banners, agent trailers, reasoning traces),
/// 4. formatting tells (decorative emoji, raw Markdown, repeated punctuation),
/// 5. opt-in statistical watermark disruption (`watermark.ngram-disrupt`, active only when
///    `Ctx::dewatermark` is enabled),
/// 6. data-driven lexical/phrasal rules constructed from `templates/core.yaml`.
pub fn builtin_rules() -> Vec<Box<dyn Rule>> {
    let mut rules: Vec<Box<dyn Rule>> = vec![
        // Whole-frame C2PA unstructured-text wrapper runs before single-char BOM/VS rules so
        // `[U+FEFF]? + VS_run("C2PATXT\0"...)` is reported and stripped as a single artifact.
        Box::new(C2paTextWrapperRule),
        // Encoding repairs run next: mojibake and HTML entities hide the real characters.
        Box::new(RepairMojibake),
        Box::new(DecodeHtmlEntities),
        // Invisible-character hygiene + typographic normalization (built-in code rules).
        Box::new(StripInvisibles),
        Box::new(StripAnsiEscapes),
        Box::new(StripControlChars),
        Box::new(StripSmuggledChars),
        Box::new(StripFillers),
        Box::new(NormalizeExoticSpaces),
        Box::new(StripBidiOverrides),
        Box::new(FlagBidiControls),
        Box::new(FlagPrivateUse),
        Box::new(StripPrivateUseMarkers),
        Box::new(FlagCombiningRuns),
        Box::new(FlagZeroWidthJoiners),
        Box::new(StripZeroWidthJoiners),
        Box::new(FlagWhitespaceStego),
        Box::new(StripWhitespaceStego),
        Box::new(FlagMathStyledText),
        Box::new(NormalizeMathStyledText),
        Box::new(FlagConfusables),
        // Opt-in statistical watermark disruption (dormant unless `cx.dewatermark == true`).
        // Placed ahead of `NormalizeQuotes` so curly-apostrophe contractions (`don’t` -> `do not`)
        // resolve cleanly in a single pass when opted in.
        Box::new(WatermarkDisruptRule::new()),
        Box::new(NormalizeQuotes),
        Box::new(ExpandLigatures),
        Box::new(NormalizeEllipsis),
        Box::new(NormalizeHyphens),
        Box::new(NormalizeEnDash),
        Box::new(NormalizeEmDash),
        Box::new(NormalizeFullwidthForms),
        Box::new(NormalizeCjkPunctuation),
        Box::new(NormalizeDetokenizer),
        // Machine signatures (exact strings, no word-boundary heuristics).
        Box::new(chat_tokens()),
        Box::new(SpecialTokenPatternRule),
        Box::new(citation_artifacts()),
        Box::new(CitationBracketRule),
        Box::new(private_use_citations()),
        Box::new(turn_citations()),
        Box::new(gemini_citations()),
        Box::new(gemini_citations_strip()),
        Box::new(c2pa_structured_manifest()),
        Box::new(c2pa_structured_manifest_strip()),
        Box::new(AiTrackingUrlFlagRule),
        Box::new(AiTrackingUrlStripRule),
        Box::new(private_use_citations_strip()),
        Box::new(turn_citations_strip()),
        Box::new(CitationBracketStripRule),
        Box::new(LegacyCitationStripRule),
        Box::new(ClaudePromptFingerprintRule),
        Box::new(agent_trailers()),
        Box::new(AgentTrailerLineRule),
        Box::new(tool_calls()),
        Box::new(ReasoningBlockRule),
        Box::new(ReasoningTraceRule),
        Box::new(chat_tokens_strip()),
        Box::new(model_identifiers()),
        Box::new(chat_ui_disclaimers()),
        Box::new(chat_ui_residue()),
        Box::new(chat_ui_disclaimers_strip()),
        Box::new(FlagHtmlTags),
        // Formatting / layout tells.
        Box::new(DecorativeEmojiOpener),
        Box::new(EmojiDensity),
        Box::new(MarkdownResidue),
        Box::new(RepeatedPunctuation),
        Box::new(DetokenizedSpacing),
        Box::new(PlaceholderResidue),
        Box::new(TitleCaseHeading),
        Box::new(InlineHeaderList),
    ];

    if let Ok(pack) = load_builtin_pack() {
        rules.extend(rules_from_pack(&pack));
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
        assert_eq!(
            before,
            ids.len(),
            "duplicate rule id in the built-in registry"
        );
    }

    #[test]
    fn mixed_french_and_spanish_prose_uses_both_language_packs() {
        use cleanup_core::ir::{ProseSpan, SpanAnchor};
        use cleanup_core::{CleanupLevel, Ctx};

        let text = "Il est important de noter ceci. Es importante señalar esto.";
        let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text);
        let cx = Ctx::new(CleanupLevel::Standard);
        let rules = builtin_rules();
        let ids: Vec<String> = rules
            .iter()
            .filter(|rule| {
                rule.id() == "phrasal.fr.assistant-formulas"
                    || rule.id() == "phrasal.es.assistant-formulas"
            })
            .flat_map(|rule| rule.detect(0, &span, &cx))
            .map(|finding| finding.rule_id)
            .collect();
        assert_eq!(ids.len(), 2, "both language-specific cues should be found");
        assert!(ids.iter().any(|id| id == "phrasal.fr.assistant-formulas"));
        assert!(ids.iter().any(|id| id == "phrasal.es.assistant-formulas"));
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
