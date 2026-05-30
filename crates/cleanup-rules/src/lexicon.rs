//! A data-driven, flag-only rule built from a template's term list. Flag-only because single
//! "AI words" are ordinary English; we surface them for review and never auto-replace.

use aho_corasick::AhoCorasick;
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
        // ASCII-case-insensitive multi-term matcher over the ORIGINAL text — correct byte
        // offsets, linear time, and (unlike the old lowercase approach) no whole-span bail-out
        // when the text contains a non-ASCII character. Our shipped term lists are ASCII.
        let matcher = AhoCorasick::builder()
            .ascii_case_insensitive(true)
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

impl Rule for LexiconFlagRule {
    fn id(&self) -> &str {
        &self.id
    }
    fn min_level(&self) -> CleanupLevel {
        self.level
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        for m in self.matcher.find_iter(text) {
            let (start, end) = (m.start(), m.end());
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
