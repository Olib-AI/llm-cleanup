use cleanup_core::ir::{DocMeta, Document, FormatId, ProseSpan, SpanAnchor, detect_newline};
use cleanup_core::splice::{render_byte_spliced, splice};
use cleanup_core::{ContainerFinding, CoreError, EditSet, Format, Result};

/// Plain-text adapter. The whole body (minus a leading BOM) is one prose region. The work
/// here is preserving encoding and line endings, and never introducing newlines.
pub struct PlainText;

/// True when a leading `U+FEFF` is the opening marker of a C2PA 2.x Unstructured Text
/// variation-selector manifest (`U+FEFF` + `0x43` `'C'` `U+E0133` + `0x32` `'2'` `U+E0122`),
/// rather than a genuine UTF-8 byte-order mark.
pub(crate) fn starts_with_c2pa_vs_wrapper(src: &str) -> bool {
    src.starts_with("\u{FEFF}\u{E0133}\u{E0122}")
}

impl Format for PlainText {
    fn id(&self) -> FormatId {
        FormatId::PlainText
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("text is not valid UTF-8: {e}")))?;
        let had_bom = src.starts_with('\u{FEFF}') && !starts_with_c2pa_vs_wrapper(src);
        let meta = DocMeta {
            format: FormatId::PlainText,
            newline: detect_newline(src),
            had_bom,
        };

        // Keep a genuine leading BOM out of the editable span (re-emitted verbatim by the splice),
        // while keeping a C2PA unstructured-text `U+FEFF` wrapper opener inside the span so the
        // text-manifest rule can strip the complete frame.
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

    fn container_findings(&self, doc: &Document) -> Vec<ContainerFinding> {
        let Ok(src) = std::str::from_utf8(&doc.source) else {
            return Vec::new();
        };
        crate::c2pa::find_structured_text_manifests(src)
            .into_iter()
            .filter(|r| src[r.clone()].contains('\n'))
            .map(|r| ContainerFinding {
                rule_id: "container.c2pa-manifest".to_string(),
                message: format!(
                    "Multi-line C2PA structured-text manifest block at bytes {}..{}",
                    r.start, r.end
                ),
                scrubbable: true,
                applied: false,
            })
            .collect()
    }

    fn scrub_container(&self, _doc: &Document, rendered: &[u8]) -> Option<Vec<u8>> {
        let src = std::str::from_utf8(rendered).ok()?;
        let edits: Vec<_> = crate::c2pa::find_structured_text_manifests(src)
            .into_iter()
            .filter(|r| src[r.clone()].contains('\n'))
            .map(|r| (r, String::new()))
            .collect();
        if edits.is_empty() {
            return None;
        }
        splice(rendered, &edits).ok()
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        // For plain text, "structure" is the line count: replacements may never add or
        // remove lines (the rule engine forbids newlines in replacements).
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("text is not valid UTF-8: {e}")))?;
        Ok(format!("lines={}", src.split('\n').count()))
    }
}
