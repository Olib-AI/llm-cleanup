//! Opt-in statistical watermark disruption (`--dewatermark`) and 5-gram window telemetry.
//!
//! Modern token-sampling watermarks — Google DeepMind's **SynthID-Text** (*Nature* 2024;
//! deployed across Gemini and adopted by Anthropic on recent Claude 5.x releases) and
//! OpenAI's **textGrain** (October 2026) — bias token selection using a pseudorandom hash of
//! the preceding `H` tokens (`H = 4`, i.e. 5-gram windows, in SynthID-Text's production
//! configuration). Because the watermark lives in token choices rather than hidden characters,
//! Unicode/metadata rules leave it untouched, and **no tool without the vendor's secret key
//! can certify its absence**.
//!
//! What deterministic local text processing *can* do — when the user explicitly opts in with
//! `--dewatermark` — is apply strictly truth-preserving, acyclic closed-class, phrasal, and
//! discourse canonicalizations that shift token boundaries and invalidate up to `H + 1 = 5`
//! consecutive tournament windows per edit, while protecting quotations, numbers, URLs, and
//! identifiers.

use crate::lexicon::{lowercase_with_source_map, match_surface_case};
use aho_corasick::{AhoCorasick, MatchKind};
use cleanup_core::ir::ProseSpan;
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};
use std::collections::HashMap;
use std::ops::Range;

/// Acyclic canonicalization table `(source_phrase_lowercase, canonical_replacement)`.
///
/// Invariant (enforced by `rhs_never_retriggers_lhs` unit test): **no target token or phrase
/// on the right-hand side may contain any left-hand-side pattern**, guaranteeing that
/// `--dewatermark` is 100% idempotent in a single pass.
const CANONICAL_SWAPS: &[(&str, &str)] = &[
    // --- Stage 1: Multi-token verbose phrasal compressions (shift token offsets) ---------
    ("due to the fact that", "because"),
    ("in light of the fact that", "because"),
    ("owing to the fact that", "because"),
    ("despite the fact that", "although"),
    ("regardless of the fact that", "even though"),
    ("in the event that", "if"),
    ("under circumstances in which", "when"),
    ("at the present time", "now"),
    ("at this point in time", "now"),
    ("in order to", "to"),
    ("so as to", "to"),
    ("for the purpose of", "for"),
    ("with the aim of", "to"),
    ("with regard to", "regarding"),
    ("with respect to", "regarding"),
    ("in relation to", "regarding"),
    ("prior to", "before"),
    ("subsequent to", "after"),
    ("in close proximity to", "near"),
    ("on a daily basis", "daily"),
    ("on a regular basis", "regularly"),
    ("a large number of", "many"),
    ("a wide range of", "many"),
    ("a wide variety of", "many"),
    ("the vast majority of", "most"),
    ("a significant number of", "many"),
    ("has the ability to", "can"),
    ("have the ability to", "can"),
    ("has the capacity to", "can"),
    ("have the capacity to", "can"),
    ("is able to", "can"),
    ("are able to", "can"),
    ("was able to", "could"),
    ("were able to", "could"),
    ("plays a crucial role in", "shapes"),
    ("plays a vital role in", "shapes"),
    ("plays a pivotal role in", "shapes"),
    ("play a crucial role in", "shape"),
    ("play a vital role in", "shape"),
    ("serves as a", "acts as a"),
    ("serve as a", "act as a"),
    ("of paramount importance", "vital"),
    ("in a manner that", "so that"),
    ("by means of", "through"),
    ("in excess of", "over"),
    // --- Stage 2: High-entropy LLM discourse connectors & formal lexical pairs -----------
    ("furthermore", "also"),
    ("moreover", "besides"),
    ("additionally", "also"),
    ("consequently", "as a result"),
    ("nevertheless", "still"),
    ("nonetheless", "even so"),
    ("subsequently", "later"),
    ("for instance", "for example"),
    ("in conclusion", "finally"),
    ("to summarize", "in short"),
    ("utilizations", "uses"),
    ("utilization", "use"),
    ("utilizing", "using"),
    ("utilized", "used"),
    ("utilizes", "uses"),
    ("utilize", "use"),
    ("facilitating", "helping"),
    ("facilitated", "helped"),
    ("facilitates", "helps"),
    ("facilitate", "help"),
    ("demonstrating", "showing"),
    ("demonstrated", "showed"),
    ("demonstrates", "shows"),
    ("demonstrate", "show"),
    ("commencing", "beginning"),
    ("commenced", "began"),
    ("commences", "begins"),
    ("commence", "begin"),
    ("endeavoring", "trying"),
    ("endeavored", "tried"),
    ("endeavors", "tries"),
    ("endeavor", "try"),
    ("approximately", "about"),
    ("frequently", "often"),
    ("occasionally", "sometimes"),
    ("numerous", "many"),
    ("sufficient", "enough"),
    ("additional", "extra"),
    ("substantial", "large"),
    ("extensive", "broad"),
    ("primarily", "mainly"),
    ("predominantly", "mostly"),
    ("essential", "vital"),
    ("crucial", "critical"),
    ("pivotal", "central"),
    ("seamlessly", "smoothly"),
    ("meticulously", "carefully"),
    ("rapidly", "quickly"),
    // --- Stage 3: Closed-class regional & auxiliary contraction canonicalizations --------
    ("whilst", "while"),
    ("amongst", "among"),
    ("amidst", "amid"),
    ("towards", "toward"),
    ("afterwards", "afterward"),
    ("don't", "do not"),
    ("doesn't", "does not"),
    ("didn't", "did not"),
    ("can't", "cannot"),
    ("won't", "will not"),
    ("isn't", "is not"),
    ("aren't", "are not"),
    ("wasn't", "was not"),
    ("weren't", "were not"),
    ("hasn't", "has not"),
    ("haven't", "have not"),
    ("hadn't", "had not"),
    ("wouldn't", "would not"),
    ("couldn't", "could not"),
    ("shouldn't", "should not"),
    ("we're", "we are"),
    ("they're", "they are"),
    ("you're", "you are"),
    ("i'm", "I am"),
    ("we've", "we have"),
    ("they've", "they have"),
    ("you've", "you have"),
    ("i've", "I have"),
    ("we'll", "we will"),
    ("they'll", "they will"),
    ("you'll", "you will"),
    ("i'll", "I will"),
];

pub struct WatermarkDisruptRule {
    matcher: AhoCorasick,
}

impl Default for WatermarkDisruptRule {
    fn default() -> Self {
        Self::new()
    }
}

impl WatermarkDisruptRule {
    pub fn new() -> Self {
        let patterns: Vec<&str> = CANONICAL_SWAPS.iter().map(|(lhs, _)| *lhs).collect();
        let matcher = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&patterns)
            .expect("canonical watermark swaps are valid aho-corasick patterns");
        Self { matcher }
    }
}

impl Rule for WatermarkDisruptRule {
    fn id(&self) -> &str {
        "watermark.ngram-disrupt"
    }

    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }

    fn detect(&self, idx: usize, span: &ProseSpan, cx: &Ctx) -> Vec<Finding> {
        if !cx.dewatermark {
            return Vec::new();
        }
        let text = span.text.as_str();
        // Normalize curly apostrophes to ASCII `'` only inside the search copy so contractions
        // match whether `typo.quotes.straight` runs in the same pass or ran previously.
        let search_src: String = text
            .chars()
            .map(|c| if c == '\u{2019}' { '\'' } else { c })
            .collect();
        let protected = protected_zones(text);
        let (lower, source_boundaries) = lowercase_with_source_map(&search_src);
        // Map `search_src` byte offsets back to `text` byte offsets (`\u{2019}` is 3B vs `'` 1B).
        let search_to_text = build_char_aligned_offset_map(&search_src, text);

        let mut out = Vec::new();
        for m in self.matcher.find_iter(&lower) {
            let s_start = source_boundaries[m.start()];
            let s_end = source_boundaries[m.end()];
            if s_start >= s_end {
                continue;
            }
            let start = search_to_text[s_start];
            let end = search_to_text[s_end];
            if protected.iter().any(|p| p.start < end && start < p.end) {
                continue;
            }
            if !has_clean_word_boundaries(text, start, end) {
                continue;
            }

            let matched = &text[start..end];
            let (_, target) = CANONICAL_SWAPS[m.pattern().as_usize()];
            let mut replacement = if target.starts_with("I ") {
                target.to_string()
            } else {
                match_surface_case(matched, target)
            };
            let mut final_range = start..end;

            // Keep English `a` / `an` agreement valid when an adjective swap flips vowel onset
            // (for example, `an extensive` -> `a broad`, `a sufficient` -> `enough` unaffected).
            if let Some((adj_start, adj_repl)) =
                adjust_indefinite_article(text, start, matched, &replacement)
                && !protected
                    .iter()
                    .any(|p| p.start < final_range.end && adj_start < p.end)
            {
                final_range.start = adj_start;
                replacement = adj_repl;
            }

            out.push(Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                range: final_range.clone(),
                message: format!(
                    "disrupted n-gram watermark window: {:?} -> {:?}",
                    &text[final_range], replacement
                ),
                replacement: Some(replacement),
            });
        }
        out
    }
}

fn build_char_aligned_offset_map(a: &str, b: &str) -> Vec<usize> {
    let mut map = vec![0usize; a.len() + 1];
    let mut b_iter = b.char_indices();
    for (a_idx, _) in a.char_indices() {
        if let Some((b_idx, _)) = b_iter.next() {
            map[a_idx] = b_idx;
        }
    }
    map[a.len()] = b.len();
    map
}

fn has_clean_word_boundaries(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let left_ok = before.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-' && c != '/');
    let right_ok = after.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-' && c != '/');
    left_ok && right_ok
}

/// Protect direct quotations (`"…"` / `“… ”`), URLs, email-like tokens, and inline backtick
/// fragments so `--dewatermark` never alters quoted speech or technical identifiers.
fn protected_zones(text: &str) -> Vec<Range<usize>> {
    let mut zones = Vec::new();

    // 1. Straight and curly double-quoted quotations within a single paragraph (<= 600 bytes).
    let mut i = 0usize;
    while i < text.len() {
        let ch = text[i..].chars().next().expect("i < text.len()");
        if ch == '"' {
            let open = i;
            let after_open = i + 1;
            if let Some(close_rel) = text[after_open..].find('"')
                && close_rel <= 600
                && !text[after_open..after_open + close_rel].contains("\n\n")
            {
                let close_end = after_open + close_rel + 1;
                zones.push(open..close_end);
                i = close_end;
                continue;
            }
        } else if ch == '\u{201C}' {
            let open = i;
            let after_open = i + ch.len_utf8();
            if let Some(close_rel) = text[after_open..].find('\u{201D}')
                && close_rel <= 600
            {
                let close_end = after_open + close_rel + '\u{201D}'.len_utf8();
                zones.push(open..close_end);
                i = close_end;
                continue;
            }
        } else if ch == '`' {
            let open = i;
            let after_open = i + 1;
            if let Some(close_rel) = text[after_open..].find('`') {
                let close_end = after_open + close_rel + 1;
                zones.push(open..close_end);
                i = close_end;
                continue;
            }
        }
        i += ch.len_utf8();
    }

    // 2. Whitespace-delimited tokens that look like URLs, paths, or technical identifiers.
    let mut tok_start = None;
    for (idx, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = tok_start.take()
                && is_technical_token(&text[s..idx])
            {
                zones.push(s..idx);
            }
        } else if tok_start.is_none() {
            tok_start = Some(idx);
        }
    }
    if let Some(s) = tok_start
        && is_technical_token(&text[s..])
    {
        zones.push(s..text.len());
    }

    zones
}

fn is_technical_token(tok: &str) -> bool {
    let trimmed =
        tok.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | ',' | '.' | ';' | ':'));
    trimmed.contains("://")
        || trimmed.contains('@')
        || trimmed.contains('_')
        || trimmed.starts_with('/')
        || trimmed.chars().any(|c| c.is_ascii_digit())
}

fn starts_with_vowel_sound(word: &str) -> bool {
    let lower: String = word
        .chars()
        .take_while(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .collect();
    if lower.is_empty() {
        return false;
    }
    // `/juː/` words ("use", "used", "useful", "unique", "uniform") take `a`, not `an`.
    if lower.starts_with("use") || lower.starts_with("uni") || lower.starts_with("util") {
        return false;
    }
    matches!(lower.as_bytes()[0], b'a' | b'e' | b'i' | b'o' | b'u')
}

fn adjust_indefinite_article(
    text: &str,
    match_start: usize,
    matched: &str,
    replacement: &str,
) -> Option<(usize, String)> {
    if starts_with_vowel_sound(matched) == starts_with_vowel_sound(replacement) {
        return None;
    }
    let prefix = &text[..match_start];
    let trimmed = prefix.trim_end_matches(' ');
    let spaces = match_start - trimmed.len();
    if spaces == 0 || spaces > 2 {
        return None;
    }
    let word_start = trimmed.rfind(|c: char| !c.is_alphabetic()).map_or(0, |p| {
        p + trimmed[p..].chars().next().map_or(1, char::len_utf8)
    });
    let article = &trimmed[word_start..];
    let want_an = starts_with_vowel_sound(replacement);
    let new_article = match (article, want_an) {
        ("a", true) => "an",
        ("A", true) => "An",
        ("an", false) => "a",
        ("An", false) => "A",
        _ => return None,
    };
    let mut new_repl = replacement.to_string();
    // If the article carried sentence-initial capitalization (`An extensive` -> `A broad`),
    // ensure the adjective stays lowercase.
    if matches!(article, "A" | "An") && matched.chars().next().is_some_and(char::is_lowercase) {
        new_repl = new_repl.chars().flat_map(char::to_lowercase).collect();
    }
    Some((
        word_start,
        format!(
            "{new_article}{}{new_repl}",
            &text[trimmed.len()..match_start]
        ),
    ))
}

// ---------------------------------------------------------------------------
// 5-Gram Tournament Window Disruption Telemetry
// ---------------------------------------------------------------------------

/// Empirical disruption summary comparing the original prose against `--dewatermark` output.
#[derive(Debug, Clone, PartialEq)]
pub struct DisruptionMetrics {
    pub tokens_before: usize,
    pub tokens_after: usize,
    /// Total 5-gram windows (`H = 4` preceding context + 1 target token) in `before`.
    pub windows_total: usize,
    /// Number of original 5-gram windows invalidated in `after`.
    pub windows_broken: usize,
    /// Fraction (`0.0..=1.0`) of 5-gram windows invalidated.
    pub window_disruption_rate: f32,
    /// Bigram multiset divergence (`0.0..=1.0`).
    pub bigram_divergence: f32,
}

/// Tokenize text into word and punctuation units suitable for n-gram window accounting.
pub fn tokenize_for_ngrams(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '\'' || ch == '\u{2019}' {
            for lc in ch.to_lowercase() {
                cur.push(if lc == '\u{2019}' { '\'' } else { lc });
            }
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if !ch.is_whitespace() {
                out.push(ch.to_string());
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Measure how many 5-gram watermark windows (`SynthID-Text` `ngram_len = 5`) and 2-grams
/// were invalidated between `before` and `after`.
pub fn measure_disruption(before: &str, after: &str) -> DisruptionMetrics {
    let t_before = tokenize_for_ngrams(before);
    let t_after = tokenize_for_ngrams(after);

    let (windows_total, windows_broken) = count_broken_ngrams(&t_before, &t_after, 5);
    let window_disruption_rate = if windows_total == 0 {
        0.0
    } else {
        windows_broken as f32 / windows_total as f32
    };

    let (bigrams_total, bigrams_broken) = count_broken_ngrams(&t_before, &t_after, 2);
    let bigram_divergence = if bigrams_total == 0 {
        0.0
    } else {
        bigrams_broken as f32 / bigrams_total as f32
    };

    DisruptionMetrics {
        tokens_before: t_before.len(),
        tokens_after: t_after.len(),
        windows_total,
        windows_broken,
        window_disruption_rate,
        bigram_divergence,
    }
}

fn count_broken_ngrams(before: &[String], after: &[String], n: usize) -> (usize, usize) {
    if before.len() < n {
        return (0, 0);
    }
    let mut after_counts: HashMap<&[String], usize> = HashMap::new();
    if after.len() >= n {
        for win in after.windows(n) {
            *after_counts.entry(win).or_insert(0) += 1;
        }
    }
    let total = before.len() - n + 1;
    let mut preserved = 0usize;
    for win in before.windows(n) {
        if let Some(c) = after_counts.get_mut(win)
            && *c > 0
        {
            *c -= 1;
            preserved += 1;
        }
    }
    (total, total - preserved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::ir::SpanAnchor;

    #[test]
    fn inactive_unless_dewatermark_enabled() {
        let rule = WatermarkDisruptRule::new();
        let text = "Furthermore, we utilize numerous tools in order to facilitate growth.";
        let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text);
        assert!(
            rule.detect(0, &span, &Ctx::new(CleanupLevel::Aggressive))
                .is_empty()
        );
        assert!(
            !rule
                .detect(
                    0,
                    &span,
                    &Ctx::new(CleanupLevel::Light).with_dewatermark(true)
                )
                .is_empty()
        );
    }

    #[test]
    fn rhs_never_retriggers_lhs_proving_single_pass_idempotency() {
        let rule = WatermarkDisruptRule::new();
        let cx = Ctx::new(CleanupLevel::Aggressive).with_dewatermark(true);
        for &(lhs, rhs) in CANONICAL_SWAPS {
            let sample = format!("Wait, {rhs} works here.");
            let span = ProseSpan::new(SpanAnchor::Bytes(0..sample.len()), sample.clone());
            let findings = rule.detect(0, &span, &cx);
            assert!(
                findings.is_empty(),
                "RHS {rhs:?} (from LHS {lhs:?}) re-triggered watermark rule: {findings:?}"
            );
        }
    }

    #[test]
    fn protects_direct_quotes_urls_and_fixes_article_agreement() {
        let rule = WatermarkDisruptRule::new();
        let cx = Ctx::new(CleanupLevel::Aggressive).with_dewatermark(true);
        let text = "An extensive review noted, \"We don't utilize shortcuts,\" at https://example.com/prior-to/1.";
        let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text);
        let findings = rule.detect(0, &span, &cx);
        assert_eq!(
            findings.len(),
            1,
            "quoted text and URL must stay untouched: {findings:?}"
        );
        assert_eq!(&text[findings[0].range.clone()], "An extensive");
        assert_eq!(findings[0].replacement.as_deref(), Some("A broad"));
    }

    #[test]
    fn invalidates_substantial_fraction_of_5gram_windows() {
        let before = "Furthermore, organizations frequently utilize numerous workflows in order to \
                      facilitate collaboration prior to launch. Consequently, teams don't need \
                      additional overhead whilst building essential features.";
        let rule = WatermarkDisruptRule::new();
        let cx = Ctx::new(CleanupLevel::Aggressive).with_dewatermark(true);
        let span = ProseSpan::new(SpanAnchor::Bytes(0..before.len()), before);
        let findings = rule.detect(0, &span, &cx);
        let mut after = String::new();
        let mut cur = 0usize;
        for f in findings {
            after.push_str(&before[cur..f.range.start]);
            after.push_str(f.replacement.as_deref().unwrap());
            cur = f.range.end;
        }
        after.push_str(&before[cur..]);
        let metrics = measure_disruption(before, &after);
        assert!(
            metrics.window_disruption_rate > 0.65,
            "expected high 5-gram disruption, got {metrics:?}"
        );
    }
}
