//! Built-in typographic normalization rules (Tier A). Deterministic, Unicode-exact,
//! meaning-neutral. Implemented in code rather than templates because correctness here is
//! not negotiable.
//!
//! Coverage is organised by the character families models actually emit (researched
//! 2026-10): office-style "smart punctuation" (curly quotes, ellipsis, en/em dashes), the
//! dash family that leaks out of word processors, math-mode characters (`−`, `⋯`) from
//! reasoning traces, and CJK fullwidth forms from Chinese-trained models.

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
            // Zero-width and non-rendering formatting characters. The math-mode invisibles
            // (U+2061–U+2064) come from reasoning traces; U+FFF9–U+FFFB are the interlinear
            // annotation controls used to hide translation text.
            '\u{200B}'
            | '\u{2060}'
            | '\u{00AD}'
            | '\u{FEFF}'
            | '\u{180E}'
            | '\u{034F}'
            | '\u{2061}'..='\u{2064}'
            | '\u{206A}'..='\u{206F}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            // Mongolian free variation selectors and the musical format controls: two
            // more invisible, copy-stable families used by the same smuggling encoders
            // (and occasionally hallucinated by models reproducing encoded text).
            | '\u{180B}'..='\u{180D}'
            | '\u{180F}'
            | '\u{1D173}'..='\u{1D182}' => Some((
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

/// Curly/smart quotes → straight quotes, including the Continental guillemets and the
/// low-9 („…“) forms that German/French-localised models produce in English text, and
/// the prime/double-prime pair (`′`/`″`, plus their reversed and modifier-letter forms)
/// that models substitute for apostrophes and inch marks.
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
            '\u{201C}' | '\u{201D}' | '\u{00AB}' | '\u{00BB}' | '\u{201E}' | '\u{201F}'
            | '\u{2033}' | '\u{2036}' | '\u{02BA}' => {
                Some(("\"".to_string(), "straightened double quote".to_string()))
            }
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2039}' | '\u{203A}'
            | '\u{02BC}' | '\u{2032}' | '\u{2035}' | '\u{02B9}' => Some((
                "'".to_string(),
                "straightened single quote / apostrophe".to_string(),
            )),
            _ => None,
        })
    }
}

/// Unicode ellipsis `…` (and the math-mode midline ellipsis `⋯` that reasoning traces emit)
/// → three ASCII dots.
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
            '\u{2026}' | '\u{22EF}' => Some(("...".to_string(), "expanded ellipsis".to_string())),
            // Two-dot leader (U+2025), occasionally emitted where a human types "..".
            '\u{2025}' => Some(("..".to_string(), "expanded two-dot leader".to_string())),
            _ => None,
        })
    }
}

/// The dash characters models use where a human would type `-`, `--` or `,`.
fn is_em_dash_like(c: char) -> bool {
    matches!(
        c,
        '\u{2014}' // em dash —
            | '\u{2015}' // horizontal bar ―
            | '\u{2E3A}' // two-em dash ⸺
            | '\u{2E3B}' // three-em dash ⸻
            | '\u{FE58}' // small em dash ﹘
    )
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
            if !is_em_dash_like(ch) {
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

/// En dash (U+2013). A spaced en dash is the British parenthetical dash that several models
/// produce; a tight one is a range or compound ("2019–2024", "Paris–London"), which takes an
/// ASCII hyphen. Digit–digit ranges are already correct typography, but ASCII is what plain
/// text and Word documents expect from us, so they are normalized too.
pub struct NormalizeEnDash;
impl Rule for NormalizeEnDash {
    fn id(&self) -> &str {
        "typo.en-dash.to-hyphen"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = &span.text;
        let mut out = Vec::new();
        for (i, ch) in text.char_indices() {
            if ch != '\u{2013}' {
                continue;
            }
            let after = i + ch.len_utf8();
            let prev = text[..i].chars().next_back();
            let next = text[after..].chars().next();
            if prev == Some(' ') && next == Some(' ') {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i - 1..after + 1,
                    message: "replaced spaced en dash with comma".to_string(),
                    replacement: Some(", ".to_string()),
                });
            } else {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..after,
                    message: "replaced en dash with hyphen".to_string(),
                    replacement: Some("-".to_string()),
                });
            }
        }
        out
    }
}

/// Hyphen-family characters that word processors and models emit instead of U+002D: the
/// plain hyphen (U+2010), the non-breaking hyphen (U+2011), the figure dash (U+2012) and the
/// minus sign (U+2212). The minus sign is only rewritten when it is arithmetic-looking
/// (flanked by digits), so mathematical prose keeps its meaning.
pub struct NormalizeHyphens;
impl Rule for NormalizeHyphens {
    fn id(&self) -> &str {
        "typo.hyphens.normalize"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = &span.text;
        let mut out = Vec::new();
        for (i, ch) in text.char_indices() {
            let convert = match ch {
                '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2043}' | '\u{FE63}' => true,
                '\u{2212}' => {
                    let prev = text[..i].chars().next_back();
                    let next = text[i + ch.len_utf8()..].chars().next();
                    prev.is_some_and(|c| c.is_ascii_digit())
                        || next.is_some_and(|c| c.is_ascii_digit())
                }
                _ => false,
            };
            if convert {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..i + ch.len_utf8(),
                    message: format!("normalized U+{:04X} to an ASCII hyphen", ch as u32),
                    replacement: Some("-".to_string()),
                });
            }
        }
        out
    }
}

/// Fullwidth ASCII forms (U+FF01–U+FF5E) → their ASCII counterparts. Chinese-trained models
/// (Qwen, DeepSeek, GLM, Kimi) routinely emit fullwidth parentheses, colons and commas inside
/// otherwise-English text, which is both a tell and a rendering hazard. The mapping is exact
/// (subtract 0xFEE0), so no meaning can change; it is Aggressive-only because CJK documents
/// legitimately use these forms.
pub struct NormalizeFullwidthForms;
impl Rule for NormalizeFullwidthForms {
    fn id(&self) -> &str {
        "typo.fullwidth.normalize"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_chars(self.id(), idx, span, |ch| {
            let v = ch as u32;
            if (0xFF01..=0xFF5E).contains(&v) {
                let ascii = char::from_u32(v - 0xFEE0)?;
                Some((
                    ascii.to_string(),
                    format!("normalized fullwidth character U+{v:04X} to {ascii:?}"),
                ))
            } else {
                None
            }
        })
    }
}

/// CJK punctuation marks that Chinese/Japanese-trained models drop into otherwise-Latin
/// prose (the fullwidth ASCII range is `NormalizeFullwidthForms`; these are the dedicated
/// CJK code points outside it): ideographic comma/full stop, corner-bracket quotes, the
/// wave dash, katakana middle dot, and the halfwidth kana punctuation forms. Aggressive
/// only, like fullwidth normalization, because a genuinely CJK document wants them.
pub struct NormalizeCjkPunctuation;
impl Rule for NormalizeCjkPunctuation {
    fn id(&self) -> &str {
        "typo.cjk-punctuation.normalize"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_chars(self.id(), idx, span, |ch| {
            let replacement = match ch {
                '\u{3001}' | '\u{FF64}' => ",",
                '\u{3002}' | '\u{FF61}' => ".",
                '\u{300C}' | '\u{300D}' | '\u{300E}' | '\u{300F}' | '\u{FF62}' | '\u{FF63}' => "\"",
                '\u{301C}' => "~",
                '\u{30FB}' | '\u{FF65}' => " ",
                _ => return None,
            };
            Some((
                replacement.to_string(),
                format!(
                    "normalized CJK punctuation U+{:04X} to {replacement:?}",
                    ch as u32
                ),
            ))
        })
    }
}

/// Detokenizer residue: characters that *represent* spaces inside a tokenizer's
/// vocabulary and should have been decoded back to spaces before anyone saw the text.
/// SentencePiece's `▁` (U+2581, used by Llama/Qwen/T5-family tokenizers) is unambiguous.
/// Byte-level BPE's `Ġ` (U+0120, space) and `Ċ` (U+010A, newline) are also genuine
/// Maltese capital letters, so they are only rewritten mid-text — when the previous
/// character is a letter or punctuation (where a space belongs) and a letter follows;
/// a word-initial `Ġ` (a name, a sentence start) is left alone. Aggressive only.
pub struct NormalizeDetokenizer;
impl Rule for NormalizeDetokenizer {
    fn id(&self) -> &str {
        "typo.detokenizer.normalize"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let chars: Vec<(usize, char)> = span.text.char_indices().collect();
        let mut out = Vec::new();
        for k in 0..chars.len() {
            let (i, ch) = chars[k];
            let convert = match ch {
                '\u{2581}' => true,
                '\u{0120}' | '\u{010A}' => {
                    let prev = if k > 0 { Some(chars[k - 1].1) } else { None };
                    let next = chars.get(k + 1).map(|c| c.1);
                    prev.is_some_and(|p| p.is_ascii_alphanumeric() || p.is_ascii_punctuation())
                        && next.is_some_and(|n| n.is_ascii_alphabetic())
                }
                _ => false,
            };
            if convert {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..i + ch.len_utf8(),
                    message: format!(
                        "replaced detokenizer space marker U+{:04X} with a plain space",
                        ch as u32
                    ),
                    replacement: Some(" ".to_string()),
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

    fn run(rule: &dyn Rule, text: &str, level: CleanupLevel) -> Vec<Finding> {
        let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text);
        rule.detect(0, &span, &Ctx::new(level))
    }

    fn apply(text: &str, findings: &[Finding]) -> String {
        let mut out = String::new();
        let mut cursor = 0usize;
        for f in findings {
            out.push_str(&text[cursor..f.range.start]);
            out.push_str(f.replacement.as_deref().unwrap_or(""));
            cursor = f.range.end;
        }
        out.push_str(&text[cursor..]);
        out
    }

    #[test]
    fn straightens_guillemets_and_low_quotes() {
        let text = "\u{201E}hallo\u{201C} \u{00AB}oui\u{00BB}";
        let got = run(&NormalizeQuotes, text, CleanupLevel::Standard);
        assert_eq!(apply(text, &got), "\"hallo\" \"oui\"");
    }

    #[test]
    fn expands_math_ellipsis() {
        let text = "wait\u{22EF}ok";
        let got = run(&NormalizeEllipsis, text, CleanupLevel::Standard);
        assert_eq!(apply(text, &got), "wait...ok");
    }

    #[test]
    fn normalizes_dash_family() {
        let text = "a\u{2015}b";
        let got = run(&NormalizeEmDash, text, CleanupLevel::Aggressive);
        assert_eq!(apply(text, &got), "a--b");

        let text = "2019\u{2013}2024";
        let got = run(&NormalizeEnDash, text, CleanupLevel::Standard);
        assert_eq!(apply(text, &got), "2019-2024");

        let text = "state\u{2011}of\u{2011}the\u{2011}art";
        let got = run(&NormalizeHyphens, text, CleanupLevel::Standard);
        assert_eq!(apply(text, &got), "state-of-the-art");
    }

    #[test]
    fn normalizes_fullwidth_ascii_only() {
        let text = "note\u{FF08}one\u{FF09} and \u{4E2D}\u{6587}";
        let got = run(&NormalizeFullwidthForms, text, CleanupLevel::Aggressive);
        assert_eq!(apply(text, &got), "note(one) and \u{4E2D}\u{6587}");
    }

    #[test]
    fn straightens_primes_and_expands_two_dot_leader() {
        let text = "6\u{2032}2\u{2033} tall\u{2025}ish";
        let quotes = run(&NormalizeQuotes, text, CleanupLevel::Standard);
        assert_eq!(apply(text, &quotes), "6'2\" tall\u{2025}ish");
        let dots = run(&NormalizeEllipsis, text, CleanupLevel::Standard);
        assert_eq!(apply(text, &dots), "6\u{2032}2\u{2033} tall..ish");
    }

    #[test]
    fn normalizes_cjk_punctuation() {
        let text = "Done\u{3002} Next\u{3001}then \u{300C}quoted\u{300D} wave\u{301C} end";
        let got = run(&NormalizeCjkPunctuation, text, CleanupLevel::Aggressive);
        assert_eq!(apply(text, &got), "Done. Next,then \"quoted\" wave~ end");
    }

    #[test]
    fn detokenizer_markers_become_spaces_but_maltese_capitals_survive() {
        let text = "hello\u{2581}world and\u{0120}again";
        let got = run(&NormalizeDetokenizer, text, CleanupLevel::Aggressive);
        assert_eq!(apply(text, &got), "hello world and again");

        // Word-initial Ġ (a Maltese capital) is a letter, not a leaked space.
        let maltese = "\u{0120}ordan went home.";
        let got = run(&NormalizeDetokenizer, maltese, CleanupLevel::Aggressive);
        assert!(got.is_empty(), "got {got:?}");
    }
}
