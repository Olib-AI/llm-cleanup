use cleanup_core::ir::{DocMeta, Document, FormatId, ProseSpan, SpanAnchor, detect_newline};
use cleanup_core::splice::render_byte_spliced;
use cleanup_core::{CoreError, EditSet, Format, Result};

/// Plain-text adapter. The whole body (minus a leading BOM) is one prose region. The work
/// here is preserving encoding and line endings, and never introducing newlines.
pub struct PlainText;

impl Format for PlainText {
    fn id(&self) -> FormatId {
        FormatId::PlainText
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("text is not valid UTF-8: {e}")))?;
        let had_bom = src.starts_with('\u{FEFF}');
        let meta = DocMeta {
            format: FormatId::PlainText,
            newline: detect_newline(src),
            had_bom,
        };

        // Keep a leading BOM out of the editable span (re-emitted verbatim by the splice).
        let start = if had_bom { '\u{FEFF}'.len_utf8() } else { 0 };
        let span = ProseSpan::new(
            SpanAnchor::Bytes(start..bytes.len()),
            src[start..].to_string(),
        );
        Ok(Document::new(bytes.to_vec(), vec![span], meta))
    }

    fn render(&self, doc: &Document, edits: &EditSet) -> Result<Vec<u8>> {
        render_byte_spliced(doc, edits)
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        // For plain text, "structure" is the line count: replacements may never add or
        // remove lines (the rule engine forbids newlines in replacements).
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("text is not valid UTF-8: {e}")))?;
        Ok(format!("lines={}", src.split('\n').count()))
    }
}
