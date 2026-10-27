//! Convert to DOCX. Markdown, plain text and HTML go through `docx-rs`; PDF uses a
//! hand-built `.docx` package (zip + OPC XML) so embedded JPEG images, page size,
//! headings, lists and tables survive the conversion.

use std::io::{Cursor, Write};

use crate::table_regions;
use crate::{ConvertError, ConvertReport, Result};
use cleanup_core::FormatId;
use cleanup_formats::{
    LayoutImage, LayoutLink, LayoutSpan, PageLayout, body_font_size, cluster_lines,
};
use docx_rs::{Docx, Paragraph, Run};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use zip::write::SimpleFileOptions;

pub fn convert(from: FormatId, src: &[u8]) -> Result<(Vec<u8>, ConvertReport)> {
    if from == FormatId::Docx {
        return Ok((
            src.to_vec(),
            ConvertReport {
                engine: "passthrough".into(),
                lossy: false,
                warnings: vec![],
            },
        ));
    }

    let (docx, warnings) = match from {
        FormatId::PlainText => (text_to_docx(src), Vec::new()),
        FormatId::Markdown => markdown_to_docx(src)?,
        FormatId::Html => {
            let paras = crate::to_text::html_paragraph_texts(src)?;
            let mut docx = Docx::new();
            for p in paras {
                docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_text(&p)));
            }
            (
                docx,
                vec!["extracted HTML blocks as plain DOCX paragraphs".to_string()],
            )
        }
        FormatId::Pdf => {
            let bytes = pdf_to_docx(src)?;
            return Ok((
                bytes,
                ConvertReport {
                    engine: "pdf-layout".into(),
                    lossy: true,
                    warnings: vec![
                        "converted from positioned text and drawings; the exact pixel layout of the source is approximated".to_string(),
                        "embedded JPEG images are embedded as image media".to_string(),
                    ],
                },
            ));
        }
        FormatId::Image => {
            return Err(ConvertError::Unsupported {
                from: "image",
                to: "docx",
            });
        }
        FormatId::Docx => unreachable!(),
    };

    let mut cursor = Cursor::new(Vec::new());
    docx.build()
        .pack(&mut cursor)
        .map_err(|e| ConvertError::Failed(format!("writing .docx: {e}")))?;
    Ok((
        cursor.into_inner(),
        ConvertReport {
            engine: "docx-rs".into(),
            lossy: true,
            warnings,
        },
    ))
}

fn text_to_docx(src: &[u8]) -> Docx {
    let s = String::from_utf8_lossy(src);
    let mut docx = Docx::new();
    for line in s.lines() {
        docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_text(line)));
    }
    docx
}

fn heading_num(l: HeadingLevel) -> usize {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn markdown_to_docx(src: &[u8]) -> Result<(Docx, Vec<String>)> {
    let s =
        std::str::from_utf8(src).map_err(|e| ConvertError::Failed(format!("not UTF-8: {e}")))?;
    let mut docx = Docx::new();
    let mut warnings: Vec<String> = Vec::new();

    let mut para = Paragraph::new();
    let mut has_content = false;
    let mut heading: Option<usize> = None;
    let mut bold = false;
    let mut italic = false;
    let mut list_depth = 0usize;
    let mut item_open = false;

    for ev in Parser::new_ext(s, crate::md_options()) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => heading = Some(heading_num(level)),
            Event::End(TagEnd::Heading(_)) => {
                if has_content {
                    if let Some(h) = heading {
                        para = para.style(&format!("Heading{h}"));
                    }
                    docx = docx.add_paragraph(std::mem::take(&mut para));
                }
                para = Paragraph::new();
                has_content = false;
                heading = None;
            }
            Event::End(TagEnd::Paragraph) | Event::End(TagEnd::CodeBlock) => {
                if has_content {
                    docx = docx.add_paragraph(std::mem::take(&mut para));
                }
                para = Paragraph::new();
                has_content = false;
            }
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::Start(Tag::Item) => item_open = true,
            Event::End(TagEnd::Item) => {
                if has_content {
                    docx = docx.add_paragraph(std::mem::take(&mut para));
                }
                para = Paragraph::new();
                has_content = false;
                item_open = false;
            }
            Event::Text(t) | Event::Code(t) => {
                let text = if item_open && !has_content {
                    item_open = false;
                    format!("{}• {t}", "    ".repeat(list_depth.saturating_sub(1)))
                } else {
                    t.to_string()
                };
                let mut run = Run::new().add_text(text);
                if bold {
                    run = run.bold();
                }
                if italic {
                    run = run.italic();
                }
                para = para.add_run(run);
                has_content = true;
            }
            Event::SoftBreak | Event::HardBreak => {
                para = para.add_run(Run::new().add_text(" "));
                has_content = true;
            }
            Event::Start(Tag::Table(_)) => {
                warnings.push("tables are dropped in Markdown→DOCX".into())
            }
            Event::Start(Tag::Image { .. }) => {
                warnings.push("images are dropped in Markdown→DOCX".into())
            }
            _ => {}
        }
    }
    if has_content {
        docx = docx.add_paragraph(para);
    }
    Ok((docx, warnings))
}

/// Convert a PDF's positioned text into a real `.docx` package (zip + OPC XML):
/// headings, bold/italic runs, lists, tables and embedded JPEG images, with the
/// page size taken from the PDF's media box.
fn pdf_to_docx(src: &[u8]) -> Result<Vec<u8>> {
    let layout = cleanup_formats::pdf_layout(src)
        .map_err(|e| ConvertError::Failed(format!("PDF layout extraction failed: {e}")))?;
    let body_size = body_font_size(
        &layout
            .pages
            .iter()
            .flat_map(|page| page.spans.iter().cloned())
            .collect::<Vec<LayoutSpan>>(),
    );
    let page_size = layout
        .pages
        .first()
        .map(|page| (page.width, page.height))
        .unwrap_or((612.0, 792.0));

    let mut zip = Vec::new();
    let result = build_zip(&mut zip, &layout, body_size, page_size);
    result.map_err(|e| ConvertError::Failed(format!("writing .docx package: {e}")))?;
    Ok(zip)
}

fn build_zip(
    out: &mut Vec<u8>,
    layout: &cleanup_formats::PdfLayout,
    body_size: f64,
    page_size: (f64, f64),
) -> zip::result::ZipResult<()> {
    let mut writer = zip::ZipWriter::new(Cursor::new(out));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(content_types_xml().as_bytes())?;

    writer.start_file("_rels/.rels", options)?;
    writer.write_all(package_rels_xml().as_bytes())?;

    // Collect embedded images in placement order.
    let mut media: Vec<&LayoutImage> = Vec::new();
    for page in &layout.pages {
        for image in &page.images {
            media.push(image);
        }
    }

    for (index, image) in media.iter().enumerate() {
        let name = format!("word/media/image{}.jpeg", index + 1);
        writer.start_file(&name, options)?;
        writer.write_all(&image.bytes)?;
    }

    let blocks: Vec<Block<'_>> = layout
        .pages
        .iter()
        .flat_map(|page| page_blocks(page))
        .collect();
    let media_ids: Vec<&LayoutImage> = dedup_media(&blocks);

    writer.start_file("word/document.xml", options)?;
    writer.write_all(document_xml(layout, body_size, page_size, &media_ids).as_bytes())?;

    writer.start_file("word/_rels/document.xml.rels", options)?;
    writer.write_all(document_rels_xml(&media_ids).as_bytes())?;

    writer.start_file("word/styles.xml", options)?;
    writer.write_all(styles_xml().as_bytes())?;

    writer.finish()?;
    Ok(())
}

/// One block of a page, in reading order. Images borrow the layout so their
/// identity (and therefore their relationship id) survives unchanged.
enum Block<'a> {
    Lines(Vec<LayoutSpan>),
    Table(Vec<Vec<LayoutSpan>>),
    Image(&'a LayoutImage),
}

/// Reading-order blocks for one page: text lines, tables and images interleaved
/// by vertical position.
fn page_blocks(page: &PageLayout) -> Vec<Block<'_>> {
    let mut blocks: Vec<Block<'_>> = Vec::new();
    let lines = cluster_lines(&page.spans);
    let regions = table_regions(&lines);
    let mut images = page.images.iter().peekable();
    let mut region_index = 0usize;
    for (line_index, line) in lines.iter().enumerate() {
        while images
            .peek()
            .is_some_and(|image| image.y < line_center_y(line))
        {
            blocks.push(Block::Image(images.next().unwrap()));
        }
        if region_index < regions.len() && regions[region_index].start == line_index {
            let region = regions[region_index];
            blocks.push(Block::Table(lines[region.start..region.end].to_vec()));
            region_index += 1;
            continue;
        }
        blocks.push(Block::Lines(line.clone()));
    }
    while let Some(image) = images.next() {
        blocks.push(Block::Image(image));
    }
    blocks
}

/// The vertical center of a line of text.
fn line_center_y(line: &[LayoutSpan]) -> f64 {
    let mut top = f64::INFINITY;
    let mut bottom = 0.0f64;
    for span in line {
        top = top.min(span.y);
        bottom = bottom.max(span.y + span.font_size);
    }
    (top + bottom) / 2.0
}

/// The images referenced by `blocks`, in first-appearance order.
fn dedup_media<'a>(blocks: &[Block<'a>]) -> Vec<&'a LayoutImage> {
    let mut out: Vec<&'a LayoutImage> = Vec::new();
    for block in blocks {
        if let Block::Image(image) = block {
            if !out.iter().any(|p| std::ptr::eq(p, image)) {
                out.push(image);
            }
        }
    }
    out
}

/// Join the spans of one line into raw text.
fn line_text(line: &[LayoutSpan]) -> String {
    let mut raw = String::new();
    for span in line {
        if !raw.is_empty() && !raw.ends_with(' ') && !span.text.starts_with(' ') {
            raw.push(' ');
        }
        raw.push_str(&span.text);
    }
    collapse_whitespace(&raw)
}

fn list_marker(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim_start();
    let bullet = "•·*-o▪‣";
    if let Some(first) = trimmed.chars().next() {
        if bullet.contains(first) {
            if let Some(second) = trimmed.chars().nth(1) {
                if second.is_ascii_whitespace() {
                    let rest = trimmed[first.len_utf8()..].trim_start();
                    return Some(("- ".to_string(), rest.to_string()));
                }
            }
        }
    }
    let digits_end = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits_end > 0 && digits_end < trimmed.len() {
        let rest = &trimmed[digits_end..];
        if let Some(rest) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
            let rest = rest.trim_start();
            if !rest.is_empty() {
                let number = &trimmed[..digits_end];
                return Some((format!("{number}. "), rest.to_string()));
            }
        }
    }
    None
}

/// Render the pages' blocks into document.xml body XML.
fn document_xml(
    layout: &cleanup_formats::PdfLayout,
    body_size: f64,
    (page_width, page_height): (f64, f64),
    media_ids: &[&LayoutImage],
) -> String {
    let mut out = String::new();
    out.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"\
          xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"\
          xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\"\
          xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"\
          xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <w:body>",
    );
    let content_width_pt = (page_width - 90.0).max(72.0);
    let content_width_twips = (content_width_pt * 20.0) as i32;
    for page in &layout.pages {
        for block in page_blocks(page) {
            match block {
                Block::Lines(line) => {
                    out.push_str(&paragraph_xml(&line, body_size, &page.links));
                }
                Block::Table(rows) => {
                    out.push_str(&table_xml(&rows, content_width_twips));
                }
                Block::Image(image) => {
                    out.push_str(&image_xml(image, media_ids, page_width));
                }
            }
        }
    }
    let twips = |points: f64| (points * 20.0) as i32;
    out.push_str(&format!(
        "<w:sectPr>\
         <w:pgSz w:w=\"{}\" w:h=\"{}\" w:orient=\"portrait\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1800\" w:bottom=\"1440\" w:left=\"1800\"\
          w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>\
         </w:sectPr>",
        twips(page_width),
        twips(page_height),
    ));
    out.push_str("</w:body></w:document>");
    out
}

/// Render one visual line of text as one paragraph.
fn paragraph_xml(line: &[LayoutSpan], body_size: f64, links: &[LayoutLink]) -> String {
    let raw = line_text(line);
    let (prefix, _body) = match list_marker(&raw) {
        Some((marker, rest)) => (marker, rest),
        None => (String::new(), raw),
    };

    let max_size = line
        .iter()
        .map(|span| span.font_size)
        .fold(0.0f64, f64::max);
    let any_bold = line.iter().any(|span| span.bold);
    let level = if max_size > body_size * 1.5 {
        1
    } else if max_size > body_size * 1.25 {
        2
    } else if max_size > body_size * 1.10 {
        3
    } else if any_bold && max_size > body_size * 1.03 {
        3
    } else {
        0
    };

    let mut ppr = String::new();
    if level > 0 {
        ppr.push_str(&format!("<w:pStyle w:val=\"Heading{level}\"/>"));
    } else if !prefix.is_empty() {
        ppr.push_str(
            "<w:pStyle w:val=\"ListParagraph\"/><w:ind w:left=\"720\" w:hanging=\"180\"/>",
        );
    }

    let mut out = String::from("<w:p>");
    if !ppr.is_empty() {
        out.push_str(&format!("<w:pPr>{ppr}</w:pPr>"));
    }
    out.push_str("<w:r><w:rPr><w:rFonts w:ascii=\"Inter\" w:hAnsi=\"Inter\"/><w:sz w:val=\"22\"/></w:rPr><w:t xml:space=\"preserve\">");
    out.push_str(&escape_xml(&prefix));
    out.push_str("</w:t></w:r>");
    for (index, span) in line.iter().enumerate() {
        let mut text = span.text.trim().to_string();
        if index == 0 {
            if let Some((_, rest)) = list_marker(&span.text) {
                text = rest.trim().to_string();
            }
        }
        if let Some(uri) = link_uri(links, span) {
            text = format!("[{}]({})", text, uri);
        }
        if text.is_empty() {
            continue;
        }
        let mut rpr = String::from("<w:rFonts w:ascii=\"Inter\" w:hAnsi=\"Inter\"/>");
        if span.bold {
            rpr.push_str("<w:b/><w:bCs/>");
        }
        if span.italic {
            rpr.push_str("<w:i/><w:iCs/>");
        }
        let size = (span.font_size * 2.0) as i32;
        if (size as f64 / 2.0 - body_size).abs() > 0.5 {
            rpr.push_str(&format!("<w:sz w:val=\"{}\"/>", size));
        }
        out.push_str(&format!(
            "<w:r><w:rPr>{rpr}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>",
            escape_xml(&text)
        ));
    }
    out.push_str("</w:p>");
    out
}

fn table_xml(rows: &[Vec<LayoutSpan>], total_width_twips: i32) -> String {
    let mut centers: Vec<f64> = Vec::new();
    for row in rows {
        for span in row {
            if centers.iter().all(|c| (c - span.x).abs() > 4.0) {
                centers.push(span.x);
            }
        }
    }
    centers.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Less));
    let col_width = total_width_twips.max(1) / centers.len().max(1) as i32;
    let mut out = String::from("<w:tbl><w:tblPr><w:tblW w:type=\"dxa\"");
    out.push_str(&format!(" w=\"{total_width_twips}\"/>"));
    out.push_str(
        "<w:tblBorders>\
         <w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
         <w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
         <w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
         <w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
         <w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
         <w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
         </w:tblBorders></w:tblPr><w:tblGrid>",
    );
    for _ in &centers {
        out.push_str(&format!("<w:gridCol w=\"{col_width}\"/>"));
    }
    out.push_str("</w:tblGrid>");
    for row in rows {
        out.push_str("<w:tr>");
        for span in row {
            let text = span.text.trim();
            let rpr = if span.bold {
                "<w:rPr><w:b/><w:bCs/></w:rPr>"
            } else {
                "<w:rPr/>"
            };
            out.push_str(&format!(
                "<w:tc><w:p><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:r>{rpr}<w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:tc>",
                escape_xml(text)
            ));
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
    out
}

fn image_xml(image: &LayoutImage, media_ids: &[&LayoutImage], page_width: f64) -> String {
    let position = media_ids.iter().position(|p| std::ptr::eq(*p, image));
    let Some(position) = position else {
        return String::new();
    };
    let rel_id = format!("rId{}", position + 1);
    let content_width_pt = (page_width - 90.0).max(72.0);
    let mut width_pt = image.width;
    let mut height_pt = image.height;
    if width_pt <= 0.0 {
        width_pt = 72.0;
    }
    if height_pt <= 0.0 {
        height_pt = 72.0;
    }
    if width_pt > content_width_pt {
        height_pt = height_pt * content_width_pt / width_pt;
        width_pt = content_width_pt;
    }
    let emu = |pt: f64| (pt * 12700.0) as i32;
    let mut out = String::from(
        "<w:p><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">",
    );
    out.push_str(&format!(
        "<wp:extent cx=\"{}\" cy=\"{}\"/>",
        emu(width_pt),
        emu(height_pt)
    ));
    out.push_str("<wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>");
    out.push_str(&format!(
        "<wp:docPr id=\"{}\" name=\"image\"/>",
        position + 1
    ));
    out.push_str("<wp:cNvGraphicFramePr/>");
    out.push_str("<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">");
    out.push_str(&format!(
        "<pic:pic><pic:nvPicPr><pic:cNvPr id=\"{}\" name=\"image\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm></pic:spPr></pic:pic>",
        position + 1, rel_id, emu(width_pt), emu(height_pt)
    ));
    out.push_str("</a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>");
    out
}

/// The package-level relationships.
fn package_rels_xml() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
     <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
     <Relationship Id=\"rId0\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
     </Relationships>"
        .to_string()
}

/// The document-level relationships (images).
fn document_rels_xml(media_ids: &[&LayoutImage]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (index, _image) in media_ids.iter().enumerate() {
        out.push_str(&format!(
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image{}.jpeg\"/>",
            index + 1,
            index + 1
        ));
    }
    out.push_str("</Relationships>");
    out
}

/// The content types of the package.
fn content_types_xml() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
     <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
     <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
     <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
     <Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
     <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
     <Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
     </Types>"
        .to_string()
}

/// Minimal styles.xml so Heading1..Heading6 and ListParagraph references validate.
fn styles_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
     <w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
     <w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Inter\" w:hAnsi=\"Inter\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault></w:docDefaults>\
     <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:pPr><w:spacing w:after=\"200\" w:line=\"276\" w:lineRule=\"auto\"/></w:pPr></w:style>\
     <w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"Heading 1\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:link w:val=\"Heading1Char\"/><w:uiPriority w:val=\"9\"/><w:qformat/><w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"60\" w:lineRule=\"auto\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"40\"/></w:rPr></w:style>\
     <w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"Heading 2\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:link w:val=\"Heading2Char\"/><w:uiPriority w:val=\"9\"/><w:qformat/><w:pPr><w:keepNext/><w:spacing w:before=\"200\" w:after=\"60\" w:lineRule=\"auto\"/><w:outlineLvl w:val=\"1\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"32\"/></w:rPr></w:style>\
     <w:style w:type=\"paragraph\" w:styleId=\"Heading3\"><w:name w:val=\"Heading 3\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:link w:val=\"Heading3Char\"/><w:uiPriority w:val=\"9\"/><w:qformat/><w:pPr><w:keepNext/><w:spacing w:before=\"160\" w:after=\"60\" w:lineRule=\"auto\"/><w:outlineLvl w:val=\"2\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"26\"/></w:rPr></w:style>\
     <w:style w:type=\"paragraph\" w:styleId=\"Heading4\"><w:name w:val=\"Heading 4\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:link w:val=\"Heading4Char\"/><w:uiPriority w:val=\"9\"/><w:pPr><w:keepNext/><w:spacing w:before=\"120\" w:after=\"60\" w:lineRule=\"auto\"/><w:outlineLvl w:val=\"3\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"24\"/></w:rPr></w:style>\
     <w:style w:type=\"paragraph\" w:styleId=\"Heading5\"><w:name w:val=\"Heading 5\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:link w:val=\"Heading5Char\"/><w:uiPriority w:val=\"9\"/><w:pPr><w:keepNext/><w:spacing w:before=\"80\" w:after=\"60\" w:lineRule=\"auto\"/><w:outlineLvl w:val=\"4\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"22\"/></w:rPr></w:style>\
     <w:style w:type=\"paragraph\" w:styleId=\"Heading6\"><w:name w:val=\"Heading 6\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:link w:val=\"Heading6Char\"/><w:uiPriority w:val=\"9\"/><w:pPr><w:keepNext/><w:spacing w:before=\"40\" w:after=\"60\" w:lineRule=\"auto\"/><w:outlineLvl w:val=\"5\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"20\"/></w:rPr></w:style>\
     <w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:link w:val=\"TitleChar\"/><w:uiPriority w:val=\"10\"/><w:qformat/><w:pPr><w:keepNext/><w:spacing w:before=\"0\" w:after=\"60\" w:lineRule=\"auto\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:sz w:val=\"58\"/></w:rPr></w:style>\
     <w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"34\"/><w:pPr><w:ind w:left=\"720\" w:hanging=\"180\"/></w:pPr></w:style>\
     <w:style w:type=\"character\" w:styleId=\"Heading1Char\"><w:name w:val=\"Heading 1 Char\"/><w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"9\"/><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"40\"/></w:rPr></w:style>\
     <w:style w:type=\"character\" w:styleId=\"Heading2Char\"><w:name w:val=\"Heading 2 Char\"/><w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"9\"/><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"32\"/></w:rPr></w:style>\
     <w:style w:type=\"character\" w:styleId=\"Heading3Char\"><w:name w:val=\"Heading 3 Char\"/><w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"9\"/><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"26\"/></w:rPr></w:style>\
     <w:style w:type=\"character\" w:styleId=\"Heading4Char\"><w:name w:val=\"Heading 4 Char\"/><w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"9\"/><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"24\"/></w:rPr></w:style>\
     <w:style w:type=\"character\" w:styleId=\"Heading5Char\"><w:name w:val=\"Heading 5 Char\"/><w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"9\"/><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"22\"/></w:rPr></w:style>\
     <w:style w:type=\"character\" w:styleId=\"Heading6Char\"><w:name w:val=\"Heading 6 Char\"/><w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"9\"/><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"20\"/></w:rPr></w:style>\
     <w:style w:type=\"character\" w:styleId=\"TitleChar\"><w:name w:val=\"Title Char\"/><w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"10\"/><w:rPr><w:b/><w:bCs/><w:sz w:val=\"58\"/></w:rPr></w:style>\
     </w:styles>"
}

fn link_uri(links: &[LayoutLink], span: &LayoutSpan) -> Option<String> {
    let cx = span.x + (span.text.len() as f64) * span.font_size * 0.5;
    let cy = span.y + span.font_size / 2.0;
    links
        .iter()
        .find(|link| link.x0 <= cx && cx <= link.x1 && link.y0 <= cy && cy <= link.y1)
        .map(|link| link.uri.clone())
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::new();
    let mut previous_space = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !previous_space && !out.is_empty() {
                out.push(' ');
                previous_space = true;
            }
        } else {
            out.push(ch);
            previous_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
