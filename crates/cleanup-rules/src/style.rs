//! Layout and formatting tells that are not single characters and not plain phrases.
//!
//! Everything here is **flag-only**. These patterns are suggestive, not conclusive (a human
//! can legitimately open a list with 🚀), so the honest behaviour is to point at them and let
//! the author decide. See `docs/FINGERPRINTS.md` for the evidence behind each one.

use cleanup_core::ir::ProseSpan;
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};

/// Emoji blocks that assistant-flavoured prose reaches for when it decorates headings and
/// bullet lists. Deliberately a curated list, not "all emoji": a rocket in a launch post is
/// ordinary writing, but a rocket *opening every bullet* is a model habit.
const DECORATION_EMOJI: &[char] = &[
    '✨', '🚀', '💡', '🔥', '✅', '🎯', '📌', '📝', '🔑', '⚡', '🌟', '💪', '🙌', '👉', '📊', '🧠',
    '🛠', '🎉', '🤖', '⏰', '📈', '🧵', '❗', '❓', '⚠', '✔', '⭐', '🔹', '🔸', '📣', '🔍', '🧩',
    '📚', '🗂', '🏁',
];

fn is_emoji(c: char) -> bool {
    let v = c as u32;
    (0x1F000..=0x1FAFF).contains(&v)
        || (0x2600..=0x27BF).contains(&v)
        || (0x2B00..=0x2BFF).contains(&v)
}

/// Flag a span whose first visible character is a decorative emoji — the "✨ **Why it
/// matters**" heading habit that became a model tell in 2024 and never really left.
pub struct DecorativeEmojiOpener;

impl Rule for DecorativeEmojiOpener {
    fn id(&self) -> &str {
        "style.emoji.opener"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let trimmed = text.trim_start();
        let offset = text.len() - trimmed.len();
        let Some(first) = trimmed.chars().next() else {
            return Vec::new();
        };
        if !DECORATION_EMOJI.contains(&first) {
            return Vec::new();
        }
        // Require it to be used as decoration (followed by whitespace or punctuation), not
        // as part of a sentence about rockets.
        let after = &trimmed[first.len_utf8()..];
        let decorative = match after.chars().next() {
            None => true,
            Some(c) => c.is_whitespace() || matches!(c, ':' | '-' | '—' | '|' | '\u{FE0F}'),
        };
        if !decorative {
            return Vec::new();
        }
        vec![Finding {
            rule_id: self.id().to_string(),
            span_index: idx,
            range: offset..offset + first.len_utf8(),
            message: format!(
                "decorative emoji opener {first:?} (AI-formatted heading / bullet style)"
            ),
            replacement: None,
        }]
    }
}

/// Flag sentences that carry three or more emoji. Density, not presence, is the signal.
pub struct EmojiDensity;

impl Rule for EmojiDensity {
    fn id(&self) -> &str {
        "style.emoji.density"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        for sentence in span.sentences() {
            let slice = &text[sentence.clone()];
            let count = slice.chars().filter(|c| is_emoji(*c)).count();
            if count >= 3 {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: sentence.clone(),
                    message: format!("{count} emoji in one sentence (assistant-style emphasis)"),
                    replacement: None,
                });
            }
        }
        out
    }
}

/// Flag Markdown emphasis markers that survived into prose. The Markdown adapter never
/// hands a `**` pair to a rule (emphasis markers are structure, not text), so any occurrence
/// is genuine residue: a pasted raw-response, or a `.txt`/`.docx` that was never rendered.
pub struct MarkdownResidue;

impl Rule for MarkdownResidue {
    fn id(&self) -> &str {
        "style.markdown.residue"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        for needle in ["**", "__", "~~", "```"] {
            let mut from = 0usize;
            while let Some(rel) = text[from..].find(needle) {
                let start = from + rel;
                let end = start + needle.len();
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..end,
                    message: format!(
                        "Markdown syntax {needle:?} left in plain prose (raw model output)"
                    ),
                    replacement: None,
                });
                from = end;
            }
        }
        out.sort_by_key(|f| (f.range.start, f.range.end));
        out
    }
}

/// Flag repeated terminal punctuation (`!!`, `??`). Deliberately does **not** look at dots:
/// this tool expands `…` to `...`, so dot runs are expected output, not a tell.
pub struct RepeatedPunctuation;

impl Rule for RepeatedPunctuation {
    fn id(&self) -> &str {
        "style.punctuation.repeated"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < chars.len() {
            let (start, ch) = chars[i];
            if ch != '!' && ch != '?' {
                i += 1;
                continue;
            }
            let mut end = start + ch.len_utf8();
            let mut n = 1usize;
            while i + 1 < chars.len() && matches!(chars[i + 1].1, '!' | '?') {
                i += 1;
                n += 1;
                end = chars[i].0 + chars[i].1.len_utf8();
            }
            if n >= 2 {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..end,
                    message: format!("repeated terminal punctuation (run of {n})"),
                    replacement: None,
                });
            }
            i += 1;
        }
        out
    }
}

/// Flag a plain ASCII space before sentence punctuation (`word ,`, `word .`, `word !`).
/// This is the visible scar of detokenization: models emit the punctuation as its own
/// token and a naive detokenizer joins tokens with spaces. Typed English essentially
/// never does this. Flag-only and aggressive-only: French typography puts a (narrow
/// no-break) space before `! ? ; :`, and a document *in* French should be reviewed by a
/// human, not silently re-spaced — the rule only fires on a regular U+0020, never on the
/// no-break spaces French actually uses.
pub struct DetokenizedSpacing;

impl Rule for DetokenizedSpacing {
    fn id(&self) -> &str {
        "style.spacing.detokenized"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut out = Vec::new();
        for k in 1..chars.len() {
            let (i, ch) = chars[k];
            if ch != ' ' {
                continue;
            }
            let prev = chars[k - 1].1;
            let Some(&(punct_at, punct)) = chars.get(k + 1) else {
                continue;
            };
            if !matches!(punct, ',' | '.' | '!' | '?' | ';' | ':') || !prev.is_alphanumeric() {
                continue;
            }
            let after = chars.get(k + 2).map(|c| c.1);
            // Spaced ellipsis ("word ...") and digit-grouping colons (times, ratios)
            // are deliberate styles, not detokenizer scars.
            if punct == '.' && after == Some('.') {
                continue;
            }
            if punct == ':' && after.is_some_and(|c| c.is_ascii_digit()) {
                continue;
            }
            out.push(Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                range: i..punct_at + punct.len_utf8(),
                message: format!(
                    "space before {punct:?} (detokenized punctuation; French typography \
                     aside, English keeps punctuation against the word)"
                ),
                replacement: None,
            });
        }
        out
    }
}

/// Flag unfilled template placeholders: `[Your Name]`, `[INSERT SOURCE]`, `[TODO: …]`,
/// `2026-XX-XX`, leftover lorem ipsum. A model asked for a template produces the
/// brackets; a finished human document almost never ships them, which is why editors
/// treat shipped placeholders as a near-definitive tell of unedited generation.
pub struct PlaceholderResidue;

impl PlaceholderResidue {
    const BRACKET_OPENERS: &[&str] = &[
        "insert",
        "your",
        "add ",
        "enter",
        "replace",
        "placeholder",
        "todo",
        "tbd",
        "xxx",
    ];
    const BRACKET_EXACT: &[&str] = &[
        "citation needed",
        "source",
        "link",
        "name",
        "date",
        "title",
        "image",
        "company",
    ];
    const MAX_BRACKET_CHARS: usize = 64;

    fn bracket_findings(&self, idx: usize, text: &str) -> Vec<Finding> {
        let lower = text.to_ascii_lowercase();
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(rel) = text[from..].find('[') {
            let start = from + rel;
            let inner_start = start + 1;
            let Some(close_rel) = text[inner_start..].find(']') else {
                break;
            };
            let close = inner_start + close_rel;
            let inner = &lower[inner_start..close];
            let trimmed = inner.trim();
            let is_placeholder = !trimmed.is_empty()
                && inner.chars().count() <= Self::MAX_BRACKET_CHARS
                && !inner.contains(['\n', '\r'])
                && (Self::BRACKET_OPENERS.iter().any(|o| trimmed.starts_with(o))
                    || Self::BRACKET_EXACT.contains(&trimmed));
            if is_placeholder {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..close + 1,
                    message: format!(
                        "unfilled placeholder {:?} shipped in the text",
                        &text[start..close + 1]
                    ),
                    replacement: None,
                });
            }
            from = close + 1;
        }
        // Lorem ipsum and digit-masked template dates (`2026-XX-XX`).
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find("lorem ipsum") {
            let start = from + rel;
            out.push(Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                range: start..start + "lorem ipsum".len(),
                message: "lorem ipsum placeholder text shipped in the document".to_string(),
                replacement: None,
            });
            from = start + "lorem ipsum".len();
        }
        let bytes = text.as_bytes();
        for (i, w) in bytes.windows(3).enumerate() {
            if (w[0] == b'-' || w[0] == b'/')
                && w[1] == b'X'
                && w[2] == b'X'
                && i > 0
                && bytes[i - 1].is_ascii_digit()
            {
                let mut end = i + 3;
                while end < bytes.len() && bytes[end] == b'X' {
                    end += 1;
                }
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..end,
                    message: "masked template value (`-XX`) shipped in the text".to_string(),
                    replacement: None,
                });
            }
        }
        out.sort_by_key(|f| (f.range.start, f.range.end));
        out
    }
}

impl Rule for PlaceholderResidue {
    fn id(&self) -> &str {
        "style.placeholder.residue"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        self.bracket_findings(idx, span.text.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::ir::SpanAnchor;

    fn detect(rule: &dyn Rule, text: &str) -> Vec<Finding> {
        let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text);
        rule.detect(0, &span, &Ctx::new(CleanupLevel::Aggressive))
    }

    #[test]
    fn flags_decorative_emoji_opener() {
        assert_eq!(
            detect(&DecorativeEmojiOpener, "✨ Why this matters").len(),
            1
        );
        // A sentence that happens to start with an emoji used as a noun is not decoration.
        assert!(detect(&DecorativeEmojiOpener, "✅because the build was green").is_empty());
    }

    #[test]
    fn flags_emoji_density_per_sentence() {
        assert_eq!(
            detect(&EmojiDensity, "Ship it 🚀🔥🎉 today. One 😀 only.").len(),
            1
        );
    }

    #[test]
    fn flags_markdown_residue() {
        assert_eq!(
            detect(&MarkdownResidue, "This is **very** important.").len(),
            2
        );
    }

    #[test]
    fn flags_repeated_bang_not_ellipsis() {
        assert_eq!(
            detect(&RepeatedPunctuation, "Really?! Yes!! Done...").len(),
            2
        );
    }

    #[test]
    fn flags_detokenized_spacing_not_real_styles() {
        assert_eq!(
            detect(&DetokenizedSpacing, "Revenue grew , then fell . Why ?").len(),
            3
        );
        assert!(detect(&DetokenizedSpacing, "Normal punctuation. Yes, really!").is_empty());
        assert!(detect(&DetokenizedSpacing, "A spaced ellipsis ... trails off").is_empty());
        assert!(detect(&DetokenizedSpacing, "Ratio 3 :2 is a style").is_empty());
        // French no-break spacing never trips the rule: it is not a U+0020 space.
        assert!(detect(&DetokenizedSpacing, "Attention\u{202F}!").is_empty());
    }

    #[test]
    fn flags_unfilled_placeholders() {
        assert_eq!(
            detect(&PlaceholderResidue, "Dear [Your Name], welcome.").len(),
            1
        );
        assert_eq!(
            detect(
                &PlaceholderResidue,
                "Per the report [INSERT SOURCE], sales rose."
            )
            .len(),
            1
        );
        assert_eq!(
            detect(&PlaceholderResidue, "Dated 2026-XX-XX in the draft.").len(),
            1
        );
        assert_eq!(
            detect(&PlaceholderResidue, "Lorem ipsum dolor sit amet.").len(),
            1
        );
        // Ordinary brackets and citations are not placeholders.
        assert!(
            detect(
                &PlaceholderResidue,
                "As shown [12], results (see appendix) hold."
            )
            .is_empty()
        );
    }

    #[test]
    fn flags_strikethrough_and_fence_residue() {
        assert_eq!(detect(&MarkdownResidue, "a ~~mistake~~ here").len(), 2);
    }
}
