//! Built-in typographic normalization rules (Tier A). Deterministic, Unicode-exact,
//! meaning-neutral. Implemented in code rather than templates because correctness here is
//! not negotiable.

use cleanup_core::ir::ProseSpan;
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};

/// Scan a span char-by-char; the callback returns `Some((replacement, message))` to emit a
/// finding for the current character. `replacement = Some("")` deletes it.
fn scan_chars<F>(rule_id: &str, span_index: usize, span: &ProseSpan, mut f: F) -> Vec<Finding>
where
    F: FnMut(char) -> Option<(String, String)>,
{
    let mut out = Vec::new();
    for (i, ch) in span.text.char_indices() {
        if let Some((replacement, message)) = f(ch) {
            out.push(Finding {
                rule_id: rule_id.to_string(),
                span_index,
                range: i..i + ch.len_utf8(),
                message,
                replacement: Some(replacement),
            });
        }
    }
    out
}

/// Remove zero-width / invisible characters that have no legitimate use in prose.
/// Deliberately conservative: ZWJ/ZWNJ (U+200C/U+200D) are left alone because they are
/// meaningful in Arabic/Indic scripts and emoji sequences.
pub struct StripInvisibles;
impl Rule for StripInvisibles {
    fn id(&self) -> &str {
        "typo.invisibles.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_chars(self.id(), idx, span, |ch| match ch {
            '\u{200B}' | '\u{2060}' | '\u{00AD}' | '\u{FEFF}' | '\u{180E}' => Some((
                String::new(),
                format!("removed invisible character U+{:04X}", ch as u32),
            )),
            // U+202F (narrow no-break space) is a notable ChatGPT/GPT-5 tell; U+00A0 is the
            // ordinary non-breaking space. Both normalize to a regular space.
            '\u{00A0}' | '\u{202F}' => {
                Some((" ".to_string(), "normalized no-break space".to_string()))
            }
            _ => None,
        })
    }
}

/// Curly/smart quotes → straight quotes.
pub struct NormalizeQuotes;
impl Rule for NormalizeQuotes {
    fn id(&self) -> &str {
        "typo.quotes.straighten"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_chars(self.id(), idx, span, |ch| match ch {
            '\u{201C}' | '\u{201D}' => {
                Some(("\"".to_string(), "straightened double quote".to_string()))
            }
            '\u{2018}' | '\u{2019}' => Some((
                "'".to_string(),
                "straightened single quote / apostrophe".to_string(),
            )),
            _ => None,
        })
    }
}

/// Unicode ellipsis `…` → three ASCII dots.
pub struct NormalizeEllipsis;
impl Rule for NormalizeEllipsis {
    fn id(&self) -> &str {
        "typo.ellipsis.expand"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_chars(self.id(), idx, span, |ch| match ch {
            '\u{2026}' => Some(("...".to_string(), "expanded ellipsis".to_string())),
            _ => None,
        })
    }
}

/// Em dash handling — Aggressive only, and conservative even then: a spaced em dash
/// (" — ", a parenthetical connector) becomes ", "; a tight em dash becomes "--"; a dash
/// between digits (a numeric range) is left untouched.
pub struct NormalizeEmDash;
impl Rule for NormalizeEmDash {
    fn id(&self) -> &str {
        "typo.em-dash.to-comma"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = &span.text;
        let mut out = Vec::new();
        for (i, ch) in text.char_indices() {
            if ch != '\u{2014}' {
                continue;
            }
            let after = i + ch.len_utf8();
            let prev = text[..i].chars().next_back();
            let next = text[after..].chars().next();

            let between_digits = prev.is_some_and(|c| c.is_ascii_digit())
                && next.is_some_and(|c| c.is_ascii_digit());
            if between_digits {
                continue; // numeric range like 1914—1918: keep
            }

            if prev == Some(' ') && next == Some(' ') {
                // " — " → ", " (consume the flanking ASCII spaces, 1 byte each)
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i - 1..after + 1,
                    message: "replaced spaced em dash with comma".to_string(),
                    replacement: Some(", ".to_string()),
                });
            } else {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..after,
                    message: "replaced em dash with --".to_string(),
                    replacement: Some("--".to_string()),
                });
            }
        }
        out
    }
}
