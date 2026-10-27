//! Cross-format conversion for llm-cleanup.
//!
//! This is the *lossy*, "render into a new container" subsystem, deliberately walled off from
//! the lossless byte-splice clean path in `cleanup-formats`. The CLI uses **clean-then-convert**:
//! a document is first cleaned losslessly in its native format, then (if the requested output
//! format differs) handed here to be rendered into the target. Engines are pure-Rust.

pub mod to_docx;
pub mod to_markdown;
pub mod to_pdf;
pub mod to_text;

mod xml_text;

use cleanup_core::FormatId;
use cleanup_formats::LayoutSpan;

/// A run of consecutive lines that forms a table.
pub(crate) struct TableRegion {
    pub start: usize,
    pub end: usize,
}

/// Detect groups of consecutive lines that look like table rows: each line has
/// two or more horizontally separated spans spanning a meaningful width.
pub(crate) fn table_regions(lines: &[Vec<LayoutSpan>]) -> Vec<TableRegion> {
    let mut regions = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !is_table_row(&lines[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        while j < lines.len() && is_table_row(&lines[j]) {
            j += 1;
        }
        regions.push(TableRegion { start, end: j });
        i = j;
    }
    regions
}

fn is_table_row(line: &[LayoutSpan]) -> bool {
    if line.len() < 2 {
        return false;
    }
    let max_x = line.iter().map(|s| s.x).fold(f64::NEG_INFINITY, f64::max);
    let min_x = line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
    (max_x - min_x) > 15.0
}

/// A conversion target (output format). PDF is both a readable input (`FormatId::Pdf`,
/// cleaned natively in place) and an output target rendered from other formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Markdown,
    Text,
    Docx,
    Pdf,
}

impl Target {
    /// Map an output-file extension to a target format.
    pub fn from_ext(ext: &str) -> Option<Target> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "md" | "markdown" | "mdown" | "mkd" => Target::Markdown,
            "txt" | "text" => Target::Text,
            "docx" => Target::Docx,
            "pdf" => Target::Pdf,
            _ => return None,
        })
    }

    /// True if this target is the same format as `f` (so no conversion is needed).
    pub fn is_same_as(self, f: FormatId) -> bool {
        matches!(
            (self, f),
            (Target::Markdown, FormatId::Markdown)
                | (Target::Text, FormatId::PlainText)
                | (Target::Docx, FormatId::Docx)
                | (Target::Pdf, FormatId::Pdf)
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::Markdown => "markdown",
            Target::Text => "text",
            Target::Docx => "docx",
            Target::Pdf => "pdf",
        }
    }

    /// The canonical file extension for this target.
    pub fn ext(self) -> &'static str {
        match self {
            Target::Markdown => "md",
            Target::Text => "txt",
            Target::Docx => "docx",
            Target::Pdf => "pdf",
        }
    }
}

/// What a conversion did and lost.
#[derive(Debug, Default)]
pub struct ConvertReport {
    pub engine: String,
    /// True for any cross-container conversion (the byte-identity guarantee no longer holds).
    pub lossy: bool,
    /// Human-readable notes about what was degraded or dropped.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error(transparent)]
    Core(#[from] cleanup_core::CoreError),
    #[error("converting {from} to {to} is not supported yet")]
    Unsupported {
        from: &'static str,
        to: &'static str,
    },
    #[error("conversion failed: {0}")]
    Failed(String),
}

pub type Result<T> = std::result::Result<T, ConvertError>;

/// Convert already-cleaned `src` bytes (in format `from`) into the `to` format.
pub fn convert(from: FormatId, src: &[u8], to: Target) -> Result<(Vec<u8>, ConvertReport)> {
    match to {
        Target::Text => to_text::convert(from, src),
        Target::Markdown => to_markdown::convert(from, src),
        Target::Docx => to_docx::convert(from, src),
        Target::Pdf => to_pdf::convert(from, src),
    }
}

/// Markdown parser options for the converters — deliberately WITHOUT smart-punctuation, so
/// converting cleaned text never re-introduces the curly quotes / dashes we just removed.
pub(crate) fn md_options() -> pulldown_cmark::Options {
    use pulldown_cmark::Options;
    let mut o = Options::empty();
    o.insert(Options::ENABLE_TABLES);
    o.insert(Options::ENABLE_STRIKETHROUGH);
    o.insert(Options::ENABLE_FOOTNOTES);
    o.insert(Options::ENABLE_TASKLISTS);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_input_converts_to_every_output_target() {
        let sources = [
            (
                FormatId::PlainText,
                "Bonjour, français ! ¡Hola, español!".as_bytes().to_vec(),
            ),
            (
                FormatId::Markdown,
                "# Bonjour\n\n¡Hola, mundo!".as_bytes().to_vec(),
            ),
        ];
        let docx = to_docx::convert(FormatId::PlainText, b"DOCX source fixture")
            .expect("DOCX fixture")
            .0;
        let mut sources = sources.to_vec();
        sources.push((FormatId::Docx, docx));
        let pdf = to_pdf::convert(FormatId::PlainText, b"PDF source fixture")
            .expect("PDF fixture")
            .0;
        sources.push((FormatId::Pdf, pdf));
        sources.push((
            FormatId::Html,
            b"<html><body><h1>Bonjour</h1><p>\xc2\xa1Hola, mundo!</p></body></html>".to_vec(),
        ));

        for (from, bytes) in sources {
            for to in [Target::Markdown, Target::Text, Target::Docx, Target::Pdf] {
                let (converted, report) = convert(from, &bytes, to)
                    .unwrap_or_else(|error| panic!("{from:?} to {to:?}: {error}"));
                assert!(
                    !converted.is_empty(),
                    "{from:?} to {to:?} returned empty output"
                );
                match to {
                    Target::Docx => assert!(converted.starts_with(b"PK")),
                    Target::Pdf => assert!(converted.starts_with(b"%PDF-1.7")),
                    Target::Text | Target::Markdown => {
                        std::str::from_utf8(&converted).expect("text target is UTF-8");
                    }
                }
                if to.is_same_as(from) {
                    assert!(!report.lossy, "identity conversion should remain lossless");
                }
            }
        }
    }
}
