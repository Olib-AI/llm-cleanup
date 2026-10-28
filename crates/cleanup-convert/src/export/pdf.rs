//! PDF exporter — re-typesets the IR with embedded fonts.
//!
//! PDF→PDF never comes through here: a PDF cleaned natively keeps its own page tree, fonts and
//! graphics, because the byte-splicer only rewrites text-showing operands. This renderer covers the
//! other direction (HTML/DOCX/Markdown/text → PDF), where the job is a real laid-out document:
//! headings sized from the IR level, tables drawn with fills and rules, embedded raster figures,
//! and selectable text with a `ToUnicode` CMap so copy/paste and search keep working.
//!
//! Fonts are embedded as `CIDFontType2` under `Identity-H`, so every glyph is addressed by its
//! TrueType glyph id. Inline style flags select the face; boxes are plain `re f` fills.

use super::{Rendered, RunStyle, asset_for};
use crate::font::TrueType;
use cleanup_core::doc::{
    Align, Block, CalloutKind, Cell, Doc, ImageAsset, ImageKind, Inline, PageSpec, Row, Table,
    decode_png,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

const REGULAR_FONT: &[u8] = include_bytes!("../../../cleanup-gui/fonts/Inter-Regular.ttf");
const BOLD_FONT: &[u8] = include_bytes!("../../../cleanup-gui/fonts/Inter-Bold.ttf");

const BODY_SIZE: f32 = 10.5;
const BODY_LEADING: f32 = 15.0;
const CODE_SIZE: f32 = 9.0;
const CELL_LEADING: f32 = 13.0;
const RULE: (f32, f32, f32) = (0.72, 0.72, 0.72);

/// Render `doc` as a PDF.
pub fn render(doc: &Doc) -> Rendered {
    let regular = match TrueType::new(REGULAR_FONT) {
        Ok(font) => font,
        Err(error) => return failed(format!("embedded font failed to parse: {error}")),
    };
    let bold = match TrueType::new(BOLD_FONT) {
        Ok(font) => font,
        Err(error) => return failed(format!("embedded bold font failed to parse: {error}")),
    };
    let mut layout = Layout::new(doc, regular, bold);
    layout.run();
    let mut warnings = layout.warnings.clone();
    if !layout.missing.is_empty() {
        let sample: String = layout.missing.iter().take(12).collect();
        warnings.push(format!(
            "the embedded font has no glyph for {} character(s) ({sample}); they were skipped in the PDF only",
            layout.missing.len()
        ));
    }
    let bytes = layout.finish();
    Rendered {
        bytes,
        assets: Vec::new(),
        warnings,
        engine: "ir-pdf",
    }
}

fn failed(warning: String) -> Rendered {
    Rendered {
        bytes: Vec::new(),
        assets: Vec::new(),
        warnings: vec![warning],
        engine: "ir-pdf",
    }
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

struct EmbeddedFont<'a> {
    ttf: TrueType<'a>,
    /// Glyph id → Unicode scalar, feeding the `ToUnicode` CMap.
    unicode_by_gid: BTreeMap<u16, u32>,
    base_name: &'static str,
}

impl<'a> EmbeddedFont<'a> {
    fn new(ttf: TrueType<'a>, base_name: &'static str) -> Self {
        EmbeddedFont {
            ttf,
            unicode_by_gid: BTreeMap::new(),
            base_name,
        }
    }

    fn glyph(&self, ch: char) -> u16 {
        self.ttf.glyph_index(ch as u32).unwrap_or(0)
    }

    fn advance(&self, gid: u16, size: f32) -> f32 {
        self.ttf
            .width(gid)
            .map(|width| f32::from(width) / 1000.0 * size)
            .unwrap_or(size * 0.5)
    }

    fn measure(&self, text: &str, size: f32) -> f32 {
        text.chars()
            .map(|ch| self.advance(self.glyph(ch), size))
            .sum()
    }

    fn encode(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() * 4);
        for ch in text.chars() {
            let gid = self.glyph(ch);
            if gid == 0 && !ch.is_whitespace() {
                continue;
            }
            self.unicode_by_gid.entry(gid).or_insert(ch as u32);
            out.push_str(&format!("{gid:04X}"));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// A wrapped line: `(text, style, advance width)`.
type Piece = (String, RunStyle, f32);
type Line = Vec<Piece>;

/// A clickable link region in PDF user space (origin bottom-left).
struct LinkAnnotation {
    rect: (f32, f32, f32, f32),
    url: String,
}

struct PageContent {
    content: String,
    images: BTreeSet<usize>,
    links: Vec<LinkAnnotation>,
}

impl PageContent {
    fn new() -> Self {
        PageContent {
            content: String::new(),
            images: BTreeSet::new(),
            links: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }
}

struct Layout<'a> {
    doc: &'a Doc,
    regular: EmbeddedFont<'a>,
    bold: EmbeddedFont<'a>,
    pages: Vec<PageContent>,
    current: PageContent,
    y: f32,
    /// Extra left shift applied while rendering a column band.
    offset: f32,
    /// Absolute right edge of the current band, `0.0` for the full text column.
    limit: f32,
    warnings: Vec<String>,
    missing: BTreeSet<char>,
}

impl<'a> Layout<'a> {
    fn new(doc: &'a Doc, regular: TrueType<'a>, bold: TrueType<'a>) -> Self {
        let y = doc.page.margin_top;
        Layout {
            doc,
            regular: EmbeddedFont::new(regular, "Inter-Regular"),
            bold: EmbeddedFont::new(bold, "Inter-Bold"),
            pages: Vec::new(),
            current: PageContent::new(),
            y,
            offset: 0.0,
            limit: 0.0,
            warnings: Vec::new(),
            missing: BTreeSet::new(),
        }
    }

    fn page(&self) -> &PageSpec {
        &self.doc.page
    }

    fn left(&self) -> f32 {
        self.page().margin_left + self.offset
    }

    fn right(&self) -> f32 {
        if self.limit > 0.0 {
            self.limit
        } else {
            self.page().width - self.page().margin_right
        }
    }

    fn width(&self) -> f32 {
        (self.right() - self.left()).max(48.0)
    }

    fn bottom(&self) -> f32 {
        self.page().height - self.page().margin_bottom
    }

    fn font(&self, bold: bool) -> &EmbeddedFont<'a> {
        if bold { &self.bold } else { &self.regular }
    }

    fn measure(&self, text: &str, size: f32, bold: bool) -> f32 {
        self.font(bold).measure(text, size)
    }

    fn new_page(&mut self) {
        let finished = std::mem::replace(&mut self.current, PageContent::new());
        if !finished.is_empty() {
            self.pages.push(finished);
        }
        self.y = self.page().margin_top;
    }

    fn ensure(&mut self, needed: f32) {
        if self.y + needed > self.bottom() {
            self.new_page();
        }
    }

    fn run(&mut self) {
        for block in &self.doc.blocks {
            self.block(block, 0.0);
        }
        let finished = std::mem::replace(&mut self.current, PageContent::new());
        if !finished.is_empty() || self.pages.is_empty() {
            self.pages.push(finished);
        }
    }

    fn block(&mut self, block: &Block, indent: f32) {
        match block {
            Block::Heading { level, inlines, .. } => {
                let size = match level {
                    0 | 1 => 20.0,
                    2 => 16.0,
                    3 => 13.5,
                    4 => 12.0,
                    5 => 11.0,
                    _ => 10.5,
                };
                self.ensure(size * 2.4);
                self.y += size * 0.55;
                self.flow(inlines, size, size * 1.25, indent);
                self.y += size * 0.35;
            }
            Block::Paragraph { inlines, style, .. } => {
                let leading = style.line_spacing.unwrap_or(BODY_LEADING).max(BODY_SIZE);
                self.flow(
                    inlines,
                    BODY_SIZE,
                    leading,
                    indent + style.indent + style.first_line_indent.max(0.0),
                );
                self.y += 4.0;
            }
            Block::List { ordered, items, .. } => {
                for (index, item) in items.iter().enumerate() {
                    let marker = if *ordered {
                        format!("{}.", index + 1)
                    } else {
                        "\u{2022}".to_string()
                    };
                    let item_indent = indent + 14.0 + item.depth as f32 * 14.0;
                    let mut inlines = vec![Inline::plain(format!("{marker} "))];
                    inlines.extend(item.inlines.iter().cloned());
                    self.flow(&inlines, BODY_SIZE, BODY_LEADING, item_indent);
                }
                self.y += 3.0;
            }
            Block::Code { lines, .. } => {
                self.ensure(CODE_SIZE * 1.4 * lines.len().clamp(1, 3) as f32 + 10.0);
                let top = self.y - 3.0;
                for line in lines {
                    self.ensure(CODE_SIZE * 1.4);
                    self.show(
                        line,
                        CODE_SIZE,
                        self.left() + indent + 8.0,
                        self.y + CODE_SIZE,
                        false,
                    );
                    self.y += CODE_SIZE * 1.4;
                }
                let height = (self.y - top + 3.0).max(CODE_SIZE * 1.4);
                self.fill_box(indent, top, height, (0.96, 0.97, 0.98));
                self.y += 5.0;
            }
            Block::Table(table) => {
                self.table(table, indent);
                self.y += 10.0;
            }
            Block::Figure(figure) => {
                let mut warnings = Vec::new();
                let asset = asset_for(self.doc, figure.asset, &mut warnings).cloned();
                self.warnings.extend(warnings);
                let Some(asset) = asset else {
                    return;
                };
                let available = self.width() - indent;
                let (mut width, mut height) = figure
                    .display_size
                    .map(|rect| (rect.width, rect.height))
                    .unwrap_or_else(|| {
                        let px_w = asset.width_px.max(1) as f32;
                        let px_h = asset.height_px.max(1) as f32;
                        let scale = (available * 0.75 / px_w).min(478.0 / px_h).max(0.1);
                        (px_w * scale, px_h * scale)
                    });
                if width > available && width > 0.0 {
                    height *= available / width;
                    width = available;
                }
                if width < 2.0 || height < 2.0 {
                    return;
                }
                self.ensure(height + 8.0);
                if self.draw_image(
                    &asset,
                    figure.asset,
                    self.left() + indent,
                    self.y,
                    width,
                    height,
                ) {
                    self.y += height + 5.0;
                    if let Some(caption) = &figure.caption {
                        self.flow(caption, 8.5, 11.5, indent);
                    }
                } else {
                    self.warnings.push(format!(
                        "{} could not be embedded in the PDF",
                        asset.file_name()
                    ));
                }
            }
            Block::Vector(shape) => {
                let color = shape
                    .stroke
                    .map(|rgb| {
                        (
                            f32::from(rgb.0) / 255.0,
                            f32::from(rgb.1) / 255.0,
                            f32::from(rgb.2) / 255.0,
                        )
                    })
                    .unwrap_or((0.6, 0.6, 0.6));
                self.ensure(12.0);
                let x0 = self.left() + indent;
                let length = (self.width() - indent).min(shape.rect.width.max(24.0));
                if shape.axis == cleanup_core::doc::Axis::Horizontal {
                    self.stroke_line(x0, self.y, x0 + length, self.y, color, shape.stroke_width);
                    self.y += 8.0;
                } else {
                    let height = shape.rect.height.clamp(12.0, 72.0);
                    self.stroke_line(x0, self.y, x0, self.y + height, color, shape.stroke_width);
                    self.y += height + 4.0;
                }
            }
            Block::Divider(_) => {
                self.ensure(10.0);
                self.fill_rect(
                    self.left() + indent,
                    self.y,
                    self.width() - indent,
                    0.75,
                    (0.6, 0.6, 0.6),
                );
                self.y += 8.0;
            }
            Block::Callout {
                kind, blocks, fill, ..
            } => {
                let tone = fill
                    .map(|rgb| {
                        (
                            f32::from(rgb.0) / 255.0,
                            f32::from(rgb.1) / 255.0,
                            f32::from(rgb.2) / 255.0,
                        )
                    })
                    .unwrap_or(match kind {
                        CalloutKind::Quote => (0.96, 0.97, 0.98),
                        _ => (1.0, 0.98, 0.90),
                    });
                let top = self.y;
                let saved = self.offset;
                self.offset += 12.0;
                self.y += 6.0;
                for block in blocks {
                    self.block(block, indent);
                }
                self.offset = saved;
                let height = (self.y - top).max(12.0);
                // Fill first, then the accent bar, so the bar stays visible.
                self.fill_box(indent + 3.0, top, height, tone);
                self.fill_rect(self.left() + indent, top, 2.5, height, (0.65, 0.71, 0.80));
                self.y = top + height + 6.0;
            }
            Block::Columns { columns, .. } => {
                let gap = 18.0;
                let count = columns.len().max(1);
                let usable = self.width() - gap * (count.saturating_sub(1) as f32);
                let total: f32 = columns.iter().map(|column| column.width.max(0.05)).sum();
                let total = if total <= 0.0 { 1.0 } else { total };
                let start_y = self.y;
                let saved_offset = self.offset;
                let saved_limit = self.limit;
                let mut cursor = self.left();
                let mut lowest = start_y;
                for column in columns {
                    let width = usable * column.width.max(0.05) / total;
                    self.offset = cursor - self.page().margin_left;
                    self.limit = cursor + width;
                    self.y = start_y;
                    for block in &column.blocks {
                        self.block(block, indent);
                    }
                    lowest = lowest.max(self.y);
                    cursor += width + gap;
                }
                self.offset = saved_offset;
                self.limit = saved_limit;
                self.y = lowest + 8.0;
            }
        }
    }

    /// Lay out flowing text, breaking lines at the current right edge.
    fn flow(&mut self, inlines: &[Inline], size: f32, leading: f32, indent: f32) {
        let lines = self.wrap(inlines, size, self.width() - indent);
        for line in lines {
            self.ensure(leading);
            let mut x = self.left() + indent;
            let baseline = self.y + size * 0.85;
            for (text, style, advance) in line {
                self.draw(&text, &style, size, x, baseline);
                x += advance;
            }
            self.y += leading;
        }
    }

    fn wrap(&self, inlines: &[Inline], size: f32, width: f32) -> Vec<Line> {
        let width = width.max(36.0);
        let mut lines: Vec<Line> = Vec::new();
        let mut current: Line = Vec::new();
        let mut used = 0.0f32;
        for inline in inlines {
            let style = RunStyle::from_inline(&inline.style);
            if inline.is_break() {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    used = 0.0;
                }
                continue;
            }
            let font = self.font(style.bold);
            for (index, word) in inline.text.split(' ').enumerate() {
                let mut piece = String::new();
                if index > 0 {
                    piece.push(' ');
                }
                piece.push_str(word);
                if piece.trim().is_empty() && current.is_empty() {
                    continue;
                }
                let advance = font.measure(&piece, size);
                if used + advance > width && !current.is_empty() && !piece.trim().is_empty() {
                    lines.push(std::mem::take(&mut current));
                    piece = piece.trim_start().to_string();
                    current.push((piece.clone(), style.clone(), font.measure(&piece, size)));
                    used = font.measure(&piece, size);
                } else {
                    current.push((piece, style.clone(), advance));
                    used += advance;
                }
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
        for line in &mut lines {
            while line
                .last()
                .is_some_and(|(text, _, _)| text.trim().is_empty())
            {
                line.pop();
            }
        }
        lines
    }

    fn draw(&mut self, text: &str, style: &RunStyle, size: f32, x: f32, baseline: f32) {
        let advance = self.font(style.bold).measure(text, size);
        let encoded = if style.bold {
            self.bold.encode(text)
        } else {
            self.regular.encode(text)
        };
        if encoded.is_empty() {
            return;
        }
        let resource = if style.bold { "F2" } else { "F1" };
        let y = self.page().height - baseline;
        if let Some(color) = style.color {
            self.current.content.push_str(&format!(
                "q {:.3} {:.3} {:.3} rg\n",
                f32::from(color.0) / 255.0,
                f32::from(color.1) / 255.0,
                f32::from(color.2) / 255.0
            ));
        }
        self.current.content.push_str(&format!(
            "BT /{resource} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm <{encoded}> Tj ET\n"
        ));
        if style.color.is_some() {
            self.current.content.push_str("Q\n");
        }
        if let Some(url) = &style.link {
            self.current.links.push(LinkAnnotation {
                rect: (x, y - size * 0.25, x + advance, y + size * 0.85),
                url: url.clone(),
            });
        }
        for ch in text.chars() {
            if !ch.is_whitespace() && self.font(style.bold).glyph(ch) == 0 {
                self.missing.insert(ch);
            }
        }
    }

    /// A stroked vector line in PDF user space, used for decorative rules and dividers.
    fn stroke_line(
        &mut self,
        x0: f32,
        top0: f32,
        x1: f32,
        top1: f32,
        color: (f32, f32, f32),
        width: f32,
    ) {
        let height = self.page().height;
        self.current.content.push_str(&format!(
            "q {:.3} {:.3} {:.3} RG {:.2} w {:.2} {:.2} m {:.2} {:.2} l S Q\n",
            color.0,
            color.1,
            color.2,
            width.clamp(0.25, 6.0),
            x0,
            height - top0,
            x1,
            height - top1
        ));
    }

    fn show(&mut self, text: &str, size: f32, x: f32, baseline: f32, bold: bool) {
        let encoded = if bold {
            self.bold.encode(text)
        } else {
            self.regular.encode(text)
        };
        if encoded.is_empty() {
            return;
        }
        let resource = if bold { "F2" } else { "F1" };
        let y = self.page().height - baseline;
        self.current.content.push_str(&format!(
            "BT /{resource} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm <{encoded}> Tj ET\n"
        ));
    }

    fn fill_rect(&mut self, x: f32, top: f32, width: f32, height: f32, color: (f32, f32, f32)) {
        if width <= 0.02 || height <= 0.02 {
            return;
        }
        let y = self.page().height - top - height;
        self.current.content.push_str(&format!(
            "{:.3} {:.3} {:.3} rg {x:.2} {y:.2} {width:.2} {height:.2} re f\n",
            color.0, color.1, color.2
        ));
    }

    /// Fill the full text column, shifted by `indent`.
    fn fill_box(&mut self, indent: f32, top: f32, height: f32, color: (f32, f32, f32)) {
        let width = self.width() - indent;
        self.fill_rect(self.left() + indent, top, width, height, color);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_image(
        &mut self,
        asset: &ImageAsset,
        index: usize,
        x: f32,
        top: f32,
        width: f32,
        height: f32,
    ) -> bool {
        if !matches!(asset.kind, ImageKind::Jpeg | ImageKind::Png) {
            return false;
        }
        let y = self.page().height - top - height;
        self.current.images.insert(index);
        self.current.content.push_str(&format!(
            "q {width:.2} 0 0 {height:.2} {x:.2} {y:.2} cm /{} Do Q\n",
            image_name(index)
        ));
        true
    }

    fn table(&mut self, table: &Table, indent: f32) {
        let columns = table.column_count().max(1);
        let available = self.width() - indent;
        let weights: Vec<f32> = (0..columns)
            .map(|index| {
                table
                    .columns
                    .get(index)
                    .map(|column| column.width.max(0.5))
                    .unwrap_or(1.0)
            })
            .collect();
        let total: f32 = weights.iter().sum::<f32>().max(0.1);
        let widths: Vec<f32> = weights
            .iter()
            .map(|weight| available * weight / total)
            .collect();
        let padding = table.cell_padding.clamp(1.0, 10.0);
        let border = table.borders.outer.max(0.4);
        let rows: Vec<&Row> = table.all_rows().collect();
        let table_top = self.y;

        for row in &rows {
            let mut cells: Vec<(usize, usize, &Cell, Vec<String>, f32)> = Vec::new();
            let mut column = 0usize;
            let mut row_height = BODY_SIZE + padding * 2.0;
            for cell in &row.cells {
                if column >= columns {
                    break;
                }
                let span = cell.colspan.max(1).min(columns - column);
                let edge = (column + span).min(columns);
                let width: f32 = widths[column..edge].iter().sum();
                let lines = self.wrap_cell(cell, width - padding * 2.0, cell.header || row.header);
                let needed = lines.len().max(1) as f32 * CELL_LEADING + padding * 2.0;
                row_height = row_height.max(needed);
                cells.push((column, span, cell, lines, width));
                column += span;
            }
            self.ensure(row_height);
            let top = self.y;
            let tone = row
                .fill
                .or_else(|| cells.iter().find_map(|(_, _, cell, _, _)| cell.fill));
            if let Some(fill) = tone {
                let color = (
                    f32::from(fill.0) / 255.0,
                    f32::from(fill.1) / 255.0,
                    f32::from(fill.2) / 255.0,
                );
                self.fill_rect(self.left() + indent, top, available, row_height, color);
            }
            for (start, _, cell, lines, width) in &cells {
                let x0: f32 = self.left() + indent + widths[..*start].iter().sum::<f32>();
                let bold = cell.header || row.header;
                let mut baseline = top + padding + BODY_SIZE;
                let align = if cell.align == Align::Left {
                    table
                        .columns
                        .get(*start)
                        .map(|column| column.align)
                        .unwrap_or(Align::Left)
                } else {
                    cell.align
                };
                for line in lines {
                    let text_width = self.measure(line, BODY_SIZE, bold);
                    let shift = match align {
                        Align::Left => 0.0,
                        Align::Center => ((width - text_width) / 2.0).max(0.0),
                        Align::Right => (width - text_width - padding).max(0.0),
                    };
                    self.show(line, BODY_SIZE, x0 + padding + shift, baseline, bold);
                    baseline += CELL_LEADING;
                }
                // Left cell edge.
                self.fill_rect(x0, top, border, row_height, RULE);
            }
            self.y += row_height;
            self.fill_rect(
                self.left() + indent,
                self.y - border,
                available,
                border,
                RULE,
            );
        }

        let height = self.y - table_top;
        if height <= 0.0 {
            return;
        }
        // Column separators plus the outer frame keep the grid legible on every page.
        let mut x = self.left() + indent;
        for (index, width) in widths.iter().enumerate() {
            if index == 0 {
                self.fill_rect(x, table_top, border, height, RULE);
            }
            x += width;
            self.fill_rect(x - border, table_top, border, height, RULE);
        }
        self.fill_rect(
            self.left() + indent,
            table_top - border.max(0.4),
            available,
            border.max(0.4),
            RULE,
        );
    }

    fn wrap_cell(&self, cell: &Cell, width: f32, bold: bool) -> Vec<String> {
        let text = cell.text();
        let mut out: Vec<String> = Vec::new();
        for paragraph in text.split('\n') {
            if paragraph.trim().is_empty() {
                continue;
            }
            let lines = self.wrap_plain(paragraph, BODY_SIZE, width.max(12.0), bold);
            out.extend(lines);
        }
        out
    }

    fn wrap_plain(&self, text: &str, size: f32, width: f32, bold: bool) -> Vec<String> {
        let font = self.font(bold);
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut used = 0.0f32;
        for word in text.split_whitespace() {
            let advance = font.measure(word, size);
            let space = font.measure(" ", size);
            if !current.is_empty() && used + space + advance > width {
                lines.push(std::mem::take(&mut current));
                used = 0.0;
            }
            if !current.is_empty() {
                current.push(' ');
                used += space;
            }
            current.push_str(word);
            used += advance;
        }
        if !current.is_empty() {
            lines.push(current);
        }
        lines
    }

    // -- object assembly ----------------------------------------------------

    fn finish(&mut self) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = vec![Vec::new(), Vec::new(), Vec::new()];
        let regular_font = font_objects(&mut objects, &self.regular);
        let bold_font = font_objects(&mut objects, &self.bold);

        let mut image_ids: BTreeMap<usize, usize> = BTreeMap::new();
        let used: BTreeSet<usize> = self
            .pages
            .iter()
            .flat_map(|page| page.images.iter().copied())
            .collect();
        for asset_index in used {
            let Some(asset) = self.doc.assets.get(asset_index) else {
                continue;
            };
            let Some((dictionary, data)) = image_payload(asset) else {
                self.warnings.push(format!(
                    "{} could not be embedded in the PDF",
                    asset.file_name()
                ));
                continue;
            };
            let mut object =
                format!("<< {dictionary} /Length {} >>\nstream\n", data.len()).into_bytes();
            object.extend_from_slice(&data);
            object.extend_from_slice(b"\nendstream");
            objects.push(object);
            image_ids.insert(asset_index, objects.len() - 1);
        }

        let mut page_ids: Vec<usize> = Vec::new();
        for page in &self.pages {
            let content_id = objects.len();
            let mut object = format!("<< /Length {} >>\nstream\n", page.content.len()).into_bytes();
            object.extend_from_slice(page.content.as_bytes());
            object.extend_from_slice(b"\nendstream");
            objects.push(object);

            let mut resources = format!(
                "<< /Font << /F1 {} 0 R /F2 {} 0 R >>",
                regular_font, bold_font
            );
            let xobjects: Vec<String> = page
                .images
                .iter()
                .filter_map(|asset| {
                    image_ids
                        .get(asset)
                        .map(|id| format!("/{} {id} 0 R", image_name(*asset)))
                })
                .collect();
            if !xobjects.is_empty() {
                resources.push_str(&format!(" /XObject << {} >>", xobjects.join(" ")));
            }
            resources.push_str(" >>");
            let mut annot_ids: Vec<usize> = Vec::new();
            for link in &page.links {
                let (x0, y0, x1, y1) = link.rect;
                let annot = format!(
                    "<< /Type /Annot /Subtype /Link /Rect [{x0:.2} {y0:.2} {x1:.2} {y1:.2}] /Border [0 0 0] /A << /Type /Action /S /URI /URI ({}) >> >>",
                    pdf_string(&link.url)
                );
                objects.push(annot.into_bytes());
                annot_ids.push(objects.len() - 1);
            }
            let annots = if annot_ids.is_empty() {
                String::new()
            } else {
                format!(
                    " /Annots [{}]",
                    annot_ids
                        .iter()
                        .map(|id| format!("{id} 0 R"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            };
            let page_id = objects.len();
            objects.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources {resources} /Contents {content_id} 0 R{annots} >>",
                    self.page().width,
                    self.page().height
                )
                .into_bytes(),
            );
            page_ids.push(page_id);
        }
        if page_ids.is_empty() {
            let content_id = objects.len();
            objects.push(b"<< /Length 0 >>\nstream\n\nendstream".to_vec());
            let page_id = objects.len();
            objects.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << /Font << /F1 {} 0 R /F2 {} 0 R >> >> /Contents {content_id} 0 R >>",
                    self.page().width,
                    self.page().height,
                    regular_font,
                    bold_font
                )
                .into_bytes(),
            );
            page_ids.push(page_id);
        }

        let kids: Vec<String> = page_ids.iter().map(|id| format!("{id} 0 R")).collect();
        objects[1] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
        objects[2] = format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            page_ids.len()
        )
        .into_bytes();
        serialize_pdf(objects)
    }
}

fn image_name(asset: usize) -> String {
    format!("Im{}", asset + 1)
}

/// Escape a Rust string as a PDF literal string without injecting operators.
fn pdf_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match *byte {
            b'\\' => out.push_str("\\\\"),
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            0x20..=0x7e => out.push(*byte as char),
            other => out.push_str(&format!("\\{:03o}", other)),
        }
    }
    out
}

/// Emit `Type0` / `CIDFontType2` / descriptor / font file / `ToUnicode`; return the `Type0` id.
fn font_objects(objects: &mut Vec<Vec<u8>>, font: &EmbeddedFont<'_>) -> usize {
    let type0 = objects.len();
    objects.push(Vec::new());
    let cid = objects.len();
    objects.push(Vec::new());
    let descriptor = objects.len();
    objects.push(Vec::new());
    let font_file = objects.len();
    objects.push(Vec::new());
    let to_unicode = objects.len();
    objects.push(Vec::new());

    let widths: Vec<String> = (0..font.ttf.glyph_count)
        .map(|glyph| {
            font.ttf
                .width(glyph)
                .map(|width| width.to_string())
                .unwrap_or_else(|_| "0".to_string())
        })
        .collect();
    let missing_tounicode = font.unicode_by_gid.is_empty();
    let mut binary = font.unicode_by_gid.clone();
    if missing_tounicode {
        binary.entry(0).or_insert(u32::from(' '));
    }
    objects[type0] = format!(
        "<< /Type /Font /Subtype /Type0 /BaseFont /{} /Encoding /Identity-H /DescendantFonts [{cid} 0 R] /ToUnicode {to_unicode} 0 R >>",
        font.base_name
    )
    .into_bytes();
    objects[cid] = format!(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {descriptor} 0 R /DW 600 /W [0 [{}]] /CIDToGIDMap /Identity >>",
        font.base_name,
        widths.join(" ")
    )
    .into_bytes();
    objects[descriptor] = format!(
        "<< /Type /FontDescriptor /FontName /{} /Flags 32 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} /CapHeight {} /StemV 80 /FontFile2 {font_file} 0 R >>",
        font.base_name,
        font.ttf.scaled(font.ttf.bbox[0]),
        font.ttf.scaled(font.ttf.bbox[1]),
        font.ttf.scaled(font.ttf.bbox[2]),
        font.ttf.scaled(font.ttf.bbox[3]),
        font.ttf.scaled(font.ttf.ascent),
        font.ttf.scaled(font.ttf.descent),
        font.ttf.scaled(font.ttf.cap_height)
    )
    .into_bytes();
    objects[font_file] = stream_object(
        &format!(
            "/Length {} /Length1 {}",
            font.ttf.data.len(),
            font.ttf.data.len()
        ),
        font.ttf.data,
    );
    let cmap = to_unicode_cmap(&binary);
    objects[to_unicode] = stream_object(&format!("/Length {}", cmap.len()), &cmap);
    type0
}

fn image_payload(asset: &ImageAsset) -> Option<(String, Vec<u8>)> {
    let width = asset.width_px;
    let height = asset.height_px;
    if width == 0 || height == 0 {
        return None;
    }
    match asset.kind {
        ImageKind::Jpeg => Some((
            format!(
                "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode"
            ),
            asset.data.clone(),
        )),
        ImageKind::Png => {
            let raster = decode_png(&asset.data)?;
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&raster.rgb).ok()?;
            let compressed = encoder.finish().ok()?;
            Some((
                format!(
                    "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
                    raster.width, raster.height
                ),
                compressed,
            ))
        }
        _ => None,
    }
}

fn stream_object(dictionary: &str, data: &[u8]) -> Vec<u8> {
    let mut object = format!("<< {dictionary} >>\nstream\n").into_bytes();
    object.extend_from_slice(data);
    object.extend_from_slice(b"\nendstream");
    object
}

fn to_unicode_cmap(mappings: &BTreeMap<u16, u32>) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /InterToUnicode def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u16, &u32)> = mappings.iter().collect();
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (glyph, codepoint) in chunk {
            let ch = char::from_u32(**codepoint).unwrap_or('\u{FFFD}');
            let mut buffer = [0u16; 2];
            cmap.push_str(&format!("<{glyph:04X}> <"));
            for unit in ch.encode_utf16(&mut buffer) {
                cmap.push_str(&format!("{unit:04X}"));
            }
            cmap.push_str(">\n");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

fn serialize_pdf(objects: Vec<Vec<u8>>) -> Vec<u8> {
    let mut output = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = vec![0usize; objects.len()];
    for (id, object) in objects.iter().enumerate().skip(1) {
        offsets[id] = output.len();
        output.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        output.extend_from_slice(object);
        output.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = output.len();
    output.extend_from_slice(format!("xref\n0 {}\n", objects.len()).as_bytes());
    output.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    output.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len()
        )
        .as_bytes(),
    );
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::doc::{ColumnSpec, ParagraphStyle};

    #[test]
    fn output_is_a_pdf_with_an_embedded_font_and_tounicode() {
        let mut doc = Doc::new(PageSpec::default());
        doc.blocks.push(Block::Heading {
            level: 1,
            inlines: vec![Inline::plain("Findings")],
            anchor: None,
        });
        doc.blocks.push(Block::Paragraph {
            inlines: vec![Inline::plain("Cafe naive - \u{2211} \u{00B1}")],
            anchor: None,
            style: ParagraphStyle::default(),
        });
        let rendered = render(&doc);
        let text = String::from_utf8_lossy(&rendered.bytes);
        assert!(text.starts_with("%PDF-1.7"));
        assert!(text.contains("/FontFile2"));
        assert!(text.contains("/ToUnicode"));
        assert!(text.contains("beginbfchar"));
        assert!(text.contains("/Count 1"));
        assert_eq!(rendered.engine, "ir-pdf");
    }

    #[test]
    fn tables_and_headings_are_drawn() {
        let mut doc = Doc::new(PageSpec::default());
        doc.blocks.push(Block::Heading {
            level: 2,
            inlines: vec![Inline::plain("Table")],
            anchor: None,
        });
        let table = Table {
            columns: vec![
                ColumnSpec {
                    width: 1.0,
                    align: Align::Left,
                },
                ColumnSpec {
                    width: 1.0,
                    align: Align::Right,
                },
            ],
            headers: vec![Row {
                cells: vec![Cell::paragraph("Region"), Cell::paragraph("Revenue")],
                header: true,
                fill: None,
            }],
            rows: vec![Row {
                cells: vec![Cell::paragraph("EMEA"), Cell::paragraph("1204")],
                header: false,
                fill: None,
            }],
            ..Table::default()
        };
        doc.blocks.push(Block::Table(table));
        let rendered = render(&doc);
        let text = String::from_utf8_lossy(&rendered.bytes);
        assert!(text.contains(" re f"));
        assert!(text.contains("/F2"));
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }
}
