use crate::{Docx, Markdown, PlainText};
use cleanup_core::{CoreError, Format, FormatId, Result};
use std::path::Path;

/// Determine the format from a file extension.
pub fn format_for_path(path: &Path) -> Option<FormatId> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "md" | "markdown" | "mdown" | "mkd" => FormatId::Markdown,
        "txt" | "text" => FormatId::PlainText,
        "docx" => FormatId::Docx,
        _ => return None,
    })
}

/// Build the right format adapter for a path, or error if the extension is unsupported.
pub fn formatter_for_path(path: &Path) -> Result<Box<dyn Format>> {
    match format_for_path(path) {
        Some(FormatId::Markdown) => Ok(Box::new(Markdown)),
        Some(FormatId::PlainText) => Ok(Box::new(PlainText)),
        Some(FormatId::Docx) => Ok(Box::new(Docx)),
        None => Err(CoreError::UnsupportedFormat(path.display().to_string())),
    }
}
