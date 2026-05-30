use std::ops::Range;
use std::sync::OnceLock;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatId {
    Markdown,
    PlainText,
    Docx,
}

impl FormatId {
    pub fn as_str(self) -> &'static str {
        match self {
            FormatId::Markdown => "markdown",
            FormatId::PlainText => "text",
            FormatId::Docx => "docx",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Newline {
    Lf,
    Crlf,
    Cr,
    Mixed,
}

/// Detect the dominant newline convention so it can be preserved (a silent CRLF->LF
/// rewrite would itself be a "style break").
pub fn detect_newline(s: &str) -> Newline {
    let crlf = s.matches("\r\n").count();
    let lf = s.matches('\n').count() - crlf; // lone \n
    let cr = s.matches('\r').count() - crlf; // lone \r
    match (crlf > 0, lf > 0, cr > 0) {
        (true, false, false) => Newline::Crlf,
        (false, true, false) => Newline::Lf,
        (false, false, true) => Newline::Cr,
        (false, false, false) => Newline::Lf, // no newline at all; default
        _ => Newline::Mixed,
    }
}

#[derive(Debug, Clone)]
pub struct DocMeta {
    pub format: FormatId,
    pub newline: Newline,
    pub had_bom: bool,
}

/// How a prose span's text maps back into the original document.
#[derive(Debug, Clone)]
pub enum SpanAnchor {
    /// A single contiguous byte range in [`Document::source`] — Markdown / plain text.
    Bytes(Range<usize>),
    /// Several disjoint byte ranges whose contents concatenate to the span text. Used for DOCX
    /// paragraphs whose text is split across multiple `<w:t>` runs: an edit is mapped back onto
    /// the runs it covers (the first run receives the replacement, later runs are trimmed), so
    /// every run keeps its own `<w:rPr>` formatting.
    Segments(Vec<Range<usize>>),
}

/// The only editable unit. Holds prose text and nothing structural — a rule that receives
/// a span cannot, by construction, see or touch a heading marker, code fence, or link URL.
#[derive(Debug)]
pub struct ProseSpan {
    pub anchor: SpanAnchor,
    pub text: String,
    sentences: OnceLock<Vec<Range<usize>>>,
}

impl ProseSpan {
    pub fn new(anchor: SpanAnchor, text: impl Into<String>) -> Self {
        ProseSpan {
            anchor,
            text: text.into(),
            sentences: OnceLock::new(),
        }
    }

    /// Absolute byte range in the source, if this span is byte-anchored.
    pub fn byte_range(&self) -> Option<Range<usize>> {
        match &self.anchor {
            SpanAnchor::Bytes(r) => Some(r.clone()),
            SpanAnchor::Segments(_) => None,
        }
    }

    /// Lazily-computed UAX-29 sentence boundaries (byte ranges within `text`).
    pub fn sentences(&self) -> &[Range<usize>] {
        self.sentences.get_or_init(|| {
            self.text
                .split_sentence_bound_indices()
                .map(|(i, s)| i..i + s.len())
                .collect()
        })
    }
}

/// A parsed document: the immutable original bytes plus the prose spans overlaid on them.
#[derive(Debug)]
pub struct Document {
    /// The bytes that prose spans are spliced into. For Markdown/text this is the file itself;
    /// for DOCX it is `word/document.xml` extracted from the zip.
    pub source: Vec<u8>,
    pub spans: Vec<ProseSpan>,
    pub meta: DocMeta,
    /// Format-specific container bytes (the original `.docx` zip), needed to re-emit the file.
    /// Empty for Markdown/plain text.
    pub aux: Vec<u8>,
}

impl Document {
    pub fn new(source: Vec<u8>, spans: Vec<ProseSpan>, meta: DocMeta) -> Self {
        Document {
            source,
            spans,
            meta,
            aux: Vec::new(),
        }
    }
}
