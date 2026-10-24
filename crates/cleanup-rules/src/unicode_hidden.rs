//! Hidden / less-obvious LLM fingerprints: invisible "smuggling" characters, exotic spaces,
//! bidirectional controls and mixed-script homoglyphs. Grounded in provider behavior and the
//! Unicode standard as of 2026-10:
//!
//! - **ChatGPT / GPT-5.x / GPT-6** emit the narrow no-break space `U+202F` in place of normal
//!   spaces (handled at `light` by [`crate::typographic::StripInvisibles`]).
//! - **Reasoning models** (o-series, DeepSeek R1/V4, Qwen3 thinking, Kimi) occasionally emit
//!   math-mode invisibles — `U+2061`–`U+2064` — because their internal notation leaks into
//!   the visible answer.
//! - **Variation selectors** (`U+FE00..=U+FE0F`, `U+E0100..=U+E01EF`) and **Unicode tag chars**
//!   (`U+E0000..=U+E007F`) are invisible data/watermark *smuggling* vectors — also a
//!   prompt-injection / data-exfiltration risk, so stripping them is document hygiene.
//! - **Filler glyphs** (`U+115F`, `U+1160`, `U+17B4`, `U+17B5`, `U+2800`, `U+3164`, `U+FFA0`)
//!   render as blank space but are ordinary characters: a favourite of "invisible text"
//!   tricks and of models padding token counts.
//! - **Bidirectional overrides** (`U+202D`/`U+202E`) are the "Trojan Source" text-disguise vector.
//!
//! OUT OF SCOPE: statistical *token* watermarks — Google **SynthID** (Gemini since 2024),
//! Anthropic's watermark for Claude models released on/after 2026-08-02, and OpenAI
//! **textGrain** (EU rollout announced 2026-10-05). They live in the probability of word
//! choices, not in characters, so no character cleanup can remove them. We say so in the
//! README rather than implying otherwise.

use cleanup_core::ir::ProseSpan;
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};

fn is_tag_char(c: char) -> bool {
    ('\u{E0000}'..='\u{E007F}').contains(&c)
}
fn is_variation_selector(c: char) -> bool {
    ('\u{FE00}'..='\u{FE0F}').contains(&c) || ('\u{E0100}'..='\u{E01EF}').contains(&c)
}

fn strip_finding(rule_id: &str, idx: usize, at: usize, ch: char, msg: &str) -> Finding {
    Finding {
        rule_id: rule_id.to_string(),
        span_index: idx,
        range: at..at + ch.len_utf8(),
        message: msg.to_string(),
        replacement: Some(String::new()),
    }
}

/// Strip invisible "smuggling" characters: Unicode tag characters (always invisible, used to
/// hide instructions/data) and variation selectors that are NOT styling a real emoji/symbol.
pub struct StripSmuggledChars;
impl Rule for StripSmuggledChars {
    fn id(&self) -> &str {
        "unicode.smuggled.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let chars: Vec<(usize, char)> = span.text.char_indices().collect();
        let mut out = Vec::new();
        for k in 0..chars.len() {
            let (i, ch) = chars[k];
            if is_tag_char(ch) {
                out.push(strip_finding(
                    self.id(),
                    idx,
                    i,
                    ch,
                    "removed invisible Unicode tag character (data/instruction smuggling vector)",
                ));
            } else if is_variation_selector(ch) {
                let prev = if k > 0 { Some(chars[k - 1].1) } else { None };
                let next = chars.get(k + 1).map(|c| c.1);
                // Keep a single variation selector that legitimately styles an emoji/symbol/CJK
                // base (base codepoint >= U+2190), and keep keycap sequences like '#\u{FE0F}\u{20E3}'.
                let keycap = ch == '\u{FE0F}' && next == Some('\u{20E3}');
                let legit_base =
                    prev.is_some_and(|p| (p as u32) >= 0x2190 && !is_variation_selector(p));
                if !keycap && !legit_base {
                    out.push(strip_finding(
                        self.id(),
                        idx,
                        i,
                        ch,
                        "removed smuggled variation selector (hidden-data vector)",
                    ));
                }
            }
        }
        out
    }
}

/// Strip invisible filler glyphs that render as blank space. They are ordinary characters,
/// never typography: nothing a human types produces them, and they are used to hide strings
/// from search, translation and detection.
pub struct StripFillers;
impl Rule for StripFillers {
    fn id(&self) -> &str {
        "unicode.fillers.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let mut out = Vec::new();
        for (i, ch) in span.text.char_indices() {
            let filler = matches!(
                ch,
                '\u{115F}'   // Hangul choseong filler
                    | '\u{1160}' // Hangul jungseong filler
                    | '\u{17B4}' // Khmer vowel inherent AQ
                    | '\u{17B5}' // Khmer vowel inherent AA
                    | '\u{2800}' // braille pattern blank
                    | '\u{3164}' // Hangul filler
                    | '\u{FFA0}' // halfwidth Hangul filler
            );
            if filler {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..i + ch.len_utf8(),
                    message: format!(
                        "removed invisible filler U+{:04X} (blank but not whitespace)",
                        ch as u32
                    ),
                    replacement: Some(String::new()),
                });
            }
        }
        out
    }
}

/// Normalize "exotic" Unicode spaces to a regular ASCII space. The common no-break and narrow
/// no-break spaces are handled at `light` by `StripInvisibles`; these rarer width spaces —
/// plus the Unicode line/paragraph separators — are normalized at `standard`.
pub struct NormalizeExoticSpaces;
impl Rule for NormalizeExoticSpaces {
    fn id(&self) -> &str {
        "unicode.spaces.normalize"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let mut out = Vec::new();
        for (i, ch) in span.text.char_indices() {
            let exotic = matches!(
                ch,
                '\u{2000}'
                    ..='\u{200A}'   // en quad .. hair space
                | '\u{205F}'              // medium mathematical space
                | '\u{3000}'              // ideographic space
                | '\u{1680}' // ogham space mark
                | '\u{2028}' // line separator
                | '\u{2029}' // paragraph separator
            );
            if exotic {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..i + ch.len_utf8(),
                    message: format!("normalized exotic space U+{:04X}", ch as u32),
                    replacement: Some(" ".to_string()),
                });
            }
        }
        out
    }
}

/// Strip bidirectional *override* controls (LRO `U+202D`, RLO `U+202E`). These have essentially
/// no legitimate use in prose and are the classic "Trojan Source" disguise vector.
pub struct StripBidiOverrides;
impl Rule for StripBidiOverrides {
    fn id(&self) -> &str {
        "unicode.bidi.strip-overrides"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let mut out = Vec::new();
        for (i, ch) in span.text.char_indices() {
            if matches!(ch, '\u{202D}' | '\u{202E}') {
                out.push(strip_finding(
                    self.id(),
                    idx,
                    i,
                    ch,
                    "removed bidirectional override (Trojan-Source disguise vector)",
                ));
            }
        }
        out
    }
}

/// Flag the remaining bidirectional formatting controls. Flag-only (not stripped) because they
/// can be legitimate in right-to-left text.
pub struct FlagBidiControls;
impl Rule for FlagBidiControls {
    fn id(&self) -> &str {
        "unicode.bidi.flag"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let mut out = Vec::new();
        for (i, ch) in span.text.char_indices() {
            let bidi = matches!(ch,
                '\u{202A}'..='\u{202C}'    // LRE, RLE, PDF
                | '\u{2066}'..='\u{2069}'  // LRI, RLI, FSI, PDI
                | '\u{200E}' | '\u{200F}'  // LRM, RLM
                | '\u{061C}'               // Arabic letter mark
            );
            if bidi {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..i + ch.len_utf8(),
                    message: format!(
                        "bidirectional control U+{:04X} (legitimate only in right-to-left text)",
                        ch as u32
                    ),
                    replacement: None,
                });
            }
        }
        out
    }
}

fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c)
}

/// Cyrillic and Greek letters that are visually identical to Latin ones, used to smuggle text
/// past detectors and to disguise URLs/brand names. Reported per word.
fn is_homoglyph_script(c: char) -> bool {
    ('\u{0370}'..='\u{03FF}').contains(&c) // Greek
        || ('\u{0400}'..='\u{04FF}').contains(&c) // Cyrillic
}

/// Flag words that mix Latin letters with Cyrillic/Greek letters. Genuine mixed-script text
/// exists (mathematics, transliteration), which is exactly why this is flag-only and only
/// active at the aggressive level.
pub struct FlagConfusables;
impl Rule for FlagConfusables {
    fn id(&self) -> &str {
        "unicode.confusables.flag"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        let mut word_start: Option<usize> = None;
        let mut has_latin = false;
        let mut has_homoglyph = false;

        let mut flush = |s: usize, e: usize, latin: bool, homo: bool, out: &mut Vec<Finding>| {
            if latin && homo {
                out.push(Finding {
                    rule_id: "unicode.confusables.flag".to_string(),
                    span_index: idx,
                    range: s..e,
                    message: format!(
                        "mixed-script word {:?}: Latin letters with Cyrillic/Greek look-alikes",
                        &text[s..e]
                    ),
                    replacement: None,
                });
            }
        };

        for (i, ch) in text.char_indices() {
            if is_latin_letter(ch) || is_homoglyph_script(ch) {
                if word_start.is_none() {
                    word_start = Some(i);
                }
                if is_latin_letter(ch) {
                    has_latin = true;
                } else {
                    has_homoglyph = true;
                }
            } else if ch.is_alphanumeric() {
                // A word in another script: keep the run open but it can never be a homoglyph
                // mix on its own, so reset the flags.
                if word_start.is_none() {
                    word_start = Some(i);
                }
                has_latin = false;
                has_homoglyph = false;
            } else if let Some(start) = word_start.take() {
                flush(start, i, has_latin, has_homoglyph, &mut out);
                has_latin = false;
                has_homoglyph = false;
            }
        }
        if let Some(start) = word_start {
            flush(start, text.len(), has_latin, has_homoglyph, &mut out);
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
    fn strips_math_invisibles_and_format_controls() {
        // U+2062 INVISIBLE TIMES, U+2060 WORD JOINER, U+200B ZWSP, U+034F CGJ.
        let text = "2\u{2062}x\u{2060}y\u{200B}z\u{034F}w";
        let found = detect(&crate::typographic::StripInvisibles, text);
        assert_eq!(found.len(), 4, "expected every invisible removed: {found:?}");
    }

    #[test]
    fn strips_filler_glyphs() {
        let text = "a\u{3164}b\u{FFA0}c\u{2800}d";
        assert_eq!(detect(&StripFillers, text).len(), 3);
    }

    #[test]
    fn normalizes_line_separator() {
        let text = "one\u{2028}two";
        let found = detect(&NormalizeExoticSpaces, text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].replacement.as_deref(), Some(" "));
    }

    #[test]
    fn flags_mixed_script_words_only() {
        // "pаypal" with a Cyrillic 'а' (U+0430) is a homoglyph mix; plain Russian is not.
        let mixed = "p\u{0430}ypal";
        assert_eq!(detect(&FlagConfusables, mixed).len(), 1);
        let russian = "\u{041F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}";
        assert!(detect(&FlagConfusables, russian).is_empty());
        assert!(detect(&FlagConfusables, "paypal").is_empty());
    }
}
