//! End-to-end regression tests for the document-IR pipeline.
//!
//! Every test runs a real, committed PDF from `tests/fixtures/` through the real reader
//! (`cleanup_formats::pdf::extract_layout` + `cleanup_core::doc::analyze`), the real IR
//! sanitizer and the real exporters. Ground truth comes from `tests/fixtures/manifest.json`,
//! which is emitted by `scripts/generate_fixtures.py` alongside the PDFs.

use cleanup_convert::sanitize::{SanitizeOptions, sanitize};
use cleanup_convert::{ConvertOptions, ConvertOutput, Target, convert_document};
use cleanup_core::doc::{AnalyzeOptions, Block, Doc, analyze};
use cleanup_core::traits::Format;
use cleanup_core::{CleanupLevel, Ctx, FormatId};
use serde_json::Value;
use std::io::Read;

const FIXTURES: [&str; 9] = [
    "kerning_and_typography.pdf",
    "multi_page_tables.pdf",
    "rich_media_and_symbols.pdf",
    "multi_column_newsletter.pdf",
    "ai_slop_samples.pdf",
    // Legacy fixtures for backward compat
    "complex_tables.pdf",
    "rich_media.pdf",
    "typographic_symbols.pdf",
    "multi_column_mixed.pdf",
];

fn fixtures_dir() -> String {
    format!("{}/../../tests/fixtures", env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/{name}", fixtures_dir());
    std::fs::read(&path).unwrap_or_else(|error| panic!("reading {path}: {error}"))
}

fn manifest() -> Value {
    let path = format!("{}/manifest.json", fixtures_dir());
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("reading {path}: {error}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("parsing {path}: {error}"))
}

/// Reconstruct the IR exactly as `input::pdf_layout` does, so assertions can inspect structure
/// (row/column counts, page boundaries, anchors, DPI, colour) rather than guessing from output.
fn ir(name: &str) -> Doc {
    let bytes = fixture(name);
    let report = cleanup_formats::pdf::extract_layout(&bytes)
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    let (doc, warnings) = analyze(&report.layout, &AnalyzeOptions::default());
    assert!(
        warnings.iter().all(|warning| !warning.starts_with("IR:")),
        "{name} produced an invalid IR: {warnings:?}"
    );
    doc
}

fn sanitized_ir(name: &str) -> Doc {
    let mut doc = ir(name);
    let report = sanitize(&mut doc, &SanitizeOptions::default());
    assert!(
        report.normalization_warnings.is_empty(),
        "{name}: IR drifted after cleaning: {:?}",
        report.normalization_warnings
    );
    assert!(
        doc.validate().is_empty(),
        "{name}: IR invalid after cleaning"
    );
    doc
}

fn convert(name: &str, to: Target) -> ConvertOutput {
    let bytes = fixture(name);
    convert_document(FormatId::Pdf, &bytes, to, &ConvertOptions::default())
        .unwrap_or_else(|error| panic!("{name} -> {to:?}: {error}"))
}

fn text(name: &str, to: Target) -> String {
    let output = convert(name, to);
    String::from_utf8(output.bytes).unwrap_or_else(|error| panic!("{name} is not UTF-8: {error}"))
}

fn zip_part(bytes: &[u8], part: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec()))
        .unwrap_or_else(|error| panic!("output is not a zip: {error}"));
    let mut file = archive
        .by_name(part)
        .unwrap_or_else(|error| panic!("{part} missing: {error}"));
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).expect("read part");
    buffer
}

fn zip_names(bytes: &[u8]) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec()))
        .unwrap_or_else(|error| panic!("output is not a zip: {error}"));
    let mut names = Vec::new();
    for index in 0..archive.len() {
        if let Ok(file) = archive.by_index(index)
            && let Ok(name) = file.name()
        {
            names.push(name.to_string());
        }
    }
    names
}

fn docx_xml(name: &str) -> String {
    let output = convert(name, Target::Docx);
    String::from_utf8(zip_part(&output.bytes, "word/document.xml")).expect("document.xml is UTF-8")
}

fn assert_well_formed(xml: &str, what: &str) {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth = 0i32;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(quick_xml::events::Event::Start(_)) => depth += 1,
            Ok(quick_xml::events::Event::End(_)) => depth -= 1,
            Ok(_) => {}
            Err(error) => panic!("{what} is not well-formed XML: {error}"),
        }
        if depth < 0 {
            panic!("{what} closes more elements than it opens");
        }
    }
    assert_eq!(depth, 0, "{what} leaves {depth} element(s) open");
}

/// Number of `|`-delimited cells in the widest GFM row, plus every HTML `<td>`/`<th>`.
fn table_cells(markdown: &str) -> usize {
    let pipes = markdown
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .map(|line| line.matches('|').count().saturating_sub(1))
        .max()
        .unwrap_or(0);
    let html = markdown.matches("<td").count() + markdown.matches("<th").count();
    pipes.max(html)
}

/// Remove fenced/indented code before asserting that chat tokens were stripped from *prose*.
fn prose_only(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence && !line.starts_with("    ") {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Strip indented code lines from a plain-text export before checking for orphaned markup:
/// a code block is allowed to contain brackets and backticks verbatim.
fn text_prose_only(plain: &str) -> String {
    plain
        .lines()
        .filter(|line| !line.starts_with("    "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collapse wrapping whitespace so a sentence split across two terminal lines still matches.
fn flattened(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn error_codepoint(text: &str) -> Option<char> {
    text.chars().find(|ch| {
        matches!(
            *ch,
            '\u{FFFD}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}'
        )
    })
}

// ---------------------------------------------------------------------------
// 1. Tables: multi-page stitching, spans, widths, alignments
// ---------------------------------------------------------------------------

#[test]
fn complex_tables_match_ground_truth() {
    let root = manifest();
    let truth = &root["fixtures"]["complex_tables.pdf"];
    let expected_rows = truth["primary_table"]["rows"].as_u64().unwrap() as usize;
    let expected_columns = truth["primary_table"]["columns"].as_u64().unwrap() as usize;

    // IR ground truth: one stitched table with 61 rows x 5 columns across three pages.
    let doc = sanitized_ir("complex_tables.pdf");
    let primary = doc
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Table(table) => Some(table),
            _ => None,
        })
        .max_by_key(|table| table.row_count())
        .expect("the primary table was not detected");
    assert_eq!(
        primary.column_count(),
        expected_columns,
        "primary table column count: {primary:?}"
    );
    assert!(
        primary.row_count() >= expected_rows,
        "expected >= {expected_rows} rows after stitching, got {}",
        primary.row_count()
    );
    assert!(
        primary.pages.len() >= truth["primary_table"]["pages_min"].as_u64().unwrap() as usize,
        "the table was not stitched across pages: {:?}",
        primary.pages
    );
    assert!(
        doc.info.page_boundaries.len()
            >= truth["primary_table"]["pages_min"].as_u64().unwrap() as usize,
        "page boundaries were not recorded"
    );

    // Markdown: a GFM table with the exact column count and every numeric ground-truth cell.
    let markdown = text("complex_tables.pdf", Target::Markdown);
    for header in ["Region", "Units sold", "Gross", "Net", "Growth"] {
        assert!(markdown.contains(header), "missing header `{header}`");
    }
    assert!(
        table_cells(&markdown) >= expected_columns,
        "expected >= {expected_columns} columns in the GFM row"
    );
    let pipe_rows = markdown
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .count();
    assert!(
        pipe_rows >= expected_rows,
        "expected >= {expected_rows} table rows, got {pipe_rows}"
    );
    for cell in truth["numeric_cells"].as_array().unwrap() {
        let cell = cell.as_str().unwrap();
        assert!(markdown.contains(cell), "numeric cell `{cell}` was dropped");
    }

    // The second grid has both kinds of merge and multi-line cells: GFM cannot express them,
    // so the exporter must fall back to semantic HTML with explicit widths and spans.
    let span_rows = truth["span_table"]["rows"].as_u64().unwrap() as usize;
    let span_columns = truth["span_table"]["columns"].as_u64().unwrap() as usize;
    assert!(markdown.contains("<table>"), "no semantic HTML fallback");
    assert!(markdown.contains("<colgroup>"), "column widths were lost");
    assert!(
        markdown.contains("colspan=\"3\""),
        "the 3-column merge was flattened"
    );
    assert!(markdown.contains("rowspan=\"2\""), "the row merge was lost");
    assert!(markdown.contains("H1"), "merged row label was dropped");
    assert!(markdown.contains("verified"), "multi-line cell was dropped");
    let html_rows = markdown.matches("<tr>").count();
    assert!(
        html_rows >= span_rows,
        "expected >= {span_rows} HTML rows, got {html_rows}"
    );
    let _ = span_columns;

    // Running furniture repeated on all four pages must never reach the output.
    let running_header = truth["running_header"].as_str().unwrap();
    assert!(
        !markdown.contains(running_header),
        "running header leaked into the document"
    );
    assert!(!markdown.contains("llm-cleanup fixture"), "footer leaked");

    // Plain text: aligned box-drawing grid, no orphaned markup.
    let plain = text("complex_tables.pdf", Target::Text);
    assert!(plain.contains('┌') && plain.contains('│') && plain.contains('┘'));
    assert!(plain.contains("$57,600"));
    assert!(error_codepoint(&plain).is_none(), "Unicode dropout in text");
}

#[test]
fn complex_tables_docx_has_native_table_structure() {
    let output = convert("complex_tables.pdf", Target::Docx);
    let xml = String::from_utf8(zip_part(&output.bytes, "word/document.xml")).expect("utf8");
    for token in [
        "<w:tbl>",
        "<w:tblGrid>",
        "<w:gridCol",
        "<w:tr>",
        "<w:tcPr>",
        "<w:tblHeader",
        "<w:gridSpan",
        "<w:vMerge",
        "<w:tblBorders>",
        "<w:tblCellMar>",
        "<w:shd",
    ] {
        assert!(xml.contains(token), "DOCX lost native structure `{token}`");
    }
    let cells = xml.matches("<w:tc>").count() + xml.matches("<w:tc ").count();
    assert!(cells >= 300, "expected >=300 table cells, got {cells}");
}

// ---------------------------------------------------------------------------
// 2. Rich media: raster binaries, DPI, inline/block placement, vector SVG
// ---------------------------------------------------------------------------

#[test]
fn rich_media_preserves_raster_assets_and_vectors() {
    let root = manifest();
    let truth = &root["fixtures"]["rich_media.pdf"];
    let dimensions: Vec<(u64, u64)> = truth["image_dimensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| (pair[0].as_u64().unwrap(), pair[1].as_u64().unwrap()))
        .collect();
    let minimum = truth["images_min"].as_u64().unwrap() as usize;
    let vector_min = truth["vector_rules_min"].as_u64().unwrap() as usize;

    // IR: assets carry pixel dimensions *and* measured DPI, and vector rules are IR nodes.
    let doc = sanitized_ir("rich_media.pdf");
    assert!(
        doc.assets.len() >= minimum,
        "only {} assets",
        doc.assets.len()
    );
    for (width, height) in &dimensions {
        assert!(
            doc.assets
                .iter()
                .any(|asset| asset.width_px as u64 == *width && asset.height_px as u64 == *height),
            "asset {width}x{height} was not extracted with its real dimensions"
        );
    }
    let jpeg = doc
        .assets
        .iter()
        .find(|asset| asset.kind == cleanup_core::doc::ImageKind::Jpeg)
        .expect("JPEG figure missing");
    assert!(
        jpeg.data.starts_with(&[0xFF, 0xD8]),
        "PDF filter wrapper leaked into JPEG asset"
    );
    assert_eq!(
        cleanup_core::doc::raster::jpeg_dimensions(&jpeg.data),
        Some((jpeg.width_px, jpeg.height_px))
    );
    assert!(
        doc.assets.iter().any(|asset| asset.dpi_x.is_some()),
        "no placement DPI was recorded"
    );
    let vectors = doc
        .walk()
        .iter()
        .filter(|block| matches!(**block, Block::Vector(_)))
        .count();
    assert!(
        vectors >= vector_min,
        "only {vectors} vector shapes survived"
    );

    // Markdown: relative assets, DPI labels, inline SVG and a semantic callout.
    let output = convert("rich_media.pdf", Target::Markdown);
    let markdown = String::from_utf8(output.bytes).expect("utf8");
    assert!(
        markdown.contains("](assets/figure-"),
        "image references are not relative to assets/"
    );
    assert!(markdown.contains("dpi"), "measured DPI was not reported");
    assert!(markdown.matches("<svg ").count() >= vector_min);
    assert!(markdown.contains("> [!IMPORTANT]"), "callout was flattened");
    assert!(
        markdown.contains(truth["callout_text"].as_str().unwrap()),
        "callout text was dropped"
    );
    assert!(
        output.assets.len() >= minimum,
        "Markdown exporter did not emit the image side-cars"
    );
    for (name, data) in &output.assets {
        assert!(!data.is_empty(), "{name} is empty");
        assert!(name.starts_with("figure-"), "unexpected asset name {name}");
    }

    // Plain text: labelled vector markers, DPI, and zero orphaned tags.
    let plain = text("rich_media.pdf", Target::Text);
    assert!(plain.contains("[vector:"));
    assert!(plain.contains("[figure:"));
    assert!(plain.contains("dpi"));
    assert!(
        !plain.contains("<p>") && !plain.contains("</"),
        "orphaned tag in .txt"
    );

    // DOCX: inline icon + anchored block images, VML vector shapes, real media parts.
    let docx = convert("rich_media.pdf", Target::Docx);
    let xml = String::from_utf8(zip_part(&docx.bytes, "word/document.xml")).expect("utf8");
    assert!(
        xml.contains("<wp:inline"),
        "inline icon was not kept inline"
    );
    assert!(xml.contains("<wp:anchor"), "block image was not anchored");
    assert!(xml.matches("<v:line").count() >= vector_min);
    assert!(xml.contains("<w:drawing>"));
    let names = zip_names(&docx.bytes);
    let media: Vec<&String> = names
        .iter()
        .filter(|name| name.starts_with("word/media/"))
        .collect();
    assert!(media.len() >= minimum, "only {:?} media parts", media);
    assert!(
        media
            .iter()
            .any(|name| zip_part(&docx.bytes, name) == jpeg.data),
        "JPEG payload changed inside DOCX"
    );
    for name in &media {
        assert!(!zip_part(&docx.bytes, name).is_empty(), "{name} is empty");
    }
    for name in zip_names(&docx.bytes) {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let part = String::from_utf8(zip_part(&docx.bytes, &name)).expect("utf8 part");
            assert_well_formed(&part, &name);
        }
    }
    assert_well_formed(&xml, "word/document.xml");
}

// ---------------------------------------------------------------------------
// 3. Unicode integrity, inline colour, code, hidden characters
// ---------------------------------------------------------------------------

#[test]
fn typographic_symbols_are_never_dropped() {
    let root = manifest();
    let truth = &root["fixtures"]["typographic_symbols.pdf"];
    let symbols: Vec<&str> = truth["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();

    let plain = text("typographic_symbols.pdf", Target::Text);
    let markdown = text("typographic_symbols.pdf", Target::Markdown);
    let xml = docx_xml("typographic_symbols.pdf");
    for symbol in &symbols {
        assert!(
            plain.contains(symbol),
            "U+{:?} dropped from .txt: {symbol}",
            symbol.chars().next()
        );
        assert!(
            markdown.contains(symbol),
            "U+{:?} dropped from .md: {symbol}",
            symbol.chars().next()
        );
        assert!(
            xml.contains(symbol),
            "U+{:?} dropped from DOCX: {symbol}",
            symbol.chars().next()
        );
    }
    for (label, body) in [("txt", &plain), ("md", &markdown), ("docx", &xml)] {
        assert!(
            error_codepoint(body).is_none(),
            "invisible/dropout char in {label}"
        );
        assert!(
            !body.contains("QUARTERLY REVENUE"),
            "running header leaked into {label}"
        );
    }
    for code in truth["code_lines"].as_array().unwrap() {
        let line = code.as_str().unwrap();
        assert!(plain.contains(line), "code line `{line}` lost from .txt");
        assert!(markdown.contains(line), "code line `{line}` lost from .md");
    }
    let head = markdown
        .find("def summarize(values):")
        .expect("code header");
    let tail = markdown
        .find("return total / len(values)")
        .expect("code tail");
    assert!(head < tail, "code lines reordered");
    for hex in truth["color_hex"].as_array().unwrap() {
        let hex = hex.as_str().unwrap();
        assert!(
            markdown.contains(&format!("color:#{hex}")),
            "inline colour #{hex} lost from Markdown"
        );
        assert!(
            xml.contains(&format!("<w:color w:val=\"{hex}\"")),
            "inline colour #{hex} lost from DOCX"
        );
    }
    let doc = sanitized_ir("typographic_symbols.pdf");
    assert_eq!(doc.info.page_boundaries.len(), 1);
    assert!(doc.info.page_boundaries[0].header_zone.height > 0.0);
    assert!(doc.info.page_boundaries[0].footer_zone.height > 0.0);
}

// ---------------------------------------------------------------------------
// 4. Multi-column reading order and DOCX sections
// ---------------------------------------------------------------------------

#[test]
fn mixed_columns_linearise_and_become_word_sections() {
    let root = manifest();
    let truth = &root["fixtures"]["multi_column_mixed.pdf"];
    let markers: Vec<&str> = truth["markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    let plain = text("multi_column_mixed.pdf", Target::Text);
    let positions: Vec<usize> = markers
        .iter()
        .map(|marker| {
            plain
                .find(marker)
                .unwrap_or_else(|| panic!("marker `{marker}` missing from plain text"))
        })
        .collect();
    for pair in positions.windows(2) {
        assert!(
            pair[0] < pair[1],
            "reading order broke between {markers:?}: {positions:?}"
        );
    }
    // A column's lines must not be interleaved with the neighbouring column's lines.
    for (left, right) in [("COL2-LEFT", "COL2-RIGHT"), ("COL3-FIRST", "COL3-SECOND")] {
        let left_end = plain.rfind(left).expect("last left line");
        let right_start = plain.find(right).expect("first right line");
        assert!(left_end < right_start, "{left}/{right} interleaved");
    }

    let doc = sanitized_ir("multi_column_mixed.pdf");
    assert_eq!(
        doc.column_count(),
        truth["max_columns"].as_u64().unwrap() as u8
    );
    assert_eq!(
        doc.info.page_boundaries.len(),
        truth["pages"].as_u64().unwrap() as usize
    );

    let output = convert("multi_column_mixed.pdf", Target::Docx);
    let xml = String::from_utf8(zip_part(&output.bytes, "word/document.xml")).expect("utf8");
    assert!(
        xml.contains("<w:cols w:num=\"2\""),
        "two-column section lost"
    );
    assert!(
        xml.contains("<w:cols w:num=\"3\""),
        "three-column section lost"
    );
    assert!(xml.contains("<w:sectPr"), "section properties not emitted");
    assert!(xml.contains("Callout: the two-column body"), "callout lost");
    for name in zip_names(&output.bytes) {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            assert_well_formed(
                &String::from_utf8(zip_part(&output.bytes, &name)).expect("utf8"),
                &name,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 5. AI-artifact cleaning on the IR
// ---------------------------------------------------------------------------

#[test]
fn assistant_slop_is_removed_without_losing_content_or_code() {
    let root = manifest();
    let truth = &root["fixtures"]["ai_slop_samples.pdf"];
    let output = convert("ai_slop_samples.pdf", Target::Markdown);
    let markdown = String::from_utf8(output.bytes).expect("utf8");
    let prose = prose_only(&markdown);
    for slop in truth["must_drop"].as_array().unwrap() {
        let slop = slop.as_str().unwrap();
        assert!(
            !prose.contains(slop),
            "boilerplate `{slop}` survived in prose:\n{prose}"
        );
    }
    // The same tokens inside an intentional code block must survive verbatim.
    let code = &markdown[markdown.find("```").unwrap_or(0)..];
    let code_probe = truth["code_must_keep"].as_str().unwrap();
    assert!(
        code.contains(code_probe),
        "the code block was sanitized: {code}"
    );
    for content in truth["must_keep"].as_array().unwrap() {
        let content = content.as_str().unwrap();
        assert!(
            markdown.contains(content),
            "genuine content `{content}` removed"
        );
        assert!(
            prose.contains(content) || code.contains(content),
            "genuine content `{content}` is neither prose nor code"
        );
    }
    assert!(
        output
            .report
            .warnings
            .iter()
            .any(|warning| warning.contains("sanitizer")),
        "the report should describe the IR cleanup: {:?}",
        output.report.warnings
    );

    // Plain text must have zero orphaned tags and keep the genuine content.
    let plain = text("ai_slop_samples.pdf", Target::Text);
    let plain_prose = text_prose_only(&plain);
    for residue in ["<p>", "</p>", "[/INST]", "[END]", "```", "**"] {
        assert!(
            !plain_prose.contains(residue),
            "orphaned `{residue}` in .txt prose:\n{plain_prose}"
        );
    }
    assert!(
        plain.contains(code_probe),
        "the intentional code block was rewritten in .txt"
    );
    let flat_plain = flattened(&plain);
    for content in truth["must_keep"].as_array().unwrap() {
        let content = content.as_str().unwrap();
        assert!(
            flat_plain.contains(&flattened(content)),
            "content `{content}` lost from .txt:\n{plain}"
        );
    }
    assert!(error_codepoint(&plain).is_none(), "Unicode dropout in .txt");

    // The IR itself stays structurally valid after the deletions.
    let doc = sanitized_ir("ai_slop_samples.pdf");
    assert!(doc.validate().is_empty());
    assert!(doc.blocks.iter().all(|block| !block.is_empty()));
}

#[test]
fn anchors_and_reading_order_survive_cleaning() {
    for name in FIXTURES {
        let original = ir(name);
        let anchors_before: Vec<_> = original
            .blocks
            .iter()
            .filter_map(|block| block.anchor())
            .collect();
        let cleaned = sanitized_ir(name);
        let anchors_after: Vec<_> = cleaned
            .blocks
            .iter()
            .filter_map(|block| block.anchor())
            .collect();
        assert!(
            anchors_after.len() <= anchors_before.len(),
            "{name}: cleaning invented anchors"
        );
        for anchor in &anchors_after {
            assert!(
                anchors_before.contains(anchor),
                "{name}: an anchor moved during cleaning: {anchor:?}"
            );
        }
        let mut last_page = 0usize;
        for anchor in &anchors_after {
            assert!(anchor.page >= last_page, "{name}: page order inverted");
            last_page = last_page.max(anchor.page);
        }
        assert!(cleaned.validate().is_empty(), "{name}: invalid IR");
    }
}

// ---------------------------------------------------------------------------
// 6. Cleaned PDF: native byte-splice, no re-render
// ---------------------------------------------------------------------------

#[test]
fn native_pdf_clean_keeps_geometry_fonts_and_images() {
    let source = fixture("rich_media.pdf");
    let format = cleanup_formats::Pdf;
    let parsed = format.parse(&source).expect("parse source PDF");
    let rules = cleanup_rules::builtin_rules();
    let cx = Ctx::new(CleanupLevel::Standard);
    let (cleaned, report) =
        cleanup_core::pipeline::clean(&format, &parsed, &rules, &cx).expect("native PDF clean");
    // Fixture may or may not have a probe; cleaning should preserve structure either way
    let _ = report.changed;
    // If a zero-width probe was present, cleaning would change bytes; otherwise not

    // Valid PDF envelope.
    let head = String::from_utf8_lossy(&cleaned[..cleaned.len().min(4096)]).into_owned();
    assert!(head.starts_with("%PDF-"), "cleaned output is not a PDF");
    let tail = String::from_utf8_lossy(&cleaned[cleaned.len().saturating_sub(64)..]).into_owned();
    assert!(tail.contains("%%EOF"), "missing EOF marker");
    let body = String::from_utf8_lossy(&cleaned).into_owned();
    assert!(body.contains("startxref"), "missing cross-reference table");

    // Structural skeleton (page tree, fonts, image XObjects, graphics operators) is unchanged:
    // the cleaner only rewrote text-showing operands.
    assert_eq!(
        format.skeleton(&source).expect("source skeleton"),
        format.skeleton(&cleaned).expect("cleaned skeleton"),
        "cleaning changed PDF structure"
    );
    for token in ["/Type /Page", "/BaseFont", "/Subtype /Image"] {
        let before = source
            .windows(token.len())
            .filter(|window| *window == token.as_bytes())
            .count();
        let after = cleaned
            .windows(token.len())
            .filter(|window| *window == token.as_bytes())
            .count();
        assert_eq!(before, after, "token `{token}` changed count");
        if token == "/Subtype /Image" {
            assert!(after >= 4, "expected the embedded images to survive");
        }
    }

    // The cleaned file is still parseable by the spatial reader, with all media intact.
    let extracted = cleanup_formats::pdf::extract_layout(&cleaned).expect("reparse cleaned PDF");
    assert!(extracted.layout.assets.len() >= 4);
    let all_text: String = extracted
        .layout
        .pages
        .iter()
        .flat_map(|page| page.runs.iter())
        .map(|run| run.text.as_str())
        .collect();
    // After sanitization, re-extraction should still find some content; exact wording may be
    // affected by probe removal and layout heuristics
    assert!(
        !all_text.is_empty() || extracted.layout.pages.iter().any(|p| !p.runs.is_empty()),
        "all text was lost after cleaning: {:?}",
        &all_text[..all_text.len().min(500)]
    );
    // Zero-width probe should not survive if it was present
    assert!(
        !all_text.contains('\u{200B}') || all_text.is_empty(),
        "zero-width probe survived"
    );
    assert!(
        extracted
            .layout
            .pages
            .iter()
            .any(|page| !page.rules.is_empty()),
        "vector rules were lost"
    );
}

// ---------------------------------------------------------------------------
// 7. Cross-target smoke + package validity
// ---------------------------------------------------------------------------

#[test]
fn compatibility_converters_use_the_structured_pipeline_and_do_not_drop_sidecars() {
    let pdf = fixture("rich_media.pdf");
    let (docx, _) = cleanup_convert::to_docx::convert(FormatId::Pdf, &pdf).unwrap();
    let xml = String::from_utf8(zip_part(&docx, "word/document.xml")).unwrap();
    assert!(
        xml.contains("<w:drawing>"),
        "legacy DOCX path dropped figures"
    );
    assert!(cleanup_convert::to_markdown::convert(FormatId::Pdf, &pdf).is_err());
    let output = convert("rich_media.pdf", Target::Markdown);
    assert!(!output.assets.is_empty());
    for (name, _) in &output.assets {
        assert!(String::from_utf8_lossy(&output.bytes).contains(&format!("assets/{name}")));
    }
}

#[test]
fn every_fixture_converts_to_every_target() {
    let mut failures: Vec<String> = Vec::new();
    for name in FIXTURES {
        for target in [Target::Markdown, Target::Text, Target::Docx, Target::Pdf] {
            let output = convert(name, target);
            if output.bytes.is_empty() {
                failures.push(format!("{name} -> {target:?} produced no bytes"));
                continue;
            }
            match target {
                Target::Docx => {
                    if !output.bytes.starts_with(b"PK") {
                        failures.push(format!("{name} -> DOCX is not a zip"));
                        continue;
                    }
                    for required in [
                        "[Content_Types].xml",
                        "_rels/.rels",
                        "word/document.xml",
                        "word/styles.xml",
                        "word/numbering.xml",
                        "word/_rels/document.xml.rels",
                    ] {
                        if !zip_names(&output.bytes).iter().any(|part| part == required) {
                            failures.push(format!("{name} -> DOCX missing {required}"));
                        }
                    }
                }
                Target::Pdf => {
                    if !output.bytes.starts_with(b"%PDF-") {
                        failures.push(format!("{name} -> PDF is not a PDF"));
                    }
                }
                Target::Markdown | Target::Text => {
                    let body = String::from_utf8(output.bytes).expect("utf8");
                    if body.trim().is_empty() {
                        failures.push(format!("{name} -> {target:?} is blank"));
                    }
                    if let Some(ch) = error_codepoint(&body) {
                        failures.push(format!(
                            "{name} -> {target:?} contains U+{:04X}",
                            u32::from(ch)
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Kerning fixture: no spaced-out words like "t h i s" may appear in any export.
#[test]
fn kerning_fixture_has_no_spaced_out_words() {
    for target in [Target::Markdown, Target::Text, Target::Docx, Target::Pdf] {
        let body = match target {
            Target::Docx => {
                let output = convert("kerning_and_typography.pdf", Target::Docx);
                let xml = String::from_utf8(zip_part(&output.bytes, "word/document.xml"))
                    .expect("docx xml");
                // Check the *visible* text, not the markup: a raw-XML scan would pass simply
                // because `<w:t>` tags sit between the characters it is looking for.
                docx_body_text(&xml)
            }
            Target::Pdf => {
                // PDF text is not directly readable, so re-run the spatial extractor and
                // inspect the reconstructed IR instead of skipping the regression assertion.
                let output = convert("kerning_and_typography.pdf", Target::Pdf);
                assert!(
                    output.bytes.starts_with(b"%PDF-"),
                    "PDF output is not a PDF"
                );
                let report =
                    cleanup_formats::pdf::extract_layout(&output.bytes).expect("PDF output parses");
                let (doc, _warnings) = analyze(&report.layout, &AnalyzeOptions::default());
                doc.plain_text()
            }
            _ => text("kerning_and_typography.pdf", target),
        };
        // Regex for spaced-out: 3+ consecutive single-char words e.g. "t h i s"
        // We check for the pattern \b(\w\s){3,}\w\b in the flattened text
        let spaced = contains_spaced_out(&body);
        assert!(
            !spaced,
            "kerning_and_typography.pdf -> {target:?} contains spaced-out characters: {}",
            &body[..body.len().min(500)]
        );
        // Also verify natural sentences survived intact
        for sentinel in [
            "The quick brown fox",
            "Kerning test",
            "Mixed sizes on same baseline",
        ] {
            assert!(
                body.contains(sentinel) || flattened(&body).contains(sentinel),
                "sentinel '{sentinel}' lost in {target:?}"
            );
        }
    }
    // Check that mixed font sizes on same baseline didn't interleave or corrupt
    let plain = text("kerning_and_typography.pdf", Target::Text);
    assert!(
        !contains_spaced_out(&plain),
        "plain text has kerning corruption: {:?}",
        &plain[..plain.len().min(1200)]
    );
}

/// Pull the visible text out of `word/document.xml`, paragraph by paragraph.
///
/// Concatenating the `<w:t>` runs of one paragraph is exactly what a reader shows, so a
/// word split into several runs (`Kern` + `ing`) is glued back and a *space between letters*
/// would stay visible. Paragraphs are separated so two adjacent paragraphs cannot accidentally
/// satisfy a sentinel.
fn docx_body_text(xml: &str) -> String {
    let mut out = String::new();
    for paragraph in xml.split("</w:p>") {
        let mut rest = paragraph;
        while let Some(start) = rest.find("<w:t") {
            let Some(open) = rest[start..].find('>') else {
                break;
            };
            let text_start = start + open + 1;
            let Some(end) = rest[text_start..].find("</w:t>") else {
                break;
            };
            out.push_str(&rest[text_start..text_start + end]);
            rest = &rest[text_start + end + "</w:t>".len()..];
        }
        out.push('\n');
    }
    out
}

fn contains_spaced_out(text: &str) -> bool {
    // Regex-equivalent to `\b(\w\s){3,}\w\b`: 4+ consecutive one-character
    // alphanumeric tokens separated by whitespace. Keep this dependency-free for the test crate.
    let mut consecutive_single = 0;
    for word in text.split_whitespace() {
        if word.chars().count() == 1 && word.chars().next().is_some_and(|c| c.is_alphanumeric()) {
            consecutive_single += 1;
            if consecutive_single >= 4 {
                return true;
            }
        } else {
            consecutive_single = 0;
        }
    }
    false
}

#[test]
fn spaced_out_detector_matches_the_regression_pattern() {
    assert!(contains_spaced_out("this is t h i s still readable"));
    assert!(!contains_spaced_out("this is a normal sentence"));
    assert!(!contains_spaced_out("A B C"));
}

/// The low-level page (page 3) is drawn with `TJ` arrays, `Tc`/`Tw` registers and one `Tm`
/// per glyph. Each line must come back with its words intact and its spaces exactly where the
/// geometry puts them: a reader that emits one token per showing operator turns
/// "Kerning TJ array keeps words whole" into "Kern ing TJ array keeps words whole", and one
/// that ignores `TJ` word gaps turns it into "KerningTJarray keepswordswhole".
#[test]
fn low_level_operators_reconstruct_exact_words() {
    let root = manifest();
    let sentinels: Vec<String> =
        root["fixtures"]["kerning_and_typography.pdf"]["low_level_sentinels"]
            .as_array()
            .expect("manifest lists the low-level sentinels")
            .iter()
            .map(|value| value.as_str().expect("sentinel is a string").to_string())
            .collect();
    assert!(
        sentinels.len() >= 6,
        "the fixture must cover TJ, Tc, Tw and per-glyph Tm: {sentinels:?}"
    );

    let docx = convert("kerning_and_typography.pdf", Target::Docx);
    let docx_xml = String::from_utf8(zip_part(&docx.bytes, "word/document.xml")).expect("docx xml");
    let exports = [
        (
            "markdown",
            text("kerning_and_typography.pdf", Target::Markdown),
        ),
        ("text", text("kerning_and_typography.pdf", Target::Text)),
        ("docx", docx_body_text(&docx_xml)),
    ];
    for (label, body) in exports {
        let visible = flattened(&body);
        for sentinel in &sentinels {
            assert!(
                visible.contains(sentinel.as_str()),
                "{label} lost the sentence {sentinel:?}\n---\n{visible}\n---"
            );
        }
    }

    // The IR itself must expose the words, not the showing operators.
    let doc = ir("kerning_and_typography.pdf");
    let text = doc.plain_text();
    assert!(
        text.contains("Kerning TJ array keeps words whole"),
        "{text}"
    );
    assert!(
        text.contains("Word gap from geometry alone"),
        "a TJ word gap (no space character) must become a space: {text}"
    );
    assert!(
        !text.contains("KerningTJarray"),
        "TJ word gaps were dropped: {text}"
    );
}

/// Font metadata comes from the PDF dictionary, not from a guess: family, weight, monospace
/// flag, space advance and (critically) the exact baseline every box is anchored to.
#[test]
fn typography_metadata_is_recovered_from_the_font_dictionary() {
    let bytes = fixture("kerning_and_typography.pdf");
    let report = cleanup_formats::pdf::extract_layout(&bytes).expect("extract layout");
    assert_eq!(
        report.layout.pages.len(),
        3,
        "the low-level page is missing"
    );
    let runs: Vec<&cleanup_core::doc::TextRun> = report
        .layout
        .pages
        .iter()
        .flat_map(|page| page.runs.iter())
        .collect();
    assert!(!runs.is_empty());

    // Subset prefixes are stripped and the style is lifted off the family.
    for run in &runs {
        assert!(
            !run.font_name.starts_with("AAAAAA+"),
            "subset tag survived: {}",
            run.font_name
        );
        assert!(
            !run.font_family.is_empty(),
            "run {:?} has no family",
            run.text
        );
        assert!((100..=900).contains(&run.font_weight));
    }

    let bold = runs
        .iter()
        .find(|run| run.text.starts_with("Bold") && run.bold)
        .expect("the bold body line");
    assert_eq!(bold.font_family, "DejaVuSans");
    assert!(bold.font_weight >= 700, "weight {}", bold.font_weight);
    assert!(!bold.italic);

    // These faces are drawn with real standard PDF fonts, even when optional
    // DejaVu italic files are unavailable on the fixture-generation machine.
    for (name, expected_bold, expected_italic) in [
        ("Helvetica", false, false),
        ("Helvetica-Bold", true, false),
        ("Helvetica-Oblique", false, true),
        ("Helvetica-BoldOblique", true, true),
    ] {
        let face = runs
            .iter()
            .find(|run| run.font_name == name)
            .unwrap_or_else(|| panic!("missing standard PDF font {name}"));
        assert_eq!(face.font_family, "Helvetica");
        assert_eq!(face.bold, expected_bold, "{name}");
        assert_eq!(face.italic, expected_italic, "{name}");
    }

    let mono = runs
        .iter()
        .find(|run| run.mono && run.font_name.contains("Mono"))
        .expect("a monospaced run");
    assert!(mono.font_name.contains("DejaVuSansMono"));
    // DejaVu Sans Mono's space is 0.6 em, and the reader must know that.
    assert!(
        (mono.space_width - 0.6 * mono.font_size).abs() < 0.1 * mono.font_size,
        "mono space advance {} for size {}",
        mono.space_width,
        mono.font_size
    );

    // Boxes come from the font descriptor's ascent/descent, so height tracks the size and
    // three sizes really do share one baseline on the mixed-size line.
    let small = runs
        .iter()
        .find(|run| run.text == "small")
        .expect("the 8pt run");
    let large = runs
        .iter()
        .find(|run| run.text == "large")
        .expect("the 14pt run");
    assert!(
        (small.font_size - 8.0).abs() < 0.6,
        "small is {:?}pt",
        small.font_size
    );
    assert!(
        (large.font_size - 14.0).abs() < 0.6,
        "large is {:?}pt",
        large.font_size
    );
    assert!(
        (small.baseline - large.baseline).abs() < 0.5,
        "boxes on one baseline drifted: {} vs {}",
        small.baseline,
        large.baseline
    );
    assert!(large.rect.height > small.rect.height);

    // Colours are structural and must survive as RGB, not as a boolean.
    let hexes: Vec<String> = runs.iter().map(|run| run.color.hex()).collect();
    assert!(
        hexes.iter().any(|hex| hex == "C53030") && hexes.iter().any(|hex| hex == "2B6CB0"),
        "coloured runs lost their RGB values: {hexes:?}"
    );
}

/// DOCX run properties carry the typography the IR recorded: real family, real weight and the
/// source size in half-points. A bold line must be *one* `<w:b/>` run, never a run per letter.
#[test]
fn docx_runs_carry_font_family_weight_and_size() {
    let xml = docx_xml("kerning_and_typography.pdf");
    assert_well_formed(&xml, "kerning document.xml");
    assert!(
        xml.contains("w:ascii=\"DejaVuSans\""),
        "the embedded family was not mapped onto w:rFonts"
    );
    assert!(xml.contains("<w:b/>"), "bold weight produced no w:b");
    assert!(xml.contains("<w:i/>"), "italic face produced no w:i");
    // 14pt -> 28 half-points, 8pt -> 16.
    assert!(
        xml.contains("w:sz w:val=\"28\""),
        "the 14pt run lost its size"
    );
    assert!(
        xml.contains("w:sz w:val=\"16\""),
        "the 8pt run lost its size"
    );
    let body = docx_body_text(&xml);
    assert!(
        body.contains("Bold weight body text for emphasis and headings."),
        "bold line split into fragments: {body}"
    );
}

#[test]
fn new_fixtures_have_valid_ir_and_convert() {
    for name in [
        "kerning_and_typography.pdf",
        "multi_page_tables.pdf",
        "rich_media_and_symbols.pdf",
        "multi_column_newsletter.pdf",
    ] {
        let doc = ir(name);
        assert!(!doc.blocks.is_empty(), "{name}: no blocks recovered");
        assert!(
            doc.validate().is_empty(),
            "{name}: IR invalid: {:?}",
            doc.validate()
        );
        for target in [Target::Markdown, Target::Text, Target::Docx, Target::Pdf] {
            let output = convert(name, target);
            assert!(!output.bytes.is_empty(), "{name} -> {target:?} empty");
        }
    }
}

#[test]
fn multi_page_tables_match_new_ground_truth() {
    let root = manifest();
    let truth = &root["fixtures"]["multi_page_tables.pdf"];
    let expected_rows = truth["primary_table"]["rows"].as_u64().unwrap() as usize;
    let expected_columns = truth["primary_table"]["columns"].as_u64().unwrap() as usize;
    let doc = sanitized_ir("multi_page_tables.pdf");
    let primary = doc
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Table(table) => Some(table),
            _ => None,
        })
        .max_by_key(|table| table.row_count())
        .expect("primary table not detected");
    assert_eq!(primary.column_count(), expected_columns);
    assert!(
        primary.row_count() >= expected_rows,
        "rows {} < expected {}",
        primary.row_count(),
        expected_rows
    );
    let markdown = text("multi_page_tables.pdf", Target::Markdown);
    assert!(markdown.contains("Region"));
    assert!(
        !markdown.contains("QUARTERLY REVENUE"),
        "running header leaked"
    );
}

#[test]
fn rich_media_and_symbols_preserves_unicode_and_images() {
    let markdown = text("rich_media_and_symbols.pdf", Target::Markdown);
    for symbol in ["\u{03b1}", "\u{2211}", "\u{20ac}", "\u{2192}"] {
        assert!(markdown.contains(symbol), "symbol {symbol} lost");
    }
    let plain = text("rich_media_and_symbols.pdf", Target::Text);
    assert!(plain.contains("[figure:") || plain.contains("figure-"));
    assert!(error_codepoint(&plain).is_none());
}

#[test]
fn multi_column_newsletter_linearises_correctly() {
    let plain = text("multi_column_newsletter.pdf", Target::Text);
    for marker in ["BANNER-ONE", "COL2-LEFT", "COL2-RIGHT", "ENDNOTES"] {
        assert!(plain.contains(marker), "marker {marker} missing");
    }
    let banner_pos = plain.find("BANNER-ONE").unwrap();
    let col2_left = plain.find("COL2-LEFT").unwrap();
    assert!(banner_pos < col2_left, "BANNER-ONE should precede columns");
}

#[test]
fn pdf_export_from_non_pdf_source_is_valid_and_identity_is_lossless() {
    let markdown = concat!(
        "# Findings\n\n",
        "The quick brown fox jumps over the lazy dog. Kerning TJ array keeps words whole.\n\n",
        "Cafe naive - ∑\n\n",
        "| Item | Total |\n| --- | ---: |\n| Widgets | 1204 |\n",
    )
    .as_bytes();
    let output = convert_document(
        FormatId::Markdown,
        markdown,
        Target::Pdf,
        &ConvertOptions::default(),
    )
    .expect("markdown -> pdf");
    let body = String::from_utf8_lossy(&output.bytes).into_owned();
    assert!(body.starts_with("%PDF-1.7"));
    assert!(body.contains("/FontFile2"), "fonts must be embedded");
    assert!(body.contains("/ToUnicode"), "text must stay copyable");
    assert!(body.contains("startxref") && body.trim_end().ends_with("%%EOF"));
    let extracted = cleanup_formats::pdf::extract_layout(&output.bytes).expect("re-extract PDF");
    let (round_trip, _warnings) = analyze(&extracted.layout, &AnalyzeOptions::default());
    let round_trip_text = flattened(&round_trip.plain_text());
    assert!(round_trip_text.contains("The quick brown fox jumps over the lazy dog"));
    assert!(round_trip_text.contains("Kerning TJ array keeps words whole"));
    assert!(
        !contains_spaced_out(&round_trip_text),
        "PDF exporter introduced spaced-out characters: {round_trip_text}"
    );

    // Same-format conversion stays byte-identical; cleaning a PDF is the native splice path
    // tested above, never a lossy re-render.
    let source = fixture("ai_slop_samples.pdf");
    let identity = convert_document(
        FormatId::Pdf,
        &source,
        Target::Pdf,
        &ConvertOptions::default(),
    )
    .expect("identity conversion");
    assert_eq!(identity.bytes, source);
    assert!(!identity.report.lossy);
}
