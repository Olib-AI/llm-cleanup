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

fn is_tag_or_reserved_ignorable(c: char) -> bool {
    // Unicode's plane-14 default-ignorable ranges, excluding the variation-selector
    // supplement handled separately below. Reserved members are included deliberately:
    // Unicode assigns them Default_Ignorable_Code_Point so new format controls can be added
    // without older implementations rendering visible boxes or carrying an invisible payload.
    ('\u{E0000}'..='\u{E00FF}').contains(&c) || ('\u{E01F0}'..='\u{E0FFF}').contains(&c)
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
            if is_tag_or_reserved_ignorable(ch) {
                out.push(strip_finding(
                    self.id(),
                    idx,
                    i,
                    ch,
                    "removed invisible Unicode tag/reserved format character \
                     (default-ignorable data-smuggling vector)",
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

/// Return the end of a complete ECMA-48 escape sequence, or `None` for a truncated/malformed
/// one. Incomplete OSC/DCS strings deliberately do not consume the rest of the document.
fn ansi_sequence_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&0x1B) || start + 1 >= bytes.len() {
        return None;
    }
    let introducer = bytes[start + 1];
    if introducer == b'[' {
        // CSI: parameter and intermediate bytes followed by one final byte.
        let mut cursor = start + 2;
        while cursor < bytes.len() && (0x20..=0x3F).contains(&bytes[cursor]) {
            cursor += 1;
        }
        return (cursor < bytes.len() && (0x40..=0x7E).contains(&bytes[cursor]))
            .then_some(cursor + 1);
    }
    if matches!(introducer, b']' | b'P' | b'X' | b'^' | b'_') {
        // OSC, DCS, SOS, PM and APC strings end at BEL or ST (ESC \\). Never cross a line
        // boundary or swallow more than 4 KiB while looking for a missing terminator.
        let mut cursor = start + 2;
        let limit = bytes.len().min(start + 4096);
        while cursor < limit {
            match bytes[cursor] {
                0x07 => return Some(cursor + 1),
                b'\n' | b'\r' => return None,
                0x1B if bytes.get(cursor + 1) == Some(&b'\\') => return Some(cursor + 2),
                0x1B => return None,
                _ => cursor += 1,
            }
        }
        return None;
    }

    // Other ESC sequences have zero or more intermediate bytes and one final byte.
    let mut cursor = start + 1;
    while cursor < bytes.len() && (0x20..=0x2F).contains(&bytes[cursor]) {
        cursor += 1;
    }
    (cursor < bytes.len() && (0x30..=0x7E).contains(&bytes[cursor])).then_some(cursor + 1)
}

/// Strip complete terminal escape sequences copied out of coding-agent and tool output.
/// Removing only the ESC control would leave visible residue such as `[31m`; handling the
/// whole CSI/OSC/DCS sequence preserves the prose and discards only terminal instructions.
pub struct StripAnsiEscapes;
impl Rule for StripAnsiEscapes {
    fn id(&self) -> &str {
        "unicode.ansi.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(relative) = text[from..].find('\u{001B}') {
            let start = from + relative;
            if let Some(end) = ansi_sequence_end(text, start) {
                out.push(strip_finding(
                    self.id(),
                    idx,
                    start,
                    '\u{001B}',
                    "removed terminal ANSI/ECMA-48 escape sequence",
                ));
                // `strip_finding` covers the introducer; widen it to the complete sequence.
                out.last_mut().expect("just pushed").range = start..end;
                from = end;
            } else {
                // `unicode.controls.strip` still removes a lone ESC without eating suffix text.
                from = start + 1;
            }
        }
        out
    }
}

/// Strip raw C0/C1 control characters and Unicode noncharacters, and flag the U+FFFD
/// replacement character. Control bytes other than tab/newline have no meaning in prose;
/// they arrive from corrupted generations, terminal transcripts pasted into documents,
/// and ANSI-adjacent agent output, and several renderers (and Word) misbehave on them.
/// Noncharacters (`U+FDD0..U+FDEF`, `U+*FFFE`/`U+*FFFF`) are guaranteed by the standard
/// never to be assigned, so their presence is always machine residue. `U+FFFD` instead
/// means "a character was already lost here" — deleting it would hide the damage, so it
/// is reported, not removed.
pub struct StripControlChars;
impl Rule for StripControlChars {
    fn id(&self) -> &str {
        "unicode.controls.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let mut out = Vec::new();
        for (i, ch) in span.text.char_indices() {
            let v = ch as u32;
            let whitespace_control = matches!(ch, '\u{000B}' | '\u{000C}' | '\u{0085}');
            let control = (v <= 0x1F && !matches!(ch, '\t' | '\n' | '\r') && !whitespace_control)
                || ((0x7F..=0x9F).contains(&v) && !whitespace_control);
            let noncharacter =
                (0xFDD0..=0xFDEF).contains(&v) || (v & 0xFFFE) == 0xFFFE || ch == '\u{FFFC}';
            if ch == '\u{FFFD}' {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..i + ch.len_utf8(),
                    message: "replacement character U+FFFD (a character was lost to an \
                              encoding error here; check the source text)"
                        .to_string(),
                    replacement: None,
                });
            } else if whitespace_control {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..i + ch.len_utf8(),
                    message: format!("normalized whitespace control U+{v:04X} to a space"),
                    replacement: Some(" ".to_string()),
                });
            } else if control || noncharacter {
                out.push(strip_finding(
                    self.id(),
                    idx,
                    i,
                    ch,
                    &format!("removed control/noncharacter U+{v:04X} (never valid in prose)"),
                ));
            }
        }
        out
    }
}

fn is_private_use(c: char) -> bool {
    ('\u{E000}'..='\u{F8FF}').contains(&c)
        || ('\u{F0000}'..='\u{FFFFD}').contains(&c)
        || ('\u{100000}'..='\u{10FFFD}').contains(&c)
}

/// The block OpenAI's interfaces use for citation/entity markup delimiters.
fn is_chat_interface_pua(c: char) -> bool {
    ('\u{E200}'..='\u{E2FF}').contains(&c)
}

/// Flag Private Use Area characters. By definition these code points have no standardized
/// meaning: outside icon fonts they are interface plumbing (chat citations, app-specific
/// glyphs) that renders as a box or as nothing in the reader's font. The `U+E200..U+E2FF`
/// block is excluded here because the citation rules own it; everything else is reported
/// per *run* so a pasted icon-font word produces one finding, not forty.
pub struct FlagPrivateUse;
impl Rule for FlagPrivateUse {
    fn id(&self) -> &str {
        "unicode.private-use.flag"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        let mut run: Option<(usize, usize, usize, char)> = None; // start, end, count, first
        let flush = |run: Option<(usize, usize, usize, char)>, out: &mut Vec<Finding>| {
            if let Some((s, e, n, first)) = run {
                out.push(Finding {
                    rule_id: "unicode.private-use.flag".to_string(),
                    span_index: idx,
                    range: s..e,
                    message: format!(
                        "{n} Private Use Area character(s) starting at U+{:04X} \
                         (no standard meaning; usually leaked interface markup)",
                        first as u32
                    ),
                    replacement: None,
                });
            }
        };
        for (i, ch) in text.char_indices() {
            if is_private_use(ch) && !is_chat_interface_pua(ch) {
                match &mut run {
                    Some((_, e, n, _)) if *e == i => {
                        *e = i + ch.len_utf8();
                        *n += 1;
                    }
                    _ => {
                        flush(run.take(), &mut out);
                        run = Some((i, i + ch.len_utf8(), 1, ch));
                    }
                }
            } else {
                flush(run.take(), &mut out);
            }
        }
        flush(run, &mut out);
        out
    }
}

/// Remove leftover chat-interface Private Use characters (`U+E200..U+E2FF`) that were not
/// part of a complete citation marker — the span brackets `U+E203`/`U+E204`/`U+E206` and
/// orphan delimiters from truncated streams. Aggressive only; complete markers are
/// removed whole by `markers.private-use-citations.strip`, which wins the overlap.
pub struct StripPrivateUseMarkers;
impl Rule for StripPrivateUseMarkers {
    fn id(&self) -> &str {
        "unicode.private-use.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let mut out = Vec::new();
        for (i, ch) in span.text.char_indices() {
            if is_chat_interface_pua(ch) && !matches!(ch, '\u{E200}' | '\u{E201}' | '\u{E202}') {
                // The envelope delimiters belong to PrivateUseCitationRule, which validates
                // the payload before deleting it. Only sibling span markers are safe to strip
                // independently; an unknown PUA payload must remain report-only.
                out.push(strip_finding(
                    self.id(),
                    idx,
                    i,
                    ch,
                    "removed chat-interface Private Use span marker (citation plumbing)",
                ));
            }
        }
        out
    }
}

/// Curated combining-mark blocks. We deliberately do not depend on full Unicode category
/// tables; these blocks cover the diacritics that "Zalgo" stacking and glitch-token
/// smuggling use.
fn is_combining_mark(c: char) -> bool {
    let v = c as u32;
    matches!(
        v,
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x0E31
            | 0x0E34..=0x0E3A
            | 0x0E47..=0x0E4E
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0x3099..=0x309A
            | 0xFE20..=0xFE2F
    )
}

/// Flag runs of three or more combining marks on one base character. Real orthographies
/// stack at most two (a Vietnamese vowel plus tone, polytonic Greek); three or more is
/// "Zalgo" text, a glitch-token artifact, or a smuggling channel — worth a human look,
/// never an automatic rewrite.
pub struct FlagCombiningRuns;
impl Rule for FlagCombiningRuns {
    fn id(&self) -> &str {
        "unicode.combining.flag"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let mut out = Vec::new();
        let mut run: Option<(usize, usize, usize)> = None; // start, end, count
        let flush = |run: Option<(usize, usize, usize)>, out: &mut Vec<Finding>| {
            if let Some((s, e, n)) = run
                && n >= 3
            {
                out.push(Finding {
                    rule_id: "unicode.combining.flag".to_string(),
                    span_index: idx,
                    range: s..e,
                    message: format!("{n} stacked combining marks (Zalgo/glitch artifact)"),
                    replacement: None,
                });
            }
        };
        for (i, ch) in span.text.char_indices() {
            if is_combining_mark(ch) {
                match &mut run {
                    Some((_, e, n)) if *e == i => {
                        *e = i + ch.len_utf8();
                        *n += 1;
                    }
                    _ => {
                        flush(run.take(), &mut out);
                        run = Some((i, i + ch.len_utf8(), 1));
                    }
                }
            } else {
                flush(run.take(), &mut out);
            }
        }
        flush(run, &mut out);
        out
    }
}

/// Flag zero-width joiners/non-joiners in contexts where they carry no linguistic meaning:
/// between two ASCII letters, or inside a run of other invisible characters. `U+200C` and
/// `U+200D` are *load-bearing* in Arabic-script and Indic text and inside emoji sequences,
/// which is why those contexts are never stripped — but between Latin letters they are the
/// classic zero-width binary-encoding channel ("sneaky bits": two invisible characters encode
/// arbitrary bytes) and a known fingerprint of encoder tools and some clipboard paths.
pub struct FlagZeroWidthJoiners;
impl Rule for FlagZeroWidthJoiners {
    fn id(&self) -> &str {
        "unicode.zero-width.flag"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let is_joiner = |c: char| matches!(c, '\u{200C}' | '\u{200D}');
        let is_invisible = |c: char| matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}');
        let chars: Vec<(usize, char)> = span.text.char_indices().collect();
        let mut out = Vec::new();
        for k in 0..chars.len() {
            let (i, ch) = chars[k];
            if !is_joiner(ch) {
                continue;
            }
            let prev = if k > 0 { Some(chars[k - 1].1) } else { None };
            let next = chars.get(k + 1).map(|c| c.1);
            if prev.is_some_and(is_joiner) {
                continue; // report only the first character of a joiner run
            }
            let inside_latin = prev.is_some_and(|p| p.is_ascii_alphanumeric())
                && next.is_some_and(|n| n.is_ascii_alphanumeric());
            let in_invisible_run = prev.is_some_and(is_invisible) || next.is_some_and(is_invisible);
            if inside_latin || in_invisible_run {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: i..i + ch.len_utf8(),
                    message: format!(
                        "zero-width {} U+{:04X} inside Latin text / an invisible run \
                         (zero-width-encoded payload vector; legitimate only in \
                         Arabic-script, Indic and emoji sequences)",
                        if ch == '\u{200D}' {
                            "joiner"
                        } else {
                            "non-joiner"
                        },
                        ch as u32
                    ),
                    replacement: None,
                });
            }
        }
        out
    }
}

/// Aggressive removal for the same narrowly-scoped joiner contexts flagged above. Arabic,
/// Indic, emoji, and other non-ASCII shaping contexts never enter this rule's candidate set.
pub struct StripZeroWidthJoiners;
impl Rule for StripZeroWidthJoiners {
    fn id(&self) -> &str {
        "unicode.zero-width.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, cx: &Ctx) -> Vec<Finding> {
        FlagZeroWidthJoiners
            .detect(idx, span, cx)
            .into_iter()
            .map(|mut finding| {
                finding.rule_id = self.id().to_string();
                finding.message = "removed zero-width joiner/non-joiner from Latin text or an \
                                   invisible encoding run"
                    .to_string();
                finding.replacement = Some(String::new());
                finding
            })
            .collect()
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

/// Cyrillic, Greek, Armenian, Cherokee, Mathematical Alphanumeric, and Fullwidth Latin
/// code points that visually impersonate ordinary ASCII Latin letters inside a word.
fn is_homoglyph_script(c: char) -> bool {
    ('\u{0370}'..='\u{03FF}').contains(&c) // Greek
        || ('\u{0400}'..='\u{04FF}').contains(&c) // Cyrillic
        || ('\u{0530}'..='\u{058F}').contains(&c) // Armenian
        || ('\u{13A0}'..='\u{13FF}').contains(&c) // Cherokee
        || ('\u{FF21}'..='\u{FF3A}').contains(&c) // Fullwidth Latin capital
        || ('\u{FF41}'..='\u{FF5A}').contains(&c) // Fullwidth Latin small
        || math_alphanumeric_to_ascii(c).is_some()
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

        let flush = |s: usize, e: usize, latin: bool, homo: bool, out: &mut Vec<Finding>| {
            if latin && homo {
                out.push(Finding {
                    rule_id: "unicode.confusables.flag".to_string(),
                    span_index: idx,
                    range: s..e,
                    message: format!(
                        "mixed-script word {:?}: Latin letters with \
                         Cyrillic/Greek/Armenian/Cherokee look-alikes",
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
            } else if ch.is_ascii_digit() {
                // Digits inside a word ("pаypal123") neither prove nor disprove a mix:
                // keep the run and its flags. (Previously any alphanumeric reset them,
                // so a trailing digit silently cleared a real homoglyph word.)
                if word_start.is_none() {
                    word_start = Some(i);
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

/// Map Unicode Mathematical Alphanumeric Symbols (`U+1D400..=U+1D7FF`) and their BMP
/// Letterlike-Symbols slots back to plain ASCII letters and digits.
pub fn math_alphanumeric_to_ascii(c: char) -> Option<char> {
    let u = c as u32;
    if (0x1D400..0x1D6A4).contains(&u) {
        let rem = (u - 0x1D400) % 52;
        return Some(if rem < 26 {
            char::from(b'A' + rem as u8)
        } else {
            char::from(b'a' + (rem - 26) as u8)
        });
    }
    if (0x1D7CE..=0x1D7FF).contains(&u) {
        let digit = (u - 0x1D7CE) % 10;
        return Some(char::from(b'0' + digit as u8));
    }
    match c {
        '\u{2102}' | '\u{212D}' => Some('C'),
        '\u{210A}' => Some('g'),
        '\u{210B}' | '\u{210C}' | '\u{210D}' => Some('H'),
        '\u{210E}' => Some('h'),
        '\u{2110}' | '\u{2111}' => Some('I'),
        '\u{2112}' => Some('L'),
        '\u{2113}' => Some('l'),
        '\u{2115}' => Some('N'),
        '\u{2119}' => Some('P'),
        '\u{211A}' => Some('Q'),
        '\u{211B}' | '\u{211C}' | '\u{211D}' => Some('R'),
        '\u{2124}' | '\u{2128}' => Some('Z'),
        '\u{212C}' => Some('B'),
        '\u{212F}' => Some('e'),
        '\u{2130}' => Some('E'),
        '\u{2131}' => Some('F'),
        '\u{2133}' => Some('M'),
        '\u{2134}' => Some('o'),
        _ => None,
    }
}

fn scan_math_styled_words(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut out = Vec::new();
    let mut run_start: Option<usize> = None;
    let mut run_end = 0usize;
    let mut count = 0usize;
    let mut ascii_buf = String::new();

    let flush = |start: Option<usize>,
                 end: usize,
                 n: usize,
                 ascii: &mut String,
                 out: &mut Vec<(std::ops::Range<usize>, String)>| {
        if let Some(s) = start
            && n >= 2
        {
            // Skip mixed-script words (`p𝖺ypal`): `FlagConfusables` handles those, and a
            // partial normalization inside an IDN/spoofed word should be reviewed, not masked.
            let left_plain = text[..s].chars().next_back().is_some_and(is_latin_letter);
            let right_plain = text[end..].chars().next().is_some_and(is_latin_letter);
            if !left_plain && !right_plain {
                out.push((s..end, std::mem::take(ascii)));
                return;
            }
        }
        ascii.clear();
    };

    for (i, ch) in text.char_indices() {
        if let Some(mapped) = math_alphanumeric_to_ascii(ch) {
            if run_start.is_none() {
                run_start = Some(i);
            }
            run_end = i + ch.len_utf8();
            count += 1;
            ascii_buf.push(mapped);
        } else {
            flush(run_start.take(), run_end, count, &mut ascii_buf, &mut out);
            count = 0;
        }
    }
    flush(run_start, run_end, count, &mut ascii_buf, &mut out);
    out
}

/// Flag multi-character words rendered in Unicode Mathematical Alphanumeric "fake bold /
/// sans / italic" (`𝗛𝗲𝗹𝗹𝗼`) — used by social-post generators to fake formatting in plain text
/// and by bypass tools to evade keyword/detector tokenizers. Single-letter mathematical
/// variables (`𝑥`, `ℝ`) are intentionally ignored.
pub struct FlagMathStyledText;
impl Rule for FlagMathStyledText {
    fn id(&self) -> &str {
        "unicode.math-styled.flag"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_math_styled_words(&span.text)
            .into_iter()
            .map(|(range, normalized)| Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                message: format!(
                    "Mathematical Alphanumeric styled word {:?} (normalizes to {normalized:?})",
                    &span.text[range.clone()]
                ),
                range,
                replacement: None,
            })
            .collect()
    }
}

/// Normalize multi-character Mathematical Alphanumeric styled words back to plain ASCII at
/// [`CleanupLevel::Aggressive`], leaving single-character math symbols (`𝑥`, `ℝ`) untouched.
pub struct NormalizeMathStyledText;
impl Rule for NormalizeMathStyledText {
    fn id(&self) -> &str {
        "unicode.math-styled.normalize"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_math_styled_words(&span.text)
            .into_iter()
            .map(|(range, normalized)| Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                message: format!(
                    "normalized Mathematical Alphanumeric word {:?} -> {normalized:?}",
                    &span.text[range.clone()]
                ),
                range,
                replacement: Some(normalized),
            })
            .collect()
    }
}

fn scan_whitespace_stego_runs(text: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut line_start = 0usize;
    let mut i = 0usize;

    let check_line_end = |start: usize, end: usize, out: &mut Vec<std::ops::Range<usize>>| {
        if end <= start {
            return;
        }
        let mut ws_start = end;
        while ws_start > start && matches!(bytes[ws_start - 1], b' ' | b'\t') {
            ws_start -= 1;
        }
        // Require non-whitespace content earlier on the line (so indented blank lines are not
        // treated as steganography) and at least one trailing `\t` (SNOW / tab-space bit
        // channels), which never participates in CommonMark's two-space hard line break.
        if ws_start > start && ws_start < end && bytes[ws_start..end].contains(&b'\t') {
            out.push(ws_start..end);
        }
    };

    while i < bytes.len() {
        if bytes[i] == b'\r' {
            check_line_end(line_start, i, &mut out);
            if bytes.get(i + 1) == Some(&b'\n') {
                i += 2;
            } else {
                i += 1;
            }
            line_start = i;
        } else if bytes[i] == b'\n' {
            check_line_end(line_start, i, &mut out);
            i += 1;
            line_start = i;
        } else {
            i += 1;
        }
    }
    check_line_end(line_start, bytes.len(), &mut out);
    out
}

/// Flag trailing tab-mixed horizontal whitespace at line ends (SNOW-style whitespace
/// steganography, which encodes bit triplets in trailing tab/space groups).
pub struct FlagWhitespaceStego;
impl Rule for FlagWhitespaceStego {
    fn id(&self) -> &str {
        "unicode.whitespace-stego.flag"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_whitespace_stego_runs(&span.text)
            .into_iter()
            .map(|range| Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                message: format!(
                    "trailing tab-mixed whitespace ({} bytes; SNOW-style steganography channel)",
                    range.len()
                ),
                range,
                replacement: None,
            })
            .collect()
    }
}

/// Strip trailing tab-mixed horizontal whitespace at [`CleanupLevel::Aggressive`].
pub struct StripWhitespaceStego;
impl Rule for StripWhitespaceStego {
    fn id(&self) -> &str {
        "unicode.whitespace-stego.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        scan_whitespace_stego_runs(&span.text)
            .into_iter()
            .map(|range| Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                message: format!(
                    "removed {} bytes of trailing tab-mixed steganographic whitespace",
                    range.len()
                ),
                range,
                replacement: Some(String::new()),
            })
            .collect()
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
        assert_eq!(
            found.len(),
            4,
            "expected every invisible removed: {found:?}"
        );
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

    #[test]
    fn homoglyphs_survive_trailing_digits_and_new_scripts() {
        // Armenian 'ա' (U+0561) in a Latin word with digits.
        assert_eq!(detect(&FlagConfusables, "p\u{0561}ypal123").len(), 1);
        // Cherokee 'Ꭺ' (U+13AA) is a capital-A look-alike.
        assert_eq!(detect(&FlagConfusables, "\u{13AA}pple").len(), 1);
    }

    #[test]
    fn strips_controls_and_noncharacters_but_keeps_tab_and_newline() {
        let text = "a\u{0000}b\u{0081}c\td\ne\u{FFFC}f\u{FDD0}g\u{1FFFE}h";
        let found = detect(&StripControlChars, text);
        assert_eq!(found.len(), 5, "got {found:?}");
        assert!(found.iter().all(|f| f.replacement.as_deref() == Some("")));
        // Whitespace controls become a space instead of gluing words together.
        let text = "one\u{000C}two";
        let found = detect(&StripControlChars, text);
        assert_eq!(found[0].replacement.as_deref(), Some(" "));
        // U+FFFD is reported, never deleted.
        let found = detect(&StripControlChars, "bad \u{FFFD} byte");
        assert_eq!(found.len(), 1);
        assert!(found[0].replacement.is_none());
    }

    #[test]
    fn private_use_runs_flag_once_and_chat_block_strips_at_aggressive() {
        let text = "icon \u{F001}\u{F002} end";
        let found = detect(&FlagPrivateUse, text);
        assert_eq!(found.len(), 1, "one finding per run: {found:?}");
        // The U+E200 block belongs to the citation rules, not this one.
        assert!(detect(&FlagPrivateUse, "x\u{E203}y").is_empty());
        let strips = detect(&StripPrivateUseMarkers, "x\u{E203}y\u{E204}z");
        assert_eq!(strips.len(), 2);
        assert!(strips.iter().all(|f| f.replacement.as_deref() == Some("")));
    }

    #[test]
    fn flags_zalgo_but_not_vietnamese() {
        let zalgo = "h\u{0300}\u{0301}\u{0302}ello";
        assert_eq!(detect(&FlagCombiningRuns, zalgo).len(), 1);
        // Two marks (circumflex + tone) are ordinary orthography.
        let vietnamese = "Ti\u{0065}\u{0302}\u{0301}ng";
        assert!(detect(&FlagCombiningRuns, vietnamese).is_empty());
    }

    #[test]
    fn strips_reserved_default_ignorables_but_preserves_variation_selectors() {
        let text = "a\u{2065}b\u{FFF0}c\u{E0080}d\u{E01F0}e";
        let found = detect(&crate::typographic::StripInvisibles, text);
        assert_eq!(found.len(), 2, "{found:?}");
        let tags = detect(&StripSmuggledChars, text);
        assert_eq!(tags.len(), 2, "{tags:?}");
        assert!(detect(&StripSmuggledChars, "\u{2764}\u{FE0F}").is_empty());
        assert!(detect(&StripSmuggledChars, "\u{2764}\u{E0100}").is_empty());
        assert_eq!(detect(&StripSmuggledChars, "\u{1F3F4}\u{E0067}").len(), 1);
    }

    #[test]
    fn strips_complete_ansi_sequences_only() {
        let text = "a\u{1b}[31mb\u{1b}]0;title\u{7}c";
        let found = detect(&StripAnsiEscapes, text);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(&text[found[0].range.clone()], "\u{1b}[31m");
        assert!(detect(&StripAnsiEscapes, "a\u{1b}]0;unterminated").is_empty());
    }

    #[test]
    fn aggressive_zero_width_strip_uses_the_same_safe_contexts() {
        assert_eq!(detect(&StripZeroWidthJoiners, "pass\u{200D}word").len(), 1);
        let emoji = "\u{1F468}\u{200D}\u{1F4BB}";
        assert!(detect(&StripZeroWidthJoiners, emoji).is_empty());
        assert!(detect(&StripZeroWidthJoiners, "\u{0645}\u{200C}\u{06CC}").is_empty());
    }

    #[test]
    fn flags_zero_width_joiners_inside_latin_words_only() {
        assert_eq!(detect(&FlagZeroWidthJoiners, "pass\u{200D}word").len(), 1);
        // Persian-style ZWNJ between Arabic-script letters is legitimate.
        let persian = "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}";
        assert!(detect(&FlagZeroWidthJoiners, persian).is_empty());
        // An emoji ZWJ sequence is legitimate.
        let emoji = "\u{1F468}\u{200D}\u{1F4BB} shipped";
        assert!(detect(&FlagZeroWidthJoiners, emoji).is_empty());
    }

    #[test]
    fn math_styled_words_normalize_while_single_variables_and_confusables_split_cleanly() {
        let styled = "Read \u{1D5DB}\u{1D5F2}\u{1D5F9}\u{1D5F9}\u{1D5FC} in \u{1D7D0}\u{1D7CE}\u{1D7D0}\u{1D7D4} for \u{1D465} \u{2208} \u{211D}.";
        let norm = detect(&NormalizeMathStyledText, styled);
        assert_eq!(
            norm.len(),
            2,
            "single math symbols x and R must stay untouched: {norm:?}"
        );
        assert_eq!(norm[0].replacement.as_deref(), Some("Hello"));
        assert_eq!(norm[1].replacement.as_deref(), Some("2026"));

        // Single mathematical sans-serif 'a' (U+1D5BA) mixed into a Latin word is flagged as a confusable.
        let mixed = "p\u{1D5BA}ypal";
        assert_eq!(detect(&FlagConfusables, mixed).len(), 1);
        assert!(detect(&NormalizeMathStyledText, mixed).is_empty());
    }

    #[test]
    fn strips_snow_tab_stego_without_touching_markdown_two_space_hard_breaks() {
        let snow = "Line one \t  \t\nLine two  \nLine three\t ";
        let found = detect(&StripWhitespaceStego, snow);
        assert_eq!(
            found.len(),
            2,
            "two-space hard break on line 2 must stay untouched: {found:?}"
        );
        assert_eq!(&snow[found[0].range.clone()], " \t  \t");
        assert_eq!(&snow[found[1].range.clone()], "\t ");
    }
}
