//! The document intermediate representation and the layout analysis that builds it.
//!
//! Readers produce [`LayoutDoc`] (positioned text runs, rules, fills and placed images).
//! [`analyze`] turns that into [`Doc`] — blocks in reading order. Exporters consume [`Doc`]
//! only, which is what stops layout from collapsing at the conversion boundary.

pub mod analyze;
pub mod model;
pub mod raster;

pub use analyze::{AnalyzeOptions, analyze};
pub use model::{
    Align, Anchor, Axis, Block, CalloutKind, Cell, Column, ColumnSpec, Divider, Doc, DocInfo,
    Figure, FillRect, ImageAsset, ImageKind, ImagePlacement, Inline, InlineStyle, LayoutDoc,
    ListItem, PageBoundary, PageLayout, PageSpec, ParagraphStyle, Rect, Rgb, Row, RuleSegment,
    SourceKind, Table, TableBorders, TextRun, VectorKind, VectorShape, inlines_text,
    normalize_spaces,
};
pub use raster::{RasterImage, decode_png, dimensions as raster_dimensions, encode_png};
