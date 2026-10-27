//! Readers: every input format becomes the same intermediate representation.
//!
//! PDF goes through the spatial reader (`cleanup_formats::pdf::extract_layout`) and the layout
//! analyser. The other formats keep their structure directly: Markdown through `pulldown-cmark`,
//! DOCX through its OpenXML body (paragraph styles, run properties, tables, media), HTML through
//! a tag walker, and plain text through paragraph / code / list heuristics.

use crate::{ConvertError, Result};
use cleanup_core::FormatId;
use cleanup_core::doc::{
    Align, Axis, Block, CalloutKind, Cell, ColumnSpec, Divider, Doc, Figure, ImageAsset, ImageKind,
    Inline, InlineStyle, ListItem, PageSpec, ParagraphStyle, Row, SourceKind, Table, TableBorders,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};
use quick_xml::Reader;
use quick_xml::events::Event as XmlEvent;
use std::collections::BTreeMap;

/// Build the IR for an already-decoded input document.
pub(crate) fn build(from: FormatId, src: &[u8]) -> Result<(Doc, Vec<String>)> {
    match from {
        FormatId::Markdown => markdown(src),
        FormatId::PlainText => plain_text(src),
        FormatId::Html => html(src),
        FormatId::Docx => docx(src),
        FormatId::Pdf => pdf_layout(src),
        FormatId::Image => Err(ConvertError::Unsupported {
            from: "image",
            to: "document",
        }),
    }
}

/// PDF: spatial reconstruction, then layout analysis.
pub(crate) fn pdf_layout(src: &[u8]) -> Result<(Doc, Vec<String>)> {
    let report = cleanup_formats::pdf::extract_layout(src)?;
    let (mut doc, analysis_warnings) = cleanup_core::doc::analyze(
        &report.layout,
        &cleanup_core::doc::AnalyzeOptions::default(),
    );
    doc.info.source = SourceKind::Pdf;
    doc.info.page_count = report.page_count;
    let mut warnings = report.warnings;
    warnings.extend(analysis_warnings);
    if report.unsupported_fragments > 0 {
        warnings.push(format!(
            "{} text fragment(s) were skipped because their font could not be mapped to Unicode",
            report.unsupported_fragments
        ));
    }
    Ok((doc, warnings))
}

// ---------------------------------------------------------------------------
// Shared table assembly
// ---------------------------------------------------------------------------

struct TableState {
    columns: Vec<ColumnSpec>,
    headers: Vec<Row>,
    rows: Vec<Row>,
    current_row: Vec<Cell>,
    current_cell: Vec<Inline>,
    in_head: bool,
    pending_colspan: usize,
}

impl TableState {
    fn new(aligns: &[pulldown_cmark::Alignment]) -> Self {
        let columns = aligns
            .iter()
            .map(|alignment| ColumnSpec {
                width: 1.0,
                align: match alignment {
                    pulldown_cmark::Alignment::Center => Align::Center,
                    pulldown_cmark::Alignment::Right => Align::Right,
                    _ => Align::Left,
                },
            })
            .collect();
        TableState {
            columns,
            headers: Vec::new(),
            rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: Vec::new(),
            in_head: false,
            pending_colspan: 1,
        }
    }

    fn finish_cell(&mut self, header: bool) {
        let inlines = trim_inlines(std::mem::take(&mut self.current_cell));
        let colspan = self.pending_colspan.max(1);
        self.pending_colspan = 1;
        self.current_row.push(Cell {
            blocks: vec![Block::Paragraph {
                inlines,
                anchor: None,
                style: ParagraphStyle::default(),
            }],
            colspan,
            rowspan: 1,
            align: Align::Left,
            fill: None,
            header,
        });
    }

    fn finish_row(&mut self) {
        let cells = std::mem::take(&mut self.current_row);
        if cells.is_empty() {
            return;
        }
        if self.in_head && self.headers.is_empty() {
            self.headers.push(Row {
                cells,
                header: true,
                fill: None,
            });
        } else {
            self.rows.push(Row {
                cells,
                header: false,
                fill: None,
            });
        }
    }

    fn into_table(self) -> Table {
        Table {
            anchor: None,
            caption: None,
            columns: self.columns,
            headers: self.headers,
            rows: self.rows,
            pages: Vec::new(),
            borders: TableBorders::default(),
            cell_padding: 4.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

fn markdown(src: &[u8]) -> Result<(Doc, Vec<String>)> {
    let text = std::str::from_utf8(src)
        .map_err(|error| ConvertError::Failed(format!("markdown input is not UTF-8: {error}")))?;
    let mut doc = Doc::new(PageSpec::default());
    doc.info.source = SourceKind::Markdown;
    let mut warnings: Vec<String> = Vec::new();
    parse_markdown(text, &mut doc, &mut warnings, 0);
    Ok((doc, warnings))
}

/// Split a Markdown source into plain chunks and block-quote chunks, so quotes can be parsed
/// recursively without depending on how the parser spells its end-of-quote event.
fn split_quotes(text: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut buffer = String::new();
    let mut quoted = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_quote = trimmed.starts_with('>');
        if is_quote != quoted && !buffer.trim().is_empty() {
            out.push((quoted, std::mem::take(&mut buffer)));
        }
        quoted = is_quote;
        if is_quote {
            let rest = trimmed[1..].strip_prefix(' ').unwrap_or(&trimmed[1..]);
            buffer.push_str(rest);
            buffer.push('\n');
        } else {
            buffer.push_str(line);
            buffer.push('\n');
        }
    }
    if !buffer.trim().is_empty() {
        out.push((quoted, buffer));
    }
    out
}

fn parse_markdown(text: &str, doc: &mut Doc, warnings: &mut Vec<String>, depth: usize) {
    if depth > 8 {
        return;
    }
    for (quoted, chunk) in split_quotes(text) {
        if quoted {
            let mut inner = Doc::new(PageSpec::default());
            parse_markdown(&chunk, &mut inner, warnings, depth + 1);
            if !inner.blocks.is_empty() {
                doc.blocks.push(Block::Callout {
                    kind: CalloutKind::Quote,
                    blocks: inner.blocks,
                    anchor: None,
                    fill: None,
                });
            }
            continue;
        }
        parse_markdown_chunk(&chunk, doc, warnings);
    }
}

struct ListBuilder {
    ordered: bool,
    items: Vec<ListItem>,
}

#[derive(Default)]
struct MarkdownState {
    inlines: Vec<Inline>,
    style: InlineStyle,
    heading: Option<u8>,
    lists: Vec<ListBuilder>,
    code: Option<(Option<String>, String)>,
    table: Option<TableState>,
    image: Option<String>,
    image_target: String,
}

fn parse_markdown_chunk(text: &str, doc: &mut Doc, warnings: &mut Vec<String>) {
    let mut state = MarkdownState::default();
    for event in Parser::new_ext(text, crate::md_options()) {
        match event {
            Event::Start(Tag::Paragraph) => state.inlines.clear(),
            Event::End(TagEnd::Paragraph) => flush_paragraph(&mut state, doc),
            Event::Start(Tag::Heading { level, .. }) => {
                state.inlines.clear();
                state.heading = Some(heading_level(level));
            }
            Event::End(TagEnd::Heading(_)) => {
                flush_paragraph(&mut state, doc);
                state.heading = None;
            }
            Event::Start(Tag::Strong) => state.style.bold = true,
            Event::End(TagEnd::Strong) => state.style.bold = false,
            Event::Start(Tag::Emphasis) => state.style.italic = true,
            Event::End(TagEnd::Emphasis) => state.style.italic = false,
            Event::Start(Tag::Strikethrough) => state.style.strike = true,
            Event::End(TagEnd::Strikethrough) => state.style.strike = false,
            Event::Start(Tag::Link { dest_url, .. }) => {
                state.style.link = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Link) => state.style.link = None,
            Event::Start(Tag::List(start)) => state.lists.push(ListBuilder {
                ordered: start.is_some(),
                items: Vec::new(),
            }),
            Event::End(TagEnd::List(_)) => {
                if let Some(list) = state.lists.pop()
                    && !list.items.is_empty()
                {
                    doc.blocks.push(Block::List {
                        ordered: list.ordered,
                        items: list.items,
                        anchor: None,
                    });
                }
            }
            Event::Start(Tag::Item) => state.inlines.clear(),
            Event::End(TagEnd::Item) => {
                let inlines = trim_inlines(std::mem::take(&mut state.inlines));
                let depth = state.lists.len().saturating_sub(1);
                if let Some(list) = state.lists.last_mut()
                    && !inlines.is_empty()
                {
                    list.items.push(ListItem { inlines, depth });
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        let info = info.to_string();
                        let first = info.split_whitespace().next().unwrap_or("").to_string();
                        if first.is_empty() { None } else { Some(first) }
                    }
                    CodeBlockKind::Indented => None,
                };
                state.code = Some((language, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((language, buffer)) = state.code.take() {
                    let mut lines: Vec<String> = buffer
                        .lines()
                        .map(|line| line.trim_end().to_string())
                        .collect();
                    while lines.last().is_some_and(|line| line.is_empty()) {
                        lines.pop();
                    }
                    doc.blocks.push(Block::Code {
                        lines,
                        anchor: None,
                        language,
                    });
                }
            }
            Event::Start(Tag::Table(aligns)) => state.table = Some(TableState::new(&aligns)),
            Event::End(TagEnd::Table) => {
                if let Some(table) = state.table.take() {
                    doc.blocks.push(Block::Table(table.into_table()));
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(table) = state.table.as_mut() {
                    table.in_head = true;
                    table.current_row.clear();
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(table) = state.table.as_mut() {
                    table.finish_row();
                    table.in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {
                if let Some(table) = state.table.as_mut() {
                    table.current_row.clear();
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(table) = state.table.as_mut() {
                    table.finish_row();
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(table) = state.table.as_mut() {
                    table.current_cell.clear();
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(table) = state.table.as_mut() {
                    let header = table.in_head;
                    table.finish_cell(header);
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                state.image = Some(String::new());
                state.image_target = dest_url.to_string();
            }
            Event::Text(text) => {
                if let Some(image) = state.image.as_mut() {
                    image.push_str(&text);
                } else if let Some((_, buffer)) = state.code.as_mut() {
                    buffer.push_str(&text);
                } else if let Some(table) = state.table.as_mut() {
                    let style = state.style.clone();
                    table
                        .current_cell
                        .push(Inline::new(text.to_string(), style));
                } else {
                    state
                        .inlines
                        .push(Inline::new(text.to_string(), state.style.clone()));
                }
            }
            Event::Code(text) => {
                let mut style = state.style.clone();
                style.code = true;
                if let Some(table) = state.table.as_mut() {
                    table
                        .current_cell
                        .push(Inline::new(text.to_string(), style));
                } else {
                    state.inlines.push(Inline::new(text.to_string(), style));
                }
            }
            Event::SoftBreak => {
                if let Some(image) = state.image.as_mut() {
                    image.push(' ');
                } else if let Some((_, buffer)) = state.code.as_mut() {
                    buffer.push('\n');
                } else if let Some(table) = state.table.as_mut() {
                    table
                        .current_cell
                        .push(Inline::new(" ", InlineStyle::default()));
                } else {
                    state.inlines.push(Inline::new(" ", InlineStyle::default()));
                }
            }
            Event::HardBreak => {
                if let Some((_, buffer)) = state.code.as_mut() {
                    buffer.push('\n');
                } else if let Some(table) = state.table.as_mut() {
                    table.current_cell.push(Inline::line_break());
                } else {
                    state.inlines.push(Inline::line_break());
                }
            }
            Event::Rule => doc.blocks.push(Block::Divider(Divider {
                anchor: None,
                axis: Axis::Horizontal,
            })),
            Event::Html(html) | Event::InlineHtml(html) => {
                let stripped = strip_tags(&html);
                if !stripped.trim().is_empty() {
                    let style = state.style.clone();
                    if let Some(table) = state.table.as_mut() {
                        table.current_cell.push(Inline::new(stripped, style));
                    } else {
                        state.inlines.push(Inline::new(stripped, style));
                    }
                }
            }
            Event::End(_) => {
                if let Some(alt) = state.image.take() {
                    let alt = alt.trim().to_string();
                    warnings.push(format!(
                        "image reference `{}` was kept as a placeholder (external images are not downloaded)",
                        state.image_target
                    ));
                    doc.blocks.push(Block::Paragraph {
                        inlines: vec![Inline::new(
                            if alt.is_empty() {
                                format!("[image: {}]", state.image_target)
                            } else {
                                format!("[image: {alt}] ({})", state.image_target)
                            },
                            InlineStyle::italic(),
                        )],
                        anchor: None,
                        style: ParagraphStyle::default(),
                    });
                }
            }
            _ => {}
        }
    }
    flush_paragraph(&mut state, doc);
}

fn flush_paragraph(state: &mut MarkdownState, doc: &mut Doc) {
    let inlines = trim_inlines(std::mem::take(&mut state.inlines));
    if inlines.is_empty() {
        return;
    }
    match state.heading {
        Some(level) => doc.blocks.push(Block::Heading {
            level: level.clamp(1, 6),
            inlines,
            anchor: None,
        }),
        None => doc.blocks.push(Block::Paragraph {
            inlines,
            anchor: None,
            style: ParagraphStyle::default(),
        }),
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Drop leading/trailing whitespace-only inlines and normalise non-breaking spaces.
pub(crate) fn trim_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        if inline.is_break() {
            out.push(inline);
            continue;
        }
        let text = inline.text.replace('\u{a0}', " ");
        if text.trim().is_empty() && out.is_empty() {
            continue;
        }
        out.push(Inline {
            text,
            style: inline.style,
        });
    }
    while out
        .last()
        .is_some_and(|inline| !inline.is_break() && inline.text.trim().is_empty())
    {
        out.pop();
    }
    // Cut the trailing space of the last text inline.
    if let Some(last) = out.last_mut()
        && !last.is_break()
    {
        last.text = last.text.trim_end().to_string();
    }
    out.retain(|inline| inline.is_break() || !inline.text.is_empty());
    out
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Plain text
// ---------------------------------------------------------------------------

fn plain_text(src: &[u8]) -> Result<(Doc, Vec<String>)> {
    let text = String::from_utf8_lossy(src).into_owned();
    let mut doc = Doc::new(PageSpec::default());
    doc.info.source = SourceKind::PlainText;
    let mut paragraph: Vec<String> = Vec::new();
    let mut code: Vec<String> = Vec::new();
    let mut items: Vec<ListItem> = Vec::new();
    let mut ordered = false;

    for line in text.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end.trim().is_empty() {
            flush_text_paragraph(&mut paragraph, &mut doc);
            flush_code(&mut code, &mut doc);
            flush_list(&mut items, ordered, &mut doc);
            continue;
        }
        let indented = trimmed_end.starts_with("    ") || trimmed_end.starts_with('\t');
        if indented {
            flush_text_paragraph(&mut paragraph, &mut doc);
            flush_list(&mut items, ordered, &mut doc);
            code.push(trimmed_end.to_string());
            continue;
        }
        flush_code(&mut code, &mut doc);
        if let Some(item) = list_item_text(trimmed_end) {
            flush_text_paragraph(&mut paragraph, &mut doc);
            ordered = starts_ordered(trimmed_end);
            items.push(ListItem {
                inlines: vec![Inline::plain(item)],
                depth: 0,
            });
            continue;
        }
        flush_list(&mut items, ordered, &mut doc);
        paragraph.push(trimmed_end.trim().to_string());
    }
    flush_text_paragraph(&mut paragraph, &mut doc);
    flush_code(&mut code, &mut doc);
    flush_list(&mut items, ordered, &mut doc);
    Ok((doc, Vec::new()))
}

fn flush_text_paragraph(paragraph: &mut Vec<String>, doc: &mut Doc) {
    if paragraph.is_empty() {
        return;
    }
    let joined = paragraph.join(" ");
    paragraph.clear();
    doc.blocks.push(Block::Paragraph {
        inlines: vec![Inline::plain(joined.trim().to_string())],
        anchor: None,
        style: ParagraphStyle::default(),
    });
}

fn flush_code(code: &mut Vec<String>, doc: &mut Doc) {
    if code.is_empty() {
        return;
    }
    let lines = std::mem::take(code);
    doc.blocks.push(Block::Code {
        lines,
        anchor: None,
        language: None,
    });
}

fn flush_list(items: &mut Vec<ListItem>, ordered: bool, doc: &mut Doc) {
    if items.is_empty() {
        return;
    }
    let items = std::mem::take(items);
    doc.blocks.push(Block::List {
        ordered,
        items,
        anchor: None,
    });
}

fn starts_ordered(line: &str) -> bool {
    let trimmed = line.trim_start();
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    matches!(trimmed[digits..].chars().next(), Some('.') | Some(')'))
}

fn list_item_text(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if matches!(first, '•' | '‣' | '◦' | '▪' | '-' | '*' | '–') {
        let after = &trimmed[first.len_utf8()..];
        if !after.starts_with(|c: char| c.is_whitespace()) {
            return None;
        }
        let rest = after.trim_start();
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
        return None;
    }
    if first.is_ascii_digit() {
        let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        let rest = &trimmed[digits.len()..];
        if let Some(rest) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
            let rest = rest.trim_start();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

fn html(src: &[u8]) -> Result<(Doc, Vec<String>)> {
    let mut doc = Doc::new(PageSpec::default());
    doc.info.source = SourceKind::Html;
    let mut warnings: Vec<String> = Vec::new();
    let mut state = HtmlState::default();
    let mut reader = Reader::from_reader(src);
    reader.config_mut().check_end_names = false;
    let mut buffer = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(XmlEvent::Start(element)) => {
                let name = element.name().as_ref().to_string();
                let empty = is_void(&name);
                state.open(&name, &element, &mut doc, &mut warnings);
                if empty {
                    state.close(&name, &mut doc);
                }
            }
            Ok(XmlEvent::Empty(element)) => {
                let name = element.name().as_ref().to_string();
                state.open(&name, &element, &mut doc, &mut warnings);
                state.close(&name, &mut doc);
            }
            Ok(XmlEvent::End(element)) => {
                let name = element.name().as_ref().to_string();
                state.close(&name, &mut doc);
            }
            Ok(XmlEvent::Text(text)) => {
                let decoded = decode_entities(text.as_ref());
                state.push_text(&decoded);
            }
            Ok(XmlEvent::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                warnings.push(format!("HTML parse stopped early: {error}"));
                break;
            }
        }
        buffer.clear();
    }
    state.flush_paragraph(&mut doc);
    Ok((doc, warnings))
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "br" | "hr" | "img" | "meta" | "link" | "input" | "source" | "wbr"
    )
}

fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    quick_xml::escape::unescape(raw)
        .map(String::from)
        .unwrap_or_else(|_| raw.to_string())
}

#[derive(Default)]
struct HtmlState {
    inlines: Vec<Inline>,
    style: InlineStyle,
    heading: Option<u8>,
    pre: bool,
    code: String,
    lists: Vec<(bool, Vec<ListItem>)>,
    table: Option<TableState>,
    quote_depth: usize,
    bold: u8,
    italic: u8,
}

impl HtmlState {
    fn push_text(&mut self, text: &str) {
        if self.pre {
            self.code.push_str(text);
            return;
        }
        let collapsed = collapse_ws(text);
        if collapsed.is_empty() {
            return;
        }
        let style = self.style.clone();
        if let Some(table) = self.table.as_mut() {
            table.current_cell.push(Inline::new(collapsed, style));
        } else {
            self.inlines.push(Inline::new(collapsed, style));
        }
    }

    fn open(
        &mut self,
        name: &str,
        element: &quick_xml::events::BytesStart<'_>,
        doc: &mut Doc,
        warnings: &mut Vec<String>,
    ) {
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_paragraph(doc);
                self.heading = name.as_bytes()[1].checked_sub(b'0');
            }
            "p" | "div" | "section" | "article" => self.flush_paragraph(doc),
            "br" => self.inlines.push(Inline::line_break()),
            "hr" => {
                self.flush_paragraph(doc);
                doc.blocks.push(Block::Divider(Divider {
                    anchor: None,
                    axis: Axis::Horizontal,
                }));
            }
            "strong" | "b" => self.bold = self.bold.saturating_add(1),
            "em" | "i" => self.italic = self.italic.saturating_add(1),
            "code" | "tt" => self.style.code = true,
            "pre" => {
                self.flush_paragraph(doc);
                self.pre = true;
                self.code.clear();
            }
            "blockquote" => {
                self.flush_paragraph(doc);
                self.quote_depth += 1;
            }
            "ul" | "ol" => {
                self.flush_paragraph(doc);
                self.lists.push((name == "ol", Vec::new()));
            }
            "li" => self.inlines.clear(),
            "table" => {
                self.flush_paragraph(doc);
                self.table = Some(TableState::new(&[]));
            }
            "thead" => {
                if let Some(table) = self.table.as_mut() {
                    table.in_head = true;
                }
            }
            "tbody" => {
                if let Some(table) = self.table.as_mut() {
                    table.in_head = false;
                }
            }
            "tr" => {
                if let Some(table) = self.table.as_mut() {
                    table.current_row.clear();
                }
            }
            "td" | "th" => {
                if let Some(table) = self.table.as_mut() {
                    table.current_cell.clear();
                    if name == "th" {
                        table.in_head = true;
                    }
                }
            }
            "img" => {
                let mut source = String::new();
                for attribute in element.attributes().flatten() {
                    if attribute.key.as_ref() == "src" {
                        let value: &str = &attribute.value;
                        source = value.to_string();
                    }
                }
                if !source.is_empty() {
                    warnings.push(format!(
                        "image reference `{source}` was kept as a placeholder (external images are not downloaded)"
                    ));
                    self.inlines.push(Inline::new(
                        format!("[image: {source}]"),
                        InlineStyle::italic(),
                    ));
                }
            }
            "a" => {
                for attribute in element.attributes().flatten() {
                    if attribute.key.as_ref() == "href" {
                        let value: &str = &attribute.value;
                        self.style.link = Some(value.to_string());
                    }
                }
            }
            _ => {}
        }
        self.sync_style();
    }

    fn close(&mut self, name: &str, doc: &mut Doc) {
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_paragraph(doc);
                self.heading = None;
            }
            "p" | "div" | "section" | "article" => self.flush_paragraph(doc),
            "strong" | "b" => self.bold = self.bold.saturating_sub(1),
            "em" | "i" => self.italic = self.italic.saturating_sub(1),
            "code" | "tt" => self.style.code = false,
            "pre" => {
                self.pre = false;
                let text = std::mem::take(&mut self.code);
                if !text.trim().is_empty() {
                    let lines: Vec<String> = text
                        .lines()
                        .map(|line| line.trim_end().to_string())
                        .collect();
                    doc.blocks.push(Block::Code {
                        lines,
                        anchor: None,
                        language: None,
                    });
                }
            }
            "blockquote" => {
                self.flush_paragraph(doc);
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            "ul" | "ol" => {
                if let Some((ordered, items)) = self.lists.pop()
                    && !items.is_empty()
                {
                    doc.blocks.push(Block::List {
                        ordered,
                        items,
                        anchor: None,
                    });
                }
            }
            "li" => {
                let inlines = trim_inlines(std::mem::take(&mut self.inlines));
                let depth = self.lists.len().saturating_sub(1);
                if let Some((_, items)) = self.lists.last_mut()
                    && !inlines.is_empty()
                {
                    items.push(ListItem { inlines, depth });
                }
            }
            "table" => {
                if let Some(table) = self.table.take() {
                    doc.blocks.push(Block::Table(table.into_table()));
                }
            }
            "thead" | "tbody" => {
                if let Some(table) = self.table.as_mut() {
                    table.in_head = false;
                }
            }
            "tr" => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_row();
                }
            }
            "td" | "th" => {
                if let Some(table) = self.table.as_mut() {
                    let header = name == "th";
                    table.finish_cell(header);
                }
            }
            "a" => self.style.link = None,
            _ => {}
        }
        self.sync_style();
    }

    fn sync_style(&mut self) {
        self.style.bold = self.bold > 0;
        self.style.italic = self.italic > 0;
    }

    fn flush_paragraph(&mut self, doc: &mut Doc) {
        let inlines = trim_inlines(std::mem::take(&mut self.inlines));
        if inlines.is_empty() {
            return;
        }
        let block = match self.heading {
            Some(level) => Block::Heading {
                level,
                inlines,
                anchor: None,
            },
            None => Block::Paragraph {
                inlines,
                anchor: None,
                style: ParagraphStyle::default(),
            },
        };
        if self.quote_depth > 0 {
            doc.blocks.push(Block::Callout {
                kind: CalloutKind::Quote,
                blocks: vec![block],
                anchor: None,
                fill: None,
            });
        } else {
            doc.blocks.push(block);
        }
    }
}

fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending = !out.is_empty();
        } else {
            if pending {
                out.push(' ');
                pending = false;
            }
            out.push(ch);
        }
    }
    if pending && !out.is_empty() {
        out.push(' ');
    }
    out
}

// ---------------------------------------------------------------------------
// DOCX
// ---------------------------------------------------------------------------

fn docx(src: &[u8]) -> Result<(Doc, Vec<String>)> {
    let document = read_zip_part(src, "word/document.xml")?;
    let relationships = read_zip_part(src, "word/_rels/document.xml.rels").unwrap_or_default();
    let images = parse_relationships(&relationships);
    let mut doc = Doc::new(PageSpec::default());
    doc.info.source = SourceKind::Docx;
    let mut warnings: Vec<String> = Vec::new();
    let mut reader = Reader::from_reader(document.as_slice());
    let mut buffer = Vec::new();

    let mut inlines: Vec<Inline> = Vec::new();
    let mut style = InlineStyle::default();
    let mut heading: Option<u8> = None;
    let mut list_item = false;
    let mut in_paragraph = false;
    let mut in_run_properties = false;
    let mut table: Option<TableState> = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(XmlEvent::Start(element)) => {
                let name = element.name().as_ref().to_string();
                match local(&name) {
                    "p" => {
                        in_paragraph = true;
                        inlines.clear();
                        heading = None;
                        list_item = false;
                        style = InlineStyle::default();
                    }
                    "rPr" => in_run_properties = true,
                    "pStyle" => {
                        for attribute in element.attributes().flatten() {
                            if local(attribute.key.as_ref()) == "val" {
                                let value: &str = &attribute.value;
                                heading = heading_from_style(&value.to_lowercase());
                                if value.to_lowercase().contains("list") {
                                    list_item = true;
                                }
                            }
                        }
                    }
                    "numPr" => list_item = true,
                    "tbl" => table = Some(TableState::new(&[])),
                    "tc" => {
                        if let Some(table) = table.as_mut() {
                            table.current_cell.clear();
                            table.pending_colspan = 1;
                        }
                    }
                    "tr" => {
                        if let Some(table) = table.as_mut() {
                            table.current_row.clear();
                        }
                    }
                    "blip" => {
                        for attribute in element.attributes().flatten() {
                            if local(attribute.key.as_ref()) == "embed" {
                                let value: &str = &attribute.value;
                                embed_image(src, &images, value, &mut doc, &mut warnings);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::Empty(element)) => {
                let name = element.name().as_ref().to_string();
                match local(&name) {
                    "pStyle" => {
                        for attribute in element.attributes().flatten() {
                            if local(attribute.key.as_ref()) == "val" {
                                let value: &str = &attribute.value;
                                let lower = value.to_lowercase();
                                heading = heading_from_style(&lower);
                                if lower.contains("list") {
                                    list_item = true;
                                }
                            }
                        }
                    }
                    "numPr" => list_item = true,
                    "b" if in_run_properties => style.bold = true,
                    "i" if in_run_properties => style.italic = true,
                    "u" if in_run_properties => style.underline = true,
                    "strike" if in_run_properties => style.strike = true,
                    "br" | "cr" => inlines.push(Inline::line_break()),
                    "tab" => inlines.push(Inline::plain("\t")),
                    "gridSpan" => {
                        for attribute in element.attributes().flatten() {
                            if local(attribute.key.as_ref()) == "val" {
                                let value: &str = &attribute.value;
                                if let Some(table) = table.as_mut()
                                    && let Ok(span) = value.parse::<usize>()
                                {
                                    table.pending_colspan = span.max(1);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::End(element)) => {
                let name = element.name().as_ref().to_string();
                match local(&name) {
                    "rPr" => in_run_properties = false,
                    "p" => {
                        in_paragraph = false;
                        let collected = trim_inlines(std::mem::take(&mut inlines));
                        if collected.is_empty() {
                            continue;
                        }
                        if let Some(table) = table.as_mut() {
                            if !table.current_cell.is_empty() {
                                table.current_cell.push(Inline::line_break());
                            }
                            table.current_cell.extend(collected);
                            continue;
                        }
                        let block = match heading {
                            Some(level) => Block::Heading {
                                level,
                                inlines: collected,
                                anchor: None,
                            },
                            None => Block::Paragraph {
                                inlines: collected,
                                anchor: None,
                                style: ParagraphStyle::default(),
                            },
                        };
                        if list_item {
                            push_list_item(&mut doc, block);
                        } else {
                            doc.blocks.push(block);
                        }
                    }
                    "tc" => {
                        if let Some(table) = table.as_mut() {
                            table.finish_cell(false);
                        }
                    }
                    "tr" => {
                        if let Some(table) = table.as_mut() {
                            table.finish_row();
                        }
                    }
                    "tbl" => {
                        if let Some(table) = table.take() {
                            let mut table = table.into_table();
                            if !table.rows.is_empty() {
                                table.headers.push(table.rows.remove(0));
                                if let Some(header) = table.headers.first_mut() {
                                    header.header = true;
                                    for cell in &mut header.cells {
                                        cell.header = true;
                                    }
                                }
                            }
                            doc.blocks.push(Block::Table(table));
                        }
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::Text(text)) => {
                if in_paragraph {
                    let decoded = decode_entities(text.as_ref());
                    if !decoded.is_empty() {
                        inlines.push(Inline::new(decoded, style.clone()));
                    }
                }
            }
            Ok(XmlEvent::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                warnings.push(format!("DOCX body parse stopped early: {error}"));
                break;
            }
        }
        buffer.clear();
    }
    Ok((doc, warnings))
}

fn push_list_item(doc: &mut Doc, block: Block) {
    let inlines = match &block {
        Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => inlines.clone(),
        other => vec![Inline::plain(other.plain_text())],
    };
    if let Some(Block::List { items, .. }) = doc.blocks.last_mut() {
        items.push(ListItem { inlines, depth: 0 });
        return;
    }
    doc.blocks.push(Block::List {
        ordered: false,
        items: vec![ListItem { inlines, depth: 0 }],
        anchor: None,
    });
}

fn embed_image(
    src: &[u8],
    images: &BTreeMap<String, String>,
    embed: &str,
    doc: &mut Doc,
    warnings: &mut Vec<String>,
) {
    let Some(target) = images.get(embed) else {
        warnings.push(format!(
            "DOCX drawing references unknown relationship `{embed}`"
        ));
        return;
    };
    let part = if target.starts_with('/') {
        target.trim_start_matches('/').to_string()
    } else {
        format!("word/{target}")
    };
    match read_zip_part(src, &part) {
        Ok(data) => {
            let kind = ImageKind::sniff(&data);
            let (width, height) =
                cleanup_core::doc::raster_dimensions(kind, &data).unwrap_or((0, 0));
            let index = doc.assets.len();
            let name = format!("figure-{}", index + 1);
            doc.assets.push(ImageAsset {
                name: name.clone(),
                kind,
                data,
                width_px: width,
                height_px: height,
            });
            doc.blocks.push(Block::Figure(Figure {
                asset: index,
                anchor: None,
                caption: None,
                alt: name,
                display_size: None,
            }));
        }
        Err(_) => warnings.push(format!("DOCX media part `{part}` could not be read")),
    }
}

fn heading_from_style(value: &str) -> Option<u8> {
    if value == "title" {
        return Some(1);
    }
    if let Some(rest) = value.strip_prefix("heading") {
        return rest
            .trim()
            .parse::<u8>()
            .ok()
            .map(|level| level.clamp(1, 6));
    }
    None
}

fn read_zip_part(src: &[u8], part: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(src))
        .map_err(|error| ConvertError::Failed(format!("not a valid .docx (zip): {error}")))?;
    let mut file = archive
        .by_name(part)
        .map_err(|error| ConvertError::Failed(format!("{part} missing from .docx: {error}")))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .map_err(|error| ConvertError::Failed(format!("reading {part}: {error}")))?;
    Ok(buffer)
}

fn local(name: &str) -> &str {
    match name.rfind(':') {
        Some(index) => &name[index + 1..],
        None => name,
    }
}

fn parse_relationships(xml: &[u8]) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(XmlEvent::Start(element)) | Ok(XmlEvent::Empty(element)) => {
                let is_relationship = local(element.name().as_ref()) == "Relationship";
                if !is_relationship {
                    buffer.clear();
                    continue;
                }
                let mut id = String::new();
                let mut target = String::new();
                for attribute in element.attributes().flatten() {
                    let key = local(attribute.key.as_ref());
                    let value: &str = &attribute.value;
                    if key == "Id" {
                        id = value.to_string();
                    } else if key == "Target" {
                        target = value.to_string();
                    }
                }
                if !id.is_empty() && !target.is_empty() {
                    map.insert(id, target);
                }
            }
            Ok(XmlEvent::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }
    map
}
