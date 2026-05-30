//! Helpers to run the full pipeline over in-memory documents and assert the invariants
//! that make the tool trustworthy: structure preservation, no-op byte-identity, idempotency.

use cleanup_core::{CleanupLevel, Ctx, Format, clean};
use cleanup_rules::builtin_rules;

/// Clean an in-memory document and return `(output_string, edits_applied)`.
pub fn clean_str(fmt: &dyn Format, input: &str, level: CleanupLevel) -> (String, usize) {
    let doc = fmt.parse(input.as_bytes()).expect("parse");
    let rules = builtin_rules();
    let cx = Ctx::new(level);
    let (out, report) = clean(fmt, &doc, &rules, &cx).expect("clean");
    (
        String::from_utf8(out).expect("utf-8 output"),
        report.edits_applied,
    )
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
}
