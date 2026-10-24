//! Helpers to run the full pipeline over in-memory documents and assert the invariants
//! that make the tool trustworthy: structure preservation, no-op byte-identity, idempotency.

use cleanup_core::{CleanReport, CleanupLevel, Ctx, Format, clean};
use cleanup_rules::builtin_rules;

/// Clean an in-memory document and return the output plus the full pipeline report, so tests
/// can assert on *flags* (what was reported) and not only on edits.
pub fn analyze(fmt: &dyn Format, input: &str, level: CleanupLevel) -> (String, CleanReport) {
    let (out, report) = analyze_bytes(fmt, input.as_bytes(), level);
    (String::from_utf8(out).expect("utf-8 output"), report)
}

/// Byte-level [`analyze`], for binary container formats (DOCX).
pub fn analyze_bytes(
    fmt: &dyn Format,
    input: &[u8],
    level: CleanupLevel,
) -> (Vec<u8>, CleanReport) {
    let doc = fmt.parse(input).expect("parse");
    let rules = builtin_rules();
    let cx = Ctx::new(level);
    clean(fmt, &doc, &rules, &cx).expect("clean")
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
    use cleanup_formats::{Docx, Markdown, PlainText};

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
    fn claude_code_prompt_fingerprint_is_removed_at_light() {
        let input = "Prompt context.\nToday\u{02BC}s date is 2026/06/30.\nMore context.\n";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Light);
        assert_eq!(
            out,
            "Prompt context.\nToday's date is 2026-06-30.\nMore context.\n"
        );
        assert!(
            report
                .applied_by_rule
                .iter()
                .any(|(rule, _)| rule == "markers.claude-prompt-fingerprint")
        );
        assert_idempotent(&PlainText, input, CleanupLevel::Light);

        // Do not turn a general discussion of dates into a Claude prompt fingerprint.
        let prose = "Today’s date is June 30, 2026.\n";
        let (prose_out, _) = clean_str(&PlainText, prose, CleanupLevel::Light);
        assert_eq!(prose_out, prose);
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

    // -----------------------------------------------------------------------
    // Pattern-tier rules and the refreshed lexical pack
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_tokens_and_future_model_trailers_are_pattern_matched() {
        // A special token no catalogue lists yet still has the unmistakable shape.
        let input = "Output follows<|frobnicate_9|> now.\n";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Standard);
        assert_eq!(
            out, input,
            "pattern hits are flagged, not edited, below aggressive"
        );
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.chat-tokens.pattern"),
            "expected pattern flag, got {:?}",
            report.flagged_by_rule
        );

        // A trailer naming a model that did not exist when the literal list was written;
        // the same shape with a human name must not fire.
        let agent = "Fix bug.\n\nCo-Authored-By: GPT-9.9-Hyperion <bot@example.com>\n";
        let (_, report) = analyze(&PlainText, agent, CleanupLevel::Standard);
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.agent-trailer-lines"),
            "expected trailer-pattern flag, got {:?}",
            report.flagged_by_rule
        );
        let human = "Fix bug.\n\nCo-Authored-By: Jacqueline Cody <jacqueline@example.com>\n";
        let (_, report) = analyze(&PlainText, human, CleanupLevel::Standard);
        assert!(
            !report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.agent-trailer-lines"),
            "human trailer must not be flagged"
        );
    }

    #[test]
    fn refreshed_lexical_pack_flags_measured_tells_but_never_rewrites() {
        let input = "The tower serves as a testament to the region's vibrant tapestry, \
                     nestled in the heart of the valley.\n";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Aggressive);
        assert_eq!(out, input, "lexical findings are flag-only at every level");
        assert_eq!(report.edits_applied, 0);
        for expected in ["phrasal.copula-avoidance", "phrasal.promotional-puffery"] {
            assert!(
                report.flags.iter().any(|f| f.rule_id == expected),
                "expected {expected} flag, got {:?}",
                report.flagged_by_rule
            );
        }
    }

    #[test]
    fn aggressive_removes_current_citation_and_token_residue() {
        let input = "A<|header_start|>user<|header_end|> B \
                     C【cite|turn0block1|turn2url0】 D (oaicite:2){index=2}.\n";
        let (out, _) = clean_str(&PlainText, input, CleanupLevel::Aggressive);
        assert!(!out.contains("header_start"));
        assert!(!out.contains("turn0block"));
        assert!(!out.contains("oaicite"));
        assert!(out.contains('A') && out.contains('D'));
        assert_idempotent(&PlainText, input, CleanupLevel::Aggressive);
    }

    #[test]
    fn unknown_special_token_shapes_remain_report_only() {
        let input = "Quote <|future_token_9|> exactly.\n";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Aggressive);
        assert_eq!(out, input);
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.chat-tokens.pattern")
        );
    }

    #[test]
    fn unknown_pua_citation_payloads_remain_report_only() {
        let input = "keep \u{E200}entity\u{E202}turn0search0\u{E201} intact";
        let (out, report) = analyze(&PlainText, input, CleanupLevel::Aggressive);
        assert_eq!(
            out, input,
            "unknown envelope was partially deleted: {out:?}"
        );
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.private-use-citations")
        );
    }

    #[test]
    fn ansi_sequences_are_removed_without_leaving_parameters() {
        let input = "plain \u{1b}[31mred\u{1b}[0m and \u{1b}]0;title\u{7}done\n";
        let (out, _) = clean_str(&PlainText, input, CleanupLevel::Light);
        assert_eq!(out, "plain red and done\n");
        let malformed = "before \u{1b}]0;unterminated visible text";
        let (out, _) = clean_str(&PlainText, malformed, CleanupLevel::Light);
        assert!(out.contains("visible text"));
    }

    #[test]
    fn markdown_link_destinations_scrub_ai_attribution_only() {
        let input = "[source](https://e.test/?a=1&utm_source=chatgpt.com&b=2#frag)\n\n\
                     <https://e.test/?utm_source=copilot.com>\n\n\
                     `[code](https://e.test/?utm_source=chatgpt.com)`\n\n\
                     [multiline](\n  <https://e.test/?utm_source=chatgpt.com>\n)\n";
        let (out, report) = analyze(&Markdown, input, CleanupLevel::Aggressive);
        assert!(
            out.contains("[source](https://e.test/?a=1&b=2#frag)"),
            "{out}"
        );
        assert_eq!(out.matches("<https://e.test/>").count(), 2, "{out}");
        assert!(out.contains("`[code](https://e.test/?utm_source=chatgpt.com)`"));
        assert!(report.container.iter().any(|c| c.applied));
        assert_idempotent(&Markdown, input, CleanupLevel::Aggressive);
    }

    // -----------------------------------------------------------------------
    // DOCX package coverage: prose everywhere + generator metadata
    // -----------------------------------------------------------------------

    use std::io::{Cursor, Read, Write};

    fn make_docx_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            for (name, xml) in entries {
                w.start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
                w.write_all(xml.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf
    }

    fn docx_part_text(zip_bytes: &[u8], part: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes)).unwrap();
        let mut s = String::new();
        archive
            .by_name(part)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        s
    }

    fn pt(text: &str) -> String {
        format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
    }

    fn wrap(root: &str, inner: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><{root} xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</{root}>"#
        )
    }

    #[test]
    fn docx_headers_footnotes_and_comments_are_cleaned() {
        let body = wrap("w:document", &pt("Body \u{201C}quote\u{201D} here."));
        let header = wrap("w:hdr", &pt("Head\u{200B}er \u{201C}text\u{201D}."));
        let footnotes = wrap(
            "w:footnotes",
            &format!(
                "<w:footnote w:id=\"1\">{}</w:footnote>",
                pt("Note \u{201C}one\u{201D}.")
            ),
        );
        let styles = "<w:styles><w:style w:styleId=\"X\"/></w:styles>";
        let bytes = make_docx_bytes(&[
            ("word/document.xml", &body),
            ("word/header1.xml", &header),
            ("word/footnotes.xml", &footnotes),
            ("word/styles.xml", styles),
        ]);

        let (out, report) = analyze_bytes(&Docx, &bytes, CleanupLevel::Standard);
        assert!(report.changed && report.edits_applied >= 4, "{report:?}");
        // Smart quotes straightened and the ZWSP gone in every prose part.
        for part in [
            "word/document.xml",
            "word/header1.xml",
            "word/footnotes.xml",
        ] {
            let xml = docx_part_text(&out, part);
            assert!(!xml.contains('\u{201C}'), "{part} still has curly quotes");
            assert!(
                !xml.contains('\u{200B}'),
                "{part} still has a zero-width space"
            );
        }
        // A non-prose part passes through byte-for-byte.
        assert_eq!(docx_part_text(&out, "word/styles.xml"), styles);

        // …and cleaning the cleaned file is a no-op.
        let (_, again) = analyze_bytes(&Docx, &out, CleanupLevel::Standard);
        assert!(!again.changed, "second pass should find nothing: {again:?}");
    }

    #[test]
    fn docx_escaped_chat_tokens_are_cleaned_without_touching_xml_structure() {
        let body = wrap("w:document", &pt("Before &lt;|im_end|&gt; after."));
        let bytes = make_docx_bytes(&[("word/document.xml", &body)]);
        let (out, report) = analyze_bytes(&Docx, &bytes, CleanupLevel::Aggressive);
        assert!(report.edits_applied >= 1, "{report:?}");
        let xml = docx_part_text(&out, "word/document.xml");
        assert!(!xml.contains("im_end"), "{xml}");
        assert!(xml.contains("Before  after."), "{xml}");
        assert!(xml.contains("<w:r><w:t>"), "run structure retained");
        let (_, again) = analyze_bytes(&Docx, &out, CleanupLevel::Aggressive);
        assert!(!again.changed, "{again:?}");
    }

    #[test]
    fn docx_external_hyperlinks_scrub_ai_attribution_only() {
        let body = wrap("w:document", &pt("A linked source."));
        let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://e.test/?a=1&amp;utm_source=chatgpt.com&amp;b=2#frag" TargetMode="External"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://e.test/?utm_source=newsletter" TargetMode="External"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="https://e.test/?utm_source=chatgpt.com" TargetMode="External"/></Relationships>"#;
        let bytes = make_docx_bytes(&[
            ("word/document.xml", &body),
            ("word/_rels/document.xml.rels", rels),
        ]);
        let (out, report) = analyze_bytes(&Docx, &bytes, CleanupLevel::Aggressive);
        assert!(report.container.iter().any(|c| c.applied), "{report:?}");
        let scrubbed = docx_part_text(&out, "word/_rels/document.xml.rels");
        assert!(
            scrubbed.contains("Target=\"https://e.test/?a=1&amp;b=2#frag\""),
            "{scrubbed}"
        );
        assert!(scrubbed.contains("Target=\"https://e.test/?utm_source=newsletter\""));
        assert!(scrubbed.contains("Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"https://e.test/?utm_source=chatgpt.com\""));
        let (_, again) = analyze_bytes(&Docx, &out, CleanupLevel::Aggressive);
        assert!(!again.changed, "{again:?}");
    }

    #[test]
    fn docx_generator_metadata_reports_then_scrubs_only_at_aggressive() {
        let body = wrap("w:document", &pt("A perfectly plain paragraph."));
        let core = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/"><dc:title>Report</dc:title><dc:creator>python-docx</dc:creator><dc:description>generated by python-docx</dc:description><dcterms:created>2013-12-23T23:15:00Z</dcterms:created></cp:coreProperties>"#;
        let bytes = make_docx_bytes(&[("word/document.xml", &body), ("docProps/core.xml", core)]);

        // Standard: reported, not touched — and not counted as a change.
        let (out, report) = analyze_bytes(&Docx, &bytes, CleanupLevel::Standard);
        assert_eq!(report.container.len(), 3, "{report:?}");
        assert!(report.container.iter().all(|c| !c.applied));
        assert!(
            !report.changed,
            "metadata flags alone must not mark a change"
        );
        assert_eq!(docx_part_text(&out, "docProps/core.xml"), core);

        // Aggressive: fields emptied, flagged as a change with zero prose edits.
        let (out, report) = analyze_bytes(&Docx, &bytes, CleanupLevel::Aggressive);
        assert_eq!(report.edits_applied, 0, "body prose was clean");
        assert!(report.changed, "a scrub is a change: {report:?}");
        assert_eq!(report.container.iter().filter(|c| c.applied).count(), 2);
        let core_out = docx_part_text(&out, "docProps/core.xml");
        assert!(!core_out.contains("python-docx"), "{core_out}");
        assert!(
            core_out.contains("<dc:title>Report</dc:title>"),
            "title kept"
        );
        assert!(
            core_out.contains("2013-12-23T23:15:00Z"),
            "the template date is report-only, never rewritten"
        );
        assert_eq!(docx_part_text(&out, "word/document.xml"), body);

        // Scrubbing is idempotent: only the report-only date still shows up.
        let (_, again) = analyze_bytes(&Docx, &out, CleanupLevel::Aggressive);
        assert!(!again.changed, "{again:?}");
        assert!(again.container.iter().all(|c| !c.scrubbable));
    }

    #[test]
    fn repairs_mojibake_and_html_entities_end_to_end() {
        // "it’s fine — really" after a Windows-1252 mis-decode, plus a copy-as-source entity.
        let input = "it\u{E2}\u{20AC}\u{2122}s fine &mdash; really";
        let (out, _) = analyze(&PlainText, input, CleanupLevel::Aggressive);
        assert_eq!(out, "it's fine, really");
        assert_idempotent(&PlainText, input, CleanupLevel::Aggressive);
    }

    #[test]
    fn strips_chat_ui_disclaimer_only_at_aggressive() {
        let input = "The answer.\n\nChatGPT can make mistakes. Check important info.\n";
        let (standard, report) = analyze(&PlainText, input, CleanupLevel::Standard);
        assert!(standard.contains("ChatGPT can make mistakes"));
        assert!(
            report
                .flags
                .iter()
                .any(|f| f.rule_id == "markers.chat-ui-disclaimer"),
            "{:?}",
            report.flags
        );
        let (aggressive, _) = analyze(&PlainText, input, CleanupLevel::Aggressive);
        assert!(!aggressive.contains("ChatGPT can make mistakes"));
        assert!(aggressive.starts_with("The answer."));
    }

    #[test]
    fn markdown_inline_header_lists_are_reported_but_never_edited() {
        let input = "- **Speed:** faster\n- **Cost:** lower\n- **Risk:** managed\n";
        let (out, report) = analyze(&Markdown, input, CleanupLevel::Standard);
        assert_eq!(out, input, "structural findings are flag-only");
        assert!(
            report
                .container
                .iter()
                .any(|c| c.rule_id == "style.list.inline-headers"),
            "{:?}",
            report.container
        );
    }
}
