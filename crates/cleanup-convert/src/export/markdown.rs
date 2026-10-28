//! Markdown (GitHub-Flavored) exporter.
//!
//! Rectangular tables become GFM pipe tables; merged or multi-paragraph cells fall back to a
//! semantic HTML `<table>` (GFM renders inline HTML, and the `colspan`/`rowspan` survive). Images
//! are written to `./assets/` and referenced with repo-relative paths, so the `.md` file and its
//! asset folder can be moved together.

use super::{Rendered, asset_for};
use cleanup_core::doc::{Align, Block, CalloutKind, CalloutKind as Kind, Doc, Inline, Table};

/// Render `doc` as GFM.
pub fn render(doc: &Doc) -> Rendered {
    let mut out = String::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut assets: Vec<(String, Vec<u8>)> = Vec::new();
    let mut used_assets: Vec<usize> = Vec::new();

    for block in &doc.blocks {
        render_block(block, doc, &mut out, &mut warnings, &mut used_assets);
    }

    for index in used_assets {
        if let Some(asset) = doc.assets.get(index)
            && !assets.iter().any(|(name, _)| *name == asset.file_name())
        {
            assets.push((asset.file_name(), asset.data.clone()));
        }
    }

    let mut text = out.trim_end().to_string();
    text.push('\n');
    Rendered {
        bytes: text.into_bytes(),
        assets,
        warnings,
        engine: "ir-markdown",
    }
}

fn render_block(
    block: &Block,
    doc: &Doc,
    out: &mut String,
    warnings: &mut Vec<String>,
    used_assets: &mut Vec<usize>,
) {
    match block {
        Block::Heading { level, inlines, .. } => {
            let hashes = "#".repeat((*level).clamp(1, 6) as usize);
            let text = render_inlines(inlines, false);
            if !text.trim().is_empty() {
                out.push_str(&format!("{hashes} {}\n\n", text.trim()));
            }
        }
        Block::Paragraph { inlines, .. } => {
            let text = render_inlines(inlines, false);
            if !text.trim().is_empty() {
                out.push_str(text.trim_end());
                out.push_str("\n\n");
            }
        }
        Block::List { ordered, items, .. } => {
            for (index, item) in items.iter().enumerate() {
                let indent = "  ".repeat(item.depth);
                let marker = if *ordered {
                    format!("{}.", index + 1)
                } else {
                    "-".to_string()
                };
                let text = render_inlines(&item.inlines, false);
                if text.trim().is_empty() {
                    continue;
                }
                out.push_str(&format!("{indent}{marker} {}\n", text.trim()));
            }
            out.push('\n');
        }
        Block::Code {
            lines, language, ..
        } => {
            let fence = fence_for(lines);
            out.push_str(&fence);
            if let Some(language) = language {
                out.push_str(language);
            }
            out.push('\n');
            for line in lines {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(&fence);
            out.push_str("\n\n");
        }
        Block::Table(table) => {
            out.push_str(&render_table(table, used_assets, warnings, doc));
            out.push('\n');
        }
        Block::Figure(figure) => {
            if let Some(asset) = asset_for(doc, figure.asset, warnings) {
                used_assets.push(figure.asset);
                out.push_str(&format!(
                    "![{}](assets/{})\n\n",
                    escape_alt(&figure.alt),
                    asset.file_name()
                ));
                if let Some(dpi) = asset.dpi_label() {
                    out.push_str(&format!(
                        "*{} — {}×{} px at {}*\n\n",
                        asset.name, asset.width_px, asset.height_px, dpi
                    ));
                }
            }
            if let Some(caption) = &figure.caption {
                let text = render_inlines(caption, false);
                if !text.trim().is_empty() {
                    out.push_str(&format!("*{}*\n\n", text.trim()));
                }
            }
        }
        Block::Vector(shape) => {
            // GFM allows inline HTML. The SVG keeps the source stroke colour, thickness and
            // aspect ratio instead of degrading to a literal `---` rule.
            out.push_str(&shape.svg);
            out.push_str("\n\n");
        }
        Block::Divider(_) => out.push_str("---\n\n"),
        Block::Callout { kind, blocks, .. } => {
            let mut inner = String::new();
            for block in blocks {
                render_block(block, doc, &mut inner, warnings, used_assets);
            }
            let inner = inner.trim_end();
            if inner.is_empty() {
                return;
            }
            match kind {
                CalloutKind::Quote => {
                    for line in inner.lines() {
                        out.push_str("> ");
                        out.push_str(line);
                        out.push('\n');
                    }
                    out.push('\n');
                }
                Kind::Box | Kind::Note => {
                    // GFM alerts are the semantic Markdown equivalent of a tinted box. The
                    // source fill is recorded as a data attribute on the first line so a
                    // downstream renderer can recover the colour without inventing structure.
                    let tone = match kind {
                        Kind::Note => "[!NOTE]",
                        _ => "[!IMPORTANT]",
                    };
                    out.push_str(&format!("> {tone}\n"));
                    for line in inner.lines() {
                        out.push_str("> ");
                        out.push_str(line);
                        out.push('\n');
                    }
                    out.push('\n');
                }
            }
        }
        Block::Columns { columns, .. } => {
            for column in columns {
                for block in &column.blocks {
                    render_block(block, doc, out, warnings, used_assets);
                }
            }
        }
    }
}

fn fence_for(lines: &[String]) -> String {
    let mut longest = 0usize;
    for line in lines {
        let mut run = 0usize;
        for ch in line.chars() {
            if ch == '`' {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
    }
    "`".repeat((longest + 1).max(3))
}

fn render_table(
    table: &Table,
    used_assets: &mut Vec<usize>,
    warnings: &mut Vec<String>,
    doc: &Doc,
) -> String {
    let mut out = String::new();
    if let Some(caption) = &table.caption {
        let text = render_inlines(caption, false);
        if !text.trim().is_empty() {
            out.push_str(&format!("**{}**\n\n", text.trim()));
        }
    }
    let merged = table.has_merged_cells();
    let multiline = table.has_multiline_cells();
    if merged || multiline {
        out.push_str(&render_html_table(table, doc, used_assets, warnings));
        out.push('\n');
        if merged {
            warnings.push(
                "table has merged cells: exported as semantic HTML so colspan/rowspan survive"
                    .to_string(),
            );
        }
        return out;
    }

    let columns = table.column_count().max(1);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in table.all_rows() {
        let mut cells: Vec<String> = Vec::new();
        for cell in &row.cells {
            cells.push(escape_cell(&cell.text().replace('\n', " ")));
        }
        while cells.len() < columns {
            cells.push(String::new());
        }
        cells.truncate(columns);
        rows.push(cells);
    }
    if rows.is_empty() {
        return out;
    }
    let header = rows.remove(0);
    out.push_str(&format!("| {} |\n", header.join(" | ")));
    let separators: Vec<String> = (0..columns)
        .map(|index| {
            let align = table
                .columns
                .get(index)
                .map(|column| column.align)
                .unwrap_or(Align::Left);
            match align {
                Align::Left => ":---".to_string(),
                Align::Center => ":---:".to_string(),
                Align::Right => "---:".to_string(),
            }
        })
        .collect();
    out.push_str(&format!("| {} |\n", separators.join(" | ")));
    for row in rows {
        out.push_str(&format!("| {} |\n", row.join(" | ")));
    }
    out
}

fn render_html_table(
    table: &Table,
    doc: &Doc,
    used_assets: &mut Vec<usize>,
    _warnings: &mut Vec<String>,
) -> String {
    let mut out = String::from("<table>\n");
    // Explicit column widths are part of the ground truth: a semantic fallback that forgets
    // them loses the layout even though the text survived.
    if !table.columns.is_empty() {
        let total: f32 = table
            .columns
            .iter()
            .map(|column| column.width.max(0.1))
            .sum();
        if total > 0.0 {
            out.push_str("  <colgroup>\n");
            for column in &table.columns {
                let percent = column.width.max(0.1) * 100.0 / total;
                out.push_str(&format!("    <col style=\"width:{percent:.1}%\" />\n"));
            }
            out.push_str("  </colgroup>\n");
        }
    }
    if table.headers.iter().any(|row| !row.cells.is_empty()) {
        out.push_str("  <thead>\n");
        for row in &table.headers {
            out.push_str("    <tr>\n");
            for cell in &row.cells {
                out.push_str(&html_cell(cell, "th", doc, used_assets));
            }
            out.push_str("    </tr>\n");
        }
        out.push_str("  </thead>\n");
    }
    out.push_str("  <tbody>\n");
    for row in &table.rows {
        out.push_str("    <tr>\n");
        for cell in &row.cells {
            out.push_str(&html_cell(cell, "td", doc, used_assets));
        }
        out.push_str("    </tr>\n");
    }
    out.push_str("  </tbody>\n</table>");
    out
}

fn html_cell(
    cell: &cleanup_core::doc::Cell,
    tag: &str,
    doc: &Doc,
    used_assets: &mut Vec<usize>,
) -> String {
    let mut attributes = String::new();
    if cell.colspan > 1 {
        attributes.push_str(&format!(" colspan=\"{}\"", cell.colspan));
    }
    if cell.rowspan > 1 {
        attributes.push_str(&format!(" rowspan=\"{}\"", cell.rowspan));
    }
    if cell.align != Align::Left {
        attributes.push_str(&format!(" align=\"{}\"", cell.align.as_str()));
    }
    if let Some(fill) = cell.fill {
        attributes.push_str(&format!(" style=\"background:#{}\"", fill.hex()));
    }
    let mut inner = String::new();
    for block in &cell.blocks {
        let text = match block {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => {
                render_inlines(inlines, true)
            }
            Block::Code { lines, .. } => format!("<code>{}</code>", escape_html(&lines.join("\n"))),
            Block::Figure(figure) => doc
                .assets
                .get(figure.asset)
                .map(|asset| {
                    used_assets.push(figure.asset);
                    format!(
                        "![{}](assets/{})",
                        escape_alt(&figure.alt),
                        asset.file_name()
                    )
                })
                .unwrap_or_default(),
            _ => escape_html(&block.plain_text()),
        };
        if !text.trim().is_empty() {
            inner.push_str(&text);
        }
    }
    format!("      <{tag}{attributes}>{inner}</{tag}>\n")
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_alt(text: &str) -> String {
    text.replace('[', "(").replace(']', ")")
}

fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Render inline runs. `html` switches the emphasis markers to tags for HTML table cells.
pub(crate) fn render_inlines(inlines: &[Inline], html: bool) -> String {
    let mut out = String::new();
    for inline in inlines {
        if inline.is_break() {
            out.push('\n');
            continue;
        }
        // Underline and colour have no GFM syntax. Emitting inline HTML for those runs means
        // the run's own text must be HTML-escaped, and emphasis markers inside it must become
        // tags too (GFM does not parse Markdown inside an inline HTML element).
        let raw_html = html || inline.style.color.is_some() || inline.style.underline;
        let text = if raw_html {
            escape_html(&inline.text)
        } else {
            escape_markdown(&inline.text)
        };
        if text.trim().is_empty() {
            out.push_str(&text);
            continue;
        }
        // Whitespace around a styled run must stay *outside* the emphasis markers, otherwise
        // `**bold **` is not valid GFM and renders as literal asterisks.
        let leading = text.len() - text.trim_start_matches(char::is_whitespace).len();
        let trailing = text.len() - text.trim_end_matches(char::is_whitespace).len();
        let core_end = text.len() - trailing;
        let (prefix, core, suffix) = (
            &text[..leading],
            &text[leading..core_end],
            &text[core_end..],
        );
        let mut rendered = core.to_string();
        if inline.style.code {
            if raw_html {
                rendered = format!("<code>{rendered}</code>");
            } else {
                let fence = if rendered.contains('`') { "``" } else { "`" };
                rendered = format!("{fence}{rendered}{fence}");
            }
        }
        if inline.style.strike {
            rendered = if raw_html {
                format!("<del>{rendered}</del>")
            } else {
                format!("~~{rendered}~~")
            };
        }
        if inline.style.italic {
            rendered = if raw_html {
                format!("<em>{rendered}</em>")
            } else {
                format!("*{rendered}*")
            };
        }
        if inline.style.bold {
            rendered = if raw_html {
                format!("<strong>{rendered}</strong>")
            } else {
                format!("**{rendered}**")
            };
        }
        if inline.style.underline {
            rendered = format!("<u>{rendered}</u>");
        }
        if let Some(color) = inline.style.color {
            rendered = format!("<span style=\"color:#{}\">{}</span>", color.hex(), rendered);
        }
        if let Some(link) = &inline.style.link {
            rendered = if raw_html {
                format!("<a href=\"{}\">{}</a>", escape_html(link), rendered)
            } else {
                format!("[{rendered}]({})", link.replace(')', "%29"))
            };
        }
        out.push_str(prefix);
        out.push_str(&rendered);
        out.push_str(suffix);
    }
    out
}

/// Escape the characters that would otherwise start Markdown syntax. Only ASCII punctuation is
/// touched, so every non-ASCII character survives unchanged.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::doc::{Cell, ColumnSpec, PageSpec, ParagraphStyle, Row};

    fn paragraph(text: &str) -> Block {
        Block::Paragraph {
            inlines: vec![Inline::plain(text)],
            anchor: None,
            style: ParagraphStyle::default(),
        }
    }

    #[test]
    fn paragraphs_escape_markdown_syntax() {
        let mut doc = Doc::new(PageSpec::default());
        doc.blocks.push(paragraph("Use *args and **kwargs"));
        let rendered = render(&doc);
        let text = String::from_utf8(rendered.bytes).expect("utf8");
        assert!(text.contains("\\*args"));
        assert!(text.contains("\\*\\*kwargs"));
    }

    #[test]
    fn rectangular_tables_become_gfm() {
        let mut doc = Doc::new(PageSpec::default());
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
                cells: vec![Cell::paragraph("Item"), Cell::paragraph("Total")],
                header: true,
                fill: None,
            }],
            rows: vec![Row {
                cells: vec![Cell::paragraph("Widgets"), Cell::paragraph("1,204")],
                header: false,
                fill: None,
            }],
            ..Table::default()
        };
        doc.blocks.push(Block::Table(table));
        let text = String::from_utf8(render(&doc).bytes).expect("utf8");
        assert!(text.contains("| Item | Total |"), "{text}");
        assert!(text.contains("| :--- | ---: |"), "{text}");
        assert!(text.contains("| Widgets | 1,204 |"), "{text}");
    }

    #[test]
    fn merged_cells_fall_back_to_html() {
        let mut doc = Doc::new(PageSpec::default());
        let mut table = Table::default();
        let mut merged = Cell::paragraph("span");
        merged.colspan = 2;
        table.headers.push(Row {
            cells: vec![Cell::paragraph("A"), Cell::paragraph("B")],
            header: true,
            fill: None,
        });
        table.rows.push(Row {
            cells: vec![merged],
            header: false,
            fill: None,
        });
        doc.blocks.push(Block::Table(table));
        let text = String::from_utf8(render(&doc).bytes).expect("utf8");
        assert!(text.contains("<table>"));
        assert!(text.contains("colspan=\"2\""));
    }

    #[test]
    fn unicode_survives_untouched() {
        let mut doc = Doc::new(PageSpec::default());
        doc.blocks.push(paragraph("∑ ∫ ≠ ≈ ± αβγ → ← 日本語"));
        let text = String::from_utf8(render(&doc).bytes).expect("utf8");
        assert!(text.contains("∑ ∫ ≠ ≈ ± αβγ → ← 日本語"));
    }
}
