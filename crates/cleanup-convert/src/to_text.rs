//! Convert to plain text.

use crate::xml_text::docx_paragraph_texts;
use crate::{ConvertError, ConvertReport, Result};
use cleanup_core::{Format, FormatId};
use cleanup_formats::pdf_layout;
use pulldown_cmark::{Event, Parser, TagEnd};

pub(crate) fn html_paragraph_texts(src: &[u8]) -> Result<Vec<String>> {
    let doc = cleanup_formats::Html.parse(src)?;
    Ok(doc
        .spans
        .into_iter()
        .map(|s| s.text.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

pub fn convert(from: FormatId, src: &[u8]) -> Result<(Vec<u8>, ConvertReport)> {
    let (text, engine, lossy) = match from {
        FormatId::PlainText => (
            String::from_utf8_lossy(src).into_owned(),
            "passthrough",
            false,
        ),
        FormatId::Markdown => (markdown_to_text(src)?, "markdown-strip", true),
        FormatId::Docx => (
            docx_paragraph_texts(src)?.join("\n\n").trim().to_string() + "\n",
            "docx-extract",
            true,
        ),
        FormatId::Html => (
            html_paragraph_texts(src)?.join("\n\n").trim().to_string() + "\n",
            "html-extract",
            true,
        ),
        FormatId::Pdf => {
            let layout = pdf_layout(src)
                .map_err(|e| ConvertError::Failed(format!("PDF layout extraction failed: {e}")))?;
            let mut parts: Vec<String> = Vec::new();
            for page in layout.pages {
                let mut previous_bottom: Option<f64> = None;
                let mut page_lines: Vec<String> = Vec::new();
                for line in cleanup_formats::cluster_lines(&page.spans) {
                    let text = line
                        .iter()
                        .map(|span| span.text.trim())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    // A vertical gap larger than half the line height starts a
                    // new paragraph.
                    let mut bottom = 0.0f64;
                    let mut top = f64::INFINITY;
                    let mut size = 0.0f64;
                    for span in line {
                        bottom = bottom.max(span.y + span.font_size);
                        top = top.min(span.y);
                        size = size.max(span.font_size);
                    }
                    size = size.max(10.0);
                    if let Some(previous) = previous_bottom {
                        if top - previous > size * 0.55 {
                            page_lines.push(String::new());
                        }
                    }
                    page_lines.push(text.to_string());
                    previous_bottom = Some(bottom);
                }
                if !page_lines.is_empty() {
                    parts.push(page_lines.join("\n"));
                }
            }
            (
                parts.join("\n\n").trim().to_string() + "\n",
                "pdf-extract",
                true,
            )
        }
        FormatId::Image => {
            return Err(ConvertError::Unsupported {
                from: "image",
                to: "text",
            });
        }
    };
    let warnings = if lossy {
        vec!["all formatting and structure removed (plain-text output)".to_string()]
    } else {
        vec![]
    };
    Ok((
        text.into_bytes(),
        ConvertReport {
            engine: engine.to_string(),
            lossy,
            warnings,
        },
    ))
}

fn markdown_to_text(src: &[u8]) -> Result<String> {
    let s =
        std::str::from_utf8(src).map_err(|e| ConvertError::Failed(format!("not UTF-8: {e}")))?;
    let mut out = String::new();
    for ev in Parser::new_ext(s, crate::md_options()) {
        match ev {
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => out.push('\n'),
            _ => {}
        }
    }
    Ok(out.trim().to_string() + "\n")
}
