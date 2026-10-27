//! Plain-text exporter.
//!
//! Reading order is linearised the same way the IR was built: a multi-column page is emitted
//! column by column (never interleaved line by line), so a terminal reader follows the document
//! the way a human would. Tables are drawn with box-drawing characters and per-column alignment;
//! figures become `[figure: …]` markers so nothing silently disappears.

use super::{Rendered, asset_for};
use cleanup_core::doc::{Align, Block, Cell, Doc, Inline, Table};

const WRAP: usize = 84;
const MAX_COLUMN: usize = 36;

/// Render `doc` as plain text.
pub fn render(doc: &Doc) -> Rendered {
    let mut out = String::new();
    let mut warnings: Vec<String> = Vec::new();
    for block in &doc.blocks {
        render_block(block, doc, &mut out, &mut warnings);
    }
    let mut text = out.trim_end().to_string();
    text.push('\n');
    Rendered {
        bytes: text.into_bytes(),
        assets: Vec::new(),
        warnings,
        engine: "ir-text",
    }
}

fn render_block(block: &Block, doc: &Doc, out: &mut String, warnings: &mut Vec<String>) {
    match block {
        Block::Heading { inlines, level, .. } => {
            let text = plain_inlines(inlines);
            if text.trim().is_empty() {
                return;
            }
            let text = text.trim();
            if *level == 1 || text.chars().count() > 40 {
                out.push_str(text);
                out.push('\n');
                out.push_str(&"-".repeat(text.chars().count().min(72)));
                out.push_str("\n\n");
            } else {
                out.push_str(text);
                out.push_str("\n\n");
            }
        }
        Block::Paragraph { inlines, .. } => {
            let text = plain_inlines(inlines);
            if text.trim().is_empty() {
                return;
            }
            for line in wrap(&text, WRAP, "") {
                out.push_str(line.trim_end());
                out.push('\n');
            }
            out.push('\n');
        }
        Block::List { ordered, items, .. } => {
            for (index, item) in items.iter().enumerate() {
                let text = plain_inlines(&item.inlines);
                if text.trim().is_empty() {
                    continue;
                }
                let indent = "  ".repeat(item.depth);
                let marker = if *ordered {
                    format!("{}.", index + 1)
                } else {
                    "-".to_string()
                };
                let prefix = format!("{indent}{marker} ");
                let body = wrap(
                    &text,
                    WRAP.saturating_sub(prefix.len()),
                    &" ".repeat(prefix.len()),
                );
                for (line_index, line) in body.iter().enumerate() {
                    if line_index == 0 {
                        out.push_str(&prefix);
                        out.push_str(line.trim_start());
                    } else {
                        out.push_str(line);
                    }
                    out.push('\n');
                }
            }
            out.push('\n');
        }
        Block::Code { lines, .. } => {
            for line in lines {
                out.push_str("    ");
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }
        Block::Table(table) => {
            for line in render_table(table) {
                out.push_str(&line);
                out.push('\n');
            }
            out.push('\n');
        }
        Block::Figure(figure) => {
            if let Some(asset) = asset_for(doc, figure.asset, warnings) {
                out.push_str(&format!(
                    "[figure: {} ({}x{} px, {})]\n",
                    asset.name,
                    asset.width_px,
                    asset.height_px,
                    asset.kind.mime()
                ));
            }
            if let Some(caption) = &figure.caption {
                let text = plain_inlines(caption);
                if !text.trim().is_empty() {
                    out.push_str(&format!("  {}\n", text.trim()));
                }
            }
            out.push('\n');
        }
        Block::Divider(_) => {
            out.push_str(&"\u{2500}".repeat(60));
            out.push_str("\n\n");
        }
        Block::Callout { blocks, .. } => {
            let mut inner = String::new();
            for block in blocks {
                render_block(block, doc, &mut inner, warnings);
            }
            for line in inner.trim_end().lines() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            out.push('\n');
        }
        Block::Columns { columns, .. } => {
            for (index, column) in columns.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                for block in &column.blocks {
                    render_block(block, doc, out, warnings);
                }
            }
        }
    }
}

fn plain_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        out.push_str(&inline.text);
    }
    out
}

/// Greedy word wrap with an optional indent applied to continuation lines.
fn wrap(text: &str, width: usize, indent: &str) -> Vec<String> {
    let width = width.max(20);
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let extra = if current.is_empty() { 0 } else { 1 };
            if !current.is_empty() && current.chars().count() + extra + word.chars().count() > width
            {
                lines.push(std::mem::take(&mut current));
                current.push_str(indent);
                current.push_str(word);
            } else {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
        }
        lines.push(current);
    }
    lines
}

/// Draw a table with box-drawing characters.
fn render_table(table: &Table) -> Vec<String> {
    let rows: Vec<&cleanup_core::doc::Row> = table.all_rows().collect();
    if rows.is_empty() {
        return Vec::new();
    }
    let columns = table.column_count().max(1);

    // Column widths: the widest single-line cell in the column, capped for terminal output.
    let mut widths = vec![3usize; columns];
    let mut occupancy: Vec<Vec<Option<(usize, usize)>>> = Vec::new();
    for row in &rows {
        let mut line: Vec<Option<(usize, usize)>> = Vec::new();
        for cell in &row.cells {
            let span = cell.colspan.max(1);
            line.push(Some((line.len(), span)));
            for offset in 1..span {
                line.push(Some((line.len() - 1, 0)));
                let _ = offset;
            }
        }
        while line.len() < columns {
            line.push(Some((line.len(), 1)));
        }
        line.truncate(columns);
        occupancy.push(line);
        let mut cursor = 0usize;
        for cell in &row.cells {
            let span = cell
                .colspan
                .max(1)
                .min(columns.saturating_sub(cursor).max(1));
            let single_line = cell.text().replace('\n', " ");
            let needed = single_line.chars().count().min(MAX_COLUMN);
            if span == 1 {
                widths[cursor] = widths[cursor].max(needed);
            } else {
                let current: usize = widths[cursor..(cursor + span).min(columns)].iter().sum();
                if needed > current {
                    let extra = (needed - current).div_ceil(span);
                    for width in widths
                        .iter_mut()
                        .take((cursor + span).min(columns))
                        .skip(cursor)
                    {
                        *width += extra;
                    }
                }
            }
            cursor += span;
        }
    }

    let mut out: Vec<String> = Vec::new();
    out.push(border(&widths, '\u{250C}', '\u{252C}', '\u{2510}'));
    for (row_index, row) in rows.iter().enumerate() {
        // Expand this row into physical lines: the tallest cell wins.
        let mut cells: Vec<(usize, usize, &Cell)> = Vec::new();
        let mut cursor = 0usize;
        for cell in &row.cells {
            let span = cell
                .colspan
                .max(1)
                .min(columns.saturating_sub(cursor).max(1));
            cells.push((cursor, span, cell));
            cursor += span;
        }
        let mut rendered: Vec<Vec<String>> = Vec::new();
        let mut height = 1usize;
        for (_, _, cell) in &cells {
            let mut lines: Vec<String> = Vec::new();
            for line in cell.text().lines() {
                if line.trim().is_empty() && !lines.is_empty() {
                    continue;
                }
                lines.push(line.trim_end().to_string());
            }
            if lines.is_empty() {
                lines.push(String::new());
            }
            height = height.max(lines.len());
            rendered.push(lines);
        }
        for line_index in 0..height {
            let mut line = String::from("\u{2502}");
            let mut column = 0usize;
            for (position, (start, span, cell)) in cells.iter().enumerate() {
                while column < *start {
                    line.push_str(&" ".repeat(widths[column]));
                    line.push('\u{2502}');
                    column += 1;
                }
                let inner_width: usize = widths[column..(column + span).min(columns)]
                    .iter()
                    .sum::<usize>()
                    + (span.saturating_sub(1)) * 3;
                let text = rendered[position]
                    .get(line_index)
                    .cloned()
                    .unwrap_or_default();
                line.push(' ');
                line.push_str(&pad(&text, inner_width.saturating_sub(2), cell.align));
                line.push(' ');
                line.push('\u{2502}');
                column += span;
            }
            while column < columns {
                line.push_str(&" ".repeat(widths[column]));
                line.push('\u{2502}');
                column += 1;
            }
            out.push(line);
        }
        let last_row = row_index + 1 == rows.len();
        if last_row {
            out.push(border(&widths, '\u{2514}', '\u{2534}', '\u{2518}'));
        } else if row_index + 1 == table.headers.len() && !table.headers.is_empty() {
            out.push(border(&widths, '\u{251C}', '\u{253C}', '\u{2524}'));
        } else {
            out.push(border(&widths, '\u{251C}', '\u{253C}', '\u{2524}'));
        }
    }
    out
}

fn border(widths: &[usize], left: char, middle: char, right: char) -> String {
    let mut out = String::new();
    out.push(left);
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            out.push(middle);
        }
        out.push_str(&"\u{2500}".repeat(width + 2));
    }
    out.push(right);
    out
}

fn pad(text: &str, width: usize, align: Align) -> String {
    let length = text.chars().count();
    if length >= width {
        return text.chars().take(width).collect();
    }
    let padding = width - length;
    match align {
        Align::Left => format!("{text}{}", " ".repeat(padding)),
        Align::Right => format!("{}{text}", " ".repeat(padding)),
        Align::Center => {
            let left = padding / 2;
            format!("{}{text}{}", " ".repeat(left), " ".repeat(padding - left))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::doc::{ColumnSpec, PageSpec, ParagraphStyle, Row};

    #[test]
    fn table_uses_box_drawing_and_alignment() {
        let mut doc = Doc::new(PageSpec::default());
        let mut table = Table::default();
        table.columns = vec![
            ColumnSpec {
                width: 1.0,
                align: Align::Left,
            },
            ColumnSpec {
                width: 1.0,
                align: Align::Right,
            },
        ];
        table.headers.push(Row {
            cells: vec![Cell::paragraph("Item"), Cell::paragraph("Total")],
            header: true,
            fill: None,
        });
        table.rows.push(Row {
            cells: vec![Cell::paragraph("Widgets"), Cell::paragraph("1204")],
            header: false,
            fill: None,
        });
        doc.blocks.push(Block::Table(table));
        let text = String::from_utf8(render(&doc).bytes).expect("utf8");
        assert!(text.contains('\u{250C}'));
        assert!(text.contains("Widgets"));
        let total_line = text
            .lines()
            .find(|line| line.contains("1204"))
            .expect("numeric row");
        assert!(total_line.contains(" 1204 \u{2502}"), "{total_line}");
    }

    #[test]
    fn columns_are_linearised_not_interleaved() {
        let mut doc = Doc::new(PageSpec::default());
        doc.blocks.push(Block::Columns {
            columns: vec![
                cleanup_core::doc::Column {
                    blocks: vec![Block::Paragraph {
                        inlines: vec![Inline::plain("left one")],
                        anchor: None,
                        style: ParagraphStyle::default(),
                    }],
                    width: 0.5,
                },
                cleanup_core::doc::Column {
                    blocks: vec![Block::Paragraph {
                        inlines: vec![Inline::plain("right one")],
                        anchor: None,
                        style: ParagraphStyle::default(),
                    }],
                    width: 0.5,
                },
            ],
            anchor: None,
        });
        let text = String::from_utf8(render(&doc).bytes).expect("utf8");
        let left = text.find("left one").expect("left");
        let right = text.find("right one").expect("right");
        assert!(left < right);
    }
}
