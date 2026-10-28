//! Glyph clustering: positioned glyph cells → words.
//!
//! A PDF rarely draws a word with one operator. A single word can arrive as several `Tj`
//! strings split at kerning pairs (`[(K) -40 (erning)] TJ`), as one string per glyph with an
//! explicit `Tm` per character, with `Tc` character tracking widening every advance, or with
//! `Tw` word spacing stretching only the blanks. Emitting one token per showing operation —
//! the naive approach — produces `K e r n i n g`, and emitting one token per glyph produces
//! `t h i s   i s`.
//!
//! This module is the single place where glyphs become words, and it uses the two facts that
//! actually distinguish a word break from letter spacing:
//!
//! * **Contiguity.** A glyph that starts at (or before) the pen position the previous glyph
//!   ended at is part of the same word. Tracking (`Tc`), horizontal scaling (`Tz`) and `TJ`
//!   displacements are already baked into the advance, so an untracked word has a gap of
//!   exactly zero and a tracked word also has a gap of exactly zero.
//! * **A real blank.** A word gap is at least half the font's own space advance. Kerning pairs
//!   are a few hundredths of an em and never reach it; a `TJ` word gap (typically `-200` to
//!   `-300`, i.e. 0.2–0.3 em) does. The threshold is expressed in the *font's* space advance
//!   rather than a fixed `0.25 × font_size`, so a condensed face with a `0.18em` space and a
//!   monospaced face with a `0.60em` space both break in the right place.
//!
//! Style changes also end a word: a run cannot mix two fonts, two weights or two colours,
//! because the exporters map one run onto one `<w:rPr>`/emphasis span. Splitting there is
//! safe — the gap across the split is zero, so the analyser glues the pieces back together
//! with no space between them.

use super::model::{Glyph, Rect, TextRun};

/// Tolerances for [`words_from_glyphs`].
#[derive(Debug, Clone, Copy)]
pub struct ClusterOptions {
    /// A gap is a word break when it exceeds this fraction of the font's space advance.
    pub space_fraction: f32,
    /// …but never below this fraction of the em (protects fonts with a tiny space advance).
    pub min_space_em: f32,
    /// Baseline drift tolerated inside one line, as a fraction of the font size.
    pub baseline_em: f32,
    /// Size difference (points) that makes two glyphs a different run.
    pub size_tolerance: f32,
}

impl Default for ClusterOptions {
    fn default() -> Self {
        ClusterOptions {
            space_fraction: 0.5,
            min_space_em: 0.12,
            baseline_em: 0.35,
            size_tolerance: 0.6,
        }
    }
}

fn compare_f32(a: f32, b: f32) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

/// The gap that counts as a word break for `glyph`.
fn space_threshold(glyph: &Glyph, options: &ClusterOptions) -> f32 {
    let natural = glyph.space_width.max(glyph.font_size * options.min_space_em);
    (natural * options.space_fraction).max(glyph.font_size * options.min_space_em)
}

/// Cluster glyphs into word runs.
///
/// Glyphs are bucketed into lines by baseline first, then ordered left to right inside each
/// line, so a two-column page cannot interleave. The returned runs are in reading order for
/// the page (top to bottom, left to right within a baseline).
pub fn words_from_glyphs(glyphs: &[Glyph]) -> Vec<TextRun> {
    words_from_glyphs_with(glyphs, &ClusterOptions::default())
}

/// [`words_from_glyphs`] with explicit tolerances.
pub fn words_from_glyphs_with(glyphs: &[Glyph], options: &ClusterOptions) -> Vec<TextRun> {
    let mut ordered: Vec<&Glyph> = glyphs
        .iter()
        .filter(|glyph| !glyph.text.is_empty())
        .collect();
    if ordered.is_empty() {
        return Vec::new();
    }
    ordered.sort_by(|a, b| {
        compare_f32(a.baseline, b.baseline).then(compare_f32(a.rect.x, b.rect.x))
    });

    let mut runs: Vec<TextRun> = Vec::new();
    let mut start = 0usize;
    while start < ordered.len() {
        let reference = ordered[start].baseline;
        let mut end = start + 1;
        while end < ordered.len() {
            let candidate = ordered[end];
            // A baseline bucket is anchored on its first glyph; superscripts (a `Ts` rise)
            // legitimately sit a little off and get their own bucket, which the analyser
            // merges back into the same visual line.
            let tolerance = (candidate.font_size.max(ordered[start].font_size)
                * options.baseline_em)
                .max(0.4);
            if (candidate.baseline - reference).abs() > tolerance {
                break;
            }
            end += 1;
        }
        let mut line: Vec<&Glyph> = ordered[start..end].to_vec();
        line.sort_by(|a, b| compare_f32(a.rect.x, b.rect.x));
        runs.extend(cluster_line(&line, options));
        start = end;
    }
    runs
}

/// Cluster one baseline's glyphs (already ordered left to right) into words.
fn cluster_line(line: &[&Glyph], options: &ClusterOptions) -> Vec<TextRun> {
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current: Option<TextRun> = None;
    // Right edge of the previous glyph's advance box. `None` means "start a new word".
    let mut edge: Option<f32> = None;
    let mut after_blank = true;

    for glyph in line {
        if glyph.is_blank() {
            // An explicit space character is a word separator by definition; its advance is
            // consumed here and re-synthesised (once, or as indentation for monospace) when
            // the analyser rebuilds the line.
            if let Some(run) = current.take() {
                runs.push(run);
            }
            edge = Some(glyph.rect.right());
            after_blank = true;
            continue;
        }

        let starts_word = match (&current, edge) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(run), Some(edge)) => {
                // A negative gap is a *tightened* pair (true kerning): the boxes overlap, so
                // it can never be a word break. Only a gap wider than the font's own space
                // advance is.
                let gap = glyph.rect.x - edge;
                let threshold = space_threshold(glyph, options);
                after_blank
                    || gap > threshold
                    || breaks_style(run, glyph, options)
            }
        };

        if starts_word {
            if let Some(run) = current.take() {
                runs.push(run);
            }
            current = Some(run_from_glyph(glyph));
        } else if let Some(run) = current.as_mut() {
            run.text.push_str(&glyph.text);
            run.rect = run.rect.union(&glyph.rect);
        }
        edge = Some(glyph.rect.right());
        after_blank = false;
    }
    if let Some(run) = current.take() {
        runs.push(run);
    }
    runs
}

/// True when `glyph` cannot join `run` because the style would have to be averaged.
fn breaks_style(run: &TextRun, glyph: &Glyph, options: &ClusterOptions) -> bool {
    (run.font_size - glyph.font_size).abs() > options.size_tolerance
        || run.font_name != glyph.font_name
        || run.font_weight != glyph.font_weight
        || run.bold != glyph.bold
        || run.italic != glyph.italic
        || run.mono != glyph.mono
        || run.color != glyph.color
}

fn run_from_glyph(glyph: &Glyph) -> TextRun {
    TextRun {
        text: glyph.text.clone(),
        rect: glyph.rect,
        font_size: glyph.font_size,
        font_name: glyph.font_name.clone(),
        font_family: glyph.font_family.clone(),
        font_weight: glyph.font_weight,
        bold: glyph.bold,
        italic: glyph.italic,
        mono: glyph.mono,
        color: glyph.color,
        baseline: glyph.baseline,
        space_width: glyph.space_width,
    }
}

/// How many spaces belong between two adjacent runs on one line.
///
/// The counterpart of [`words_from_glyphs`] for the analyser: runs whose advance boxes touch
/// glue together with no space (that is exactly the case that used to turn "Kerning" into
/// "K e r n i n g"), one space otherwise. Monospaced lines quantise the gap so that code
/// indentation survives as indentation instead of collapsing to a single blank.
pub fn gap_spaces(previous: Rect, next: Rect, space_width: f32, mono: bool) -> u16 {
    let gap = next.x - previous.right();
    let space = space_width.max(1.0);
    // Half a space advance is the same threshold the clusterer uses, so a run the clusterer
    // split off (kerning, a style change) is re-joined with no space.
    if gap <= space * 0.5 {
        return 0;
    }
    if mono {
        let count = ((gap + space * 0.35) / space).round();
        return count.clamp(1.0, 12.0) as u16;
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::model::Rgb;

    fn glyph(text: &str, x: f32, baseline: f32, size: f32, advance: f32) -> Glyph {
        glyph_with(text, x, baseline, size, advance, "DejaVuSans", 400)
    }

    fn glyph_with(
        text: &str,
        x: f32,
        baseline: f32,
        size: f32,
        advance: f32,
        name: &str,
        weight: u16,
    ) -> Glyph {
        let (ascent, descent, space) = crate::doc::fontmap::base_metrics(
            &crate::doc::fontmap::family(name),
            crate::doc::fontmap::name_is_mono(name),
        );
        Glyph {
            text: text.to_string(),
            rect: Rect::new(
                x,
                baseline - ascent * size,
                advance.max(0.01),
                (ascent + descent) * size,
            ),
            baseline,
            space_width: space * size,
            font_size: size,
            font_name: name.to_string(),
            font_family: crate::doc::fontmap::family(name),
            font_weight: weight,
            bold: weight >= 700,
            italic: false,
            mono: crate::doc::fontmap::name_is_mono(name),
            color: Rgb::BLACK,
        }
    }

    /// Lay out `text` as a single string with per-character advances: no tracking, no
    /// kerning, exactly what an untracked `Tj` produces.
    fn lay_out(text: &str, x: f32, baseline: f32, size: f32) -> (Vec<Glyph>, f32) {
        let mut glyphs = Vec::new();
        let mut pen = x;
        for ch in text.chars() {
            let advance = if ch == ' ' { size * 0.318 } else { size * 0.55 };
            glyphs.push(glyph(&ch.to_string(), pen, baseline, size, advance));
            pen += advance;
        }
        (glyphs, pen)
    }

    #[test]
    fn a_plain_line_reconstructs_whole_words() {
        let (glyphs, _) = lay_out("the quick brown fox", 10.0, 100.0, 10.0);
        let runs = words_from_glyphs(&glyphs);
        let words: Vec<&str> = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(words, ["the", "quick", "brown", "fox"]);
        // Boxes must stay in order and never overlap a neighbour.
        for pair in runs.windows(2) {
            assert!(pair[1].rect.x >= pair[0].rect.right());
        }
    }

    #[test]
    fn kerning_splits_do_not_become_spaces() {
        // `[(Kern) -40 (ing)] TJ`: one word delivered as two showing operations with a
        // negative displacement between them.
        let (first, pen) = lay_out("Kern", 10.0, 100.0, 10.0);
        let mut glyphs = first;
        glyphs.push(glyph("i", pen - 0.4, 100.0, 10.0, 2.8));
        glyphs.push(glyph("n", pen + 2.4, 100.0, 10.0, 6.3));
        glyphs.push(glyph("g", pen + 8.7, 100.0, 10.0, 6.1));
        let runs = words_from_glyphs(&glyphs);
        assert_eq!(runs.len(), 1, "kerning must not split the word: {runs:?}");
        assert_eq!(runs[0].text, "Kerning");
    }

    #[test]
    fn per_glyph_positioning_keeps_one_word() {
        // One `Tm` per glyph, each placed exactly where the previous advance ended.
        let mut glyphs = Vec::new();
        let mut pen = 30.0;
        for ch in "aaaa".chars() {
            glyphs.push(glyph(&ch.to_string(), pen, 90.0, 9.0, 5.0));
            pen += 5.0;
        }
        let runs = words_from_glyphs(&glyphs);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "aaaa");
    }

    #[test]
    fn character_tracking_keeps_words_whole() {
        // Tc = 2 at 10pt: every advance grows, so the pen-to-pen gap stays zero.
        let mut glyphs = Vec::new();
        let mut pen = 10.0;
        for ch in "tracked".chars() {
            glyphs.push(glyph(&ch.to_string(), pen, 50.0, 10.0, 7.0));
            pen += 7.0;
        }
        let runs = words_from_glyphs(&glyphs);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "tracked");
        assert!((runs[0].rect.width - 49.0).abs() < 0.01);
    }

    #[test]
    fn a_tj_word_gap_becomes_a_space_and_a_kern_does_not() {
        let (mut glyphs, pen) = lay_out("ab", 10.0, 70.0, 10.0);
        // 0.25em gap: a word break, because the font's space is 0.30em.
        glyphs.push(glyph("c", pen + 2.5, 70.0, 10.0, 5.0));
        let runs = words_from_glyphs(&glyphs);
        let words: Vec<&str> = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(words, ["ab", "c"]);

        // 0.05em gap: kerning, not a break.
        let (mut glyphs, pen) = lay_out("ab", 10.0, 70.0, 10.0);
        glyphs.push(glyph("c", pen + 0.5, 70.0, 10.0, 5.0));
        let runs = words_from_glyphs(&glyphs);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "abc");
    }

    #[test]
    fn mixed_sizes_on_one_baseline_split_runs_but_not_words() {
        let (mut glyphs, pen) = lay_out("and", 10.0, 40.0, 10.0);
        let (large, _) = lay_out("big", pen + 0.0, 40.0, 16.0);
        glyphs.extend(large);
        let runs = words_from_glyphs(&glyphs);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!(runs[0].text, "and");
        assert_eq!(runs[1].text, "big");
        assert!(runs[1].rect.height > runs[0].rect.height);
        assert!((runs[1].baseline - runs[0].baseline).abs() < 0.01);
    }

    #[test]
    fn style_changes_split_a_run_without_inserting_space() {
        let (plain, pen) = lay_out("bold", 10.0, 60.0, 10.0);
        let mut glyphs = plain;
        glyphs.push(glyph_with("x", pen, 60.0, 10.0, 5.0, "DejaVuSans-Bold", 700));
        let runs = words_from_glyphs(&glyphs);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "bold");
        assert_eq!(runs[1].text, "x");
        assert_eq!(runs[1].font_weight, 700);
        assert!(runs[1].bold);
    }

    #[test]
    fn two_columns_never_interleave() {
        let (mut left, _) = lay_out("left column", 20.0, 100.0, 10.0);
        let (right, _) = lay_out("right column", 320.0, 100.0, 10.0);
        left.extend(right);
        let runs = words_from_glyphs(&left);
        let text: Vec<&str> = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(text, ["left", "column", "right", "column"]);
    }

    #[test]
    fn monospaced_indentation_keeps_its_columns() {
        let size = 8.0;
        let space = 0.6 * size;
        let mut glyphs = Vec::new();
        // Four-space indent, then `return`, then one space, then `x`.
        let mut pen = 60.0;
        for _ in 0..4 {
            glyphs.push(glyph(" ", pen, 200.0, size, space));
            pen += space;
        }
        for ch in "return".chars() {
            glyphs.push(glyph_with(
                &ch.to_string(),
                pen,
                200.0,
                size,
                space,
                "DejaVuSansMono",
                400,
            ));
            pen += space;
        }
        let indent = pen;
        glyphs.push(glyph_with(" ", pen, 200.0, size, space, "DejaVuSansMono", 400));
        pen += space;
        glyphs.push(glyph_with("x", pen, 200.0, size, space, "DejaVuSansMono", 400));
        let runs = words_from_glyphs(&glyphs);
        assert_eq!(runs.len(), 2);
        // The four-space indent is preserved as geometry: the word starts one indent in.
        assert!((runs[0].rect.x - (60.0 + 4.0 * space)).abs() < 0.01);
        assert!((indent - runs[0].rect.x - 6.0 * space).abs() < 0.01);
        assert!(runs[1].rect.x > runs[0].rect.right());
    }

    #[test]
    fn gaps_become_one_space_or_none() {
        let kerning = Rect::new(10.0, 0.0, 40.0, 10.0);
        // Touching boxes: the same word, split only by a style change.
        let touching = Rect::new(50.0, 0.0, 20.0, 10.0);
        assert_eq!(gap_spaces(kerning, touching, 3.0, false), 0);
        // A normal word gap is slightly wider than the space advance (Tw stretch included).
        let spaced = Rect::new(53.5, 0.0, 20.0, 10.0);
        assert_eq!(gap_spaces(kerning, spaced, 3.0, false), 1);
        // Monospaced indentation keeps its column count.
        let four_spaces = Rect::new(50.0 + 4.0 * 4.8, 0.0, 20.0, 10.0);
        assert_eq!(gap_spaces(kerning, four_spaces, 4.8, true), 4);
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert!(words_from_glyphs(&[]).is_empty());
    }
}
