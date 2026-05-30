//! Convert to plain text.

use crate::xml_text::docx_paragraph_texts;
use crate::{ConvertError, ConvertReport, Result};
use cleanup_core::FormatId;
use pulldown_cmark::{Event, Parser, TagEnd};

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
