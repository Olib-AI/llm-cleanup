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
    '✨',
    '🚀',
    '💡',
    '🔥',
    '✅',
    '🎯',
    '📌',
    '📝',
    '🔑',
    '⚡',
    '🌟',
    '💪',
    '🙌',
    '👉',
    '📊',
    '🧠',
    '🛠',
    '🎉',
    '🤖',
    '⏰',
    '📈',
    '🧵',
    '❗',
    '❓',
    '⚠',
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
        for needle in ["**", "__"] {
            let mut from = 0usize;
            while let Some(rel) = text[from..].find(needle) {
                let start = from + rel;
                let end = start + needle.len();
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..end,
                    message: format!(
                        "Markdown emphasis marker {needle:?} left in plain prose (raw model output)"
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
        assert_eq!(detect(&DecorativeEmojiOpener, "✨ Why this matters").len(), 1);
        // A sentence that happens to start with an emoji used as a noun is not decoration.
        assert!(detect(&DecorativeEmojiOpener, "✅because the build was green").is_empty());
    }

    #[test]
    fn flags_emoji_density_per_sentence() {
        assert_eq!(detect(&EmojiDensity, "Ship it 🚀🔥🎉 today. One 😀 only.").len(), 1);
    }

    #[test]
    fn flags_markdown_residue() {
        assert_eq!(detect(&MarkdownResidue, "This is **very** important.").len(), 2);
    }

    #[test]
    fn flags_repeated_bang_not_ellipsis() {
        assert_eq!(detect(&RepeatedPunctuation, "Really?! Yes!! Done...").len(), 2);
    }
}
