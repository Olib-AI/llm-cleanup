//! Hidden / less-obvious LLM fingerprints: invisible "smuggling" characters, exotic spaces,
//! and bidirectional controls. Grounded in current provider behavior (researched 2026-05):
//!
//! - **ChatGPT / GPT-5** emit the narrow no-break space `U+202F` in place of normal spaces
//!   (handled at `light` by `StripInvisibles`).
//! - **Variation selectors** (`U+FE00..=U+FE0F`, `U+E0100..=U+E01EF`) and **Unicode tag chars**
//!   (`U+E0000..=U+E007F`) are invisible data/watermark *smuggling* vectors — also a
//!   prompt-injection / data-exfiltration risk, so stripping them is document hygiene.
//! - **Bidirectional overrides** (`U+202D`/`U+202E`) are the "Trojan Source" text-disguise vector.
//!
//! OUT OF SCOPE: statistical *token* watermarks such as Google **SynthID** (Gemini) live in the
//! probability of word choices, not in characters, and cannot be removed by character cleanup —
//! we intentionally do not target them.

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

/// Normalize "exotic" Unicode spaces to a regular ASCII space. The common no-break and narrow
/// no-break spaces are handled at `light` by `StripInvisibles`; these rarer width spaces are
/// normalized at `standard`.
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
