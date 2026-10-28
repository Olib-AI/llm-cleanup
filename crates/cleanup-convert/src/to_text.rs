//! Compatibility wrapper for the structured Text exporter.
//!
//! For Markdown with images use `convert_document`, which also returns sidecars.

use crate::{ConvertReport, Result, Target};
use cleanup_core::FormatId;

/// Convert through the same layout-aware pipeline as the CLI and GUI.
pub fn convert(from: FormatId, src: &[u8]) -> Result<(Vec<u8>, ConvertReport)> {
    crate::convert(from, src, Target::Text)
}
