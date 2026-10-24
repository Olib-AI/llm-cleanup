//! Report-only stylometric cadence and watermark-window profiler.
//!
//! Ships with zero external dependencies and no neural weights, keeping `aiclean` a single
//! deterministic static binary. Never drives automatic edits; scores are strictly advisory
//! and return `None` on passages shorter than 60 words or 4 sentences, where statistical
//! cadence estimates have no evidentiary value.

use std::collections::HashSet;

const DISCOURSE_MARKERS: &[&str] = &[
    "furthermore",
    "moreover",
    "additionally",
    "consequently",
    "nevertheless",
    "nonetheless",
    "subsequently",
    "notably",
    "importantly",
    "ultimately",
    "overall",
    "delve",
    "delves",
    "underscore",
    "underscores",
    "pivotal",
    "crucial",
    "tapestry",
    "seamless",
    "seamlessly",
    "meticulously",
];

/// Advisory stylometric and watermark-exposure profile over a multi-sentence passage.
#[derive(Debug, Clone, PartialEq)]
pub struct StylometryProfile {
    pub word_count: usize,
    pub sentence_count: usize,
    pub mean_sentence_words: f32,
    /// Coefficient of variation (`stddev / mean`) of sentence lengths in words. Human prose
    /// typically exhibits higher burstiness (`>= 0.55`) than unedited RLHF prose (`<= 0.35`).
    pub sentence_burstiness_cv: f32,
    /// Moving-Average Type-Token Ratio across 50-word windows (`0.0..=1.0`).
    pub mattr_50: f32,
    /// Fraction of 5-gram windows whose preceding 4-token context is unique in the passage
    /// (directly reflecting SynthID-Text's repeated-context masking behavior).
    pub unique_4gram_context_ratio: f32,
    /// Measured AI-excess discourse markers per 100 words.
    pub discourse_marker_per_100w: f32,
    /// Composite report-only AI-cadence score in `0.0..=1.0`.
    pub score: f32,
}

/// Compute a stylometric and n-gram exposure profile for `text`, or `None` if the passage is
/// shorter than 60 words or 4 sentences.
pub fn analyze_prose(text: &str) -> Option<StylometryProfile> {
    let sentences = split_sentences(text);
    let mut sentence_lengths = Vec::new();
    let mut all_words = Vec::new();

    for sent in &sentences {
        let words = extract_words(sent);
        if !words.is_empty() {
            sentence_lengths.push(words.len());
            all_words.extend(words);
        }
    }

    if all_words.len() < 60 || sentence_lengths.len() < 4 {
        return None;
    }

    let n_sent = sentence_lengths.len() as f32;
    let mean = sentence_lengths.iter().sum::<usize>() as f32 / n_sent;
    let variance = sentence_lengths
        .iter()
        .map(|&len| {
            let d = len as f32 - mean;
            d * d
        })
        .sum::<f32>()
        / n_sent;
    let cv = if mean > 0.0 {
        variance.sqrt() / mean
    } else {
        0.0
    };

    let mattr_50 = compute_mattr(&all_words, 50);
    let unique_4gram_context_ratio = compute_unique_context_ratio(&all_words);

    let marker_hits = all_words
        .iter()
        .filter(|w| DISCOURSE_MARKERS.contains(&w.as_str()))
        .count();
    let discourse_marker_per_100w = (marker_hits as f32 * 100.0) / (all_words.len() as f32);

    // Calibrated combination:
    // 1. Low burstiness (`cv < 0.55` -> higher uniformity signal)
    // 2. Narrow RLHF mean sentence band (`15..=25` words)
    // 3. Elevated discourse-transition density
    let low_burstiness = ((0.62 - cv) / 0.45).clamp(0.0, 1.0);
    let band_closeness = (1.0 - ((mean - 19.5).abs() / 12.0)).clamp(0.0, 1.0);
    let marker_signal = (discourse_marker_per_100w / 3.5).clamp(0.0, 1.0);
    let mattr_band = (1.0 - ((mattr_50 - 0.78).abs() / 0.22)).clamp(0.0, 1.0);

    let score =
        (0.42 * low_burstiness + 0.18 * band_closeness + 0.28 * marker_signal + 0.12 * mattr_band)
            .clamp(0.0, 1.0);

    Some(StylometryProfile {
        word_count: all_words.len(),
        sentence_count: sentence_lengths.len(),
        mean_sentence_words: mean,
        sentence_burstiness_cv: cv,
        mattr_50,
        unique_4gram_context_ratio,
        discourse_marker_per_100w,
        score,
    })
}

/// Document-level report-only AI-cadence score in `0.0..=1.0`, or `None` when the text is too
/// short (`< 60` words or `< 4` sentences) for meaningful statistical measurement.
pub fn ai_likelihood_score(text: &str) -> Option<f32> {
    analyze_prose(text).map(|p| p.score)
}

fn split_sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = text[i..].chars().next().expect("i < bytes.len()");
        let next_i = i + ch.len_utf8();
        let is_term = matches!(ch, '.' | '!' | '?')
            && (next_i == bytes.len()
                || text[next_i..].chars().next().is_some_and(|n| {
                    n.is_whitespace() || matches!(n, '"' | ')' | ']' | '\u{201D}')
                }));
        let is_para = ch == '\n' && bytes.get(next_i) == Some(&b'\n');
        if is_term || is_para {
            let slice = text[start..next_i].trim();
            if !slice.is_empty() {
                out.push(slice);
            }
            start = next_i;
        }
        i = next_i;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn extract_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_alphabetic() || ch == '\'' || ch == '\u{2019}' {
            for lc in ch.to_lowercase() {
                cur.push(if lc == '\u{2019}' { '\'' } else { lc });
            }
        } else if !cur.is_empty() {
            let trimmed = cur.trim_matches('\'');
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
            cur.clear();
        }
    }
    if !cur.is_empty() {
        let trimmed = cur.trim_matches('\'');
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn compute_mattr(words: &[String], window: usize) -> f32 {
    if words.is_empty() {
        return 0.0;
    }
    if words.len() <= window {
        let uniq: HashSet<&str> = words.iter().map(String::as_str).collect();
        return uniq.len() as f32 / words.len() as f32;
    }
    let mut sum = 0.0f32;
    let count = words.len() - window + 1;
    for win in words.windows(window) {
        let uniq: HashSet<&str> = win.iter().map(String::as_str).collect();
        sum += uniq.len() as f32 / window as f32;
    }
    sum / count as f32
}

fn compute_unique_context_ratio(words: &[String]) -> f32 {
    if words.len() < 5 {
        return 0.0;
    }
    let total = words.len() - 4;
    let mut seen: HashSet<[&str; 4]> = HashSet::with_capacity(total);
    let mut unique = 0usize;
    for win in words.windows(5) {
        let ctx = [
            win[0].as_str(),
            win[1].as_str(),
            win[2].as_str(),
            win[3].as_str(),
        ];
        if seen.insert(ctx) {
            unique += 1;
        }
    }
    unique as f32 / total as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_passages_return_none() {
        assert_eq!(ai_likelihood_score("Too short to judge."), None);
    }

    #[test]
    fn uniform_ai_cadence_scores_higher_than_bursty_human_prose() {
        let ai_like = "Furthermore, modern software architectures meticulously integrate \
            distributed components across cloud environments to ensure seamless reliability. \
            Moreover, engineering teams consistently evaluate pivotal observability metrics when \
            deploying critical microservices into production clusters. Additionally, automated \
            pipelines crucial to operational stability underscore the value of deterministic \
            testing workflows across enterprise platforms. Consequently, organizations delve \
            deeper into platform governance so infrastructure changes remain predictable across \
            global regions. Overall, standardized deployment practices foster resilient systems \
            while minimizing operational friction during routine maintenance windows.";

        let human_like = "Stop. Have you ever stared at a broken socket timeout at three in the \
            morning while the pager refuses to shut up and the coffee pot sits cold on the \
            counter? We tried everything — tcpdump, strace, ripping out half the proxy layer, \
            blaming DNS twice, reverting Tuesday's kernel bump, swapping NIC drivers on two \
            spare blades — and nothing moved the needle by a single millisecond. Nobody spoke. \
            Then Maya walked into the aisle with a flashlight and tugged patch cord seven. \
            Turned out to be a cracked latch on cage four's uplink fiber jumper. One click. \
            Dead silence.";

        let ai_prof = analyze_prose(ai_like).expect("long enough");
        let hu_prof = analyze_prose(human_like).expect("long enough");
        assert!(
            ai_prof.score > hu_prof.score + 0.30,
            "expected AI cadence ({ai_prof:?}) > human bursty prose ({hu_prof:?})"
        );
        assert!(hu_prof.sentence_burstiness_cv > ai_prof.sentence_burstiness_cv);
    }
}
