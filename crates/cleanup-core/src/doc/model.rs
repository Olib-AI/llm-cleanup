//! The document intermediate representation (IR).
//!
//! Two layers live here:
//!
//! * **Raw geometry** ([`LayoutDoc`], [`TextRun`], [`RuleSegment`], [`FillRect`],
//!   [`ImagePlacement`]) — exactly what a spatial reader recovered from a page: positioned
//!   text runs with their font metrics, painted vector rules, filled boxes, and placed
//!   raster images. This layer has no interpretation in it, which makes it a stable target
//!   for readers and a stable input for the analyser.
//! * **Interpreted IR** ([`Doc`], [`Block`], [`Inline`], [`Table`], …) — reading order,
//!   columns, headings, lists, code, callouts and tables.
//!
//! Coordinate convention: every `Rect` is in **page space with the origin at the top-left
//! corner and `y` growing downwards**, in PDF points (1/72 inch). The PDF reader flips the
//! native PDF y-axis on the way in so that "smaller y" always means "higher on the page".

use std::fmt;

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// An axis-aligned rectangle in top-left-origin page space (PDF points).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn center_x(&self) -> f32 {
        self.x + self.width / 2.0
    }

    pub fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    pub fn area(&self) -> f32 {
        if self.is_empty() {
            0.0
        } else {
            self.width * self.height
        }
    }

    pub fn contains_x(&self, x: f32) -> bool {
        x >= self.x && x <= self.right()
    }

    pub fn contains_y(&self, y: f32) -> bool {
        y >= self.y && y <= self.bottom()
    }

    /// True when `point` falls inside the rectangle.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.contains_x(x) && self.contains_y(y)
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            width: self.right().max(other.right()) - x,
            height: self.bottom().max(other.bottom()) - y,
        }
    }

    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 || y1 <= y0 {
            None
        } else {
            Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
        }
    }

    /// Horizontal overlap of two rectangles, in points (0 when disjoint).
    pub fn overlap_x(&self, other: &Rect) -> f32 {
        (self.right().min(other.right()) - self.x.max(other.x)).max(0.0)
    }

    /// Vertical gap between two rectangles; negative when they overlap.
    pub fn gap_y(&self, other: &Rect) -> f32 {
        (other.y - self.bottom()).max(self.y - other.bottom())
    }

    pub fn inset(&self, amount: f32) -> Rect {
        Rect::new(
            self.x + amount,
            self.y + amount,
            (self.width - 2.0 * amount).max(0.0),
            (self.height - 2.0 * amount).max(0.0),
        )
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{:.1},{:.1} {:.1}x{:.1}]",
            self.x, self.y, self.width, self.height
        )
    }
}

/// Axis of a vector rule (a hairline divider drawn as a filled rectangle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// An 8-bit RGB colour, as painted by the source document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0, 0, 0);
    pub const WHITE: Rgb = Rgb(255, 255, 255);

    pub fn is_dark(&self) -> bool {
        self.luminance() < 96.0
    }

    pub fn is_white(&self) -> bool {
        self.0 > 245 && self.1 > 245 && self.2 > 245
    }

    pub fn luminance(&self) -> f32 {
        0.299 * f32::from(self.0) + 0.587 * f32::from(self.1) + 0.114 * f32::from(self.2)
    }

    pub fn hex(&self) -> String {
        format!("{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

// ---------------------------------------------------------------------------
// Raw geometry recovered from a page
// ---------------------------------------------------------------------------

/// One text-showing run recovered from a page, with its font metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub text: String,
    /// Ink box of the run in top-left-origin page space.
    pub rect: Rect,
    /// Nominal font size in points.
    pub font_size: f32,
    /// PostScript base font name, e.g. `Inter` or `ABCDEF+SourceCodePro`.
    pub font_name: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub color: Rgb,
}

impl TextRun {
    /// Baseline y in page space, approximated from the ink box.
    pub fn baseline_y(&self) -> f32 {
        self.rect.y + self.font_size * 0.8
    }

    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// A vector divider rule (`re … f` / `m … l … S`), classified by its aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuleSegment {
    pub rect: Rect,
    pub axis: Axis,
    pub thickness: f32,
    pub color: Rgb,
}

impl RuleSegment {
    pub fn is_horizontal(&self) -> bool {
        self.axis == Axis::Horizontal
    }
}

/// A filled (and/or stroked) rectangle: callout backgrounds, table header fills, borders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillRect {
    pub rect: Rect,
    pub fill: Option<Rgb>,
    pub stroke: Option<Rgb>,
    pub stroke_width: f32,
}

impl FillRect {
    /// A box is a plausible callout background when it is large enough to hold text and
    /// is not page-sized (a full-bleed background is page furniture, not a callout).
    pub fn is_callout_sized(&self, page: &Rect) -> bool {
        let area = self.rect.area();
        if area < 1200.0 {
            return false;
        }
        if let Some(fill) = self.fill
            && (fill.is_white() || fill.luminance() > 245.0)
            && self.stroke.is_none()
        {
            return false;
        }
        area < page.area() * 0.85
    }
}

/// The raster formats the pipeline round-trips without re-encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Unknown,
}

impl ImageKind {
    pub fn mime(self) -> &'static str {
        match self {
            ImageKind::Png => "image/png",
            ImageKind::Jpeg => "image/jpeg",
            ImageKind::Gif => "image/gif",
            ImageKind::Webp => "image/webp",
            ImageKind::Unknown => "application/octet-stream",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ImageKind::Png => "png",
            ImageKind::Jpeg => "jpeg",
            ImageKind::Gif => "gif",
            ImageKind::Webp => "webp",
            ImageKind::Unknown => "bin",
        }
    }

    /// Sniff a raster header.
    pub fn sniff(bytes: &[u8]) -> ImageKind {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            ImageKind::Png
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            ImageKind::Jpeg
        } else if bytes.starts_with(b"GIF8") {
            ImageKind::Gif
        } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            ImageKind::Webp
        } else {
            ImageKind::Unknown
        }
    }
}

/// An embedded raster image, stored once and referenced by placement.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageAsset {
    /// Stable file stem used by the Markdown/DOCX writers (`figure-1`, `figure-2`, …).
    pub name: String,
    pub kind: ImageKind,
    pub data: Vec<u8>,
    pub width_px: u32,
    pub height_px: u32,
}

impl ImageAsset {
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.name, self.kind.extension())
    }

    /// Effective dots per inch when the asset is drawn into `rect`.
    pub fn dpi(&self, rect: &Rect) -> Option<f32> {
        if rect.width <= 1.0 || self.width_px == 0 {
            return None;
        }
        Some(self.width_px as f32 * 72.0 / rect.width)
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty() || self.width_px == 0 || self.height_px == 0
    }
}

/// One placement of an image asset on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct ImagePlacement {
    /// Index into [`LayoutDoc::assets`].
    pub asset: usize,
    pub rect: Rect,
}

/// Everything recovered from one page.
#[derive(Debug, Clone, Default)]
pub struct PageLayout {
    pub width: f32,
    pub height: f32,
    pub runs: Vec<TextRun>,
    pub rules: Vec<RuleSegment>,
    pub fills: Vec<FillRect>,
    pub images: Vec<ImagePlacement>,
}

impl PageLayout {
    pub fn rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width, self.height)
    }
}

/// A spatially reconstructed document: pages of raw geometry plus the shared asset table.
#[derive(Debug, Clone, Default)]
pub struct LayoutDoc {
    pub pages: Vec<PageLayout>,
    pub assets: Vec<ImageAsset>,
}

impl LayoutDoc {
    pub fn is_empty(&self) -> bool {
        self.pages
            .iter()
            .all(|page| page.runs.is_empty() && page.images.is_empty() && page.rules.is_empty())
    }
}

// ---------------------------------------------------------------------------
// Interpreted IR
// ---------------------------------------------------------------------------

/// Where a block came from: page index plus its source bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub page: usize,
    pub rect: Rect,
}

impl Anchor {
    pub fn new(page: usize, rect: Rect) -> Self {
        Anchor { page, rect }
    }
}

/// Inline character style. Inline nodes carry only text; the style flags are what the
/// exporters map onto `**…**`, `<w:b/>`, `\x1b[1m`-free plain text, and PDF font switching.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub underline: bool,
    pub strike: bool,
    pub link: Option<String>,
}

impl InlineStyle {
    pub fn plain() -> Self {
        InlineStyle::default()
    }

    pub fn bold() -> Self {
        InlineStyle {
            bold: true,
            ..InlineStyle::default()
        }
    }

    pub fn italic() -> Self {
        InlineStyle {
            italic: true,
            ..InlineStyle::default()
        }
    }

    pub fn code() -> Self {
        InlineStyle {
            code: true,
            ..InlineStyle::default()
        }
    }

    pub fn with_link(mut self, link: impl Into<String>) -> Self {
        self.link = Some(link.into());
        self
    }

    pub fn is_plain(&self) -> bool {
        !self.bold
            && !self.italic
            && !self.code
            && !self.underline
            && !self.strike
            && self.link.is_none()
    }
}

/// A run of inline text with one style.
#[derive(Debug, Clone, PartialEq)]
pub struct Inline {
    pub text: String,
    pub style: InlineStyle,
}

impl Inline {
    pub fn new(text: impl Into<String>, style: InlineStyle) -> Self {
        Inline {
            text: text.into(),
            style,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Inline::new(text, InlineStyle::plain())
    }

    pub fn bold(text: impl Into<String>) -> Self {
        Inline::new(text, InlineStyle::bold())
    }

    pub fn italic(text: impl Into<String>) -> Self {
        Inline::new(text, InlineStyle::italic())
    }

    pub fn code(text: impl Into<String>) -> Self {
        Inline::new(text, InlineStyle::code())
    }

    /// A hard line break inside a paragraph.
    pub fn line_break() -> Self {
        Inline::plain("\n")
    }

    pub fn is_break(&self) -> bool {
        self.text == "\n"
    }
}

/// Horizontal alignment inside a table cell (and, for `Paragraph`, of the block itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

impl Align {
    pub fn as_str(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }
}

/// Paragraph-level layout hints recovered from the source.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ParagraphStyle {
    pub align: Align,
    /// Left indent in points, relative to the text column.
    pub indent: f32,
    /// First-line indent in points (negative for hanging indents).
    pub first_line_indent: f32,
    /// Baseline-to-baseline distance in points, when the source was unambiguous.
    pub line_spacing: Option<f32>,
}

/// One list item: its text plus nesting depth.
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub inlines: Vec<Inline>,
    pub depth: usize,
}

/// What sort of callout box this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutKind {
    /// A tinted/bordered box drawn behind text.
    Box,
    /// A block quote (`> …`).
    Quote,
    /// A note/warning box with a leading label.
    Note,
}

/// Per-column table metadata: relative width and cell alignment.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColumnSpec {
    /// Relative width (all columns of a table are normalised before export).
    pub width: f32,
    pub align: Align,
}

/// One table cell. Cells hold blocks so multi-paragraph cells survive.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cell {
    pub blocks: Vec<Block>,
    /// Number of grid columns this cell spans.
    pub colspan: usize,
    /// Number of grid rows this cell spans.
    pub rowspan: usize,
    pub align: Align,
    /// Background fill, when the source painted one.
    pub fill: Option<Rgb>,
    /// True when the source marked the cell as a header cell.
    pub header: bool,
}

impl Cell {
    pub fn paragraph(text: impl Into<String>) -> Self {
        Cell {
            blocks: vec![Block::Paragraph {
                inlines: vec![Inline::plain(text)],
                anchor: None,
                style: ParagraphStyle::default(),
            }],
            colspan: 1,
            rowspan: 1,
            align: Align::Left,
            fill: None,
            header: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks
            .iter()
            .all(|block| block.plain_text().trim().is_empty())
    }

    /// Text with paragraph breaks flattened to newlines; used by the table exporters.
    pub fn text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for block in &self.blocks {
            let text = block.plain_text();
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
        parts.join("\n")
    }

    pub fn line_count(&self) -> usize {
        let text = self.text();
        if text.is_empty() {
            1
        } else {
            text.lines().count()
        }
    }
}

/// One table row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub header: bool,
    pub fill: Option<Rgb>,
}

impl Row {
    /// Number of grid columns this row occupies (sum of colspans).
    pub fn width(&self) -> usize {
        self.cells.iter().map(|cell| cell.colspan.max(1)).sum()
    }
}

/// Border widths recovered from the source grid, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TableBorders {
    pub outer: f32,
    pub inner_horizontal: f32,
    pub inner_vertical: f32,
}

impl Default for TableBorders {
    fn default() -> Self {
        TableBorders {
            outer: 0.75,
            inner_horizontal: 0.5,
            inner_vertical: 0.5,
        }
    }
}

impl TableBorders {
    /// No visible rules (a "booktabs"-free, spacing-only table).
    pub fn none() -> Self {
        TableBorders {
            outer: 0.0,
            inner_horizontal: 0.0,
            inner_vertical: 0.0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.outer <= 0.01 && self.inner_horizontal <= 0.01 && self.inner_vertical <= 0.01
    }
}

/// The rendered table.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub anchor: Option<Anchor>,
    pub caption: Option<Vec<Inline>>,
    pub columns: Vec<ColumnSpec>,
    /// Header rows, repeated when the table is exported to a paginated target.
    pub headers: Vec<Row>,
    pub rows: Vec<Row>,
    /// Page indices the table was stitched across (1-based, for reporting).
    pub pages: Vec<usize>,
    /// Border widths painted by the source grid.
    pub borders: TableBorders,
    /// Cell padding in points.
    pub cell_padding: f32,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            anchor: None,
            caption: None,
            columns: Vec::new(),
            headers: Vec::new(),
            rows: Vec::new(),
            pages: Vec::new(),
            borders: TableBorders::default(),
            cell_padding: 4.0,
        }
    }
}

impl Table {
    /// Total number of cells across header and body rows.
    pub fn cell_count(&self) -> usize {
        self.headers
            .iter()
            .map(|row| row.cells.len())
            .sum::<usize>()
            + self.rows.iter().map(|row| row.cells.len()).sum::<usize>()
    }

    /// True when any cell spans more than one grid column or row.
    pub fn has_merged_cells(&self) -> bool {
        self.headers
            .iter()
            .chain(self.rows.iter())
            .flat_map(|row| row.cells.iter())
            .any(|cell| cell.colspan > 1 || cell.rowspan > 1)
    }

    /// True when any cell holds more than one line of text.
    pub fn has_multiline_cells(&self) -> bool {
        self.headers
            .iter()
            .chain(self.rows.iter())
            .flat_map(|row| row.cells.iter())
            .any(|cell| cell.line_count() > 1)
    }

    pub fn column_count(&self) -> usize {
        let mut max = self.columns.len();
        for row in self.headers.iter().chain(self.rows.iter()) {
            max = max.max(row.width());
        }
        max
    }

    pub fn row_count(&self) -> usize {
        self.headers.len() + self.rows.len()
    }

    pub fn all_rows(&self) -> impl Iterator<Item = &Row> {
        self.headers.iter().chain(self.rows.iter())
    }
}

/// A figure (placed raster image) with its caption.
#[derive(Debug, Clone, PartialEq)]
pub struct Figure {
    /// Index into [`Doc::assets`].
    pub asset: usize,
    pub anchor: Option<Anchor>,
    pub caption: Option<Vec<Inline>>,
    pub alt: String,
    /// The box the image occupied in the source, when known (points).
    pub display_size: Option<Rect>,
}

/// A divider rule that carried meaning (a horizontal rule between sections).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Divider {
    pub anchor: Option<Anchor>,
    pub axis: Axis,
}

/// A run of consecutive lines that share one column geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub blocks: Vec<Block>,
    /// Relative width of this column inside its parent region.
    pub width: f32,
}

/// The IR block vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        anchor: Option<Anchor>,
    },
    Paragraph {
        inlines: Vec<Inline>,
        anchor: Option<Anchor>,
        style: ParagraphStyle,
    },
    List {
        ordered: bool,
        items: Vec<ListItem>,
        anchor: Option<Anchor>,
    },
    Code {
        lines: Vec<String>,
        anchor: Option<Anchor>,
        language: Option<String>,
    },
    Table(Table),
    Figure(Figure),
    Divider(Divider),
    Callout {
        kind: CalloutKind,
        blocks: Vec<Block>,
        anchor: Option<Anchor>,
        fill: Option<Rgb>,
    },
    Columns {
        columns: Vec<Column>,
        anchor: Option<Anchor>,
    },
}

impl Block {
    pub fn anchor(&self) -> Option<Anchor> {
        match self {
            Block::Heading { anchor, .. }
            | Block::Paragraph { anchor, .. }
            | Block::List { anchor, .. }
            | Block::Code { anchor, .. }
            | Block::Figure(Figure { anchor, .. })
            | Block::Callout { anchor, .. }
            | Block::Columns { anchor, .. } => *anchor,
            Block::Table(table) => table.anchor,
            Block::Divider(divider) => divider.anchor,
        }
    }

    pub fn set_anchor(&mut self, anchor: Option<Anchor>) {
        match self {
            Block::Heading { anchor: slot, .. }
            | Block::Paragraph { anchor: slot, .. }
            | Block::List { anchor: slot, .. }
            | Block::Code { anchor: slot, .. }
            | Block::Callout { anchor: slot, .. }
            | Block::Columns { anchor: slot, .. } => *slot = anchor,
            Block::Figure(figure) => figure.anchor = anchor,
            Block::Table(table) => table.anchor = anchor,
            Block::Divider(divider) => divider.anchor = anchor,
        }
    }

    /// Flattened text of this block, *not* descending into tables or column containers
    /// (those have their own `plain_text`).
    pub fn plain_text(&self) -> String {
        match self {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => {
                inlines_text(inlines)
            }
            Block::List { items, .. } => items
                .iter()
                .map(|item| inlines_text(&item.inlines))
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Code { lines, .. } => lines.join("\n"),
            Block::Table(table) => table
                .all_rows()
                .map(|row| {
                    row.cells
                        .iter()
                        .map(Cell::text)
                        .collect::<Vec<_>>()
                        .join("\t")
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Figure(figure) => figure.alt.clone(),
            Block::Divider(_) => String::new(),
            Block::Callout { blocks, .. } => blocks
                .iter()
                .map(Block::plain_text)
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Columns { columns, .. } => columns
                .iter()
                .map(|column| {
                    column
                        .blocks
                        .iter()
                        .map(Block::plain_text)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// Inlines for blocks that carry them directly (used by the sanitizer).
    pub fn inlines(&self) -> Option<&Vec<Inline>> {
        match self {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => Some(inlines),
            _ => None,
        }
    }

    pub fn inlines_mut(&mut self) -> Option<&mut Vec<Inline>> {
        match self {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => Some(inlines),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Block::Divider(_) => false,
            Block::Table(table) => table.row_count() == 0,
            Block::Figure(_) => false,
            other => other.plain_text().trim().is_empty(),
        }
    }
}

/// Page geometry and margins for the exported document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSpec {
    pub width: f32,
    pub height: f32,
    pub margin_top: f32,
    pub margin_right: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
}

impl Default for PageSpec {
    /// US Letter with one-inch margins — the same default Word and Google Docs use.
    fn default() -> Self {
        PageSpec {
            width: 612.0,
            height: 792.0,
            margin_top: 72.0,
            margin_right: 72.0,
            margin_bottom: 72.0,
            margin_left: 72.0,
        }
    }
}

impl PageSpec {
    pub fn content_width(&self) -> f32 {
        (self.width - self.margin_left - self.margin_right).max(36.0)
    }

    pub fn content_height(&self) -> f32 {
        (self.height - self.margin_top - self.margin_bottom).max(36.0)
    }
}

/// Which reader produced the IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceKind {
    #[default]
    Unknown,
    Pdf,
    Docx,
    Markdown,
    Html,
    PlainText,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Unknown => "unknown",
            SourceKind::Pdf => "pdf",
            SourceKind::Docx => "docx",
            SourceKind::Markdown => "markdown",
            SourceKind::Html => "html",
            SourceKind::PlainText => "text",
        }
    }
}

/// Document-level metadata that survives conversion.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocInfo {
    pub title: Option<String>,
    pub author: Option<String>,
    /// Number of source pages, when the source had pages.
    pub page_count: usize,
    pub source: SourceKind,
    /// Running header/footer text that was recognised and lifted out of the flow.
    pub running_headers: Vec<String>,
    pub running_footers: Vec<String>,
}

/// The interpreted document: what every exporter consumes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc {
    pub page: PageSpec,
    pub blocks: Vec<Block>,
    pub assets: Vec<ImageAsset>,
    pub info: DocInfo,
}

impl Doc {
    pub fn new(page: PageSpec) -> Self {
        Doc {
            page,
            blocks: Vec::new(),
            assets: Vec::new(),
            info: DocInfo::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(Block::is_empty) && self.assets.is_empty()
    }

    /// All block text in reading order (excluding table cells, see [`Block::plain_text`]).
    pub fn plain_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for block in &self.blocks {
            let text = block.plain_text();
            if !text.trim().is_empty() {
                parts.push(text);
            }
        }
        parts.join("\n\n")
    }

    /// Total number of blocks, descending into containers.
    pub fn block_count(&self) -> usize {
        fn count(blocks: &[Block]) -> usize {
            blocks
                .iter()
                .map(|block| {
                    1 + match block {
                        Block::Callout { blocks, .. } => count(blocks),
                        Block::Columns { columns, .. } => {
                            columns.iter().map(|c| count(&c.blocks)).sum::<usize>()
                        }
                        Block::Table(table) => table
                            .all_rows()
                            .flat_map(|row| row.cells.iter())
                            .map(|cell| count(&cell.blocks))
                            .sum::<usize>(),
                        _ => 0,
                    }
                })
                .sum()
        }
        count(&self.blocks)
    }

    /// Visit every block, descending into containers.
    pub fn walk(&self) -> Vec<&Block> {
        fn push<'a>(blocks: &'a [Block], out: &mut Vec<&'a Block>) {
            for block in blocks {
                out.push(block);
                match block {
                    Block::Callout { blocks, .. } => push(blocks, out),
                    Block::Columns { columns, .. } => {
                        for column in columns {
                            push(&column.blocks, out);
                        }
                    }
                    Block::Table(table) => {
                        for row in table.all_rows() {
                            for cell in &row.cells {
                                push(&cell.blocks, out);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        push(&self.blocks, &mut out);
        out
    }

    /// Find the asset index for a figure, if it still exists.
    pub fn asset(&self, index: usize) -> Option<&ImageAsset> {
        self.assets.get(index)
    }
}

/// Join inline text, keeping hard breaks as newlines.
pub fn inlines_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        out.push_str(&inline.text);
    }
    out
}

/// Collapse a whitespace-y inline sequence into plain words (used for headings that came
/// from widely justified text).
pub fn normalize_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() && ch != '\n' {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_union_and_intersection() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        let union = a.union(&b);
        assert_eq!(union, Rect::new(0.0, 0.0, 15.0, 15.0));
        let intersection = a.intersection(&b).expect("overlap");
        assert_eq!(intersection, Rect::new(5.0, 5.0, 5.0, 5.0));
        assert!(a.intersection(&Rect::new(20.0, 20.0, 1.0, 1.0)).is_none());
    }

    #[test]
    fn image_kind_sniffs_headers() {
        assert_eq!(ImageKind::sniff(b"\x89PNG\r\n\x1a\n0000"), ImageKind::Png);
        assert_eq!(ImageKind::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), ImageKind::Jpeg);
        assert_eq!(ImageKind::sniff(b"GIF89a"), ImageKind::Gif);
        assert_eq!(ImageKind::sniff(b"not an image"), ImageKind::Unknown);
    }

    #[test]
    fn table_reports_merged_cells_and_shape() {
        let mut table = Table::default();
        table.headers.push(Row {
            cells: vec![Cell::paragraph("A"), Cell::paragraph("B")],
            header: true,
            fill: None,
        });
        let mut merged = Cell::paragraph("merged");
        merged.colspan = 2;
        table.rows.push(Row {
            cells: vec![merged],
            header: false,
            fill: None,
        });
        assert_eq!(table.column_count(), 2);
        assert_eq!(table.row_count(), 2);
        assert_eq!(table.cell_count(), 3);
        assert!(table.has_merged_cells());
    }

    #[test]
    fn plain_text_of_document_joins_blocks_with_blank_lines() {
        let mut doc = Doc::new(PageSpec::default());
        doc.blocks.push(Block::Paragraph {
            inlines: vec![Inline::plain("one")],
            anchor: None,
            style: ParagraphStyle::default(),
        });
        doc.blocks.push(Block::Heading {
            level: 1,
            inlines: vec![Inline::plain("two")],
            anchor: None,
        });
        assert_eq!(doc.plain_text(), "one\n\ntwo");
    }
}
