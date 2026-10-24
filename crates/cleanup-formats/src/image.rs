//! Standalone lossless provenance adapter for PNG, JPEG, WebP, and SVG media files.
//!
//! Unlike text formats, an image carries no prose spans: pixel streams (`IDAT`, JPEG entropy
//! segments after `SOS`, WebP `VP8 `/`VP8L`) are never decoded or re-encoded. At every level
//! `container_findings` reports embedded C2PA Content Credentials (`caBX`, `APP11` JUMBF,
//! `C2PA` RIFF chunks, SVG `<metadata>`) and AI generation metadata (IPTC
//! `trainedAlgorithmicMedia`, Stable Diffusion `parameters`, ComfyUI graphs, AI XMP packets);
//! at [`CleanupLevel::Aggressive`](cleanup_core::CleanupLevel::Aggressive), `scrub_container`
//! splices those chunks out losslessly.

use crate::c2pa::{self, MediaKind};
use cleanup_core::ir::{DocMeta, Document, FormatId, Newline};
use cleanup_core::{ContainerFinding, CoreError, EditSet, Format, Result};

#[derive(Debug, Default, Clone, Copy)]
pub struct ImageFormat;

impl ImageFormat {
    fn kind_label(kind: MediaKind) -> &'static str {
        match kind {
            MediaKind::Png => "PNG image",
            MediaKind::Jpeg => "JPEG image",
            MediaKind::Webp => "WebP image",
            MediaKind::Svg => "SVG graphic",
        }
    }
}

impl Format for ImageFormat {
    fn id(&self) -> FormatId {
        FormatId::Image
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let Some(_kind) = c2pa::sniff_media(bytes) else {
            return Err(CoreError::Parse(
                "unrecognized media container (expected PNG, JPEG, WebP, or SVG)".into(),
            ));
        };
        Ok(Document::new(
            bytes.to_vec(),
            Vec::new(),
            DocMeta {
                format: FormatId::Image,
                newline: Newline::Lf,
                had_bom: false,
            },
        ))
    }

    fn render(&self, doc: &Document, _edits: &EditSet) -> Result<Vec<u8>> {
        Ok(doc.source.clone())
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        let Some(kind) = c2pa::sniff_media(bytes) else {
            return Err(CoreError::Parse("corrupt media header".into()));
        };
        Ok(format!("media:{kind:?}:{}", bytes.len()))
    }

    fn container_findings(&self, doc: &Document) -> Vec<ContainerFinding> {
        let Some(kind) = c2pa::sniff_media(&doc.source) else {
            return Vec::new();
        };
        c2pa::inspect_media(&doc.source, Self::kind_label(kind))
    }

    fn scrub_container(&self, _doc: &Document, rendered: &[u8]) -> Option<Vec<u8>> {
        c2pa::scrub_media(rendered)
    }
}
