//! Layout analysis: raw page geometry in, interpreted IR out.
//!
//! Everything here is pure and deterministic — no I/O, no clock, no randomness — so the hard
//! geometric decisions (reading order, column gutters, table grids, running headers, multi-page
//! table spans) can be unit-tested against synthetic [`LayoutDoc`]s without needing a PDF.
//!
//! The analysis is deliberately conservative. When a signal is absent the analyser falls back to
//! "one column of paragraphs" rather than inventing structure, and every threshold is expressed
//! both absolutely (points) and relative to the text size, so a 9 pt footnote page and a 40 pt
//! poster both behave.

use super::model::*;
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Knobs for [`analyze`]. The defaults are what the CLI uses; tests turn pieces off to isolate
/// a heuristic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyzeOptions {
    pub extract_headers_footers: bool,
    pub detect_tables: bool,
    pub detect_columns: bool,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        AnalyzeOptions {
            extract_headers_footers: true,
            detect_tables: true,
            detect_columns: true,
        }
    }
}

impl AnalyzeOptions {
    /// No layout inference at all: one column of paragraphs per page, in paint order.
    pub fn flat() -> Self {
        AnalyzeOptions {
            extract_headers_footers: false,
            detect_tables: false,
            detect_columns: false,
        }
    }
}

/// A whitespace-delimited token with geometry and style.
#[derive(Debug, Clone, PartialEq)]
struct Word {
    text: String,
    rect: Rect,
    font_size: f32,
    bold: bool,
    italic: bool,
    mono: bool,
    color: Option<Rgb>,
}

/// A visual line: the words that share a baseline.
#[derive(Debug, Clone)]
struct Line {
    page: usize,
    words: Vec<Word>,
    rect: Rect,
    font_size: f32,
    bold: bool,
    italic: bool,
    mono: bool,
    consumed: bool,
}

impl Line {
    fn from_run(run: &TextRun, page: usize) -> Line {
        let mut line = Line {
            page,
            words: split_run_into_words(run),
            rect: run.rect,
            font_size: run.font_size,
            bold: run.bold,
            italic: run.italic,
            mono: run.mono,
            consumed: false,
        };
        line.finish();
        line
    }

    fn finish(&mut self) {
        self.words.sort_by(|a, b| compare_f32(a.rect.x, b.rect.x));
        if self.words.is_empty() {
            return;
        }
        let mut rect = self.words[0].rect;
        let mut weight = 0usize;
        let mut size_sum = 0.0f32;
        for word in &self.words {
            rect = rect.union(&word.rect);
            let chars = word.text.chars().count().max(1);
            weight += chars;
            size_sum += word.font_size * chars as f32;
        }
        self.rect = rect;
        if weight > 0 {
            self.font_size = size_sum / weight as f32;
        }
        let mut bold = true;
        let mut italic = true;
        let mut mono = true;
        for word in &self.words {
            bold &= word.bold;
            italic &= word.italic;
            mono &= word.mono;
        }
        self.bold = bold;
        self.italic = italic;
        self.mono = mono;
    }

    fn text(&self) -> String {
        let mut out = String::new();
        for (index, word) in self.words.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            out.push_str(&word.text);
        }
        out
    }

    fn char_count(&self) -> usize {
        self.words.iter().map(|w| w.text.chars().count()).sum()
    }

    fn center_y(&self) -> f32 {
        self.rect.center_y()
    }

    fn is_blank(&self) -> bool {
        self.words.is_empty()
    }
}

fn compare_f32(a: f32, b: f32) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

fn split_run_into_words(run: &TextRun) -> Vec<Word> {
    let chars: Vec<char> = run.text.chars().collect();
    let total = chars.len().max(1) as f32;
    let width = run.rect.width.max(0.0);
    let mut words: Vec<Word> = Vec::new();
    let mut start: Option<usize> = None;
    let height = run.rect.height.max(run.font_size * 0.9);
    let mut push = |from: usize, to: usize| {
        let text: String = chars[from..to].iter().collect();
        if text.is_empty() {
            return;
        }
        let x = run.rect.x + width * (from as f32 / total);
        let w = width * ((to - from) as f32 / total);
        words.push(Word {
            text,
            rect: Rect::new(x, run.rect.y, w.max(0.0), height),
            font_size: run.font_size,
            bold: run.bold,
            italic: run.italic,
            mono: run.mono,
            color: if run.color == Rgb::BLACK {
                None
            } else {
                Some(run.color)
            },
        });
    };
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_whitespace() {
            if let Some(from) = start.take() {
                push(from, index);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(from) = start {
        push(from, chars.len());
    }
    words
}

fn build_lines(page: &PageLayout, page_index: usize) -> Vec<Line> {
    let mut runs: Vec<&TextRun> = page.runs.iter().filter(|run| !run.is_blank()).collect();
    runs.sort_by(|a, b| {
        compare_f32(a.rect.center_y(), b.rect.center_y()).then(compare_f32(a.rect.x, b.rect.x))
    });
    let mut lines: Vec<Line> = Vec::new();
    for run in runs {
        let mut placed = false;
        if let Some(line) = lines.last_mut() {
            let tolerance = (line.font_size.max(run.font_size) * 0.45).max(1.5);
            // Words on one baseline belong to one line, unless the gap between them is a
            // column gutter: merging across a gutter would interleave the reading order.
            // A *negative* gap means this run starts behind the line's current right edge —
            // the paint order returned to the other column. That must open a new line too,
            // otherwise `left-1, right-1, left-2` becomes one interleaved line.
            let gap = run.rect.x - line.rect.right();
            let split = (line.font_size.max(run.font_size) * 1.6).max(8.0);
            let min_gap = -(line.font_size.max(run.font_size) * 0.35).max(1.5);
            if (line.center_y() - run.rect.center_y()).abs() <= tolerance
                && gap >= min_gap
                && gap <= split
            {
                for word in split_run_into_words(run) {
                    line.words.push(word);
                }
                line.finish();
                placed = true;
            }
        }
        if !placed {
            lines.push(Line::from_run(run, page_index));
        }
    }
    lines.retain(|line| !line.is_blank());
    lines.sort_by(|a, b| compare_f32(a.rect.y, b.rect.y).then(compare_f32(a.rect.x, b.rect.x)));
    lines
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Turn raw page geometry into the interpreted IR.
///
/// Returns the document and a list of human-readable warnings (skipped page-sized background
/// images, stitched tables, …).
pub fn analyze(layout: &LayoutDoc, options: &AnalyzeOptions) -> (Doc, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();
    if layout.pages.is_empty() {
        return (
            Doc::new(PageSpec::default()),
            vec!["document has no pages".to_string()],
        );
    }
    let page_count = layout.pages.len();
    let mut page_lines: Vec<Vec<Line>> = Vec::with_capacity(page_count);
    for (index, page) in layout.pages.iter().enumerate() {
        page_lines.push(build_lines(page, index));
    }

    let mut running_headers: Vec<String> = Vec::new();
    let mut running_footers: Vec<String> = Vec::new();
    if options.extract_headers_footers && page_count > 1 {
        detect_running_furniture(
            &mut page_lines,
            layout,
            &mut running_headers,
            &mut running_footers,
        );
    }

    let page_spec = derive_page_spec(layout);
    let mut blocks: Vec<Block> = Vec::new();
    for (index, (page, lines)) in layout.pages.iter().zip(page_lines.iter_mut()).enumerate() {
        blocks.extend(analyze_page(
            index,
            page,
            lines,
            layout,
            options,
            &mut warnings,
        ));
    }

    let blocks = stitch_tables(blocks, &mut warnings);
    let blocks = merge_adjacent_lists(blocks);

    let mut doc = Doc::new(page_spec);
    doc.blocks = blocks;
    doc.assets = layout.assets.clone();
    // Record the effective resolution of every extracted raster from its first placement.
    // Dimensions alone cannot tell a 900 px icon from a 900 px poster; DPI can.
    for page in &layout.pages {
        for placement in &page.images {
            if let Some(asset) = doc.assets.get_mut(placement.asset) {
                asset.observe_dpi(&placement.rect);
            }
        }
    }
    // Per-page geometry: ink bounds, furniture zones and the detected column count. These are
    // metadata, not block ranges, so stitching a cross-page table cannot make them stale.
    let mut columns_by_page: BTreeMap<usize, u8> = BTreeMap::new();
    for block in &doc.blocks {
        if let Block::Columns {
            columns,
            anchor: Some(anchor),
            ..
        } = block
        {
            let count = columns.len().clamp(1, u8::MAX as usize) as u8;
            let entry = columns_by_page.entry(anchor.page).or_insert(1);
            *entry = (*entry).max(count);
        }
    }
    let page_boundaries = layout
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let mut boundary = PageBoundary::new(index, page.width.max(1.0), page.height.max(1.0));
            let mut ink: Option<Rect> = None;
            for run in page.runs.iter().filter(|run| !run.is_blank()) {
                ink = Some(ink.map_or(run.rect, |rect| rect.union(&run.rect)));
            }
            for placement in &page.images {
                ink = Some(ink.map_or(placement.rect, |rect| rect.union(&placement.rect)));
            }
            if let Some(ink) = ink {
                boundary.content = ink;
            }
            boundary.columns = *columns_by_page.get(&index).unwrap_or(&1);
            boundary
        })
        .collect();
    doc.info = DocInfo {
        title: first_heading_text(&doc.blocks),
        author: None,
        page_count,
        source: SourceKind::Pdf,
        running_headers,
        running_footers,
        page_boundaries,
    };
    if doc.blocks.is_empty() {
        warnings.push("no text could be recovered from the document".to_string());
    }
    for warning in doc.validate() {
        warnings.push(format!("IR: {warning}"));
    }
    (doc, warnings)
}

fn first_heading_text(blocks: &[Block]) -> Option<String> {
    for block in blocks {
        if let Block::Heading { inlines, .. } = block {
            let text = inlines_text(inlines).trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn derive_page_spec(layout: &LayoutDoc) -> PageSpec {
    let Some(first) = layout.pages.first() else {
        return PageSpec::default();
    };
    let width = if first.width > 1.0 {
        first.width
    } else {
        612.0
    };
    let height = if first.height > 1.0 {
        first.height
    } else {
        792.0
    };
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    for page in &layout.pages {
        for run in &page.runs {
            if run.is_blank() {
                continue;
            }
            min_x = min_x.min(run.rect.x);
            max_x = max_x.max(run.rect.right());
            min_y = min_y.min(run.rect.y);
            max_y = max_y.max(run.rect.bottom());
        }
    }
    if min_x == f32::MAX {
        return PageSpec {
            width,
            height,
            ..PageSpec::default()
        };
    }
    PageSpec {
        width,
        height,
        margin_top: min_y.clamp(9.0, height * 0.4),
        margin_right: (width - max_x).clamp(9.0, width * 0.4),
        margin_bottom: (height - max_y).clamp(9.0, height * 0.4),
        margin_left: min_x.clamp(9.0, width * 0.4),
    }
}

// ---------------------------------------------------------------------------
// Running headers and footers
// ---------------------------------------------------------------------------

fn normalize_furniture_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            out.push('#');
        } else if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        } else if ch.is_whitespace() && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// Strip text that repeats in the top or bottom band of multiple pages: running heads, page
/// numbers, "CONFIDENTIAL" stamps and similar page furniture. A line is only removed when the
/// same normalised text reappears on another page, so unique content in a header band is kept.
fn detect_running_furniture(
    page_lines: &mut [Vec<Line>],
    layout: &LayoutDoc,
    headers: &mut Vec<String>,
    footers: &mut Vec<String>,
) {
    let page_count = layout.pages.len();
    let threshold = if page_count >= 6 { 3 } else { 2 };
    let mut top: BTreeMap<String, (usize, Vec<(usize, usize)>)> = BTreeMap::new();
    let mut bottom: BTreeMap<String, (usize, Vec<(usize, usize)>)> = BTreeMap::new();
    for (page_index, lines) in page_lines.iter().enumerate() {
        let height = layout.pages[page_index].height.max(1.0);
        for (line_index, line) in lines.iter().enumerate() {
            let normalized = normalize_furniture_text(&line.text());
            if normalized.is_empty() {
                continue;
            }
            if line.rect.bottom() <= height * 0.09 {
                let entry = top.entry(normalized).or_insert((0, Vec::new()));
                entry.0 += 1;
                entry.1.push((page_index, line_index));
            } else if line.rect.y >= height * 0.91 {
                let entry = bottom.entry(normalized).or_insert((0, Vec::new()));
                entry.0 += 1;
                entry.1.push((page_index, line_index));
            }
        }
    }
    let mut remove: Vec<(usize, usize)> = Vec::new();
    for (text, (count, positions)) in &top {
        if *count >= threshold {
            headers.push(text.clone());
            remove.extend(positions.iter().copied());
        }
    }
    for (text, (count, positions)) in &bottom {
        if *count >= threshold {
            footers.push(text.clone());
            remove.extend(positions.iter().copied());
        }
    }
    for (page_index, line_index) in remove {
        if let Some(line) = page_lines
            .get_mut(page_index)
            .and_then(|lines| lines.get_mut(line_index))
        {
            line.consumed = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Page analysis
// ---------------------------------------------------------------------------

struct Element {
    sort_y: f32,
    sort_x: f32,
    block: Block,
}

fn analyze_page(
    page_index: usize,
    page: &PageLayout,
    lines: &mut [Line],
    layout: &LayoutDoc,
    options: &AnalyzeOptions,
    warnings: &mut Vec<String>,
) -> Vec<Block> {
    let mut elements: Vec<Element> = Vec::new();

    let tables = if options.detect_tables {
        detect_tables(page_index, page, lines)
    } else {
        Vec::new()
    };
    let mut table_rects: Vec<Rect> = Vec::new();
    for (rect, table) in tables {
        table_rects.push(rect);
        elements.push(Element {
            sort_y: rect.y,
            sort_x: rect.x,
            block: Block::Table(table),
        });
    }

    for (rect, figure) in detect_figures(page_index, page, lines, layout, warnings) {
        elements.push(Element {
            sort_y: rect.y,
            sort_x: rect.x,
            block: Block::Figure(figure),
        });
    }

    for (rect, shape) in detect_vector_shapes(page_index, page, &table_rects) {
        elements.push(Element {
            sort_y: rect.y,
            sort_x: rect.x,
            block: Block::Vector(shape),
        });
    }

    for slice in build_slices(lines, options) {
        let groups = emit_slice(&slice, lines);
        if groups.is_empty() {
            continue;
        }
        if groups.len() == 1 {
            let group = groups.into_iter().next().unwrap_or_default();
            for block in group {
                let anchor = block.anchor();
                elements.push(Element {
                    sort_y: anchor.map(|a| a.rect.y).unwrap_or(slice.rect.y),
                    sort_x: anchor.map(|a| a.rect.x).unwrap_or(slice.rect.x),
                    block,
                });
            }
            continue;
        }
        elements.push(Element {
            sort_y: slice.rect.y,
            sort_x: slice.rect.x,
            block: columns_block(page_index, &slice, groups),
        });
    }

    elements.sort_by(|a, b| compare_f32(a.sort_y, b.sort_y).then(compare_f32(a.sort_x, b.sort_x)));
    apply_callout_boxes(page_index, page, &mut elements);
    elements.into_iter().map(|element| element.block).collect()
}

fn columns_block(page_index: usize, slice: &Slice, groups: Vec<Vec<Block>>) -> Block {
    let total: f32 = slice
        .columns
        .iter()
        .map(|column| (column.x1 - column.x0).max(1.0))
        .sum();
    let mut columns: Vec<Column> = Vec::with_capacity(groups.len());
    for (index, blocks) in groups.into_iter().enumerate() {
        let (x0, x1) = slice
            .columns
            .get(index)
            .map(|column| (column.x0, column.x1))
            .unwrap_or((slice.rect.x, slice.rect.right()));
        columns.push(Column {
            blocks,
            width: (x1 - x0).max(1.0) / total.max(1.0),
        });
    }
    Block::Columns {
        columns,
        anchor: Some(Anchor::new(page_index, slice.rect)),
    }
}

// ---------------------------------------------------------------------------
// Columns and reading order
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Slice {
    rect: Rect,
    columns: Vec<SliceColumn>,
}

#[derive(Debug)]
struct SliceColumn {
    lines: Vec<usize>,
    x0: f32,
    x1: f32,
}

/// How much of the content width a line must cover before it is treated as spanning columns.
const FULL_WIDTH_FRACTION: f32 = 0.72;
const MIN_GUTTER: f32 = 11.0;

fn build_slices(lines: &[Line], options: &AnalyzeOptions) -> Vec<Slice> {
    let indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.consumed && !line.is_blank())
        .map(|(index, _)| index)
        .collect();
    if indices.is_empty() {
        return Vec::new();
    }
    let mut bounds = lines[indices[0]].rect;
    for index in &indices {
        bounds = bounds.union(&lines[*index].rect);
    }
    let avg_size = average_font_size(lines, &indices);

    let gutters = if options.detect_columns {
        find_gutters(lines, &indices, &bounds)
    } else {
        Vec::new()
    };
    if gutters.is_empty() {
        let mut sorted = indices;
        sorted.sort_by(|a, b| {
            compare_f32(lines[*a].rect.y, lines[*b].rect.y)
                .then(compare_f32(lines[*a].rect.x, lines[*b].rect.x))
        });
        return vec![Slice {
            rect: bounds,
            columns: vec![SliceColumn {
                lines: sorted,
                x0: bounds.x,
                x1: bounds.right(),
            }],
        }];
    }

    let column_spans = column_spans_from_gutters(&gutters, &bounds);
    let mut full_width: Vec<usize> = Vec::new();
    let mut columnar: Vec<usize> = Vec::new();
    for index in &indices {
        let line = &lines[*index];
        let spans_gutter = column_spans
            .iter()
            .any(|(x0, x1)| line.rect.x < *x0 - 1.0 && line.rect.right() > *x1 + 1.0);
        if spans_gutter || line.rect.width > bounds.width * FULL_WIDTH_FRACTION {
            full_width.push(*index);
        } else {
            columnar.push(*index);
        }
    }
    // A short last line of a full-width banner is narrower than the column threshold and has
    // no twin at the same height on the other side of a gutter. Without this pass it would be
    // pulled into column 0 and orphaned from the banner it belongs to.
    let span_of = |line: &Line| -> Option<usize> {
        let center = line.rect.center_x();
        column_spans
            .iter()
            .position(|(x0, x1)| center >= *x0 - 1.0 && center <= *x1 + 1.0)
    };
    let mut solo = Vec::new();
    for (position, index) in columnar.iter().enumerate() {
        let line = &lines[*index];
        let own_span = span_of(line);
        let has_twin = columnar.iter().enumerate().any(|(other_position, other)| {
            other_position != position
                && (lines[*other].rect.center_y() - line.rect.center_y()).abs() <= avg_size * 0.8
                && span_of(&lines[*other]) != own_span
        });
        if !has_twin && line.rect.x <= bounds.x + 2.0 {
            solo.push(*index);
            full_width.push(*index);
        }
    }
    columnar.retain(|index| !solo.contains(index));
    full_width.sort_by(|a, b| compare_f32(lines[*a].rect.y, lines[*b].rect.y));

    let mut bands: Vec<(f32, f32, Vec<usize>)> = Vec::new();
    for index in full_width {
        let line = &lines[index];
        let mut merged = false;
        if let Some(last) = bands.last_mut() {
            let gap = line.rect.y - last.1;
            if gap <= (avg_size * 1.9).max(6.0) {
                last.1 = last.1.max(line.rect.bottom());
                last.2.push(index);
                merged = true;
            }
        }
        if !merged {
            bands.push((line.rect.y, line.rect.bottom(), vec![index]));
        }
    }

    let band_bottoms: Vec<f32> = bands.iter().map(|(_, bottom, _)| *bottom).collect();

    // Columnar lines are grouped by the vertical band they sit in, so a two-column body under
    // a full-width banner stays one sliced stack and reads column by column.
    let mut banded: Vec<(usize, Vec<usize>)> = Vec::new();
    for index in columnar {
        let center = lines[index].rect.center_y();
        let band = band_bottoms
            .iter()
            .filter(|bottom| **bottom <= center)
            .count();
        match banded.iter_mut().find(|(current, _)| *current == band) {
            Some((_, group)) => group.push(index),
            None => banded.push((band, vec![index])),
        }
    }

    let mut slices: Vec<Slice> = Vec::new();
    for (top, bottom, group) in bands {
        let mut sorted = group;
        sorted.sort_by(|a, b| {
            compare_f32(lines[*a].rect.y, lines[*b].rect.y)
                .then(compare_f32(lines[*a].rect.x, lines[*b].rect.x))
        });
        slices.push(Slice {
            rect: Rect::new(bounds.x, top, bounds.width, (bottom - top).max(1.0)),
            columns: vec![SliceColumn {
                lines: sorted,
                x0: bounds.x,
                x1: bounds.right(),
            }],
        });
    }

    for (_, group) in banded {
        let mut by_column: Vec<(usize, Vec<usize>)> = Vec::new();
        for index in group {
            let line = &lines[index];
            let mut chosen = 0usize;
            let mut best = f32::MAX;
            for (position, (x0, x1)) in column_spans.iter().enumerate() {
                let center = line.rect.center_x();
                let distance = if center < *x0 {
                    *x0 - center
                } else if center > *x1 {
                    center - *x1
                } else {
                    0.0
                };
                if distance < best {
                    best = distance;
                    chosen = position;
                }
            }
            match by_column
                .iter_mut()
                .find(|(position, _)| *position == chosen)
            {
                Some((_, column_lines)) => column_lines.push(index),
                None => by_column.push((chosen, vec![index])),
            }
        }
        by_column.sort_by_key(|(position, _)| *position);
        let mut columns: Vec<SliceColumn> = Vec::new();
        let mut top = f32::MAX;
        let mut bottom = f32::MIN;
        for (position, mut column_lines) in by_column {
            column_lines.sort_by(|a, b| {
                compare_f32(lines[*a].rect.y, lines[*b].rect.y)
                    .then(compare_f32(lines[*a].rect.x, lines[*b].rect.x))
            });
            let (mut x0, mut x1) = column_spans[position];
            for index in &column_lines {
                x0 = x0.min(lines[*index].rect.x);
                x1 = x1.max(lines[*index].rect.right());
                top = top.min(lines[*index].rect.y);
                bottom = bottom.max(lines[*index].rect.bottom());
            }
            columns.push(SliceColumn {
                lines: column_lines,
                x0,
                x1,
            });
        }
        if columns.is_empty() {
            continue;
        }
        let left = columns.iter().map(|c| c.x0).fold(f32::MAX, f32::min);
        let right = columns.iter().map(|c| c.x1).fold(f32::MIN, f32::max);
        slices.push(Slice {
            rect: Rect::new(left, top, (right - left).max(1.0), (bottom - top).max(1.0)),
            columns,
        });
    }

    slices.sort_by(|a, b| compare_f32(a.rect.y, b.rect.y).then(compare_f32(a.rect.x, b.rect.x)));
    slices
}

fn average_font_size(lines: &[Line], indices: &[usize]) -> f32 {
    let mut total = 0.0f32;
    let mut weight = 0usize;
    for index in indices {
        let line = &lines[*index];
        let chars = line.char_count().max(1);
        total += line.font_size * chars as f32;
        weight += chars;
    }
    if weight == 0 {
        11.0
    } else {
        total / weight as f32
    }
}

/// Vertical white corridors that genuinely separate two columns: a corridor counts only when
/// text exists on *both* sides at the same height, which is what distinguishes a column gutter
/// from a short centred line.
fn find_gutters(lines: &[Line], indices: &[usize], bounds: &Rect) -> Vec<(f32, f32)> {
    if indices.len() < 4 || bounds.width < 60.0 {
        return Vec::new();
    }
    let bins = (bounds.width.ceil() as usize + 1).max(2);
    let mut covered = vec![false; bins];
    for index in indices {
        let line = &lines[*index];
        // A full-width banner sits *above* the columns and a callout box can straddle the
        // gutter lower down; letting either fill the coverage bitmap erases the white corridor
        // and the columns get interleaved. Only lines narrower than 55% of the page's ink can
        // vote on where a gutter is — a real column line never spans half the page.
        if line.rect.width > bounds.width * 0.55 {
            continue;
        }
        let start = ((line.rect.x - bounds.x).max(0.0) as usize).min(bins - 1);
        let end = ((line.rect.right() - bounds.x).max(0.0) as usize).min(bins - 1);
        for slot in covered.iter_mut().take(end + 1).skip(start) {
            *slot = true;
        }
    }
    let mut gutters: Vec<(f32, f32)> = Vec::new();
    let mut run_start: Option<usize> = None;
    for (position, filled) in covered.iter().enumerate() {
        if *filled {
            if let Some(start) = run_start.take() {
                let width = (position - start) as f32;
                if width >= MIN_GUTTER && start > 0 && position < bins - 1 {
                    gutters.push((bounds.x + start as f32, bounds.x + position as f32));
                }
            }
        } else if run_start.is_none() {
            run_start = Some(position);
        }
    }
    gutters.retain(|(x0, x1)| {
        let mut left: Vec<&Line> = Vec::new();
        let mut right: Vec<&Line> = Vec::new();
        for index in indices {
            let line = &lines[*index];
            if line.rect.right() <= *x0 + 1.0 {
                left.push(line);
            } else if line.rect.x >= *x1 - 1.0 {
                right.push(line);
            }
        }
        if left.len() < 2 || right.len() < 2 {
            return false;
        }
        left.iter().any(|l| {
            right.iter().any(|r| {
                let overlap = l.rect.bottom().min(r.rect.bottom()) - l.rect.y.max(r.rect.y);
                overlap > l.rect.height.max(r.rect.height) * 0.5
            })
        })
    });
    merge_gutters(gutters)
}

fn merge_gutters(gutters: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let mut sorted = gutters;
    sorted.sort_by(|a, b| compare_f32(a.0, b.0));
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for (x0, x1) in sorted {
        match merged.last_mut() {
            Some(last) if x0 <= last.1 + 2.0 => last.1 = last.1.max(x1),
            _ => merged.push((x0, x1)),
        }
    }
    if merged.len() > 3 {
        merged.sort_by(|a, b| compare_f32(b.1 - b.0, a.1 - a.0));
        merged.truncate(3);
        merged.sort_by(|a, b| compare_f32(a.0, b.0));
    }
    merged
}

fn column_spans_from_gutters(gutters: &[(f32, f32)], bounds: &Rect) -> Vec<(f32, f32)> {
    let mut spans: Vec<(f32, f32)> = Vec::new();
    let mut cursor = bounds.x;
    for (x0, x1) in gutters {
        if *x0 > cursor {
            spans.push((cursor, *x0));
        }
        cursor = *x1;
    }
    if bounds.right() > cursor {
        spans.push((cursor, bounds.right()));
    }
    if spans.is_empty() {
        spans.push((bounds.x, bounds.right()));
    }
    spans
}

// ---------------------------------------------------------------------------
// Flow blocks inside a slice
// ---------------------------------------------------------------------------

fn emit_slice(slice: &Slice, lines: &mut [Line]) -> Vec<Vec<Block>> {
    let mut groups: Vec<Vec<Block>> = Vec::with_capacity(slice.columns.len());
    for column in &slice.columns {
        groups.push(build_flow_blocks(&column.lines, lines));
    }
    groups
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowKind {
    Body,
    Heading,
    Bullet,
    Code,
}

fn build_flow_blocks(indices: &[usize], lines: &mut [Line]) -> Vec<Block> {
    if indices.is_empty() {
        return Vec::new();
    }
    let body_size = average_font_size(lines, indices);
    let mut sizes: Vec<f32> = Vec::new();
    for index in indices {
        let line = &lines[*index];
        if line.font_size > body_size * 1.12 && line.char_count() <= 140 {
            sizes.push(round_half(line.font_size));
        }
    }
    sizes.sort_by(|a, b| compare_f32(*b, *a));
    sizes.dedup();

    let mut blocks: Vec<Block> = Vec::new();
    let mut cursor = 0usize;
    while cursor < indices.len() {
        match classify_line(&lines[indices[cursor]], body_size) {
            FlowKind::Bullet => {
                let (list, next) = take_list(indices, lines, cursor);
                blocks.push(list);
                cursor = next;
            }
            FlowKind::Code => {
                let (code, next) = take_code(indices, lines, cursor);
                blocks.push(code);
                cursor = next;
            }
            FlowKind::Heading => {
                let index = indices[cursor];
                let line = &lines[index];
                let level = heading_level(line.font_size, line.bold, &sizes);
                blocks.push(Block::Heading {
                    level,
                    inlines: inlines_from_words(&line.words),
                    anchor: Some(Anchor::new(line.page, line.rect)),
                });
                lines[index].consumed = true;
                cursor += 1;
            }
            FlowKind::Body => {
                let (paragraph, next) = take_paragraph(indices, lines, cursor, body_size);
                blocks.push(paragraph);
                cursor = next;
            }
        }
    }
    blocks
}

fn round_half(value: f32) -> f32 {
    (value * 2.0).round() / 2.0
}

fn heading_level(size: f32, bold: bool, sizes: &[f32]) -> u8 {
    let rounded = round_half(size);
    if let Some(position) = sizes
        .iter()
        .position(|candidate| (*candidate - rounded).abs() < 1.0)
    {
        return (position as u8 + 1).clamp(1, 6);
    }
    // A bold-only heading sits below the sized hierarchy.
    let offset = sizes.len() as u8 + if bold { 1 } else { 2 };
    offset.clamp(1, 6)
}

fn classify_line(line: &Line, body_size: f32) -> FlowKind {
    let text = line.text();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return FlowKind::Body;
    }
    if is_bullet_prefix(trimmed) {
        return FlowKind::Bullet;
    }
    if line.mono && line.font_size <= body_size * 1.15 {
        return FlowKind::Code;
    }
    if line.font_size > body_size * 1.12 && line.char_count() <= 140 {
        return FlowKind::Heading;
    }
    if line.bold
        && line.char_count() <= 90
        && !trimmed.ends_with(['.', ',', ';', ':'])
        && trimmed.split_whitespace().count() <= 12
    {
        return FlowKind::Heading;
    }
    FlowKind::Body
}

fn is_bullet_prefix(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if matches!(
        first,
        '•' | '‣' | '◦' | '▪' | '·' | '●' | '○' | '■' | '□' | '–' | '-' | '*'
    ) {
        return text.chars().nth(1).is_some_and(|c| c.is_whitespace());
    }
    if first.is_ascii_digit() {
        let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
        let rest: String = text.chars().skip(digits.len()).take(2).collect();
        if rest.starts_with('.') || rest.starts_with(')') {
            return rest.len() == 1 || rest.chars().nth(1).is_some_and(|c| c.is_whitespace());
        }
    }
    false
}

fn strip_bullet(text: &str) -> String {
    let mut rest = text.trim_start();
    if let Some(first) = rest.chars().next() {
        if !first.is_alphanumeric() {
            rest = rest[first.len_utf8()..].trim_start();
            return rest.to_string();
        }
        if first.is_ascii_digit() {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            rest = rest[digits.len()..].trim_start();
            if let Some(marker) = rest.chars().next()
                && (marker == '.' || marker == ')')
            {
                rest = rest[marker.len_utf8()..].trim_start();
            }
        }
    }
    rest.to_string()
}

fn drop_marker_word(words: &mut Vec<Word>, text: &str) {
    let marker_is_symbol = text
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| !c.is_alphanumeric());
    let marker_is_number = text
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit());
    if marker_is_symbol || marker_is_number {
        words.remove(0);
    }
}

fn take_list(indices: &[usize], lines: &mut [Line], start: usize) -> (Block, usize) {
    let first_index = indices[start];
    let ordered = lines[first_index]
        .text()
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit());
    let base_x = indices
        .iter()
        .map(|index| lines[*index].rect.x)
        .fold(f32::MAX, f32::min);
    let mut items: Vec<ListItem> = Vec::new();
    let mut cursor = start;
    let mut anchor_rect = lines[first_index].rect;
    let anchor_page = lines[first_index].page;
    while cursor < indices.len() {
        let index = indices[cursor];
        let text = lines[index].text();
        let trimmed = text.trim();
        if !is_bullet_prefix(trimmed) {
            break;
        }
        let mut item_text = strip_bullet(trimmed);
        let mut item_words = lines[index].words.clone();
        if !item_words.is_empty() {
            drop_marker_word(&mut item_words, trimmed);
        }
        let rect = lines[index].rect;
        let item_size = lines[index].font_size;
        anchor_rect = anchor_rect.union(&rect);
        let indent = (rect.x - base_x).max(0.0);
        let depth = ((indent / (item_size * 1.6).max(4.0)).round() as usize).min(4);
        lines[index].consumed = true;
        cursor += 1;
        while cursor < indices.len() {
            let next_index = indices[cursor];
            let previous_bottom = lines[indices[cursor - 1]].rect.bottom();
            // Copy everything needed out of the borrow before marking the line consumed.
            let next_rect = lines[next_index].rect;
            let next_size = lines[next_index].font_size;
            let next_text = lines[next_index].text().trim().to_string();
            let next_words = lines[next_index].words.clone();
            let gap = next_rect.y - previous_bottom;
            if gap > item_size * 0.9 || is_bullet_prefix(&next_text) || next_size > item_size * 1.15
            {
                break;
            }
            if !item_text.is_empty() {
                item_text.push(' ');
            }
            item_text.push_str(&next_text);
            item_words.extend(next_words);
            lines[next_index].consumed = true;
            cursor += 1;
        }
        items.push(ListItem {
            inlines: if item_words.is_empty() {
                vec![Inline::plain(item_text.trim().to_string())]
            } else {
                inlines_from_words(&item_words)
            },
            depth,
        });
    }
    (
        Block::List {
            ordered,
            items,
            anchor: Some(Anchor::new(anchor_page, anchor_rect)),
        },
        cursor,
    )
}

fn take_code(indices: &[usize], lines: &mut [Line], start: usize) -> (Block, usize) {
    let mut collected: Vec<String> = Vec::new();
    let mut cursor = start;
    let mut anchor_rect = lines[indices[start]].rect;
    let anchor_page = lines[indices[start]].page;
    let base_x = lines[indices[start]].rect.x;
    let char_width = (lines[indices[start]].font_size * 0.6).max(1.0);
    while cursor < indices.len() {
        let index = indices[cursor];
        let line = &lines[index];
        if !line.mono {
            break;
        }
        let indent = (((line.rect.x - base_x) / char_width).round() as i32).max(0) as usize;
        let text = line.text();
        collected.push(format!("{}{}", " ".repeat(indent), text.trim_end()));
        anchor_rect = anchor_rect.union(&line.rect);
        lines[index].consumed = true;
        cursor += 1;
    }
    (
        Block::Code {
            lines: collected,
            anchor: Some(Anchor::new(anchor_page, anchor_rect)),
            language: None,
        },
        cursor,
    )
}

fn take_paragraph(
    indices: &[usize],
    lines: &mut [Line],
    start: usize,
    body_size: f32,
) -> (Block, usize) {
    let mut words: Vec<Word> = Vec::new();
    let mut cursor = start;
    let mut rect = lines[indices[start]].rect;
    let first_size = lines[indices[start]].font_size;
    while cursor < indices.len() {
        let index = indices[cursor];
        let line = &lines[index];
        if cursor > start {
            let previous = &lines[indices[cursor - 1]];
            let gap = line.rect.y - previous.rect.bottom();
            let spacing = line.font_size.max(body_size);
            let size_changed = (line.font_size - first_size).abs() > 1.2;
            let kind_changed = matches!(
                classify_line(line, body_size),
                FlowKind::Bullet | FlowKind::Code | FlowKind::Heading
            );
            if gap > spacing * 0.85 || size_changed || kind_changed {
                break;
            }
            // Rejoin a word hyphenated across the line break: drop the hyphen and glue the
            // first word of this line onto the previous word, so the paragraph keeps one
            // continuous token and no stray space.
            let first_word = line.words.first().cloned();
            if let (Some(previous_word), Some(first_word)) = (words.last_mut(), first_word)
                && previous_word.text.ends_with('-')
                && !previous_word.text.ends_with("--")
                && first_word
                    .text
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_lowercase())
            {
                previous_word.text.pop();
                previous_word.text.push_str(&first_word.text);
                words.extend(line.words.iter().skip(1).cloned());
                rect = rect.union(&line.rect);
                lines[index].consumed = true;
                cursor += 1;
                continue;
            }
        }
        words.extend(line.words.iter().cloned());
        rect = rect.union(&line.rect);
        lines[index].consumed = true;
        cursor += 1;
    }
    let page = lines[indices[cursor.saturating_sub(1)]].page;
    (
        Block::Paragraph {
            inlines: inlines_from_words(&words),
            anchor: Some(Anchor::new(page, rect)),
            style: ParagraphStyle::default(),
        },
        cursor,
    )
}

fn inlines_from_words(words: &[Word]) -> Vec<Inline> {
    let mut inlines: Vec<Inline> = Vec::new();
    let mut current: Option<(InlineStyle, String)> = None;
    for word in words {
        let style = InlineStyle {
            bold: word.bold,
            italic: word.italic,
            code: word.mono,
            color: word.color,
            ..InlineStyle::default()
        };
        match current.as_mut() {
            Some((existing, text)) if *existing == style => {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&word.text);
            }
            _ => {
                if let Some((existing, text)) = current.take() {
                    inlines.push(Inline::new(text, existing));
                    // A style change does not delete the space between two words. Keep the
                    // separator as its own plain run so a coloured/bold span never inherits a
                    // leading space (which would break Markdown's emphasis parsing).
                    inlines.push(Inline::plain(" "));
                }
                current = Some((style, word.text.clone()));
            }
        }
    }
    if let Some((style, text)) = current {
        inlines.push(Inline::new(text, style));
    }
    if inlines.is_empty() {
        inlines.push(Inline::plain(String::new()));
    }
    inlines
}

// ---------------------------------------------------------------------------
// Callout boxes
// ---------------------------------------------------------------------------

fn apply_callout_boxes(page_index: usize, page: &PageLayout, elements: &mut [Element]) {
    let page_rect = page.rect();
    let boxes: Vec<FillRect> = page
        .fills
        .iter()
        .copied()
        .filter(|fill| fill.is_callout_sized(&page_rect))
        .collect();
    if boxes.is_empty() {
        return;
    }
    for element in elements.iter_mut() {
        // Code blocks already carry their own shaded container; tables and vector rules are
        // structure, not prose that needs a callout wrapper. Wrapping any of them would turn a
        // code frame into a quote box and hide the fence from GFM.
        if matches!(
            element.block,
            Block::Callout { .. }
                | Block::Code { .. }
                | Block::Table(_)
                | Block::Vector(_)
                | Block::Divider(_)
        ) {
            continue;
        }
        let Some(anchor) = element.block.anchor() else {
            continue;
        };
        let Some(rect) = block_rect(&element.block) else {
            continue;
        };
        let Some(container) = boxes.iter().find(|fill| {
            fill.rect
                .intersection(&rect)
                .is_some_and(|overlap| overlap.area() >= rect.area() * 0.6)
        }) else {
            continue;
        };
        let inner = element.block.clone();
        element.block = Block::Callout {
            kind: CalloutKind::Box,
            blocks: vec![inner],
            anchor: Some(Anchor::new(
                anchor.page.max(page_index.min(anchor.page)),
                anchor.rect,
            )),
            fill: container.fill,
        };
    }
}

/// Keep the meaningful vector decorations of a page as IR shapes with inline SVG.
///
/// Table rules are excluded (they are already represented as cells), as are the four edges of a
/// page-sized frame and the outlines of callout boxes (those become `Block::Callout`). What is
/// left — section rules, vertical dividers, dashed rule sets — is exactly the vector layer that
/// used to disappear between PDF and Markdown/DOCX.
fn detect_vector_shapes(
    page_index: usize,
    page: &PageLayout,
    table_rects: &[Rect],
) -> Vec<(Rect, VectorShape)> {
    let page_rect = page.rect();
    let callouts: Vec<FillRect> = page
        .fills
        .iter()
        .copied()
        .filter(|fill| fill.is_callout_sized(&page_rect))
        .collect();
    let mut shapes: Vec<(Rect, VectorShape)> = Vec::new();
    for rule in &page.rules {
        let rect = rule.rect;
        if rect.width.max(rect.height) < 20.0 || rule.thickness > 8.0 {
            continue;
        }
        if table_rects
            .iter()
            .any(|grid| rect.intersection(grid).is_some())
        {
            continue;
        }
        if callouts
            .iter()
            .any(|fill| rect.intersection(&fill.rect).is_some())
        {
            continue;
        }
        // Page-frame edges are furniture; exporting four full-page SVGs is noise, not fidelity.
        let near_left = rect.x < page.width * 0.09;
        let near_right = rect.right() > page.width * 0.91;
        let near_top = rect.y < page.height * 0.09;
        let near_bottom = rect.bottom() > page.height * 0.91;
        let is_frame = if rule.is_horizontal() {
            (near_top || near_bottom) && rect.width > page.width * 0.8
        } else {
            (near_left || near_right) && rect.height > page.height * 0.8
        };
        if is_frame {
            continue;
        }
        if shapes.iter().any(|(existing, _)| {
            (existing.x - rect.x).abs() < 2.0 && (existing.y - rect.y).abs() < 2.0
        }) {
            continue;
        }
        let shape = VectorShape::new(
            Some(Anchor::new(page_index, rect)),
            rect,
            VectorKind::Line,
            Some(rule.color),
            rule.thickness.max(0.4),
            None,
        );
        shapes.push((rect, shape));
        if shapes.len() >= 16 {
            break;
        }
    }
    shapes
}

fn block_rect(block: &Block) -> Option<Rect> {
    match block {
        Block::Heading { anchor, .. }
        | Block::Paragraph { anchor, .. }
        | Block::List { anchor, .. }
        | Block::Code { anchor, .. }
        | Block::Callout { anchor, .. }
        | Block::Columns { anchor, .. } => anchor.map(|a| a.rect),
        Block::Table(table) => table.anchor.map(|a| a.rect),
        Block::Figure(figure) => figure.anchor.map(|a| a.rect).or(figure.display_size),
        Block::Divider(divider) => divider.anchor.map(|a| a.rect),
        Block::Vector(shape) => shape.anchor.map(|a| a.rect),
    }
}

// ---------------------------------------------------------------------------
// Figures
// ---------------------------------------------------------------------------

fn detect_figures(
    page_index: usize,
    page: &PageLayout,
    lines: &mut [Line],
    layout: &LayoutDoc,
    warnings: &mut Vec<String>,
) -> Vec<(Rect, Figure)> {
    let mut figures: Vec<(Rect, Figure)> = Vec::new();
    let page_rect = page.rect();
    for placement in &page.images {
        let Some(asset) = layout.assets.get(placement.asset) else {
            continue;
        };
        if asset.is_empty() {
            continue;
        }
        let rect = placement.rect;
        if rect.width < 6.0 || rect.height < 6.0 {
            continue;
        }
        if rect.area() > page_rect.area() * 0.8 {
            warnings.push(format!(
                "page {}: skipped a page-sized background image ({}x{} px)",
                page_index + 1,
                asset.width_px,
                asset.height_px
            ));
            continue;
        }
        // An inline icon shares a baseline with a text line and is no taller than two lines
        // of body text. Keeping that distinction lets DOCX anchor block images while leaving
        // icons as inline drawings.
        let inline = lines.iter().any(|line| {
            !line.consumed
                && rect.height <= line.font_size * 2.4
                && (rect.center_y() - line.center_y()).abs() <= line.font_size.max(rect.height)
        });
        let caption = take_caption(lines, &rect);
        figures.push((
            rect,
            Figure {
                asset: placement.asset,
                anchor: Some(Anchor::new(page_index, rect)),
                caption,
                alt: format!(
                    "{}: {}x{} px, {}",
                    asset.name,
                    asset.width_px,
                    asset.height_px,
                    asset.kind.mime()
                ),
                display_size: Some(rect),
                inline,
            },
        ));
    }
    figures
}

fn take_caption(lines: &mut [Line], image_rect: &Rect) -> Option<Vec<Inline>> {
    let mut best: Option<usize> = None;
    let mut best_distance = f32::MAX;
    for (index, line) in lines.iter().enumerate() {
        if line.consumed {
            continue;
        }
        let distance = line.rect.y - image_rect.bottom();
        if !(-2.0..48.0).contains(&distance) {
            continue;
        }
        if line.rect.overlap_x(image_rect) < line.rect.width * 0.5 {
            continue;
        }
        let text = line.text();
        let lower = text.trim_start().to_lowercase();
        let looks_like_caption = lower.starts_with("figure")
            || lower.starts_with("fig.")
            || lower.starts_with("table")
            || lower.starts_with("chart")
            || lower.starts_with("exhibit")
            || lower.starts_with("source:");
        if looks_like_caption && distance < best_distance {
            best_distance = distance;
            best = Some(index);
        }
    }
    let index = best?;
    let inlines = inlines_from_words(&lines[index].words);
    lines[index].consumed = true;
    Some(inlines)
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// A connected group of drawing rules that may describe a table grid.
struct RuleComponent {
    vertical: Vec<(f32, f32, f32)>,
    horizontal: Vec<(f32, f32, f32)>,
    bounds: Rect,
    thickness: f32,
}

fn detect_tables(page_index: usize, page: &PageLayout, lines: &mut [Line]) -> Vec<(Rect, Table)> {
    let mut tables: Vec<(Rect, Table)> = Vec::new();
    for component in rule_components(page) {
        if component.vertical.len() < 2 || component.horizontal.len() < 2 {
            continue;
        }
        let Some(table) = build_table(page_index, page, &component, lines) else {
            continue;
        };
        // Cell text belongs to the table, not to the prose flow. Without this, every table is
        // emitted twice (once as cells, once as a giant "paragraph" of concatenated cell text)
        // and the duplicate blocks also prevent cross-page tables from being stitched.
        let bounds = component.bounds;
        for line in lines.iter_mut() {
            if !line.consumed && bounds.contains(line.rect.center_x(), line.rect.center_y()) {
                line.consumed = true;
            }
        }
        tables.push((bounds, table));
    }
    tables
}

fn rule_components(page: &PageLayout) -> Vec<RuleComponent> {
    let mut components: Vec<RuleComponent> = Vec::new();
    let mut rules: Vec<(Rect, Axis, f32)> = page
        .rules
        .iter()
        .filter(|rule| rule.thickness <= 6.0)
        .map(|rule| (rule.rect, rule.axis, rule.thickness))
        .collect();
    rules.sort_by(|a, b| compare_f32(a.0.y, b.0.y).then(compare_f32(a.0.x, b.0.x)));

    for (rect, axis, thickness) in rules {
        let mut hit: Option<usize> = None;
        for (index, component) in components.iter().enumerate() {
            let expanded = Rect::new(
                component.bounds.x - 6.0,
                component.bounds.y - 6.0,
                component.bounds.width + 12.0,
                component.bounds.height + 12.0,
            );
            if expanded.intersection(&rect).is_some() {
                hit = Some(index);
                break;
            }
        }
        let target = match hit {
            Some(index) => index,
            None => {
                components.push(RuleComponent {
                    vertical: Vec::new(),
                    horizontal: Vec::new(),
                    bounds: rect,
                    thickness: 0.5,
                });
                components.len() - 1
            }
        };
        let component = &mut components[target];
        component.bounds = component.bounds.union(&rect);
        component.thickness = component.thickness.max(thickness);
        if axis == Axis::Vertical {
            component
                .vertical
                .push((rect.center_x(), rect.y, rect.bottom()));
        } else {
            component
                .horizontal
                .push((rect.center_y(), rect.x, rect.right()));
        }
    }
    components
}

/// Longest union of rule segments lying on one grid coordinate.
///
/// `entries` is `(position, start, end)`; `positions` are the clustered grid coordinates. A
/// separator must be *internal* (not the outer border) and cover at least `min_fraction` of the
/// ruled extent. This is what distinguishes a table grid from a page frame or a rounded card.
fn grid_separator(
    entries: &[(f32, f32, f32)],
    positions: &[f32],
    extent: f32,
    min_fraction: f32,
) -> bool {
    if positions.len() < 3 || extent <= 0.0 {
        return false;
    }
    for position in positions.iter().skip(1).take(positions.len() - 2) {
        let mut segments: Vec<(f32, f32)> = entries
            .iter()
            .filter(|(value, _, _)| (value - *position).abs() <= 3.5)
            .map(|(_, start, end)| (*start, *end))
            .collect();
        segments.sort_by(|a, b| compare_f32(a.0, b.0));
        let mut covered = 0.0f32;
        let mut reach = f32::MIN;
        for (start, end) in segments {
            if end <= start {
                continue;
            }
            if start > reach {
                covered += end - start;
            } else if end > reach {
                covered += end - reach;
            }
            reach = reach.max(end);
        }
        if covered >= extent * min_fraction {
            return true;
        }
    }
    false
}

fn has_internal_separators(component: &RuleComponent, xs: &[f32], ys: &[f32]) -> bool {
    let width = (xs[xs.len() - 1] - xs[0]).max(1.0);
    let height = (ys[ys.len() - 1] - ys[0]).max(1.0);
    let rows = grid_separator(&component.horizontal, ys, width, 0.5);
    let columns = grid_separator(&component.vertical, xs, height, 0.4);
    // A one-column "table" is a bordered box (code, card, frame), never a data grid.
    rows && columns && xs.len() >= 3
}

fn cluster(values: &[f32], tolerance: f32) -> Vec<f32> {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| compare_f32(*a, *b));
    let mut out: Vec<f32> = Vec::new();
    for value in sorted {
        match out.last_mut() {
            Some(last) if (value - *last).abs() <= tolerance => {
                *last = (*last + value) / 2.0;
            }
            _ => out.push(value),
        }
    }
    out
}

fn build_table(
    page_index: usize,
    page: &PageLayout,
    component: &RuleComponent,
    lines: &[Line],
) -> Option<Table> {
    let bounds = component.bounds;
    if bounds.width < 30.0 || bounds.height < 12.0 {
        return None;
    }
    let xs = cluster(
        &component
            .vertical
            .iter()
            .map(|(x, _, _)| *x)
            .collect::<Vec<_>>(),
        3.0,
    );
    let ys = cluster(
        &component
            .horizontal
            .iter()
            .map(|(y, _, _)| *y)
            .collect::<Vec<_>>(),
        3.0,
    );
    if xs.len() < 2 || ys.len() < 2 {
        return None;
    }
    // A page frame, a callout card and a code-block border are also closed rectangles of
    // rules. Treating one as a table swallowed whole pages into a single bogus cell in the
    // previous iteration. A real grid has *internal* separators: at least one horizontal rule
    // between two rows spanning most of the grid width, and at least one vertical rule between
    // two columns spanning most of its height. Both are measured after clustering, so a rule
    // split by a merged cell still counts.
    let page_rect = page.rect();
    if bounds.width > page_rect.width * 0.86 && bounds.height > page_rect.height * 0.86 {
        return None;
    }
    if !has_internal_separators(component, &xs, &ys) {
        return None;
    }
    // An open table (rules only between columns, no closing border) stops short of its own
    // text: the outermost rule is an inner boundary, so extend the grid over the words.
    let mut xs = xs;
    let search = Rect::new(
        bounds.x - 60.0,
        bounds.y,
        bounds.width + 120.0,
        bounds.height,
    );
    let mut text_right = f32::MIN;
    for line in lines {
        for word in &line.words {
            if search.contains(word.rect.center_x(), word.rect.center_y()) {
                text_right = text_right.max(word.rect.right());
            }
        }
    }
    if text_right.is_finite() && text_right > *xs.last().unwrap_or(&bounds.x) + 2.0 {
        xs.push(text_right + 2.0);
    }
    let ruled_left = component
        .horizontal
        .iter()
        .map(|(_, x0, _)| *x0)
        .fold(f32::MAX, f32::min);
    let ruled_right = component
        .horizontal
        .iter()
        .map(|(_, _, x1)| *x1)
        .fold(f32::MIN, f32::max);
    let ruled_top = component
        .vertical
        .iter()
        .map(|(_, y0, _)| *y0)
        .fold(f32::MAX, f32::min);
    let ruled_bottom = component
        .vertical
        .iter()
        .map(|(_, _, y1)| *y1)
        .fold(f32::MIN, f32::max);
    let column_count = xs.len() - 1;
    let row_count = ys.len() - 1;
    if column_count > 40 || row_count > 400 {
        return None;
    }

    let vertical_present: Vec<Vec<bool>> = xs
        .iter()
        .map(|x| {
            (0..row_count)
                .map(|row| {
                    let y0 = ys[row].max(ruled_top);
                    let y1 = ys[row + 1].min(ruled_bottom).max(y0);
                    component.vertical.iter().any(|(vx, vy0, vy1)| {
                        (vx - x).abs() <= 3.0 && *vy0 <= y0 + 2.0 && *vy1 >= y1 - 2.0
                    })
                })
                .collect()
        })
        .collect();
    let horizontal_present: Vec<Vec<bool>> = ys
        .iter()
        .map(|y| {
            (0..column_count)
                .map(|column| {
                    let x0 = xs[column].max(ruled_left);
                    let x1 = xs[column + 1].min(ruled_right).max(x0);
                    component.horizontal.iter().any(|(hy, hx0, hx1)| {
                        (hy - y).abs() <= 3.0 && *hx0 <= x0 + 2.0 && *hx1 >= x1 - 2.0
                    })
                })
                .collect()
        })
        .collect();

    let mut occupied = vec![vec![false; column_count]; row_count];
    let mut rows: Vec<Row> = Vec::with_capacity(row_count);
    let mut right_votes = vec![0usize; column_count];
    let mut center_votes = vec![0usize; column_count];
    let mut left_votes = vec![0usize; column_count];
    let mut column_specs: Vec<ColumnSpec> = (0..column_count)
        .map(|index| ColumnSpec {
            width: (xs[index + 1] - xs[index]).max(1.0),
            align: Align::Left,
        })
        .collect();

    for row in 0..row_count {
        let mut cells: Vec<Cell> = Vec::new();
        for column in 0..column_count {
            if occupied[row][column] {
                continue;
            }
            let mut colspan = 1usize;
            while column + colspan < column_count
                && !vertical_present[column + colspan][row]
                && !occupied[row][column + colspan]
            {
                colspan += 1;
            }
            let mut rowspan = 1usize;
            'extend: while row + rowspan < row_count {
                for offset in 0..colspan {
                    if horizontal_present[row + rowspan][column + offset] {
                        break 'extend;
                    }
                }
                if (0..colspan).any(|offset| occupied[row + rowspan][column + offset]) {
                    break;
                }
                rowspan += 1;
            }
            for extra_row in 0..rowspan {
                for offset in 0..colspan {
                    occupied[row + extra_row][column + offset] = true;
                }
            }
            let cell_rect = Rect::new(
                xs[column],
                ys[row],
                xs[(column + colspan).min(column_count)] - xs[column],
                ys[(row + rowspan).min(row_count)] - ys[row],
            );
            let text_lines = words_in_rect(lines, &cell_rect);
            let align = vote_alignment(&text_lines, &cell_rect);
            match align {
                Align::Right => right_votes[column] += 1,
                Align::Center => center_votes[column] += 1,
                Align::Left => left_votes[column] += 1,
            }
            let all_bold =
                !text_lines.is_empty() && text_lines.iter().flatten().all(|word| word.bold);
            let header = row == 0 && (all_bold || row_count >= 3);
            cells.push(Cell {
                blocks: blocks_from_text_lines(&text_lines),
                colspan,
                rowspan,
                align,
                fill: fill_at(page, cell_rect.center_x(), cell_rect.center_y()),
                header,
            });
        }
        let fill = cells.iter().find_map(|cell| cell.fill);
        rows.push(Row {
            cells,
            header: row == 0 && row_count >= 3,
            fill,
        });
    }

    for (index, spec) in column_specs.iter_mut().enumerate() {
        spec.align = if right_votes[index] > left_votes[index]
            && right_votes[index] >= center_votes[index]
        {
            Align::Right
        } else if center_votes[index] > left_votes[index] {
            Align::Center
        } else {
            Align::Left
        };
    }

    let headers = if rows.first().is_some_and(|row| row.header) {
        vec![rows.remove(0)]
    } else {
        Vec::new()
    };
    let mut table = Table {
        anchor: Some(Anchor::new(page_index, bounds)),
        caption: None,
        columns: column_specs,
        headers,
        rows,
        pages: vec![page_index + 1],
        borders: TableBorders {
            outer: component.thickness.max(0.5),
            inner_horizontal: 0.5,
            inner_vertical: 0.5,
        },
        cell_padding: 4.0,
    };
    while table
        .rows
        .last()
        .is_some_and(|row| row.cells.iter().all(Cell::is_empty))
    {
        table.rows.pop();
    }
    if table.cell_count() == 0 {
        return None;
    }
    Some(table)
}

fn fill_at(page: &PageLayout, x: f32, y: f32) -> Option<Rgb> {
    page.fills
        .iter()
        .rev()
        .filter(|fill| fill.rect.contains(x, y))
        .filter_map(|fill| fill.fill)
        .find(|color| !color.is_white())
}

/// Text lines (each a vector of words) inside a rectangle, ordered top to bottom.
fn words_in_rect(lines: &[Line], rect: &Rect) -> Vec<Vec<Word>> {
    let mut buckets: Vec<(f32, Vec<Word>)> = Vec::new();
    for line in lines {
        if line.consumed {
            continue;
        }
        for word in &line.words {
            if !rect.contains(word.rect.center_x(), word.rect.center_y()) {
                continue;
            }
            let center = word.rect.center_y();
            let mut placed = false;
            for (bucket_center, bucket) in buckets.iter_mut() {
                if (center - *bucket_center).abs() <= word.font_size * 0.5 {
                    bucket.push(word.clone());
                    placed = true;
                    break;
                }
            }
            if !placed {
                buckets.push((center, vec![word.clone()]));
            }
        }
    }
    buckets.sort_by(|a, b| compare_f32(a.0, b.0));
    buckets
        .into_iter()
        .map(|(_, mut words)| {
            words.sort_by(|a, b| compare_f32(a.rect.x, b.rect.x));
            words
        })
        .collect()
}

fn vote_alignment(text_lines: &[Vec<Word>], cell: &Rect) -> Align {
    let Some(first) = text_lines.iter().find(|line| !line.is_empty()) else {
        return Align::Left;
    };
    let left = first.first().map(|w| w.rect.x).unwrap_or(cell.x);
    let right = first.last().map(|w| w.rect.right()).unwrap_or(cell.right());
    let pad = 6.0;
    let left_delta = (left - cell.x).abs();
    let right_delta = (cell.right() - right).abs();
    let center_delta = (((left + right) / 2.0) - cell.center_x()).abs();
    if right_delta <= pad && left_delta > pad + center_delta {
        Align::Right
    } else if center_delta <= pad && left_delta > pad && right_delta > pad {
        Align::Center
    } else {
        Align::Left
    }
}

fn blocks_from_text_lines(text_lines: &[Vec<Word>]) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for words in text_lines {
        if words.is_empty() {
            continue;
        }
        blocks.push(Block::Paragraph {
            inlines: inlines_from_words(words),
            anchor: None,
            style: ParagraphStyle::default(),
        });
    }
    blocks
}

// ---------------------------------------------------------------------------
// Post-processing
// ---------------------------------------------------------------------------

/// Merge a table that continues on the next page into the table it belongs to: identical
/// header rows, identical column count, and the two tables adjacent in reading order.
fn stitch_tables(blocks: Vec<Block>, warnings: &mut Vec<String>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let Block::Table(current) = &block {
            let continuation = !current.headers.is_empty()
                && matches!(out.last(), Some(Block::Table(previous))
                    if previous.headers.len() == current.headers.len()
                        && previous.column_count() == current.column_count()
                        && header_text(previous) == header_text(current));
            if continuation && let Some(Block::Table(previous)) = out.last_mut() {
                let added = current.rows.len();
                previous.rows.extend(current.rows.iter().cloned());
                previous.pages.extend(current.pages.iter().copied());
                warnings.push(format!(
                    "stitched a table across pages {:?} ({} continuation rows)",
                    previous.pages, added
                ));
                continue;
            }
        }
        out.push(block);
    }
    out
}

fn header_text(table: &Table) -> String {
    table
        .headers
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| cell.text().trim().to_lowercase())
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Two consecutive lists of the same kind separated by nothing become one list.
fn merge_adjacent_lists(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let Block::List { ordered, items, .. } = &block {
            let ordered = *ordered;
            let joinable = matches!(out.last(), Some(Block::List { ordered: previous, .. }) if *previous == ordered);
            if joinable
                && let Some(Block::List {
                    items: previous_items,
                    ..
                }) = out.last_mut()
            {
                previous_items.extend(items.iter().cloned());
                continue;
            }
        }
        out.push(block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, x: f32, y: f32, size: f32) -> TextRun {
        TextRun {
            text: text.to_string(),
            rect: Rect::new(x, y, text.chars().count() as f32 * size * 0.5, size),
            font_size: size,
            font_name: "Test".to_string(),
            bold: false,
            italic: false,
            mono: false,
            color: Rgb::BLACK,
        }
    }

    fn page_of(runs: Vec<TextRun>) -> LayoutDoc {
        LayoutDoc {
            pages: vec![PageLayout {
                width: 612.0,
                height: 792.0,
                runs,
                rules: Vec::new(),
                fills: Vec::new(),
                images: Vec::new(),
            }],
            assets: Vec::new(),
        }
    }

    #[test]
    fn single_column_becomes_paragraphs_and_headings() {
        let mut runs = vec![run("Quarterly Report", 72.0, 60.0, 24.0)];
        for index in 0..6 {
            runs.push(run(
                "Body copy for the report.",
                72.0,
                120.0 + index as f32 * 14.0,
                11.0,
            ));
        }
        let (doc, _warnings) = analyze(&page_of(runs), &AnalyzeOptions::default());
        assert!(matches!(doc.blocks[0], Block::Heading { level: 1, .. }));
        assert!(
            doc.blocks
                .iter()
                .any(|block| matches!(block, Block::Paragraph { .. }))
        );
    }

    #[test]
    fn two_columns_are_read_column_by_column() {
        let mut runs = Vec::new();
        for index in 0..4 {
            runs.push(run(
                "left column text",
                60.0,
                100.0 + index as f32 * 14.0,
                10.0,
            ));
            runs.push(run(
                "right column text",
                320.0,
                100.0 + index as f32 * 14.0,
                10.0,
            ));
        }
        let (doc, _warnings) = analyze(&page_of(runs), &AnalyzeOptions::default());
        let text = doc.plain_text();
        let left = text.find("left column text").expect("left present");
        let last_left = text.rfind("left column text").expect("left present");
        let right = text.find("right column text").expect("right present");
        assert!(left < right, "left column must come first");
        assert!(
            last_left < right,
            "the columns must not interleave: left block first, then right block"
        );
    }

    #[test]
    fn ruled_grid_becomes_a_table_with_header_row() {
        let mut rules = Vec::new();
        for x in [72.0f32, 236.0, 400.0] {
            rules.push(RuleSegment {
                rect: Rect::new(x, 100.0, 0.5, 60.0),
                axis: Axis::Vertical,
                thickness: 0.5,
                color: Rgb::BLACK,
            });
        }
        for y in [100.0f32, 120.0, 140.0, 160.0] {
            rules.push(RuleSegment {
                rect: Rect::new(72.0, y, 328.0, 0.5),
                axis: Axis::Horizontal,
                thickness: 0.5,
                color: Rgb::BLACK,
            });
        }
        let mut runs = Vec::new();
        for (row, y) in [104.0f32, 124.0, 144.0].iter().enumerate() {
            for (column, x) in [78.0f32, 242.0, 406.0].iter().enumerate() {
                runs.push(run(&format!("r{row}c{column}"), *x, *y, 9.0));
            }
        }
        let mut layout = page_of(runs);
        layout.pages[0].rules = rules;
        let (doc, _warnings) = analyze(&layout, &AnalyzeOptions::default());
        let table = doc
            .blocks
            .iter()
            .find_map(|block| match block {
                Block::Table(table) => Some(table),
                _ => None,
            })
            .expect("table detected");
        assert_eq!(table.column_count(), 3);
        assert_eq!(table.headers.len(), 1);
        assert_eq!(table.row_count(), 3);
        assert_eq!(table.headers[0].cells[0].text(), "r0c0");
        assert_eq!(table.rows[1].cells[2].text(), "r2c2");
    }

    #[test]
    fn repeated_header_and_footer_are_lifted_out() {
        let mut pages = Vec::new();
        for page_index in 0..3 {
            let mut runs = vec![
                run("ACME Annual Report", 72.0, 24.0, 9.0),
                run(&format!("Unique body {page_index}"), 72.0, 300.0, 11.0),
                run(&format!("Page {}", page_index + 1), 300.0, 760.0, 9.0),
            ];
            runs.truncate(3);
            pages.push(PageLayout {
                width: 612.0,
                height: 792.0,
                runs,
                rules: Vec::new(),
                fills: Vec::new(),
                images: Vec::new(),
            });
        }
        let layout = LayoutDoc {
            pages,
            assets: Vec::new(),
        };
        let (doc, _warnings) = analyze(&layout, &AnalyzeOptions::default());
        let text = doc.plain_text();
        assert!(!text.contains("ACME Annual Report"));
        assert!(!text.contains("Page 1"));
        assert!(text.contains("Unique body 0"));
        assert_eq!(doc.info.running_headers.len(), 1);
        assert_eq!(doc.info.running_footers.len(), 1);
    }
}
