//! Structure-aware PDF output.
//!
//! Unlike the old text-only renderer, this re-typesets the source into a real
//! document: headings, bold/italic text, lists and tables are rebuilt from the
//! source structure, and an embedded Inter font keeps the text searchable and
//! copyable. Exact pixel-level layout of the source document is not reproduced
//! (that is what same-format PDF cleaning is for).

use crate::{ConvertError, ConvertReport, Result};
use cleanup_core::FormatId;
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use std::collections::BTreeMap;

const FONT_REGULAR: &[u8] = include_bytes!("../cleanup-gui/fonts/Inter-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../cleanup-gui/fonts/Inter-Bold.ttf");

const PAGE_WIDTH: f64 = 612.0;
const PAGE_HEIGHT: f64 = 792.0;
const LEFT_MARGIN: f64 = 54.0;
const RIGHT_MARGIN: f64 = 54.0;
const TOP_MARGIN: f64 = 54.0;
const BOTTOM_MARGIN: f64 = 144.0; // keeps a full page of body text on one page
const MAX_WIDTH: f64 = PAGE_WIDTH - LEFT_MARGIN - RIGHT_MARGIN;
const LINE_WIDTH: usize = 92;
const PAGE_LINES: usize = 48;
const SPACE_WIDTH_PT: f64 = 5.52; // advance of the space glyph at 10pt

/// One run of text with a single style.
#[derive(Debug, Clone)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
}

/// One positioned piece of text on a rendered line.
#[derive(Debug, Clone)]
struct Span {
    x: f64,
    font: usize, // 0 = regular, 1 = bold
    size: f64,
    italic: bool,
    text: String,
}

/// One rendered line.
#[derive(Debug, Clone)]
struct Line {
    spans: Vec<Span>,
    size: f64,
}

/// One block-level flow produced from the source document.
#[derive(Debug, Clone)]
enum Flow {
    Paragraph(Vec<Run>),
    Heading { level: usize, runs: Vec<Run> },
    ListItem { runs: Vec<Run> },
    Table(Vec<Vec<Vec<Run>>>), // rows → cells → runs
}

/// Minimal TrueType reader: just enough to map code points to glyph ids and to
/// read advance widths. No outlines are parsed — this app never re-uses the
/// source drawing, only its text.
#[derive(Debug, Clone)]
struct TrueType {
    data: Vec<u8>,
    units_per_em: f64,
    ascent: i16,
    descent: i16,
    cap_height: i16,
    bbox: [i16; 4],
    glyph_count: u16,
    hmtx_offset: usize,
    num_hmetrics: u16,
    cmap_offset: usize,
    code_len: usize,
}

impl TrueType {
    fn new(data: &[u8]) -> Result<TrueType> {
        if data.len() < 12 {
            return Err(ConvertError::Failed("not a TrueType font".into()));
        }
        let num_tables = u16::from_be_bytes([data[4], data[5]]) as usize;
        let mut tables = Vec::new();
        for i in 0..num_tables {
            let base = 12 + i * 16;
            if base + 16 > data.len() {
                break;
            }
            let tag = [data[base], data[base + 1], data[base + 2], data[base + 3]];
            let offset =
                u32::from_be_bytes([data[base + 8], data[base + 9], data[base + 10], data[base + 11]])
                    as usize;
            tables.push((tag, offset));
        }
        let find = |tag: &[u8; 4]| -> Option<usize> {
            tables.iter().find(|(t, _)| t == tag).map(|(_, o)| *o)
        };

        let head = find(b"head").ok_or_else(|| ConvertError::Failed("no head table".into()))?;
        if head + 44 > data.len() {
            return Err(ConvertError::Failed("head table truncated".into()));
        }
        let units_per_em = u16::from_be_bytes([data[head + 18], data[head + 19]]) as f64;
        let bbox = [
            i16::from_be_bytes([data[head + 36], data[head + 37]]),
            i16::from_be_bytes([data[head + 38], data[head + 39]]),
            i16::from_be_bytes([data[head + 40], data[head + 41]]),
            i16::from_be_bytes([data[head + 42], data[head + 43]]),
        ];

        let hhea = find(b"hhea").ok_or_else(|| ConvertError::Failed("no hhea table".into()))?;
        if hhea + 36 > data.len() {
            return Err(ConvertError::Failed("hhea table truncated".into()));
        }
        let ascent = i16::from_be_bytes([data[hhea + 4], data[hhea + 5]]);
        let descent = i16::from_be_bytes([data[hhea + 6], data[hhea + 7]]);
        let num_hmetrics = u16::from_be_bytes([data[hhea + 34], data[hhea + 35]]);

        let cap_height = find(b"OS/2")
            .filter(|&o| o + 70 <= data.len() && data[o] <= 5)
            .map(|o| i16::from_be_bytes([data[o + 68], data[o + 69]]))
            .unwrap_or(ascent);

        let maxp = find(b"maxp").ok_or_else(|| ConvertError::Failed("no maxp table".into()))?;
        if maxp + 6 > data.len() {
            return Err(ConvertError::Failed("maxp table truncated".into()));
        }
        let glyph_count = u16::from_be_bytes([data[maxp + 4], data[maxp + 5]]);

        let hmtx = find(b"hmtx").ok_or_else(|| ConvertError::Failed("no hmtx table".into()))?;
        let cmap = find(b"cmap").ok_or_else(|| ConvertError::Failed("no cmap table".into()))?;

        let code_len = if glyph_count <= 256 { 1 } else { 2 };

        Ok(TrueType {
            data: data.to_vec(),
            units_per_em: units_per_em.max(1.0),
            ascent,
            descent,
            cap_height,
            bbox,
            glyph_count,
            hmtx_offset: hmtx,
            num_hmetrics,
            cmap_offset: cmap,
            code_len,
        })
    }

    /// Advance width of a glyph in font units.
    fn width(&self, glyph: u16) -> Result<u16> {
        let last = self.num_hmetrics.saturating_sub(1) as usize;
        let index = if glyph as usize < self.num_hmetrics as usize {
            glyph as usize
        } else {
            last
        };
        let base = self.hmtx_offset + index * 2;
        if base + 2 > self.data.len() {
            return Err(ConvertError::Failed("hmtx table truncated".into()));
        }
        Ok(u16::from_be_bytes([self.data[base], self.data[base + 1]]))
    }

    /// Map a Unicode code point to a glyph id via the cmap (format 4 subtable).
    fn glyph_index(&self, ch: u32) -> Option<u16> {
        let subtable = self.cmap_offset + self.select_cmap_subtable()?;
        let data = &self.data;
        let get_u16 = |offset: usize| -> Option<u16> {
            Some(u16::from_be_bytes([
                *data.get(offset)?,
                *data.get(offset + 1)?,
            ]))
        };
        let get_i16 = |offset: usize| -> Option<i16> {
            Some(i16::from_be_bytes([
                *data.get(offset)?,
                *data.get(offset + 1)?,
            ]))
        };
        if subtable + 6 > data.len() {
            return None;
        }
        let format = get_u16(subtable)?;
        if format != 4 {
            return None;
        }
        let seg_count = (get_u16(subtable + 6)? as usize) / 2;
        let end_base = subtable + 14;
        let start_base = end_base + 2 * seg_count + 2;
        let delta_base = start_base + 2 * seg_count;
        let offset_base = delta_base + 2 * seg_count;
        for i in 0..seg_count {
            let end = u32::from(get_u16(end_base + 2 * i)?);
            let start = u32::from(get_u16(start_base + 2 * i)?);
            if ch < start || ch > end {
                continue;
            }
            let delta = get_i16(delta_base + 2 * i)?;
            let id_offset = get_u16(offset_base + 2 * i)?;
            let code = i32::try_from(ch).unwrap_or(0);
            if id_offset == 0 {
                return Some(u16::try_from((code + delta as i32) & 0xFFFF).unwrap_or(0));
            }
            let field_addr = offset_base + 2 * i;
            let entry_addr = field_addr + id_offset as usize + 2 * (ch - start) as usize;
            let glyph = get_u16(entry_addr)?;
            if glyph == 0 {
                return Some(0);
            }
            return Some(u16::try_from((glyph as i32 + delta as i32) & 0xFFFF).unwrap_or(0));
        }
        None
    }

    /// Pick the best cmap subtable: Windows Unicode (3,1), then Unicode (0,0).
    fn select_cmap_subtable(&self) -> Option<usize> {
        let data = &self.data;
        let base = self.cmap_offset;
        let num = u16::from_be_bytes([data[base + 2], data[base + 3]]) as usize;
        let mut best: Option<(u8, usize)> = None;
        let mut offset = 4;
        for _ in 0..num {
            if offset + 8 > data.len() {
                break;
            }
            let platform = u16::from_be_bytes([data[base + offset], data[base + offset + 1]]);
            let encoding =
                u16::from_be_bytes([data[base + offset + 2], data[base + offset + 3]]);
            let sel = u32::from_be_bytes([
                data[base + offset + 4],
                data[base + offset + 5],
                data[base + offset + 6],
                data[base + offset + 7],
            ]) as usize;
            offset += 8;
            let priority = match (platform, encoding) {
                (3, 1) => 0,
                (0, 0) => 1,
                _ => 2,
            };
            let replace = match best {
                None => true,
                Some((p, _)) => priority < p,
            };
            if replace {
                best = Some((priority, sel));
            }
        }
        best.map(|(_, sel)| sel)
    }

    fn scaled(&self, units: i16) -> f64 {
        (units as f64) * 1000.0 / self.units_per_em
    }
}

pub fn convert(from: FormatId, src: &[u8]) -> Result<(Vec<u8>, ConvertReport)> {
    if from == FormatId::Pdf {
        // A PDF cleaned natively is already a PDF: never flatten it through the
        // text renderer below, which would destroy its layout and images.
        return Ok((
            src.to_vec(),
            ConvertReport {
                engine: "passthrough".into(),
                lossy: false,
                warnings: vec![],
            },
        ));
    }

    let regular = TrueType::new(FONT_REGULAR)?;
    let bold = TrueType::new(FONT_BOLD)?;
    let fonts = [regular, bold];

    let flows: Vec<Flow> = match from {
        FormatId::PlainText => {
            let text = std::str::from_utf8(src)
                .map_err(|e| ConvertError::Failed(format!("not UTF-8: {e}")))?;
            let mut flows = Vec::new();

            for line in wrap_lines(text) {
                if line.is_empty() {
                    continue;
                }
                flows.push(Flow::Paragraph(vec![Run {
                    text: line,
                    bold: false,
                    italic: false,
                }]));
            }
            flows
        }
        FormatId::Markdown => markdown_flows(src)?,
        FormatId::Docx => {
            let mut flows = Vec::new();
            for paragraph in crate::xml_text::docx_paragraph_texts(src)? {
                if paragraph.trim().is_empty() {
                    continue;
                }
                flows.push(Flow::Paragraph(vec![Run {
                    text: paragraph,
                    bold: false,
                    italic: false,
                }]));
            }
            flows
        }
        FormatId::Html => {
            let mut flows = Vec::new();
            for paragraph in crate::to_text::html_paragraph_texts(src)? {
                if paragraph.trim().is_empty() {
                    continue;
                }
                flows.push(Flow::Paragraph(vec![Run {
                    text: paragraph,
                    bold: false,
                    italic: false,
                }]));
            }
            flows
        }
        FormatId::Image => {
            return Err(ConvertError::Unsupported {
                from: "image",
                to: "pdf",
            });
        }
        FormatId::Pdf => unreachable!(),
    };

    let pdf = render_pdf(flows, &fonts)?;
    Ok((
        pdf,
        ConvertReport {
            engine: "embedded-inter-pdf".into(),
            lossy: true,
            warnings: vec!["PDF output is text-only: images and complex vector graphics are not preserved; headings, lists, bold text and tables are rebuilt from the source structure".to_string()],
        },
    ))
}

/// Split text on hard newlines, flagging the breaks.
fn split_breaks(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for (index, part) in text.split('\n').enumerate() {
        if index > 0 {
            out.push((String::new(), true));
        }
        if !part.is_empty() {
            out.push((part.to_string(), false));
        }
    }
    out
}

/// The advance width of `text`, in font units (1/1000ths of an em).
fn measure_units(fonts: &[TrueType; 2], font_index: usize, text: &str) -> f64 {
    let mut width = 0.0;
    for ch in text.chars() {
        let glyph = fonts[font_index]
            .glyph_index(ch as u32)
            .filter(|g| *g != 0)
            .unwrap_or(0);
        width += fonts[font_index].width(glyph).unwrap_or(600) as f64;
    }
    width
}

/// Turn one block-level flow into rendered lines.
fn flow_to_lines(flow: &Flow, fonts: &[TrueType; 2]) -> Vec<Line> {
    match flow {
        Flow::Paragraph(runs) => wrapped_lines(runs, 10.0, fonts, LEFT_MARGIN),
        Flow::Heading { level, runs } => {
            let size = match level {
                1 => 24.0,
                2 => 20.0,
                3 => 17.0,
                4 => 14.0,
                5 => 12.0,
                _ => 10.5,
            };
            wrapped_lines(runs, size, fonts, LEFT_MARGIN)
        }
        Flow::ListItem { runs } => {
            let mut item_runs = vec![Run {
                text: "• ".to_string(),
                bold: false,
                italic: false,
            }];
            item_runs.extend(runs.iter().cloned());
            wrapped_lines(&item_runs, 10.0, fonts, LEFT_MARGIN + 18.0),

        }
        Flow::Table(rows) => {
            let size = 9.0;
            let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(1);
            let mut col_widths = vec![0.0f64; col_count];
            for row in rows {
                for (cell_index, cell) in row.iter().enumerate() {
                    let width = cell_text_width(cell, size, fonts);
                    if width > col_widths[cell_index] {
                        col_widths[cell_index] = width;
                    }
                }
            }
            let total_width: f64 = col_widths.iter().sum::<f64>() + 6.0 * col_count as f64;
            let scale = (total_width / MAX_WIDTH).min(1.0);
            let mut x = LEFT_MARGIN;
            let mut lines = Vec::new();
            for (row_index, row) in rows.iter().enumerate() {
                let mut spans = Vec::new();
                for (cell_index, cell) in row.iter().enumerate() {
                    let mut text = String::new();
                    for run in cell {
                        text.push_str(&run.text);
                    }
                    if !text.is_empty() {
                        spans.push(Span {
                            x: x + 3.0,
                            font: if row_index == 0 { 1 } else { 0 },
                            size,
                            italic: false,
                            text,
                        });
                    }
                    x += col_widths[cell_index] / scale + 6.0;
                }
                lines.push(Line { spans, size });
            }
            lines
        }
    }
}

fn cell_text_width(cell: &[Run], size: f64, fonts: &[TrueType; 2]) -> f64 {
    let mut width = 0.0;
    for run in cell {
        let font_index = if run.bold { 1 } else { 0 };
        width += measure_units(fonts, font_index, &run.text) * size / 1000.0;
        width += SPACE_WIDTH_PT;
    }
    width
}

/// Wrap runs into lines measuring with the real font metrics.
fn wrapped_lines(
    runs: &[Run],
    size: f64,
    fonts: &[TrueType; 2],
    x_start: f64,
) -> Vec<Line> {
    let max_units = MAX_WIDTH * 1000.0 / size;
    let space_units = SPACE_WIDTH_PT * 1000.0 / size;
    let mut lines: Vec<Line> = Vec::new();
    let mut cur: Vec<(usize, bool, String)> = Vec::new();
    let mut cur_units = 0.0;

    let mut flush = |cur: &mut Vec<(usize, bool, String)>,
                     cur_units: &mut f64,
                     lines: &mut Vec<Line>,
                     x_start: f64,
                     size: f64,
                     fonts: &[TrueType; 2]| {
        if cur.is_empty() {
            return;
        }
        let mut spans = Vec::new();
        let mut x = x_start;
        for (font, italic, word) in cur.drain(..) {
            spans.push(Span {
                x,
                font,
                size,
                italic,
                text: word,
            });
            x += measure_units(fonts, font, &word) * size / 1000.0 + SPACE_WIDTH_PT;
        }
        *cur_units = 0.0;
        lines.push(Line { spans, size });
    };

    for run in runs {
        let font = if run.bold { 1 } else { 0 };
        for (word, forced) in split_breaks(&run.text) {
            if forced {
                flush(&mut cur, &mut cur_units, &mut lines, x_start, size, fonts);
                continue;
            }
            let word_units = measure_units(fonts, font, &word);
            let needed = if cur.is_empty() {
                word_units
            } else {
                cur_units + space_units + word_units
            };

            if needed > max_units {
                flush(&mut cur, &mut cur_units, &mut lines, x_start, size, fonts);
                cur.push((font, run.italic, word));
                cur_units = word_units;
            } else {
                if !cur.is_empty() {
                    cur_units += space_units;
                }
                cur.push((font, run.italic, word));
                cur_units += word_units;
            }
        }
    }
    flush(&mut cur, &mut cur_units, &mut lines, x_start, size, fonts);
    lines
}

/// Turn flows into pages of lines, paginating by measured height.
fn paginate(flows: Vec<Flow>, fonts: &[TrueType; 2]) -> Vec<Vec<Line>> {
    let mut lines: Vec<Line> = Vec::new();
    for flow in flows {
        let mut flow_lines = flow_to_lines(&flow, fonts);
        if flow_lines.is_empty() {
            continue;
        }
        lines.append(&mut flow_lines);
        lines.push(Line {
            spans: Vec::new(),
            size: 10.0,
        });
    }
    if lines.last().map_or(true, |l| l.spans.is_empty()) {
        lines.pop();
    }

    let mut pages: Vec<Vec<Line>> = Vec::new();
    let mut current: Vec<Line> = Vec::new();
    let mut y = PAGE_HEIGHT - TOP_MARGIN;
    for line in lines {
        let height = if line.spans.is_empty() {
            12.0
        } else {
            line.spans
                .iter()
                .map(|s| s.size)
                .fold(0.0f64, f64::max)
                * 1.25
        };
        if y - height < BOTTOM_MARGIN && !current.is_empty() {
            pages.push(current);
            current = Vec::new();
            y = PAGE_HEIGHT - TOP_MARGIN;
        }
        current.push(line);
        y -= height;
    }
    if !current.is_empty() {
        pages.push(current);
    }
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    pages
}

fn render_pdf(flows: Vec<Flow>, fonts: &[TrueType; 2]) -> Result<Vec<u8>> {
    let pages = paginate(flows, fonts);
    let mut unicode_maps = [BTreeMap::new(), BTreeMap::new()];

    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(Vec::new()); // object 0 is unused
    fn push_object(objects: &mut Vec<Vec<u8>>, body: Vec<u8>) -> u32 {
        objects.push(body);
        (objects.len() - 1) as u32
    }

    // Per font the objects are created consecutively: font dict, CID font,
    // font descriptor, font file, ToUnicode CMap.
    let mut font_ids = [0u32, 0u32];
    for (index, font) in fonts.iter().enumerate() {
        let widths = (0..font.glyph_count)
            .map(|glyph| font.width(glyph).map(|w| w.to_string()))
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        let font_dict_id = push_object(
            &mut objects,
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /F{} /Encoding /Identity-H /DescendantFonts [{cid} 0 R] /ToUnicode {tounicode} 0 R >>",
                index + 1,
                cid = font_dict_id + 1,
                tounicode = font_dict_id + 4,
            )
            .into_bytes(),
        );
        let _cid = push_object(
            &mut objects,
            format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /F{} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {desc} 0 R /DW 600 /W [0 [{widths}]] /CIDToGIDMap /Identity >>",
                index + 1,
                desc = font_dict_id + 2,
            )
            .into_bytes(),
        );
        let _desc = push_object(
            &mut objects,
            format!(
                "<< /Type /FontDescriptor /FontName /F{} /Flags 32 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} /CapHeight {} /StemV 80 /FontFile2 {file} 0 R >>",
                index + 1,
                font.scaled(font.bbox[0]),
                font.scaled(font.bbox[1]),
                font.scaled(font.bbox[2]),
                font.scaled(font.bbox[3]),
                font.scaled(font.ascent),
                font.scaled(font.descent),
                font.scaled(font.cap_height),
                file = font_dict_id + 3,
            )
            .into_bytes(),
        );
        push_object(
            &mut objects,
            stream_object(
                &format!("/Length {} /Length1 {}", font.data.len(), font.data.len()),
                &font.data,
            ),
        ); // font_dict_id + 3
        push_object(&mut objects, Vec::new()); // font_dict_id + 4, ToUnicode placeholder
        font_ids[index] = font_dict_id;
    }

    // One content stream placeholder + one page object per page.
    let mut page_refs: Vec<(u32, u32)> = Vec::new();
    for _ in &pages {
        let content_id = push_object(&mut objects, Vec::new()); // placeholder
        let page_id = push_object(&mut objects, Vec::new());    // placeholder
        page_refs.push((page_id, content_id));
    }

    // Fill content streams.
    for (index, page_lines) in pages.iter().enumerate() {
        let (_page_id, content_id) = page_refs[index];
        let mut content = "BT\n".to_string();
        let mut y = PAGE_HEIGHT - TOP_MARGIN;
        for line in page_lines {
            let height = if line.spans.is_empty() {
                12.0
            } else {
                line.spans
                    .iter()
                    .map(|s| s.size)
                    .fold(0.0f64, f64::max)
                    * 1.25
            };
            y -= height;
            if line.spans.is_empty() {
                continue;
            }
            for span in &line.spans {
                let font = &fonts[span.font];
                let mut encoded = Vec::new();
                for ch in span.text.chars() {
                    let glyph = font
                        .glyph_index(ch as u32)
                        .filter(|g| *g != 0)
                        .unwrap_or(0);
                    unicode_maps[span.font].entry(glyph).or_insert(ch);
                    for shift in (0..font.code_len).rev() {
                        encoded.push(u8::try_from((glyph >> (shift * 8)) & 0xFF).unwrap_or(0));
                    }
                }
                let matrix = if span.italic {
                    format!("1 0 0.25 1 {:.2} {:.2} Tm", span.x, y)
                } else {
                    format!("1 0 0 1 {:.2} {:.2} Tm", span.x, y)
                };
                content.push_str(&format!("/F{} {:.2} Tf {} ", span.font + 1, span.size, matrix));
                content.push('<');
                for byte in &encoded {
                    content.push_str(&format!("{byte:02X}"));
                }
                content.push_str("> Tj\n");
            }
        }
        content.push_str("ET\n");
        let content_bytes = stream_object(&format!("/Length {}", content.len()), content.as_bytes());
        objects[content_id as usize] = content_bytes;
    }

    // Page objects, then the pages tree, then the catalog.
    let kids: String = page_refs
        .iter()
        .map(|(page_id, _)| format!("{page_id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    let pages_id = push_object(
        &mut objects,
        format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).into_bytes(),
    );
    for (page_id, content_id) in &page_refs {
        objects[*page_id as usize] = format!(
            "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {PAGE_WIDTH} {PAGE_HEIGHT}] /Resources << /Font << /F1 {} 0 R /F2 {} 0 R >> >> /Contents {content_id} 0 R >>",
            font_ids[0], font_ids[1],
        )
        .into_bytes();
    }

    // Fill the ToUnicode placeholder objects (font_dict_id + 4 for each font).
    for (index, &font_dict_id) in font_ids.iter().enumerate() {
        let tounicode_id = font_dict_id + 4;
        let cmap = to_unicode_cmap(&unicode_maps[index]);
        objects[tounicode_id as usize] =
            stream_object(&format!("/Length {}", cmap.len()), &cmap);
    }

    let catalog_id = push_object(
        &mut objects,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").into_bytes(),
    );

    let mut output = b"%PDF-1.7\n%\xe2\xe3\cf\xd3\n".to_vec();
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
            "trailer\n<< /Size {} /Root {catalog_id} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len(),
        )
        .as_bytes(),
    );
    Ok(output)
}

fn stream_object(dictionary: &str, stream: &[u8]) -> Vec<u8> {
    let mut object = format!("<< {dictionary} >>\nstream\n").into_bytes();
    object.extend_from_slice(stream);
    object.extend_from_slice(b"\nendstream");
    object
}

fn to_unicode_cmap(mappings: &BTreeMap<u16, u32>) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /InterToUnicode def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in mappings.iter().collect::<Vec<_>>().chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (glyph, codepoint) in chunk {
            let utf16: Vec<u16> = char::from_u32(**codepoint)
                .unwrap_or('\u{FFFD}')
                .encode_utf16(&mut [0; 2])
                .collect();
            cmap.push_str(&format!("<{:04X}> <", glyph));
            for unit in utf16 {
                cmap.push_str(&format!("{unit:04X}"));
            }
            cmap.push_str(">\n");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

/// Wrap one paragraph of text into lines of at most `LINE_WIDTH` characters.
fn wrap_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.lines() {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > LINE_WIDTH {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            let mut chars = word.chars().peekable();
            while chars.peek().is_some() {
                if line.chars().count() >= LINE_WIDTH {
                    out.push(std::mem::take(&mut line));
                }
                line.push(chars.next().expect("peeked character"));
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

fn markdown_flows(src: &[u8]) -> Result<Vec<Flow>> {
    let s = std::str::from_utf8(src).map_err(|e| ConvertError::Failed(format!("not UTF-8: {e}")))?;
    let mut flows: Vec<Flow> = Vec::new();
    let mut current: Option<Vec<Run>> = None;
    let mut bold = false;
    let mut italic = false;
    let mut heading_level: Option<usize> = None;
    let mut heading_runs: Vec<Run> = Vec::new();
    let mut item_runs: Option<Vec<Run>> = None;
    let mut table_rows: Vec<Vec<Vec<Run>>> = Vec::new();
    let mut current_row: Option<Vec<Vec<Run>>> = None;
    let mut current_cell: Option<Vec<Run>> = None;

    let push_paragraph = |current: &mut Option<Vec<Run>>, flows: &mut Vec<Flow>| {
        if let Some(runs) = current.take() {
            if !runs.is_empty() {
                flows.push(Flow::Paragraph(runs));
            }
        }
    };

    for ev in Parser::new_ext(s, crate::md_options()) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                push_paragraph(&mut current, &mut flows);
                heading_level = Some(heading_num(level));
                heading_runs = Vec::new();
            }
            Event::End(TagEnd::Heading(_)) => {
                flows.push(Flow::Heading {
                    level: heading_level.take().unwrap_or(1),
                    runs: std::mem::take(&mut heading_runs),
                });
            }
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::List(_)) => {}
            Event::End(TagEnd::List(_)) => {}
            Event::Start(Tag::Item) => {
                item_runs = Some(vec![Run {
                    text: "• ".to_string(),
                    bold: false,
                    italic: false,
                }]);
            }
            Event::End(TagEnd::Item) => {
                if let Some(runs) = item_runs.take() {
                    if !runs.is_empty() {
                        flows.push(Flow::ListItem { runs });
                    }
                }
            }
            Event::Start(Tag::Table(_)) => {
                table_rows = Vec::new();
                current_row = Some(Vec::new());
                current_cell = Some(Vec::new());
            }
            Event::End(TagEnd::Table) => {
                flows.push(Flow::Table(std::mem::take(&mut table_rows)));
                current_row = None;
                current_cell = None;
            }
            Event::Start(Tag::TableRow) => {
                current_row = Some(Vec::new());
                current_cell = Some(Vec::new());
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(cell) = current_cell.take() {
                    if let Some(row) = current_row.as_mut() {
                        row.push(cell);
                    }
                }
                if let Some(row) = current_row.take() {
                    table_rows.push(row);
                }
            }
            Event::Start(Tag::TableCell) => {
                current_cell = Some(Vec::new());
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(cell) = current_cell.take() {
                    if let Some(row) = current_row.as_mut() {
                        row.push(cell);
                    }
                }
            }
            Event::Start(Tag::Link { .. }) | Event::Start(Tag::Image { .. }) => {}
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {}
            Event::Text(t) => {
                let run = Run {
                    text: t.to_string(),
                    bold,
                    italic,
                };
                if heading_level.is_some() {
                    heading_runs.push(run);
                } else if item_runs.is_some() {
                    item_runs.as_mut().unwrap().push(run);
                } else if current_cell.is_some() {
                    current_cell.as_mut().unwrap().push(run);
                } else {
                    current.get_or_insert_with(Vec::new).push(run);
                }
            }
            Event::Code(t) => {
                let run = Run {
                    text: t.to_string(),
                    bold,
                    italic,
                };
                if heading_level.is_some() {
                    heading_runs.push(run);
                } else if item_runs.is_some() {
                    item_runs.as_mut().unwrap().push(run);
                } else if current_cell.is_some() {
                    current_cell.as_mut().unwrap().push(run);
                } else {
                    current.get_or_insert_with(Vec::new).push(run);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                add_run(" ", bold, italic, false);
            }
            _ => {}
        }
    }
    push_paragraph(&mut current, &mut flows);
    Ok(flows)
}

fn heading_num(l: HeadingLevel) -> usize {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_pdf_embeds_font_and_text_mapping() {
        let (pdf, report) = convert(
            FormatId::PlainText,
            "Bonjour, français !\n¡Hola, español!\n日本語".as_bytes(),
        )
        .expect("PDF should be generated");
        let pdf = String::from_utf8_lossy(&pdf);
        assert!(pdf.starts_with("%PDF-1.7"));
        assert!(pdf.contains("/FontFile2"));
        assert!(pdf.contains("/ToUnicode"));
        assert!(pdf.contains("beginbfchar"));
        assert!(report.lossy);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("text-only"))
        );
    }

    #[test]
    fn wraps_long_lines_and_creates_multiple_pages() {
        let words = 19 * (PAGE_LINES + 1);
        let text = format!("{}\n", "word ".repeat(words));
        let (pdf, _) = convert(FormatId::PlainText, text.as_bytes()).expect("PDF generation");
        let pdf = String::from_utf8_lossy(&pdf);
        assert!(pdf.contains("/Count 2"));
        assert!(pdf.contains("/MediaBox [0 0 612 792]"));
    }

    #[test]
    fn markdown_headings_lists_and_tables_render() {
        let source = "# Title\n\n## Section\n\n- first\n- second\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
        let (pdf, _) = convert(FormatId::Markdown, source.as_bytes()).expect("PDF generation");
        let pdf = String::from_utf8_lossy(&pdf);
        assert!(pdf.starts_with("%PDF-1.7"));

        // Round-trip through our own PDF parser to prove the text survived.
        let parsed = cleanup_formats::Pdf.parse(pdf.as_bytes()).expect("output parses");
        let texts: Vec<String> = parsed.spans.iter().map(|s| s.text.clone()).collect();
        let joined = texts.join(" ");
        assert!(joined.contains("Title"), "{joined:?}");
        assert!(joined.contains("Section"), "{joined:?}");
        assert!(joined.contains("first"), "{joined:?}");
        assert!(joined.contains("second"), "{joined:?}");
        assert!(joined.contains("A"), "{joined:?}");
        assert!(joined.contains("1"), "{joined:?}");
    }
}
