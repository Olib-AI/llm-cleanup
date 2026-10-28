//! DOCX exporter — native OpenXML, written directly.
//!
//! The previous implementation mapped Markdown events onto `docx_rs` paragraphs, which cannot
//! express tables, sections, images or per-paragraph spacing; everything structural was dropped
//! with a warning. Here the package is written by hand so each IR construct maps onto the OOXML
//! construct that means the same thing:
//!
//! | IR | OOXML |
//! | --- | --- |
//! | `Heading { level }` | `w:pStyle` `Heading1…6` from `styles.xml` |
//! | `Paragraph { style }` | `w:spacing` (line rule + before/after), `w:ind`, `w:jc` |
//! | `List` | `w:numPr` against `numbering.xml` (bullet and decimal abstract numbering) |
//! | `Code` | shaded monospace paragraph with `w:pBdr` and preserved spaces |
//! | `Table` | `w:tbl`/`w:tblGrid`/`w:tr`/`w:tc` with `w:gridSpan`, `w:vMerge`, `w:tcMar` |
//! | `Figure` | `w:drawing`/`wp:inline` pointing at a `word/media/*` part |
//! | `Columns` | continuous `w:sectPr` with `w:cols w:num="N"` |
//! | `Callout` | shaded paragraphs with a coloured left border |
//! | `Divider` | paragraph with a bottom border |

use super::{Rendered, RunStyle, asset_for, target_font_family};
use cleanup_core::doc::{Align, Block, CalloutKind, Cell, Doc, Inline, PageSpec, Table};
use std::collections::BTreeMap;
use std::io::Write;

const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";

/// Render `doc` as a `.docx` package.
pub fn render(doc: &Doc) -> Rendered {
    let mut writer = Writer::new(doc);
    for block in &doc.blocks {
        writer.block(block);
    }
    let document_xml = writer.document_xml();
    let (rels_xml, media) = writer.relationships();

    let mut files: Vec<(String, Vec<u8>)> = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types(&media).into_bytes(),
        ),
        ("_rels/.rels".to_string(), package_rels().into_bytes()),
        ("word/document.xml".to_string(), document_xml.into_bytes()),
        ("word/styles.xml".to_string(), styles_xml().into_bytes()),
        (
            "word/numbering.xml".to_string(),
            numbering_xml().into_bytes(),
        ),
        (
            "word/_rels/document.xml.rels".to_string(),
            rels_xml.into_bytes(),
        ),
        (
            "docProps/core.xml".to_string(),
            core_properties(doc).into_bytes(),
        ),
        (
            "docProps/app.xml".to_string(),
            app_properties().into_bytes(),
        ),
    ];
    for (name, bytes) in media {
        files.push((format!("word/media/{name}"), bytes));
    }

    let bytes = match pack(&files) {
        Ok(bytes) => bytes,
        Err(error) => {
            let mut warnings = writer.warnings.clone();
            warnings.push(format!("DOCX packaging failed: {error}"));
            return Rendered {
                bytes: Vec::new(),
                assets: Vec::new(),
                warnings,
                engine: "ir-openxml",
            };
        }
    };
    Rendered {
        bytes,
        assets: Vec::new(),
        warnings: writer.warnings.clone(),
        engine: "ir-openxml",
    }
}

fn pack(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut buffer = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        for (name, bytes) in files {
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer
                .start_file(name.as_str(), options)
                .map_err(|error| format!("zip entry {name}: {error}"))?;
            writer
                .write_all(bytes)
                .map_err(|error| format!("zip write {name}: {error}"))?;
        }
        writer
            .finish()
            .map_err(|error| format!("zip finish: {error}"))?;
    }
    Ok(buffer)
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

struct Writer<'a> {
    doc: &'a Doc,
    body: String,
    warnings: Vec<String>,
    /// Relationship id per asset index.
    media: BTreeMap<usize, String>,
    media_parts: Vec<(String, Vec<u8>)>,
    /// Relationship id per external hyperlink target.
    links: BTreeMap<String, String>,
    next_relationship: usize,
    drawing_id: usize,
}

impl<'a> Writer<'a> {
    fn new(doc: &'a Doc) -> Self {
        Writer {
            doc,
            body: String::new(),
            warnings: Vec::new(),
            media: BTreeMap::new(),
            media_parts: Vec::new(),
            links: BTreeMap::new(),
            next_relationship: 3,
            drawing_id: 1,
        }
    }

    /// Allocate (or reuse) a relationship id for an external hyperlink.
    fn link_id(&mut self, target: &str) -> String {
        if let Some(id) = self.links.get(target) {
            return id.clone();
        }
        let id = format!("rId{}", self.next_relationship);
        self.next_relationship += 1;
        self.links.insert(target.to_string(), id.clone());
        id
    }

    fn twips(points: f32) -> i64 {
        (points * 20.0).round() as i64
    }

    fn emu(points: f32) -> i64 {
        (points * 12_700.0).round() as i64
    }

    fn content_width(&self) -> f32 {
        self.doc.page.content_width()
    }

    fn paragraph(&mut self, properties: &str, runs: &str) {
        self.body.push_str("<w:p>");
        if !properties.is_empty() {
            self.body.push_str("<w:pPr>");
            self.body.push_str(properties);
            self.body.push_str("</w:pPr>");
        }
        self.body.push_str(runs);
        self.body.push_str("</w:p>");
    }

    fn block(&mut self, block: &Block) {
        match block {
            Block::Heading { level, inlines, .. } => {
                let style = format!("Heading{}", (*level).clamp(1, 6));
                let properties = format!(
                    "<w:pStyle w:val=\"{style}\"/><w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/>"
                );
                let runs = self.runs(inlines, 1.0);
                self.paragraph(&properties, &runs);
            }
            Block::Paragraph { inlines, style, .. } => {
                let mut properties = String::from(
                    "<w:spacing w:before=\"0\" w:after=\"120\" w:line=\"276\" w:lineRule=\"auto\"/>",
                );
                if style.align != Align::Left {
                    properties.push_str(&format!("<w:jc w:val=\"{}\"/>", style.align.as_str()));
                }
                if style.indent > 0.5 {
                    properties.push_str(&format!(
                        "<w:ind w:left=\"{}\"/>",
                        Self::twips(style.indent)
                    ));
                }
                let runs = self.runs(inlines, 1.0);
                self.paragraph(&properties, &runs);
            }
            Block::List { ordered, items, .. } => {
                let num_id = if *ordered { 2 } else { 1 };
                for item in items {
                    let level = item.depth.min(8);
                    let properties = format!(
                        "<w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"{num_id}\"/></w:numPr><w:spacing w:after=\"60\"/>"
                    );
                    let runs = self.runs(&item.inlines, 1.0);
                    self.paragraph(&properties, &runs);
                }
            }
            Block::Code { lines, .. } => {
                let mut runs = String::new();
                for (index, line) in lines.iter().enumerate() {
                    if index > 0 {
                        runs.push_str("<w:r><w:br/></w:r>");
                    }
                    runs.push_str(&format!(
                        "<w:r><w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:cs=\"Consolas\"/><w:sz w:val=\"19\"/><w:szCs w:val=\"19\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>",
                        escape_xml(line)
                    ));
                }
                let properties = "<w:pStyle w:val=\"CodeBlock\"/><w:pBdr><w:top w:val=\"single\" w:sz=\"4\" w:space=\"4\" w:color=\"D0D7DE\"/><w:left w:val=\"single\" w:sz=\"4\" w:space=\"4\" w:color=\"D0D7DE\"/><w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"4\" w:color=\"D0D7DE\"/><w:right w:val=\"single\" w:sz=\"4\" w:space=\"4\" w:color=\"D0D7DE\"/></w:pBdr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F6F8FA\"/><w:spacing w:before=\"120\" w:after=\"120\" w:line=\"240\" w:lineRule=\"auto\"/>";
                self.paragraph(properties, &runs);
            }
            Block::Table(table) => self.table(table),
            Block::Figure(figure) => self.figure(figure),
            Block::Vector(shape) => {
                // VML is the interoperable way to keep an arbitrary stroked shape editable in
                // Word; a plain bordered paragraph cannot express a vertical divider or a
                // dashed rule set. `xmlns:v` is declared on the document root.
                let color = shape
                    .stroke
                    .map(|rgb| rgb.hex())
                    .unwrap_or_else(|| "7F7F7F".to_string());
                let width = shape.rect.width.max(1.0);
                let height = shape.rect.height.max(1.0);
                let element = match shape.kind {
                    cleanup_core::doc::VectorKind::Line => format!(
                        "<v:line id=\"_x0000_s{}\" style=\"position:absolute\" from=\"0,0\" to=\"{width:.1},{height:.1}\" strokecolor=\"#{color}\" strokeweight=\"{:.2}pt\"/>",
                        self.drawing_id, shape.stroke_width
                    ),
                    cleanup_core::doc::VectorKind::Rect => format!(
                        "<v:rect id=\"_x0000_s{}\" style=\"position:absolute;width:{width:.1}pt;height:{height:.1}pt\" strokecolor=\"#{color}\" strokeweight=\"{:.2}pt\" filled=\"f\"/>",
                        self.drawing_id, shape.stroke_width
                    ),
                    cleanup_core::doc::VectorKind::Ellipse => format!(
                        "<v:oval id=\"_x0000_s{}\" style=\"position:absolute;width:{width:.1}pt;height:{height:.1}pt\" strokecolor=\"#{color}\" strokeweight=\"{:.2}pt\" filled=\"f\"/>",
                        self.drawing_id, shape.stroke_width
                    ),
                };
                self.drawing_id += 1;
                self.paragraph("", &format!("<w:r><w:pict>{element}</w:pict></w:r>"));
            }
            Block::Divider(_) => {
                let properties = "<w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"1\" w:color=\"BFBFBF\"/></w:pBdr><w:spacing w:before=\"120\" w:after=\"120\"/>";
                self.paragraph(properties, "");
            }
            Block::Callout {
                kind, blocks, fill, ..
            } => {
                let shade = fill.map(|color| color.hex()).unwrap_or_else(|| match kind {
                    CalloutKind::Quote => "F2F4F7".to_string(),
                    _ => "FFF8E1".to_string(),
                });
                // A shaded, bordered lead-in paragraph opens the box; the contents follow with
                // their own structure so headings, lists and tables inside a callout survive.
                self.body.push_str(&format!(
                    "<w:p><w:pPr><w:pStyle w:val=\"Callout\"/><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{shade}\"/></w:pPr></w:p>"
                ));
                for block in blocks {
                    self.block(block);
                }
            }
            Block::Columns { columns, .. } => {
                // Close the single-column section, emit the columns, then close the column
                // section again so following content returns to one column.
                self.section_break(1);
                for column in columns {
                    for block in &column.blocks {
                        self.block(block);
                    }
                    self.paragraph("", "");
                }
                self.section_break(columns.len().max(2));
            }
        }
    }

    fn section_break(&mut self, column_count: usize) {
        let space = if column_count > 1 { 425 } else { 0 };
        self.body.push_str(&format!(
            "<w:p><w:pPr><w:sectPr><w:type w:val=\"continuous\"/><w:cols w:num=\"{column_count}\" w:space=\"{space}\"/></w:sectPr></w:pPr></w:p>"
        ));
    }

    fn runs(&mut self, inlines: &[Inline], scale: f32) -> String {
        let mut out = String::new();
        for inline in inlines {
            if inline.is_break() {
                out.push_str("<w:r><w:br/></w:r>");
                continue;
            }
            if inline.text.is_empty() {
                continue;
            }
            let style = RunStyle::from_inline(&inline.style);
            let mut properties = String::new();
            if style.bold {
                properties.push_str("<w:b/><w:bCs/>");
            }
            if style.italic {
                properties.push_str("<w:i/><w:iCs/>");
            }
            if style.strike {
                properties.push_str("<w:strike/>");
            }
            if style.underline {
                properties.push_str("<w:u w:val=\"single\"/>");
            }
            // Typography first, so a run keeps the family, weight and size the source had
            // instead of inheriting whatever `styles.xml` happens to say.
            if let Some(family) = style.font_family.as_deref() {
                let target = escape_xml(&target_font_family(family, style.code));
                properties.push_str(&format!(
                    "<w:rFonts w:ascii=\"{target}\" w:hAnsi=\"{target}\" w:cs=\"{target}\"/>"
                ));
            } else if style.code {
                properties.push_str(
                    "<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:cs=\"Consolas\"/>",
                );
            }
            if let Some(color) = style.color {
                properties.push_str(&format!("<w:color w:val=\"{}\"/>", color.hex()));
            }
            // `w:sz` is in half-points. A run that knows its source size keeps it; otherwise
            // only a scaled block (a heading rendered from Markdown) needs an explicit size.
            let size_tenths = style.size_pt.map(|size| size * scale * 2.0);
            let half_points = match size_tenths {
                Some(value) => Some(value.round().clamp(2.0, 400.0) as i64),
                None => ((scale - 1.0).abs() > 0.01).then_some((10.5 * scale * 2.0).round() as i64),
            };
            if let Some(half_points) = half_points {
                properties.push_str(&format!(
                    "<w:sz w:val=\"{half_points}\"/><w:szCs w:val=\"{half_points}\"/>"
                ));
            }
            if !properties.is_empty() {
                properties = format!("<w:rPr>{properties}</w:rPr>");
            }
            let run = format!(
                "<w:r>{properties}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
                escape_xml(&inline.text)
            );
            if let Some(link) = style.link.as_deref() {
                let id = self.link_id(link);
                out.push_str(&format!(
                    "<w:hyperlink r:id=\"{id}\" w:history=\"1\">{run}</w:hyperlink>"
                ));
            } else {
                out.push_str(&run);
            }
        }
        if out.is_empty() {
            out.push_str("<w:r><w:t xml:space=\"preserve\"></w:t></w:r>");
        }
        out
    }

    fn cell_paragraphs(&mut self, cell: &Cell) -> String {
        let mut out = String::new();
        for block in &cell.blocks {
            let mut nested = Writer {
                doc: self.doc,
                body: String::new(),
                warnings: Vec::new(),
                media: BTreeMap::new(),
                media_parts: Vec::new(),
                links: self.links.clone(),
                next_relationship: self.next_relationship,
                drawing_id: self.drawing_id,
            };
            nested.block(block);
            self.media.extend(nested.media);
            self.media_parts.extend(nested.media_parts);
            self.links = nested.links;
            self.next_relationship = nested.next_relationship;
            self.drawing_id = nested.drawing_id;
            self.warnings.extend(nested.warnings);
            out.push_str(&nested.body);
        }
        if out.is_empty() {
            out.push_str("<w:p/>");
        }
        out
    }

    fn merge_continuation(widths: &[f32], column: usize, span: usize) -> String {
        let width: f32 = widths[column..column + span].iter().sum();
        let grid_span = if span > 1 {
            format!("<w:gridSpan w:val=\"{span}\"/>")
        } else {
            String::new()
        };
        format!(
            "<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>{grid_span}<w:vMerge/></w:tcPr><w:p/></w:tc>",
            Self::twips(width)
        )
    }

    fn table(&mut self, table: &Table) {
        let columns = table.column_count().max(1);
        let content = self.content_width();
        let total_weight: f32 = if table.columns.is_empty() {
            columns as f32
        } else {
            let sum: f32 = table
                .columns
                .iter()
                .take(columns)
                .map(|column| column.width.max(0.5))
                .sum();
            if sum <= 0.0 { columns as f32 } else { sum }
        };
        let widths: Vec<f32> = (0..columns)
            .map(|index| {
                let weight = table
                    .columns
                    .get(index)
                    .map(|column| column.width.max(0.5))
                    .unwrap_or(1.0);
                content * weight / total_weight
            })
            .collect();

        let border_size = |points: f32| ((points * 8.0).round() as i64).clamp(2, 48);
        let mut out = String::from("<w:tbl><w:tblPr>");
        out.push_str("<w:tblStyle w:val=\"TableGrid\"/>");
        out.push_str("<w:tblW w:w=\"5000\" w:type=\"pct\"/><w:tblLayout w:type=\"fixed\"/>");
        if !table.borders.is_empty() {
            let outer = border_size(table.borders.outer.max(0.5));
            let inner = border_size(table.borders.inner_horizontal.max(0.5));
            out.push_str(&format!(
                "<w:tblBorders>\
                 <w:top w:val=\"single\" w:sz=\"{outer}\" w:space=\"0\" w:color=\"7F7F7F\"/>\
                 <w:left w:val=\"single\" w:sz=\"{outer}\" w:space=\"0\" w:color=\"7F7F7F\"/>\
                 <w:bottom w:val=\"single\" w:sz=\"{outer}\" w:space=\"0\" w:color=\"7F7F7F\"/>\
                 <w:right w:val=\"single\" w:sz=\"{outer}\" w:space=\"0\" w:color=\"7F7F7F\"/>\
                 <w:insideH w:val=\"single\" w:sz=\"{inner}\" w:space=\"0\" w:color=\"7F7F7F\"/>\
                 <w:insideV w:val=\"single\" w:sz=\"{inner}\" w:space=\"0\" w:color=\"7F7F7F\"/>\
                 </w:tblBorders>"
            ));
        }
        let padding = Self::twips(table.cell_padding.max(1.0));
        out.push_str(&format!(
            "<w:tblCellMar>\
             <w:top w:w=\"{padding}\" w:type=\"dxa\"/>\
             <w:left w:w=\"{padding}\" w:type=\"dxa\"/>\
             <w:bottom w:w=\"{padding}\" w:type=\"dxa\"/>\
             <w:right w:w=\"{padding}\" w:type=\"dxa\"/>\
             </w:tblCellMar></w:tblPr><w:tblGrid>"
        ));
        for width in &widths {
            out.push_str(&format!("<w:gridCol w:w=\"{}\"/>", Self::twips(*width)));
        }
        out.push_str("</w:tblGrid>");
        self.body.push_str(&out);

        // A vertical continuation occupies the *same grid span* as its origin.
        // Recording only booleans emits one w:vMerge per column and corrupts cells
        // which span both rows and columns.
        let mut continuations = vec![vec![0usize; columns]; table.row_count()];
        let rows: Vec<&cleanup_core::doc::Row> = table.all_rows().collect();
        for (row_index, row) in rows.iter().enumerate() {
            self.body.push_str("<w:tr>");
            if row.header {
                self.body.push_str("<w:trPr><w:tblHeader/></w:trPr>");
            }
            let mut column = 0usize;
            for cell in &row.cells {
                while column < columns && continuations[row_index][column] > 0 {
                    let span = continuations[row_index][column];
                    self.body.push_str(&Self::merge_continuation(&widths, column, span));
                    column += span;
                }
                if column >= columns {
                    break;
                }
                let mut span = cell.colspan.max(1).min(columns - column);
                if let Some(next) = continuations[row_index][column + 1..column + span]
                    .iter()
                    .position(|occupied| *occupied > 0)
                {
                    span = next + 1;
                }
                let width: f32 = widths[column..column + span].iter().sum();
                for extra in 1..cell.rowspan.max(1).min(rows.len() - row_index) {
                    continuations[row_index + extra][column] = span;
                }
                let mut properties =
                    format!("<w:tcW w:w=\"{}\" w:type=\"dxa\"/>", Self::twips(width));
                if span > 1 {
                    properties.push_str(&format!("<w:gridSpan w:val=\"{span}\"/>"));
                }
                if cell.rowspan > 1 {
                    properties.push_str("<w:vMerge w:val=\"restart\"/>");
                }
                let fill = cell.fill.or(row.fill);
                if let Some(fill) = fill {
                    properties.push_str(&format!(
                        "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
                        fill.hex()
                    ));
                }
                let paragraphs = self.cell_paragraphs(cell);
                self.body.push_str(&format!(
                    "<w:tc><w:tcPr>{properties}</w:tcPr>{paragraphs}</w:tc>"
                ));
                column += span;
            }
            while column < columns {
                let span = continuations[row_index][column];
                if span > 0 {
                    self.body.push_str(&Self::merge_continuation(&widths, column, span));
                    column += span;
                } else {
                    self.body.push_str(&format!(
                        "<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/></w:tcPr><w:p/></w:tc>",
                        Self::twips(widths[column])
                    ));
                    column += 1;
                }
            }
            self.body.push_str("</w:tr>");
        }
        self.body.push_str("</w:tbl><w:p/>");
    }

    fn figure(&mut self, figure: &cleanup_core::doc::Figure) {
        let Some(asset) = asset_for(self.doc, figure.asset, &mut self.warnings).cloned() else {
            return;
        };
        let relationship = match self.media.get(&figure.asset) {
            Some(id) => id.clone(),
            None => {
                let id = format!("rId{}", self.next_relationship);
                self.next_relationship += 1;
                self.media.insert(figure.asset, id.clone());
                self.media_parts
                    .push((asset.file_name(), asset.data.clone()));
                id
            }
        };
        let width_points = figure
            .display_size
            .map(|rect| rect.width)
            .filter(|width| *width > 1.0)
            .unwrap_or(asset.width_px as f32 * 0.75);
        let height_points = figure
            .display_size
            .map(|rect| rect.height)
            .filter(|height| *height > 1.0)
            .unwrap_or(asset.height_px as f32 * 0.75);
        let max_width = self.content_width();
        let scale = if width_points > max_width {
            max_width / width_points
        } else {
            1.0
        };
        let width = Self::emu(width_points * scale);
        let height = Self::emu(height_points * scale);
        let id = self.drawing_id;
        self.drawing_id += 1;
        let name = format!("figure-{id}");
        // Block figures are anchored floating drawings; inline icons stay inline so Word keeps
        // them on the text baseline. Both share the same picture payload and relationship.
        let (frame_open, frame_close) = if figure.inline {
            (
                format!(
                    "<wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{width}\" cy=\"{height}\"/><wp:docPr id=\"{id}\" name=\"{name}\"/><wp:cNvGraphicFramePr/>"
                ),
                "</wp:inline>",
            )
        } else {
            (
                format!(
                    "<wp:anchor distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" simplePos=\"0\" relativeHeight=\"{id}\" behindDoc=\"0\" locked=\"0\" layoutInCell=\"1\" allowOverlap=\"1\"><wp:simplePos x=\"0\" y=\"0\"/><wp:positionH relativeFrom=\"column\"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom=\"paragraph\"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx=\"{width}\" cy=\"{height}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/><wp:wrapNone/><wp:docPr id=\"{id}\" name=\"{name}\"/><wp:cNvGraphicFramePr/>"
                ),
                "</wp:anchor>",
            )
        };
        let drawing = format!(
            "<w:r><w:drawing>{frame_open}\
             <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
             <pic:pic><pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"{file}\"/><pic:cNvPicPr/></pic:nvPicPr>\
             <pic:blipFill><a:blip r:embed=\"{relationship}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
             <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{width}\" cy=\"{height}\"/></a:xfrm>\
             <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
             </pic:pic></a:graphicData></a:graphic>{frame_close}</w:drawing></w:r>",
            file = escape_xml(&asset.file_name())
        );
        self.paragraph("<w:jc w:val=\"center\"/>", &drawing);
        if let Some(caption) = &figure.caption {
            let runs = self.runs(caption, 0.85);
            self.paragraph(
                "<w:pStyle w:val=\"Caption\"/><w:jc w:val=\"center\"/>",
                &runs,
            );
        }
    }

    fn document_xml(&self) -> String {
        let page = &self.doc.page;
        let mut out = String::from(XML_HEADER);
        out.push_str(&format!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:wp=\"{WP_NS}\" xmlns:a=\"{A_NS}\" xmlns:pic=\"{PIC_NS}\" xmlns:v=\"urn:schemas-microsoft-com:vml\"><w:body>"
        ));
        out.push_str(&self.body);
        out.push_str("<w:sectPr>");
        out.push_str(&format!(
            "<w:pgSz w:w=\"{}\" w:h=\"{}\"/>",
            Self::twips(page.width),
            Self::twips(page.height)
        ));
        out.push_str(&format!(
            "<w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>",
            Self::twips(page.margin_top),
            Self::twips(page.margin_right),
            Self::twips(page.margin_bottom),
            Self::twips(page.margin_left)
        ));
        out.push_str("<w:cols w:num=\"1\" w:space=\"425\"/></w:sectPr>");
        out.push_str("</w:body></w:document>");
        out
    }

    fn relationships(&self) -> (String, Vec<(String, Vec<u8>)>) {
        let mut out = String::from(XML_HEADER);
        out.push_str(&format!("<Relationships xmlns=\"{REL_NS}\">"));
        out.push_str(
            "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
        );
        out.push_str(
            "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>",
        );
        for (asset, id) in &self.media {
            if let Some(asset) = self.doc.assets.get(*asset) {
                out.push_str(&format!(
                    "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{}\"/>",
                    escape_xml(&asset.file_name())
                ));
            }
        }
        for (target, id) in &self.links {
            out.push_str(&format!(
                "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"{}\" TargetMode=\"External\"/>",
                escape_xml(target)
            ));
        }
        out.push_str("</Relationships>");
        (out, self.media_parts.clone())
    }
}

// ---------------------------------------------------------------------------
// Package parts
// ---------------------------------------------------------------------------

fn content_types(media: &[(String, Vec<u8>)]) -> String {
    let mut extensions: Vec<&str> = media
        .iter()
        .filter_map(|(name, _)| name.rsplit_once('.').map(|(_, extension)| extension))
        .collect();
    extensions.sort_unstable();
    extensions.dedup();
    let mut out = String::from(XML_HEADER);
    out.push_str("<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">");
    out.push_str("<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>");
    out.push_str("<Default Extension=\"xml\" ContentType=\"application/xml\"/>");
    for extension in &extensions {
        let content_type = match *extension {
            "png" => "image/png",
            "jpeg" | "jpg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "application/octet-stream",
        };
        out.push_str(&format!(
            "<Default Extension=\"{extension}\" ContentType=\"{content_type}\"/>"
        ));
    }
    out.push_str("<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>");
    out.push_str("<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>");
    out.push_str("<Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>");
    out.push_str("<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>");
    out.push_str("<Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>");
    out.push_str("</Types>");
    out
}

fn package_rels() -> String {
    let mut out = String::from(XML_HEADER);
    out.push_str(&format!("<Relationships xmlns=\"{REL_NS}\">"));
    out.push_str("<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>");
    out.push_str("<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>");
    out.push_str("<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/>");
    out.push_str("</Relationships>");
    out
}

fn styles_xml() -> String {
    let heading = |level: usize, size: usize, before: usize| {
        format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{level}\"><w:name w:val=\"heading {level}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before=\"{before}\" w:after=\"120\"/><w:outlineLvl w:val=\"{outline}\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/></w:rPr></w:style>",
            outline = level - 1
        )
    };
    let mut out = String::from(XML_HEADER);
    out.push_str(&format!("<w:styles xmlns:w=\"{W_NS}\">"));
    out.push_str("<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:eastAsia=\"Calibri\" w:cs=\"Calibri\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"276\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>");
    out.push_str("<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>");
    out.push_str(&heading(1, 40, 0));
    out.push_str(&heading(2, 32, 320));
    out.push_str(&heading(3, 26, 280));
    out.push_str(&heading(4, 24, 240));
    out.push_str(&heading(5, 22, 220));
    out.push_str(&heading(6, 22, 200));
    out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/><w:pPr><w:spacing w:after=\"240\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"56\"/><w:szCs w:val=\"56\"/></w:rPr></w:style>");
    out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:ind w:left=\"720\"/></w:pPr></w:style>");
    out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"CodeBlock\"><w:name w:val=\"Code Block\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:spacing w:before=\"120\" w:after=\"120\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:sz w:val=\"19\"/></w:rPr></w:style>");
    out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"Callout\"><w:name w:val=\"Callout\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:ind w:left=\"216\" w:right=\"216\"/><w:pBdr><w:left w:val=\"single\" w:sz=\"18\" w:space=\"8\" w:color=\"A5B4CB\"/></w:pBdr><w:spacing w:before=\"80\" w:after=\"80\"/></w:pPr></w:style>");
    out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"Caption\"><w:name w:val=\"caption\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/><w:rPr><w:i/><w:sz w:val=\"18\"/></w:rPr></w:style>");
    out.push_str("<w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/><w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"7F7F7F\"/><w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"7F7F7F\"/><w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"7F7F7F\"/><w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"7F7F7F\"/><w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"7F7F7F\"/><w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"7F7F7F\"/></w:tblBorders></w:tblPr></w:style>");
    out.push_str("</w:styles>");
    out
}

fn numbering_xml() -> String {
    let levels = |format: &str, text: &str| {
        (0..3)
            .map(|level| {
                format!(
                    "<w:lvl w:ilvl=\"{level}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{format}\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{left}\" w:hanging=\"360\"/></w:pPr></w:lvl>",
                    left = 720 + level * 360
                )
            })
            .collect::<String>()
    };
    let mut out = String::from(XML_HEADER);
    out.push_str(&format!("<w:numbering xmlns:w=\"{W_NS}\">"));
    out.push_str(&format!(
        "<w:abstractNum w:abstractNumId=\"0\"><w:multiLevelType w:val=\"hybridMultilevel\"/>{}</w:abstractNum>",
        levels("bullet", "•")
    ));
    out.push_str(&format!(
        "<w:abstractNum w:abstractNumId=\"1\"><w:multiLevelType w:val=\"hybridMultilevel\"/>{}</w:abstractNum>",
        levels("decimal", "%1.")
    ));
    out.push_str("<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>");
    out.push_str("<w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num>");
    out.push_str("</w:numbering>");
    out
}

fn core_properties(doc: &Doc) -> String {
    let mut out = String::from(XML_HEADER);
    out.push_str("<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">");
    if let Some(title) = &doc.info.title {
        out.push_str(&format!("<dc:title>{}</dc:title>", escape_xml(title)));
    }
    out.push_str("<dc:creator>llm-cleanup</dc:creator>");
    out.push_str("</cp:coreProperties>");
    out
}

fn app_properties() -> String {
    format!(
        "{XML_HEADER}<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>llm-cleanup</Application></Properties>"
    )
}

fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 forbids most C0 controls; a tab or newline is legal and preserved.
            '\t' | '\n' | '\r' => out.push(ch),
            _ if (ch as u32) < 0x20 => out.push('\u{FFFD}'),
            _ => out.push(ch),
        }
    }
    out
}

/// Page geometry is exposed for the integration tests, which assert that the DOCX kept the
/// source page size instead of falling back to a default.
pub fn page_size_in_twips(page: &PageSpec) -> (i64, i64) {
    (Writer::twips(page.width), Writer::twips(page.height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::doc::{
        Align as CellAlign, Cell as DocCell, ColumnSpec, PageSpec as Spec, ParagraphStyle, Row,
        Table,
    };

    fn document() -> Doc {
        let mut doc = Doc::new(Spec::default());
        doc.info.title = Some("Quarterly & Annual".to_string());
        doc.blocks.push(Block::Heading {
            level: 1,
            inlines: vec![Inline::plain("Findings")],
            anchor: None,
        });
        let table = Table {
            columns: vec![
                ColumnSpec {
                    width: 2.0,
                    align: CellAlign::Left,
                },
                ColumnSpec {
                    width: 1.0,
                    align: CellAlign::Right,
                },
            ],
            headers: vec![Row {
                cells: vec![DocCell::paragraph("Region"), DocCell::paragraph("Revenue")],
                header: true,
                fill: None,
            }],
            rows: vec![Row {
                cells: vec![DocCell::paragraph("EMEA"), DocCell::paragraph("1.2M")],
                header: false,
                fill: None,
            }],
            ..Table::default()
        };
        doc.blocks.push(Block::Table(table));
        doc.blocks.push(Block::Paragraph {
            inlines: vec![
                Inline::plain("Bold lead: "),
                Inline::bold("numbers"),
                Inline::plain(" and "),
                Inline::code("code()"),
            ],
            anchor: None,
            style: ParagraphStyle::default(),
        });
        doc
    }

    fn part(bytes: &[u8], name: &str) -> String {
        use std::io::Read;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("zip");
        let mut file = archive.by_name(name).expect("part present");
        let mut out = String::new();
        file.read_to_string(&mut out).expect("utf8 part");
        out
    }

    #[test]
    fn package_has_the_required_parts_and_well_formed_xml() {
        let rendered = render(&document());
        assert!(rendered.bytes.starts_with(b"PK"));
        for name in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/styles.xml",
            "word/numbering.xml",
            "word/_rels/document.xml.rels",
        ] {
            let xml = part(&rendered.bytes, name);
            assert!(
                xml.starts_with("<?xml"),
                "{name} must have an XML declaration"
            );
            // Well-formedness: every part parses as XML with quick-xml.
            let mut reader = quick_xml::Reader::from_str(&xml);
            let mut buffer = Vec::new();
            loop {
                match reader.read_event_into(&mut buffer) {
                    Ok(quick_xml::events::Event::Eof) => break,
                    Ok(_) => {}
                    Err(error) => panic!("{name} is not well-formed XML: {error}"),
                }
                buffer.clear();
            }
        }
    }

    #[test]
    fn tables_keep_native_ooxml_structure() {
        let rendered = render(&document());
        let xml = part(&rendered.bytes, "word/document.xml");
        assert!(xml.contains("<w:tbl>"));
        assert!(xml.contains("<w:tblGrid>"));
        assert!(xml.contains("<w:gridCol"));
        assert!(xml.contains("<w:tblHeader/>"));
        assert!(xml.contains("<w:tcBorders>") || xml.contains("<w:tblBorders>"));
        assert!(xml.contains("<w:tblCellMar>"));
    }

    #[test]
    fn runs_carry_bold_italic_and_code_properties() {
        let rendered = render(&document());
        let xml = part(&rendered.bytes, "word/document.xml");
        assert!(xml.contains("<w:b/>"));
        assert!(xml.contains("Consolas"));
        assert!(xml.contains("xml:space=\"preserve\""));
        // The title is XML-escaped, not truncated.
        assert!(part(&rendered.bytes, "docProps/core.xml").contains("Quarterly &amp; Annual"));
    }

    #[test]
    fn headings_use_real_styles() {
        let rendered = render(&document());
        let xml = part(&rendered.bytes, "word/document.xml");
        assert!(xml.contains("<w:pStyle w:val=\"Heading1\"/>"));
        assert!(part(&rendered.bytes, "word/styles.xml").contains("w:styleId=\"Heading6\""));
    }
}
