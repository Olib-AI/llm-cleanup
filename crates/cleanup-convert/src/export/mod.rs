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

/// Inline formatting flags that all text exporters need.
///
/// `color` and `link` are part of the run, not the block: a coloured or hyperlinked phrase must
/// survive a split across source runs. `Clone` (rather than `Copy`) is the price of carrying the
/// link target; the PDF layout engine clones a style per wrapped piece. `size_pt` is an `f32`,
/// so the style is `PartialEq` but not `Eq` — nothing hashes it.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct RunStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: Option<cleanup_core::doc::Rgb>,
    pub link: Option<String>,
    /// Source family/weight/size, straight from the reader. Exporters map these onto the
    /// container's own properties (`w:rFonts`, `w:b`, `w:sz`) instead of collapsing every
    /// non-regular weight onto the same "bold" font.
    pub font_family: Option<String>,
    pub font_weight: Option<u16>,
    pub size_pt: Option<f32>,
}

impl RunStyle {
    pub(crate) fn from_inline(style: &cleanup_core::doc::InlineStyle) -> RunStyle {
        RunStyle {
            // `Medium`/`SemiBold` are not reachable through a binary bold flag but they are
            // heavier than regular text, so anything from 600 up selects the bold face.
            bold: style.bold || style.font_weight.is_some_and(|weight| weight >= 600),
            italic: style.italic,
            code: style.code,
            underline: style.underline,
            strike: style.strike,
            color: style.color,
            link: style.link.clone(),
            font_family: style.font_family.clone(),
            font_weight: style.font_weight,
            size_pt: style.size_pt,
        }
    }
}

/// Map a source family onto a family the target container can actually resolve.
///
/// Word substitutes silently when asked for a font it does not have, which is how a Helvetica
/// document comes back set in something surprising. The base-14 families have well-known
/// Microsoft metric equivalents, so name those; anything else (an embedded foundry face) is
/// passed through unchanged because the original name is the only honest answer left.
pub(crate) fn target_font_family(family: &str, code: bool) -> String {
    if code {
        return "Consolas".to_string();
    }
    let lowered = family.to_ascii_lowercase();
    let mapped = match lowered.as_str() {
        "helvetica" => "Arial",
        "times" => "Times New Roman",
        "courier" => "Courier New",
        "symbol" => "Symbol",
        "zapfdingbats" => "Wingdings",
        _ => {
            if lowered.contains("helvetica") {
                "Arial"
            } else if lowered.contains("times") {
                "Times New Roman"
            } else if lowered.contains("courier") {
                "Courier New"
            } else {
                return family.to_string();
            }
        }
    };
    mapped.to_string()
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
