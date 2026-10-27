//! Convert to Markdown.

use crate::to_text::html_paragraph_texts;
use crate::xml_text::docx_paragraph_texts;
use crate::{ConvertError, ConvertReport, Result};
use cleanup_core::FormatId;
use cleanup_formats::{
    LayoutImage, LayoutLink, LayoutSpan, PdfLayout, body_font_size, cluster_lines, pdf_layout,
    table_regions,
};

/// Convert already-cleaned `src` bytes (in format `from`) into Markdown.
pub fn convert(from: FormatId, src: &[u8]) -> Result<(Vec<u8>, ConvertReport)> {
    match from {
        // Plain text is already valid CommonMark prose; Markdown passes through unchanged.
        FormatId::PlainText | FormatId::Markdown => Ok((
            src.to_vec(),
            ConvertReport {
                engine: "passthrough".into(),
                lossy: false,
                warnings: vec![],
            },
        )),
        FormatId::Docx => {
            let md = docx_paragraph_texts(src)?
                .into_iter()
                .filter(|p| !p.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
                + "\n";
            Ok((
                md.into_bytes(),
                ConvertReport {
                    engine: "docx-extract".into(),
                    lossy: true,
                    warnings: vec![
                        "extracted prose as paragraphs; headings, bold/italic, tables and images are not yet mapped to Markdown".to_string(),
                    ],
                },
            ))
        }
        FormatId::Html => {
            let md = html_paragraph_texts(src)?.join("\n\n") + "\n";
            Ok((
                md.into_bytes(),
                ConvertReport {
                    engine: "html-extract".into(),
                    lossy: true,
                    warnings: vec![
                        "extracted HTML blocks as paragraphs; inline HTML tags and styles are stripped".to_string(),
                    ],
                },
            ))
        }
        FormatId::Pdf => pdf_to_markdown(src),
        FormatId::Image => Err(ConvertError::Unsupported {
            from: "image",
            to: "markdown",
        }),
    }
}

/// Render a PDF's positioned text as Markdown: headings from font size, lists
/// from bullet/number markers, tables from aligned columns, links from link
/// annotations and embedded JPEG images as base64 data URIs.
fn pdf_to_markdown(src: &[u8]) -> Result<(Vec<u8>, ConvertReport)> {
    let layout = pdf_layout(src)
        .map_err(|e| ConvertError::Failed(format!("PDF layout extraction failed: {e}")))?;
    let mut markdown = String::new();
    for (page_index, page) in layout.pages.iter().enumerate() {
        if page_index > 0 {
            markdown.push_str("\n\n---\n\n");
        }
        let body_size = body_font_size(&page.spans);
        let lines = cluster_lines(&page.spans);
        let regions = table_regions(&lines);
        let mut images = page.images.iter().peekable();
        let mut region_index = 0usize;
        for (line_index, line) in lines.iter().enumerate() {
            while images
                .peek()
                .is_some_and(|image| image.y < line_center_y(line))
            {
                markdown.push_str(&image_markdown(images.next().unwrap()));
                markdown.push('\n');
            }
            if region_index < regions.len() && regions[region_index].start == line_index {
                let region = regions[region_index];
                markdown.push_str(&table_to_markdown(&lines[region.start..region.end]));
                markdown.push('\n');
                region_index += 1;
                continue;
            }
            markdown.push_str(&line_to_markdown(line, body_size, &page.links));
            markdown.push('\n');
        }
        while let Some(image) = images.next() {
            markdown.push_str(&image_markdown(image));
            markdown.push('\n');
        }
    }
    let mut warnings = vec![
        "converted from positioned text and drawings; the exact pixel layout of the source is approximated"
            .to_string(),
    ];
    if layout.pages.iter().any(PageLayout::has_images) {
        warnings.push("embedded JPEG images are inlined as base64 data URIs".to_string());
    }
    Ok((
        markdown.into_bytes(),
        ConvertReport {
            engine: "pdf-layout".into(),
            lossy: true,
            warnings,
        },
    ))
}

impl PageLayout {
    fn has_images(&self) -> bool {
        !self.images.is_empty()
    }
}

/// The vertical center of a line of text, used to interleave images with text.
fn line_center_y(line: &[LayoutSpan]) -> f64 {
    let mut top = f64::INFINITY;
    let mut bottom = 0.0f64;
    for span in line {
        top = top.min(span.y);
        bottom = bottom.max(span.y + span.font_size);
    }
    (top + bottom) / 2.0
}

fn table_to_markdown(rows: &[Vec<LayoutSpan>]) -> String {
    let mut centers: Vec<f64> = Vec::new();
    for row in rows {
        for span in row {
            if centers.iter().all(|c| (c - span.x).abs() > 4.0) {
                centers.push(span.x);
            }
        }
    }
    centers.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Less));
    let mut out = String::new();
    for (row_index, row) in rows.iter().enumerate() {
        out.push('|');
        for center in &centers {
            let mut cell = String::new();
            for span in row {
                if (span.x - center).abs() <= 4.0 {
                    if !cell.is_empty() {
                        cell.push(' ');
                    }
                    cell.push_str(&span.text);
                }
            }
            let cell = cell.replace('\n', " ").replace('|', "\\|");
            out.push_str(&format!(" {cell} |"));
        }
        out.push('\n');
        if row_index == 0 {
            for _ in &centers {
                out.push_str("|:---");
            }
            out.push_str("|\n");
        }
    }
    out
}

/// Render one visual line of text: headings (by font size), bullet or numbered
/// lists (by marker), links (by link annotations) and inline bold/italic style.
fn line_to_markdown(line: &[LayoutSpan], body_size: f64, links: &[LayoutLink]) -> String {
    let raw = line_spans_text(line);

    // List marker on the whole line.
    let (prefix, body) = if let Some((prefix, rest)) = list_marker(&raw) {
        (prefix, rest.to_string())
    } else {
        (String::new(), raw)
    };

    // Heading detection by font size relative to the body size.
    let mut max_size = 0.0f64;
    for span in line {
        max_size = max_size.max(span.font_size);
    }
    let any_bold = line.iter().any(|span| span.bold);
    let level = if max_size > body_size * 1.5 {
        1
    } else if max_size > body_size * 1.25 {
        2
    } else if max_size > body_size * 1.10 {
        3
    } else if any_bold && max_size > body_size * 1.03 {
        3
    } else {
        0
    };

    let mut styled = String::new();
    for (index, span) in line.iter().enumerate() {
        let mut text = span.text.trim().to_string();
        // Strip a list marker that fell on the first span.
        if index == 0 {
            if let Some((_, rest)) = list_marker(&span.text) {
                text = rest.trim().to_string();
            }
        }
        if let Some(uri) = link_uri(links, span) {
            text = format!("[{}]({})", text, uri);
        }
        let text = if span.bold && span.italic {
            format!("***{}***", text)
        } else if span.bold {
            format!("**{}**", text)
        } else if span.italic {
            format!("*{}*", text)
        } else {
            text
        };
        styled.push_str(&text);
    }
    let styled = collapse_whitespace(&styled);
    let body = if styled.is_empty() { &body } else { &styled };

    match level {
        1 => format!("# {prefix}{body}"),
        2 => format!("## {prefix}{body}"),
        3 => format!("### {prefix}{body}"),
        _ => format!("{prefix}{body}"),
    }
}

/// Join the spans of one line into raw text, collapsing whitespace.
fn line_spans_text(line: &[LayoutSpan]) -> String {
    let mut raw = String::new();
    for span in line {
        if !raw.is_empty() && !raw.ends_with(' ') && !span.text.starts_with(' ') {
            raw.push(' ');
        }
        raw.push_str(&span.text);
    }
    collapse_whitespace(&raw)
}

fn list_marker(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim_start();
    let bullet = "•·*-o▪‣";
    if let Some(first) = trimmed.chars().next() {
        if bullet.contains(first) {
            if let Some(second) = trimmed.chars().nth(1) {
                if second.is_ascii_whitespace() {
                    let rest = trimmed[first.len_utf8()..].trim_start();
                    return Some(("- ".to_string(), rest.to_string()));
                }
            }
        }
    }
    let digits_end = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits_end > 0 && digits_end < trimmed.len() {
        let rest = &trimmed[digits_end..];
        if let Some(rest) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
            let rest = rest.trim_start();
            if !rest.is_empty() {
                let number = &trimmed[..digits_end];
                return Some((format!("{number}. "), rest.to_string()));
            }
        }
    }
    None
}

fn link_uri(links: &[LayoutLink], span: &LayoutSpan) -> Option<String> {
    let cx = span.x + (span.text.len() as f64) * span.font_size * 0.5;
    let cy = span.y + span.font_size / 2.0;
    links
        .iter()
        .find(|link| link.x0 <= cx && cx <= link.x1 && link.y0 <= cy && cy <= link.y1)
        .map(|link| link.uri.clone())
}

fn image_markdown(image: &LayoutImage) -> String {
    format!(
        "![image](data:{};base64,{})",
        image.mime,
        base64_encode(&image.bytes)
    )
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len() * 4 / 3 + 4);
    for chunk in input.chunks_exact(3) {
        out.push(TABLE[(chunk[0] >> 2) as usize] as char);
        out.push(TABLE[(((chunk[0] & 3) << 4) | (chunk[1] >> 4)) as usize] as char);
        out.push(TABLE[(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6)) as usize] as char);
        out.push(TABLE[(chunk[2] & 0x3f) as usize] as char);
    }
    let rem = input.chunks_exact(3).remainder();
    match rem.len() {
        1 => {
            out.push(TABLE[(rem[0] >> 2) as usize] as char);
            out.push(TABLE[((rem[0] & 3) << 4) as usize] as char);
            out.push_str("==");
        }
        2 => {
            out.push(TABLE[(rem[0] >> 2) as usize] as char);
            out.push(TABLE[(((rem[0] & 3) << 4) | (rem[1] >> 4)) as usize] as char);
            out.push(TABLE[((rem[1] & 0x0f) << 2) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

/// Collapse runs of whitespace to a single space and trim.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::new();
    let mut previous_space = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !previous_space && !out.is_empty() {
                out.push(' ');
                previous_space = true;
            }
        } else {
            out.push(ch);
            previous_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}
