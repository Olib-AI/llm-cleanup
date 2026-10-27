//! Exporters: the IR is rendered into each target container.
//!
//! Every exporter is a pure function of [`cleanup_core::doc::Doc`]; none of them re-parses the
//! source. Assets (`word/media/*`, `./assets/*`, PDF image XObjects) are produced from
//! [`cleanup_core::doc::ImageAsset`], so an image extracted once is embedded everywhere.

pub mod docx;
pub mod markdown;
pub mod pdf;
pub mod text;

use cleanup_core::doc::{Doc, ImageAsset};

/// A rendered document plus the side files it needs.
#[derive(Debug, Default)]
pub struct Rendered {
    pub bytes: Vec<u8>,
    /// Side-car files, as `(file name, bytes)`. Empty for single-file formats.
    pub assets: Vec<(String, Vec<u8>)>,
    pub warnings: Vec<String>,
    pub engine: &'static str,
}

/// Inline formatting flags that all three text exporters need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RunStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub underline: bool,
    pub strike: bool,
}

impl RunStyle {
    pub(crate) fn from_inline(style: &cleanup_core::doc::InlineStyle) -> RunStyle {
        RunStyle {
            bold: style.bold,
            italic: style.italic,
            code: style.code,
            underline: style.underline,
            strike: style.strike,
        }
    }
}

/// Look up an asset by index, warning when a figure points at a missing asset.
pub(crate) fn asset_for<'a>(
    doc: &'a Doc,
    index: usize,
    warnings: &mut Vec<String>,
) -> Option<&'a ImageAsset> {
    match doc.assets.get(index) {
        Some(asset) if !asset.is_empty() => Some(asset),
        Some(_) => {
            warnings.push(format!("figure {} has no image data", index + 1));
            None
        }
        None => {
            warnings.push(format!("figure {} references a missing asset", index + 1));
            None
        }
    }
}
