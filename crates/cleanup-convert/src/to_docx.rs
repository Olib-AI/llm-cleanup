//! Convert to DOCX (pure-Rust, via `docx-rs`). Maps headings, bold/italic, paragraphs and
//! list items; tables and images are not yet emitted.

use crate::{ConvertError, ConvertReport, Result};
use cleanup_core::FormatId;
use docx_rs::{Docx, Paragraph, Run};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use std::io::Cursor;

pub fn convert(from: FormatId, src: &[u8]) -> Result<(Vec<u8>, ConvertReport)> {
    if from == FormatId::Docx {
        return Ok((
            src.to_vec(),
            ConvertReport {
                engine: "passthrough".into(),
                lossy: false,
                warnings: vec![],
            },
        ));
    }

    let (docx, warnings) = match from {
        FormatId::PlainText => (text_to_docx(src), Vec::new()),
        FormatId::Markdown => markdown_to_docx(src)?,
        FormatId::Docx => unreachable!(),
    };

    let mut cursor = Cursor::new(Vec::new());
    docx.build()
        .pack(&mut cursor)
        .map_err(|e| ConvertError::Failed(format!("writing .docx: {e}")))?;
    Ok((
        cursor.into_inner(),
        ConvertReport {
            engine: "docx-rs".into(),
            lossy: true,
            warnings,
        },
    ))
}

fn text_to_docx(src: &[u8]) -> Docx {
    let s = String::from_utf8_lossy(src);
    let mut docx = Docx::new();
    for line in s.lines() {
        docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_text(line)));
    }
    docx
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

fn markdown_to_docx(src: &[u8]) -> Result<(Docx, Vec<String>)> {
    let s =
        std::str::from_utf8(src).map_err(|e| ConvertError::Failed(format!("not UTF-8: {e}")))?;
    let mut docx = Docx::new();
    let mut warnings: Vec<String> = Vec::new();

    let mut para = Paragraph::new();
    let mut has_content = false;
    let mut heading: Option<usize> = None;
    let mut bold = false;
    let mut italic = false;
    let mut list_depth = 0usize;
    let mut item_open = false;

    for ev in Parser::new_ext(s, crate::md_options()) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => heading = Some(heading_num(level)),
            Event::End(TagEnd::Heading(_)) => {
                if has_content {
                    if let Some(h) = heading {
                        para = para.style(&format!("Heading{h}"));
                    }
                    docx = docx.add_paragraph(std::mem::take(&mut para));
                }
                para = Paragraph::new();
                has_content = false;
                heading = None;
            }
            Event::End(TagEnd::Paragraph) | Event::End(TagEnd::CodeBlock) => {
                if has_content {
                    docx = docx.add_paragraph(std::mem::take(&mut para));
                }
                para = Paragraph::new();
                has_content = false;
            }
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::Start(Tag::Item) => item_open = true,
            Event::End(TagEnd::Item) => {
                if has_content {
                    docx = docx.add_paragraph(std::mem::take(&mut para));
                }
                para = Paragraph::new();
                has_content = false;
                item_open = false;
            }
            Event::Text(t) | Event::Code(t) => {
                let text = if item_open && !has_content {
                    item_open = false;
                    format!("{}• {t}", "    ".repeat(list_depth.saturating_sub(1)))
                } else {
                    t.to_string()
                };
                let mut run = Run::new().add_text(text);
                if bold {
                    run = run.bold();
                }
                if italic {
                    run = run.italic();
                }
                para = para.add_run(run);
                has_content = true;
            }
            Event::SoftBreak | Event::HardBreak => {
                para = para.add_run(Run::new().add_text(" "));
                has_content = true;
            }
            Event::Start(Tag::Table(_)) => {
                warnings.push("tables are dropped in Markdown→DOCX".into())
            }
            Event::Start(Tag::Image { .. }) => {
                warnings.push("images are dropped in Markdown→DOCX".into())
            }
            _ => {}
        }
    }
    if has_content {
        docx = docx.add_paragraph(para);
    }
    Ok((docx, warnings))
}
