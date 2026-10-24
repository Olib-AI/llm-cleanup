//! A data-driven, flag-only rule built from a template's term list. Flag-only because single
//! "AI words" are ordinary language; we surface them for review and never auto-replace.

use aho_corasick::{AhoCorasick, MatchKind};
use cleanup_core::ir::ProseSpan;
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};

pub struct LexiconFlagRule {
    id: String,
    level: CleanupLevel,
    matcher: AhoCorasick,
    message: String,
}

impl LexiconFlagRule {
    pub fn new(id: String, level: CleanupLevel, terms: Vec<String>, message: String) -> Self {
        // Search over Unicode-lowercased text. Keep a boundary map back to the original UTF-8
        // so accented upper-case words (ÉLÉMENT, PANORAMA, etc.) can be matched without
        // corrupting reported byte ranges. Lowercasing may expand a character, so normalized
        // offsets must never be used directly against the source.
        let terms: Vec<String> = terms
            .iter()
            .map(|term| term.chars().flat_map(char::to_lowercase).collect())
            .collect();
        // LeftmostLongest so an inflected or phrasal term ("delves", "deep dive") wins over
        // a shorter nested term ("delve", "dive") starting at the same offset.
        let matcher = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&terms)
            .expect("lexicon terms are valid aho-corasick patterns");
        let message = if message.is_empty() {
            "possible AI-favored wording".to_string()
        } else {
            message
        };
        LexiconFlagRule {
            id,
            level,
            matcher,
            message,
        }
    }
}

/// Unicode lowercase text and an index for each normalized byte boundary pointing back to
/// the corresponding original byte boundary. Rule matches are guaranteed to begin/end on
/// UTF-8 boundaries, which is all the caller needs.
fn lowercase_with_source_map(text: &str) -> (String, Vec<usize>) {
    let mut lower = String::with_capacity(text.len());
    let mut source_boundaries = vec![0];
    for (source_start, ch) in text.char_indices() {
        let source_end = source_start + ch.len_utf8();
        let folded: String = ch.to_lowercase().collect();
        for (folded_start, folded_char) in folded.char_indices() {
            lower.push(folded_char);
            let folded_end = folded_start + folded_char.len_utf8();
            for byte_offset in 1..=folded_char.len_utf8() {
                let is_last_byte_of_folded_char = byte_offset == folded_char.len_utf8();
                let is_last_char_of_source_char = folded_end == folded.len();
                source_boundaries.push(
                    if is_last_byte_of_folded_char && is_last_char_of_source_char {
                        source_end
                    } else {
                        source_start
                    },
                );
            }
        }
    }
    (lower, source_boundaries)
}

impl Rule for LexiconFlagRule {
    fn id(&self) -> &str {
        &self.id
    }
    fn min_level(&self) -> CleanupLevel {
        self.level
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let (lower, source_boundaries) = lowercase_with_source_map(text);
        let mut out = Vec::new();
        for m in self.matcher.find_iter(&lower) {
            let start = source_boundaries[m.start()];
            let end = source_boundaries[m.end()];
            // A lowercase expansion (for example, U+0130) can contain a pattern only in
            // part of one source character. Such a match has no source-byte range to report.
            if start >= end {
                continue;
            }
            // Unicode-aware word boundaries: the chars flanking the match must not be
            // alphanumeric (so "delve" doesn't match inside "delved" or "vitalité").
            let before_ok = text[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            let after_ok = text[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            if before_ok && after_ok {
                out.push(Finding {
                    rule_id: self.id.clone(),
                    span_index: idx,
                    range: start..end,
                    message: format!("{}: \"{}\"", self.message, &text[start..end]),
                    replacement: None,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::ir::SpanAnchor;

    fn detect(rule: &LexiconFlagRule, text: &str) -> Vec<Finding> {
        let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text);
        rule.detect(0, &span, &Ctx::new(CleanupLevel::Aggressive))
    }

    #[test]
    fn longest_term_wins_so_inflections_match() {
        // With the default match kind, "delve" would match inside "delves", fail the
        // word-boundary check, and the inflected form would never be reported.
        let rule = LexiconFlagRule::new(
            "test.inflections".to_string(),
            CleanupLevel::Standard,
            vec!["delve".to_string(), "delves".to_string()],
            "test".to_string(),
        );
        let found = detect(&rule, "It delves, then we delve.");
        assert_eq!(found.len(), 2, "got {found:?}");
        assert_eq!(found[0].range, 3..9);
    }

    #[test]
    fn substrings_of_longer_words_do_not_match() {
        let rule = LexiconFlagRule::new(
            "test.boundaries".to_string(),
            CleanupLevel::Standard,
            vec!["delve".to_string()],
            "test".to_string(),
        );
        assert!(detect(&rule, "The delver indelves.").is_empty());
    }

    #[test]
    fn unicode_case_folding_preserves_source_ranges() {
        let rule = LexiconFlagRule::new(
            "test.french".to_string(),
            CleanupLevel::Standard,
            vec!["il est important de noter".to_string()],
            "test".to_string(),
        );
        let text = "ÉLÉMENT — IL EST IMPORTANT DE NOTER ceci.";
        let found = detect(&rule, text);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(&text[found[0].range.clone()], "IL EST IMPORTANT DE NOTER");
    }
}
