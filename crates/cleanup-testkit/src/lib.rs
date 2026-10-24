//! Helpers to run the full pipeline over in-memory documents and assert the invariants
//! that make the tool trustworthy: structure preservation, no-op byte-identity, idempotency.

use cleanup_core::{CleanReport, CleanupLevel, Ctx, Format, clean};
use cleanup_rules::builtin_rules;

/// Clean an in-memory document and return the output plus the full pipeline report, so tests
/// can assert on *flags* (what was reported) and not only on edits.
pub fn analyze(fmt: &dyn Format, input: &str, level: CleanupLevel) -> (String, CleanReport) {
    let doc = fmt.parse(input.as_bytes()).expect("parse");
    let rules = builtin_rules();
    let cx = Ctx::new(level);
    let (out, report) = clean(fmt, &doc, &rules, &cx).expect("clean");
    (String::from_utf8(out).expect("utf-8 output"), report)
}

/// Clean an in-memory document and return `(output_string, edits_applied)`.
pub fn clean_str(fmt: &dyn Format, input: &str, level: CleanupLevel) -> (String, usize) {
    let (out, report) = analyze(fmt, input, level);
    (out, report.edits_applied)
}

/// Assert that cleaning is idempotent: `clean(clean(x)) == clean(x)`.
pub fn assert_idempotent(fmt: &dyn Format, input: &str, level: CleanupLevel) {
    let (once, _) = clean_str(fmt, input, level);
    let (twice, _) = clean_str(fmt, &once, level);
    assert_eq!(once, twice, "cleaning is not idempotent");
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_formats::{Markdown, PlainText};

    #[test]
    fn markdown_preserves_structure_and_cleans_prose() {
        let input = "# Title\n\nHe said \u{201C}hi\u{201D} and left a \u{200B}zero-width space.\n\n```\ncode \u{201C}stays\u{201D} curly\n```\n";
        let (out, edits) = clean_str(&Markdown, input, CleanupLevel::Standard);

        assert!(out.contains("# Title"), "heading marker preserved");
        assert!(out.contains("\"hi\""), "smart quotes straightened in prose");
        assert!(
            out.contains("code \u{201C}stays\u{201D} curly"),
            "code block left completely untouched"
        );
        assert!(!out.contains('\u{200B}'), "zero-width space removed");
        assert!(edits >= 3, "expected >= 3 edits, got {edits}");
    }

    #[test]
    fn no_op_is_byte_identical() {
        let input = "Plain ascii text with no fingerprints at all.\n";
        let (out, edits) = clean_str(&PlainText, input, CleanupLevel::Light);
        assert_eq!(out, input);
        assert_eq!(edits, 0);
    }

    #[test]
    fn idempotent_typography() {
        let input =
            "Curly \u{201C}quotes\u{201D}, an apostrophe\u{2019}s, and an ellipsis\u{2026} here.\n";
        assert_idempotent(&PlainText, input, CleanupLevel::Aggressive);
    }

    #[test]
    fn light_level_leaves_quotes_but_strips_invisibles() {
        // Light must NOT touch visible punctuation, but MUST strip zero-width chars.
        let input = "Smart \u{201C}quotes\u{201D} stay\u{200B} at light.";
        let (out, _) = clean_str(&PlainText, input, CleanupLevel::Light);
        assert!(out.contains('\u{201C}'), "light keeps curly quotes");
        assert!(!out.contains('\u{200B}'), "light strips zero-width space");
    }

    #[test]
    fn strips_hidden_smuggling_and_watermark_chars() {
        // Tag char + zero-width + narrow no-break space (a GPT tell); a smuggled variation
        // selector after an ASCII letter is removed, but a legit emoji selector is preserved.
        let input = "Hi\u{E0041}\u{200B} there\u{202F}now a\u{FE0F} keep \u{2764}\u{FE0F} end.";
        let (out, _) = clean_str(&PlainText, input, CleanupLevel::Light);
        assert!(!out.contains('\u{E0041}'), "Unicode tag char stripped");
        assert!(!out.contains('\u{200B}'), "zero-width space stripped");
        assert!(
            !out.contains('\u{202F}'),
            "narrow no-break space normalized"
        );
        assert!(
            out.contains("\u{2764}\u{FE0F}"),
            "legitimate emoji variation selector preserved"
        );
        assert!(
            out.contains("a keep"),
            "smuggled variation selector after ASCII removed"
        );
    }

    #[test]
    fn strips_bidi_override_at_standard() {
        let input = "begin\u{202E}reversed end";
        let (out, _) = clean_str(&PlainText, input, CleanupLevel::Standard);
        assert!(
            !out.contains('\u{202E}'),
            "bidi override (Trojan Source) stripped"
        );
    }

    #[test]
    fn normalizes_exotic_spaces_at_standard() {
        let input = "thin\u{2009}space ideographic\u{3000}space.";
        let (out, _) = clean_str(&PlainText, input, CleanupLevel::Standard);
        assert!(!out.contains('\u{2009}'), "thin space normalized");
        assert!(!out.contains('\u{3000}'), "ideographic space normalized");
        assert!(
            out.contains("thin space"),
            "exotic space becomes a regular space"
        );
    }
    // -----------------------------------------------------------------------
    // October 2026 fingerprint coverage
    // -----------------------------------------------------------------------

    #[test]
    fn chat_template_tokens_are_flagged_then_removed() {
        let input = "Answer<|im_end|> done.\n";

        // Flagged, never silently deleted, at the light and standard levels.
        let (light_out, light_report) = analyze(&PlainText, input, CleanupLevel::Light);
        assert_eq!(light_out, input, "flags never edit the text");
        assert!(
            light_report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.chat-tokens"),
            "chat-template token should be reported"
        );

        // Removed at aggressive: it is a serialization artifact, never author text.
        let (out, edits) = clean_str(&PlainText, input, CleanupLevel::Aggressive);
        assert_eq!(out, "Answer done.\n");
        assert!(edits >= 1);
    }

    #[test]
    fn chatgpt_citation_artifacts_are_reported_not_removed() {
        let input = "Revenue grew\u{3010}12\u{2020}L20-L24\u{3011} in Q3.\n";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Aggressive);
        assert_eq!(out, input, "citation markers are reported for review only");
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.citation-brackets"),
            "expected a citation-bracket flag, got {:?}",
            report.flagged_by_rule
        );
    }

    #[test]
    fn agent_attribution_trailers_are_reported() {
        let input = "Fix the parser.\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Standard);
        assert_eq!(out, input);
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.agent-trailers")
        );
    }

    #[test]
    fn fullwidth_forms_normalize_at_aggressive_only() {
        let input = "Note\u{FF08}one\u{FF09}\u{FF0C}done.\n";
        let (standard_out, _) = clean_str(&PlainText, input, CleanupLevel::Standard);
        assert_eq!(
            standard_out, input,
            "CJK text may legitimately use fullwidth forms"
        );

        let (out, _) = clean_str(&PlainText, input, CleanupLevel::Aggressive);
        assert_eq!(out, "Note(one),done.\n");
    }

    #[test]
    fn mixed_script_homoglyphs_are_flagged() {
        let input = "Pay at p\u{0430}ypal today.\n";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Aggressive);
        assert_eq!(out, input, "mixed-script words are reported, not rewritten");
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "unicode.confusables.flag")
        );
    }

    #[test]
    fn reasoning_blocks_are_flagged_and_stay_untouched() {
        let input = "Answer. <think>let me reconsider</think>Done.\n";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Standard);
        assert_eq!(out, input);
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.reasoning-block")
        );
    }

    #[test]
    fn new_invisible_classes_are_stripped_at_light() {
        // Math-mode invisibles, a shorthand format control and a filler glyph.
        let input = "a\u{2062}b\u{034F}c\u{3164}d\n";
        let (out, _) = clean_str(&PlainText, input, CleanupLevel::Light);
        assert!(out.contains("ab"), "U+2062 removed: {out:?}");
        assert!(out.contains("bc"), "U+034F removed: {out:?}");
        // The filler is only stripped at standard (it can be legitimate in braille/CJK text).
        assert!(out.contains('\u{3164}'));
        let (standard_out, _) = clean_str(&PlainText, input, CleanupLevel::Standard);
        assert!(!standard_out.contains('\u{3164}'));
    }

    #[test]
    fn aggressive_cleaning_is_idempotent_on_a_mixed_fingerprint_document() {
        let input = "It's worth noting\u{202F}that\u{200B} we \u{201C}delve\u{201D} into\
                     \u{2014}revenue\u{2026} and 2019\u{2013}2024 too.\n";
        assert_idempotent(&PlainText, input, CleanupLevel::Aggressive);
    }

    #[test]
    fn clean_text_stays_byte_identical() {
        // A document with no fingerprints at all must come back untouched at every level.
        let input = "The parser reads a file and writes a file. Nothing else.\n";
        for level in [
            CleanupLevel::Light,
            CleanupLevel::Standard,
            CleanupLevel::Aggressive,
        ] {
            let (out, edits) = clean_str(&PlainText, input, level);
            assert_eq!(out, input, "level {level} changed clean text");
            assert_eq!(edits, 0, "level {level} applied edits to clean text");
        }
    }
}
