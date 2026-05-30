//! Cross-format conversion for llm-cleanup.
//!
//! This is the *lossy*, "render into a new container" subsystem, deliberately walled off from
//! the lossless byte-splice clean path in `cleanup-formats`. The CLI uses **clean-then-convert**:
//! a document is first cleaned losslessly in its native format, then (if the requested output
//! format differs) handed here to be rendered into the target. Engines are pure-Rust.

pub mod to_docx;
pub mod to_markdown;
pub mod to_text;

mod xml_text;

use cleanup_core::FormatId;

/// A conversion target (output format). Unlike [`FormatId`], includes `Pdf` (write-only).
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
        Target::Pdf => Err(ConvertError::Unsupported {
            from: from.as_str(),
            to: "pdf",
        }),
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
