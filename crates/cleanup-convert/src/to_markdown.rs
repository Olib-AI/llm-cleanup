//! Convert to Markdown.

use crate::xml_text::docx_paragraph_texts;
use crate::{ConvertReport, Result};
use cleanup_core::FormatId;

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
    }
}
