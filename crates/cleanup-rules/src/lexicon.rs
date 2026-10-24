//! Declarative lexicon / phrase rule built from a template's term list (`templates/core.yaml`
//! or user-supplied `--pack` files).
//!
//! Built-in lexical rules in `core.yaml` use `action: flag` (single "AI words" are ordinary
//! language, so built-in rules surface them for review without rewriting). Custom packs may
//! also use `action: replace` (with `replacement` or `replacements`) and `action: delete`.

use crate::schema::{Action, RuleSpec};
use aho_corasick::{AhoCorasick, MatchKind};
use cleanup_core::ir::ProseSpan;
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};

pub struct LexiconFlagRule {
    id: String,
    level: CleanupLevel,
    matcher: AhoCorasick,
    pattern_targets: Vec<Option<String>>,
    preserve_case: bool,
    message: String,
}

impl LexiconFlagRule {
    pub fn new(id: String, level: CleanupLevel, terms: Vec<String>, message: String) -> Self {
        let lowered: Vec<String> = terms
            .iter()
            .map(|term| term.chars().flat_map(char::to_lowercase).collect())
            .collect();
        let pattern_targets = vec![None; lowered.len()];
        let matcher = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&lowered)
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
            pattern_targets,
            preserve_case: false,
            message,
        }
    }

    pub fn from_spec(spec: RuleSpec) -> Option<Self> {
        let mut patterns: Vec<String> = Vec::new();
        let mut pattern_targets: Vec<Option<String>> = Vec::new();

        match spec.action {
            Action::Flag => {
                for term in &spec.terms {
                    patterns.push(term.chars().flat_map(char::to_lowercase).collect());
                    pattern_targets.push(None);
                }
            }
            Action::Delete => {
                for term in &spec.terms {
                    patterns.push(term.chars().flat_map(char::to_lowercase).collect());
                    pattern_targets.push(Some(String::new()));
                }
            }
            Action::Replace => {
                for (src_term, dst_term) in &spec.replacements {
                    patterns.push(src_term.chars().flat_map(char::to_lowercase).collect());
                    pattern_targets.push(Some(dst_term.clone()));
                }
                if let Some(uniform) = &spec.replacement {
                    for term in &spec.terms {
                        patterns.push(term.chars().flat_map(char::to_lowercase).collect());
                        pattern_targets.push(Some(uniform.clone()));
                    }
                }
            }
        }

        if patterns.is_empty() {
            return None;
        }
        let matcher = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&patterns)
            .expect("lexicon terms are valid aho-corasick patterns");
        let message = if spec.description.is_empty() {
            "possible AI-favored wording".to_string()
        } else {
            spec.description
        };
        Some(LexiconFlagRule {
            id: spec.id,
            level: spec.level,
            matcher,
            pattern_targets,
            preserve_case: spec.action == Action::Replace,
            message,
        })
    }
}

pub(crate) fn match_surface_case(matched: &str, replacement: &str) -> String {
    if replacement.is_empty() {
        return String::new();
    }
    let mut letters = matched.chars().filter(|c| c.is_alphabetic());
    let Some(first) = letters.next() else {
        return replacement.to_string();
    };
    if first.is_uppercase() && letters.clone().next().is_some() && letters.all(char::is_uppercase) {
        return replacement.chars().flat_map(char::to_uppercase).collect();
    }
    if first.is_uppercase() {
        let mut chars = replacement.chars();
        if let Some(r_first) = chars.next() {
            let mut out: String = r_first.to_uppercase().collect();
            out.push_str(chars.as_str());
            return out;
        }
    }
    replacement.to_string()
}

/// Unicode lowercase text and an index for each normalized byte boundary pointing back to
/// the corresponding original byte boundary. Rule matches are guaranteed to begin/end on
/// UTF-8 boundaries, which is all the caller needs.
pub(crate) fn lowercase_with_source_map(text: &str) -> (String, Vec<usize>) {
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
            if start >= end {
                continue;
            }
            let matched = &text[start..end];
            let starts_word = matched.chars().next().is_some_and(|c| c.is_alphanumeric());
            let ends_word = matched
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
            let before_ok = !starts_word
                || text[..start]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric());
            let after_ok = !ends_word
                || text[end..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric());
            if before_ok && after_ok {
                let replacement =
                    self.pattern_targets[m.pattern().as_usize()]
                        .as_ref()
                        .map(|target| {
                            if self.preserve_case {
                                match_surface_case(matched, target)
                            } else {
                                target.clone()
                            }
                        });
                out.push(Finding {
                    rule_id: self.id.clone(),
                    span_index: idx,
                    range: start..end,
                    message: format!("{}: \"{matched}\"", self.message),
                    replacement,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Category;
    use cleanup_core::ir::SpanAnchor;
    use std::collections::BTreeMap;

    fn detect(rule: &LexiconFlagRule, text: &str) -> Vec<Finding> {
        let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text);
        rule.detect(0, &span, &Ctx::new(CleanupLevel::Aggressive))
    }

    #[test]
    fn longest_term_wins_so_inflections_match() {
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

    #[test]
    fn declarative_replace_and_delete_actions_materialize_cleanly() {
        let mut replacements = BTreeMap::new();
        replacements.insert("utilize".to_string(), "use".to_string());
        replacements.insert("utilizes".to_string(), "uses".to_string());
        let replace_rule = LexiconFlagRule::from_spec(RuleSpec {
            id: "custom.replace".to_string(),
            description: "prefer plain verb".to_string(),
            category: Category::Lexical,
            level: CleanupLevel::Standard,
            action: Action::Replace,
            terms: Vec::new(),
            replacement: None,
            replacements,
        })
        .unwrap();
        let found = detect(&replace_rule, "Utilize simple tools; she utilizes them.");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].replacement.as_deref(), Some("Use"));
        assert_eq!(found[1].replacement.as_deref(), Some("uses"));

        let delete_rule = LexiconFlagRule::from_spec(RuleSpec {
            id: "custom.delete".to_string(),
            description: "drop marker".to_string(),
            category: Category::Structural,
            level: CleanupLevel::Light,
            action: Action::Delete,
            terms: vec!["[DRAFT] ".to_string()],
            replacement: None,
            replacements: BTreeMap::new(),
        })
        .unwrap();
        let del_found = detect(&delete_rule, "[DRAFT] Hello.");
        assert_eq!(del_found.len(), 1);
        assert_eq!(del_found[0].replacement.as_deref(), Some(""));
    }
}
