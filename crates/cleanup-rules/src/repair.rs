//! Repair of text that was *encoded* incorrectly rather than written badly.
//!
//! Two artifacts dominate AI document pipelines in 2026:
//!
//! * **Mojibake** — UTF-8 bytes read as Windows-1252 and re-saved as UTF-8. A model
//!   writes `it’s “quoted”`, some middleware decodes it as Latin-1, and the document ends
//!   up containing `itâ€™s â€œquotedâ€`. It is deterministic damage, so it has a
//!   deterministic repair: map every character back to the byte it came from, then decode
//!   the run as UTF-8 again. Double-encoded text (`Ã¢â‚¬â„¢`) is decoded until stable.
//! * **HTML entities** — `&nbsp;`, `&mdash;`, `&#8217;` left behind when an answer is
//!   copied out of a chat UI as source rather than as text.
//!
//! Both repairs feed their output through the same level-aware mapping the typographic
//! rules use, so a repaired `â€”` becomes `—` at standard and `--` at aggressive — exactly
//! what that character would have become had it survived the pipeline intact. That is what
//! keeps cleaning idempotent at every level.

use cleanup_core::ir::ProseSpan;
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};

/// Longest run of Windows-1252 characters decoded in one attempt. A UTF-8 sequence is at
/// most four bytes and mojibake turns each byte into one character, so a single code point
/// needs at most four; the cap only bounds the work for long accented passages.
const MAX_RUN_CHARS: usize = 64;

/// Longest entity body we inspect (named or numeric).
const MAX_ENTITY_BODY: usize = 16;

/// Map a character back to the Windows-1252 byte that produced it. This is the inverse of
/// the decoder every mis-configured middleware applied, so `â` becomes `0xE2` and `€`
/// becomes `0x80`. Characters outside the code page have no byte and end the run.
fn cp1252_byte(ch: char) -> Option<u8> {
    let value = ch as u32;
    if value <= 0xFF {
        // Latin-1 proper, plus the C1 controls a *strict* ISO-8859-1 decode leaves behind
        // (`itâ\u{80}\u{99}s` is the same damage as `itâ€™s`).
        return u8::try_from(value).ok();
    }
    Some(match ch {
        '\u{20AC}' => 0x80, // €
        '\u{201A}' => 0x82, // ‚
        '\u{0192}' => 0x83, // ƒ
        '\u{201E}' => 0x84, // „
        '\u{2026}' => 0x85, // …
        '\u{2020}' => 0x86, // †
        '\u{2021}' => 0x87, // ‡
        '\u{02C6}' => 0x88, // ˆ
        '\u{2030}' => 0x89, // ‰
        '\u{0160}' => 0x8A, // Š
        '\u{2039}' => 0x8B, // ‹
        '\u{0152}' => 0x8C, // Œ
        '\u{017D}' => 0x8E, // Ž
        '\u{2018}' => 0x91, // ‘
        '\u{2019}' => 0x92, // ’
        '\u{201C}' => 0x93, // “
        '\u{201D}' => 0x94, // ”
        '\u{2022}' => 0x95, // •
        '\u{2013}' => 0x96, // –
        '\u{2014}' => 0x97, // —
        '\u{02DC}' => 0x98, // ˜
        '\u{2122}' => 0x99, // ™
        '\u{0161}' => 0x9A, // š
        '\u{203A}' => 0x9B, // ›
        '\u{0153}' => 0x9C, // œ
        '\u{017E}' => 0x9E, // ž
        '\u{0178}' => 0x9F, // Ÿ
        _ => return None,
    })
}

fn decode_once(text: &str) -> Option<String> {
    let bytes: Vec<u8> = text.chars().map(cp1252_byte).collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

/// Decode until the text stops changing, so double- and triple-encoded documents are
/// repaired in a single pass (and a second clean run is a no-op).
fn decode_repeatedly(text: &str) -> Option<String> {
    let mut current = decode_once(text)?;
    for _ in 0..3 {
        match decode_once(&current) {
            Some(decoded) if decoded != current => current = decoded,
            _ => break,
        }
    }
    Some(current)
}

/// A decode is only accepted when the result is text a document could contain: no control
/// characters, no noncharacters, no replacement character.
fn plausible(decoded: &str) -> bool {
    !decoded.is_empty()
        && !decoded.chars().any(|ch| {
            let value = ch as u32;
            value < 0x20
                || (0x7F..=0x9F).contains(&value)
                || (0xFDD0..=0xFDEF).contains(&value)
                || (value & 0xFFFE) == 0xFFFE
                || ch == '\u{FFFD}'
        })
}

/// The replacement a decoded character takes at the active level, mirroring
/// [`crate::typographic`]. `None` means "keep the character".
///
/// Duplicating the table keeps the repair rule independent of rule ordering; the
/// `repair_policy_matches_typographic_rules` test pins the two together.
fn map_decoded(ch: char, level: CleanupLevel) -> Option<String> {
    // Quotes and ellipses are normalized at standard, exactly as `typo.quotes.straighten`
    // and `typo.ellipsis.expand` do.
    let quote = match ch {
        '\u{201C}' | '\u{201D}' | '\u{00AB}' | '\u{00BB}' | '\u{201E}' | '\u{201F}'
        | '\u{2033}' | '\u{2036}' | '\u{02BA}' => Some("\""),
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2039}' | '\u{203A}'
        | '\u{02BC}' | '\u{2032}' | '\u{2035}' | '\u{02B9}' | '\u{00B4}' => Some("'"),
        _ => None,
    };
    if let Some(replacement) = quote {
        return Some(replacement.to_string());
    }
    match ch {
        '\u{2026}' => return Some("...".to_string()),
        '\u{2025}' => return Some("..".to_string()),
        // Invisible characters carry no meaning; deleting them here is what
        // `typo.invisibles.strip` would have done had they survived as themselves.
        '\u{200B}'
        | '\u{2060}'
        | '\u{00AD}'
        | '\u{FEFF}'
        | '\u{180E}'
        | '\u{034F}'
        | '\u{2061}'..='\u{2065}'
        | '\u{206A}'..='\u{206F}'
        | '\u{FFF0}'..='\u{FFF8}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{180B}'..='\u{180D}'
        | '\u{180F}'
        | '\u{1D173}'..='\u{1D182}' => return Some(String::new()),
        '\u{00A0}'
        | '\u{202F}'
        | '\u{1680}'
        | '\u{2000}'..='\u{200A}'
        | '\u{205F}'
        | '\u{3000}'
        | '\u{2028}'
        | '\u{2029}' => return Some(" ".to_string()),
        _ => {}
    }
    if level.includes(CleanupLevel::Standard) && ch == '\u{2013}' {
        return Some("-".to_string());
    }
    if level.includes(CleanupLevel::Aggressive) {
        let value = ch as u32;
        if matches!(
            ch,
            '\u{2014}' | '\u{2015}' | '\u{2E3A}' | '\u{2E3B}' | '\u{FE58}'
        ) {
            return Some("--".to_string());
        }
        if (0xFF01..=0xFF5E).contains(&value) {
            return char::from_u32(value - 0xFEE0).map(|ascii| ascii.to_string());
        }
        let cjk = match ch {
            '\u{3001}' | '\u{FF64}' => Some(","),
            '\u{3002}' | '\u{FF61}' => Some("."),
            '\u{300C}' | '\u{300D}' | '\u{300E}' | '\u{300F}' | '\u{FF62}' | '\u{FF63}' => {
                Some("\"")
            }
            '\u{301C}' => Some("~"),
            '\u{30FB}' | '\u{FF65}' => Some(" "),
            _ => None,
        };
        if let Some(replacement) = cjk {
            return Some(replacement.to_string());
        }
    }
    None
}

/// The comma replacement a *spaced* parenthetical dash takes at this level — the same
/// mapping as `typo.en-dash.to-hyphen` (standard) and `typo.em-dash.to-comma` (aggressive).
fn spaced_dash_comma(ch: char, level: CleanupLevel) -> Option<&'static str> {
    match ch {
        '\u{2013}' if level.includes(CleanupLevel::Standard) => Some(", "),
        '\u{2014}' | '\u{2015}' | '\u{2E3A}' | '\u{2E3B}' | '\u{FE58}'
            if level.includes(CleanupLevel::Aggressive) =>
        {
            Some(", ")
        }
        _ => None,
    }
}

fn finalize(decoded: &str, level: CleanupLevel) -> String {
    let mut out = String::with_capacity(decoded.len());
    for ch in decoded.chars() {
        match map_decoded(ch, level) {
            Some(replacement) => out.push_str(&replacement),
            None => out.push(ch),
        }
    }
    out
}

/// Does the text at `at` continue as another entity (`lt;`, `#8217;`, …)? Used to refuse a
/// decode that would merely reveal a second encoding layer.
fn entity_follows(text: &str, at: usize) -> bool {
    let Some(semicolon) = text[at..].find(';') else {
        return false;
    };
    if semicolon == 0 || semicolon > MAX_ENTITY_BODY {
        return false;
    }
    entity_char(&text[at..at + semicolon]).is_some()
}

/// Repair UTF-8 text that was decoded as Windows-1252 somewhere upstream.
///
/// The scan only starts on a character that maps to a UTF-8 *lead* byte followed by a
/// continuation byte, so ordinary Latin-1 prose (`© 2026`, `café`) is never touched. A
/// partial run — damaged text followed by clean accented text — is decoded up to the
/// longest valid prefix and the remainder is rescanned.
pub struct RepairMojibake;

impl Rule for RepairMojibake {
    fn id(&self) -> &str {
        "typo.mojibake.repair"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        let mut cursor = 0usize;
        while cursor < text.len() {
            let ch = text[cursor..]
                .chars()
                .next()
                .expect("cursor is inside the span text");
            let continuation = text[cursor + ch.len_utf8()..]
                .chars()
                .next()
                .and_then(cp1252_byte);
            let looks_damaged = cp1252_byte(ch).is_some_and(|b| (0xC2..=0xF4).contains(&b))
                && continuation.is_some_and(|b| (0x80..=0xBF).contains(&b));
            if !looks_damaged {
                cursor += ch.len_utf8();
                continue;
            }
            match detect_mojibake_at(idx, text, cursor, cx.level) {
                Some(finding) => {
                    cursor = finding.range.end.max(cursor + ch.len_utf8());
                    out.push(finding);
                }
                None => cursor += ch.len_utf8(),
            }
        }
        out
    }
}

fn detect_mojibake_at(
    idx: usize,
    text: &str,
    start: usize,
    level: CleanupLevel,
) -> Option<Finding> {
    let run: String = text[start..]
        .chars()
        .take_while(|ch| !ch.is_ascii() && cp1252_byte(*ch).is_some())
        .take(MAX_RUN_CHARS)
        .collect();
    let bytes: Vec<u8> = run.chars().map(cp1252_byte).collect::<Option<Vec<u8>>>()?;
    let valid_len = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) => error.valid_up_to(),
    };
    if valid_len == 0 {
        return None;
    }
    // One Windows-1252 character per byte, so `valid_len` bytes is `valid_len` characters
    // and `damaged` is a true substring of the source.
    let damaged: String = run.chars().take(valid_len).collect();
    let decoded = decode_repeatedly(&damaged)?;
    if !plausible(&decoded) || decoded == damaged {
        return None;
    }
    let end = start + damaged.len();

    // A single decoded dash between spaces is the parenthetical dash, and takes the comma
    // form the typographic rules give it. The flanking spaces are consumed so the result
    // matches what ` — ` would have become.
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    if decoded.chars().count() == 1
        && let Some(dash) = decoded.chars().next()
        && let Some(comma) = spaced_dash_comma(dash, level)
        && before == Some(' ')
        && after == Some(' ')
    {
        return Some(Finding {
            rule_id: "typo.mojibake.repair".to_string(),
            span_index: idx,
            range: start - 1..end + 1,
            message: format!(
                "repaired mojibake {:?} (UTF-8 decoded as Windows-1252 upstream)",
                &text[start..end]
            ),
            replacement: Some(comma.to_string()),
        });
    }

    let replacement = finalize(&decoded, level);
    if replacement == text[start..end] {
        return None;
    }
    Some(Finding {
        rule_id: "typo.mojibake.repair".to_string(),
        span_index: idx,
        range: start..end,
        message: format!(
            "repaired mojibake {:?} -> {:?} (UTF-8 decoded as Windows-1252 upstream)",
            &text[start..end],
            replacement
        ),
        replacement: Some(replacement),
    })
}

// ---------------------------------------------------------------------------
// HTML entities and tags
// ---------------------------------------------------------------------------

/// Named entities worth decoding. Deliberately a curated list of the entities that appear
/// in pasted web/chat output; an unknown entity is left alone (it may be a legitimate
/// escaped example in documentation).
const NAMED_ENTITIES: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", ' '),
    ("ensp", ' '),
    ("emsp", ' '),
    ("thinsp", ' '),
    ("mdash", '\u{2014}'),
    ("ndash", '\u{2013}'),
    ("hellip", '\u{2026}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("ldquo", '\u{201C}'),
    ("rdquo", '\u{201D}'),
    ("laquo", '\u{00AB}'),
    ("raquo", '\u{00BB}'),
    ("bull", '\u{2022}'),
    ("middot", '\u{00B7}'),
    ("copy", '\u{00A9}'),
    ("reg", '\u{00AE}'),
    ("trade", '\u{2122}'),
    ("deg", '\u{00B0}'),
    ("times", '\u{00D7}'),
    ("divide", '\u{00F7}'),
    ("plusmn", '\u{00B1}'),
    ("frac12", '\u{00BD}'),
    ("frac14", '\u{00BC}'),
    ("frac34", '\u{00BE}'),
    ("sect", '\u{00A7}'),
    ("para", '\u{00B6}'),
    ("dagger", '\u{2020}'),
    ("Dagger", '\u{2021}'),
    ("larr", '\u{2190}'),
    ("rarr", '\u{2192}'),
    ("harr", '\u{2194}'),
    ("euro", '\u{20AC}'),
    ("pound", '\u{00A3}'),
    ("yen", '\u{00A5}'),
    ("cent", '\u{00A2}'),
];

fn entity_char(body: &str) -> Option<char> {
    if let Some((_, ch)) = NAMED_ENTITIES.iter().find(|(name, _)| *name == body) {
        return Some(*ch);
    }
    let (digits, radix) =
        if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
            (hex, 16)
        } else if let Some(decimal) = body.strip_prefix('#') {
            (decimal, 10)
        } else {
            return None;
        };
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    u32::from_str_radix(digits, radix)
        .ok()
        .and_then(char::from_u32)
}

/// Decode the HTML entities that web and chat UIs leave behind when their output is copied
/// as source. The decoded characters go through the same level-aware typographic mapping as
/// everything else, so `&mdash;` becomes `—` at standard and `--` at aggressive.
pub struct DecodeHtmlEntities;

impl Rule for DecodeHtmlEntities {
    fn id(&self) -> &str {
        "typo.html-entities.decode"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(relative) = text[from..].find('&') {
            let start = from + relative;
            let Some(body_end) = text[start + 1..]
                .find(';')
                .map(|relative| start + 1 + relative)
            else {
                break;
            };
            let body = &text[start + 1..body_end];
            if body.is_empty() || body.len() > MAX_ENTITY_BODY {
                from = start + 1;
                continue;
            }
            let Some(decoded) = entity_char(body) else {
                from = start + 1;
                continue;
            };
            let end = body_end + 1;
            // `&amp;lt;` is a double encoding: decoding once would leave `&lt;`, which a
            // second run would decode again. Leave such text for the author.
            if decoded == '&' && entity_follows(text, end) {
                from = end;
                continue;
            }
            let before = text[..start].chars().next_back();
            let after = text[end..].chars().next();
            let (replacement, range) = if let Some(comma) = spaced_dash_comma(decoded, cx.level)
                && before == Some(' ')
                && after == Some(' ')
            {
                (comma.to_string(), start - 1..end + 1)
            } else {
                let replacement =
                    map_decoded(decoded, cx.level).unwrap_or_else(|| decoded.to_string());
                (replacement, start..end)
            };
            if replacement != text[range.clone()] {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: range.clone(),
                    message: format!(
                        "decoded HTML entity {:?} left by a web/chat copy-paste",
                        &text[range]
                    ),
                    replacement: Some(replacement),
                });
            }
            from = end;
        }
        out
    }
}

/// Stray HTML tags in prose. Flag-only: a document *about* HTML legitimately quotes them,
/// but `<br>` in the middle of a paragraph is almost always residue from copying a chat
/// answer out of the page source.
pub struct FlagHtmlTags;

impl FlagHtmlTags {
    const TAGS: &[&str] = &[
        "<br>",
        "<br/>",
        "<br />",
        "</p>",
        "<p>",
        "<p ",
        "</div>",
        "<div ",
        "<span",
        "</span>",
        "<strong>",
        "</strong>",
        "<em>",
        "</em>",
        "<ul>",
        "</ul>",
        "<ol>",
        "</ol>",
        "<li>",
        "</li>",
        "<a href=",
        "<img ",
        "<table>",
        "</table>",
        "<tr>",
        "<td>",
        "<h1>",
        "<h2>",
        "<h3>",
        "<code>",
        "</code>",
    ];
}

impl Rule for FlagHtmlTags {
    fn id(&self) -> &str {
        "markers.html-tags"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let lower = text.to_ascii_lowercase();
        let mut out = Vec::new();
        for tag in Self::TAGS {
            let mut from = 0usize;
            while let Some(relative) = lower[from..].find(tag) {
                let start = from + relative;
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..start + tag.len(),
                    message: format!(
                        "HTML markup {tag:?} left in prose (copied as page source, not text)"
                    ),
                    replacement: None,
                });
                from = start + tag.len();
            }
        }
        out.sort_by_key(|f| (f.range.start, f.range.end));
        out.dedup_by(|a, b| a.range == b.range);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::ir::SpanAnchor;

    fn detect(rule: &dyn Rule, text: &str, level: CleanupLevel) -> Vec<Finding> {
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

    fn repair(text: &str, level: CleanupLevel) -> String {
        apply(text, &detect(&RepairMojibake, text, level))
    }

    #[test]
    fn repairs_curly_quotes_and_apostrophes() {
        // "it’s ‘quoted’ here" after a Windows-1252 mis-decode.
        let broken =
            "it\u{E2}\u{20AC}\u{2122}s \u{E2}\u{20AC}\u{2DC}quoted\u{E2}\u{20AC}\u{2122} here";
        assert_eq!(
            repair(broken, CleanupLevel::Standard),
            "it's 'quoted' here",
            "got {broken:?}"
        );
    }

    #[test]
    fn repairs_accents_without_touching_the_rest() {
        // "café" after the same damage.
        let broken = "caf\u{C3}\u{A9} au lait";
        assert_eq!(repair(broken, CleanupLevel::Standard), "caf\u{E9} au lait");
    }

    #[test]
    fn repairs_from_latin1_control_variant_too() {
        // The same damage through a strict ISO-8859-1 decode: C1 controls instead of the
        // Windows-1252 punctuation the browser would have used.
        let broken = "it\u{E2}\u{80}\u{99}s";
        assert_eq!(repair(broken, CleanupLevel::Standard), "it's");
    }

    #[test]
    fn repairs_em_dash_by_level_and_stays_idempotent() {
        let broken = "wait \u{E2}\u{20AC}\u{201D} what";
        let standard = repair(broken, CleanupLevel::Standard);
        assert_eq!(standard, "wait \u{2014} what");
        assert_eq!(repair(&standard, CleanupLevel::Standard), standard);

        let aggressive = repair(broken, CleanupLevel::Aggressive);
        assert_eq!(aggressive, "wait, what");
        assert_eq!(repair(&aggressive, CleanupLevel::Aggressive), aggressive);
    }

    #[test]
    fn repairs_double_encoded_text_in_one_pass() {
        // "’" encoded three times over: Ã¢â‚¬â„¢.
        let broken = "\u{C3}\u{A2}\u{E2}\u{201A}\u{AC}\u{E2}\u{201E}\u{A2}";
        let repaired = repair(broken, CleanupLevel::Standard);
        assert_eq!(repaired, "'", "got {repaired:?}");
        assert_eq!(repair(&repaired, CleanupLevel::Standard), repaired);
    }

    #[test]
    fn leaves_ordinary_latin_text_and_symbols_alone() {
        for text in [
            "caf\u{E9} au lait, \u{A9} 2026 ACME",
            "na\u{EF}ve r\u{E9}sum\u{E9}",
            "Gr\u{FC}\u{DF}e aus M\u{FC}nchen",
            "price: 5 \u{20AC}",
        ] {
            assert!(
                detect(&RepairMojibake, text, CleanupLevel::Aggressive).is_empty(),
                "false positive in {text:?}"
            );
        }
    }

    #[test]
    fn repairs_only_the_damaged_prefix_of_a_run() {
        // Damaged "é" immediately followed by a genuine "é".
        let text = "\u{C3}\u{A9}\u{E9}";
        assert_eq!(repair(text, CleanupLevel::Standard), "\u{E9}\u{E9}");
    }

    #[test]
    fn decodes_common_entities_and_respects_level() {
        let text = "Tom &amp; Jerry&nbsp;said&hellip; &ldquo;hi&rdquo; &mdash; ok";
        let standard = apply(
            text,
            &detect(&DecodeHtmlEntities, text, CleanupLevel::Standard),
        );
        assert_eq!(standard, "Tom & Jerry said... \"hi\" \u{2014} ok");
        let aggressive = apply(
            text,
            &detect(&DecodeHtmlEntities, text, CleanupLevel::Aggressive),
        );
        assert_eq!(aggressive, "Tom & Jerry said... \"hi\", ok");
    }

    #[test]
    fn decodes_numeric_references() {
        let text = "&#8217;tis &#x2014; done&#33;";
        let standard = apply(
            text,
            &detect(&DecodeHtmlEntities, text, CleanupLevel::Standard),
        );
        assert_eq!(standard, "'tis \u{2014} done!");
    }

    #[test]
    fn does_not_unwrap_a_double_encoded_ampersand() {
        let text = "&amp;lt;script&amp;gt;";
        assert!(detect(&DecodeHtmlEntities, text, CleanupLevel::Standard).is_empty());
        // A plain ampersand entity followed by ordinary prose is repaired.
        let text = "AT&amp;T and R&amp;D";
        let out = apply(
            text,
            &detect(&DecodeHtmlEntities, text, CleanupLevel::Standard),
        );
        assert_eq!(out, "AT&T and R&D");
    }

    #[test]
    fn flags_html_markup_but_never_edits_it() {
        let text = "First line<br>second line</p>";
        let found = detect(&FlagHtmlTags, text, CleanupLevel::Standard);
        assert_eq!(found.len(), 2, "got {found:?}");
        assert!(found.iter().all(|f| f.replacement.is_none()));
        assert!(detect(&FlagHtmlTags, "a < b and c > d", CleanupLevel::Standard).is_empty());
    }
}
