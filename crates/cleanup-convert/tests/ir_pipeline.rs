//! End-to-end integration tests for the document-IR pipeline.
//!
//! These run the real fixtures in `tests/fixtures/` through a real reader (`pdf::extract_layout`
//! plus the layout analyser) and assert that structure survives the trip into each target: table
//! cells and spans, image extraction, native OpenXML tables, well-formed package XML, column
//! linearisation, and byte-for-byte Unicode retention.

use cleanup_convert::{ConvertOptions, ConvertOutput, Target, convert_document};
use cleanup_core::FormatId;
use std::io::Read;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|error| panic!("reading {path}: {error}"))
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

/// Read one part out of an OpenXML package.
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
        let Ok(file) = archive.by_index(index) else {
            continue;
        };
        if let Ok(name) = file.name() {
            names.push(name.into_owned());
        }
    }
    names
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
    let pipes: usize = markdown
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .map(|line| line.matches('|').count().saturating_sub(1))
        .max()
        .unwrap_or(0);
    let html = markdown.matches("<td").count() + markdown.matches("<th").count();
    pipes.max(html)
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

#[test]
fn multi_page_table_cells_and_text_survive() {
    let output = convert("multi_page_tables.pdf", Target::Markdown);
    let markdown = String::from_utf8(output.bytes).expect("markdown is UTF-8");

    // Every cell label from all three pages, plus the repeat header.
    for label in [
        "Region",
        "Units",
        "Gross",
        "Net",
        "Growth",
        "EMEA",
        "LATAM",
        "India",
        "Nordics",
        "Quarter",
        "Marketing",
        "Research",
    ] {
        assert!(
            markdown.contains(label),
            "table text `{label}` was dropped; output was:\n{}",
            &markdown[..markdown.len().min(2000)]
        );
    }

    // A 5-column table existed in the source: the widest row must have 5 cells, and the
    // 30 body rows of the paginated table must all be present in one logical table.
    assert!(
        table_cells(&markdown) >= 5,
        "expected at least 5 table cells across a row, got {} in:\n{}",
        table_cells(&markdown),
        &markdown[..markdown.len().min(2000)]
    );
    let table_rows = markdown
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .count();
    assert!(
        table_rows >= 25,
        "expected the 30-row paginated table to survive as >=25 rows, got {table_rows}:\n{}",
        &markdown[..markdown.len().min(2000)]
    );
}

#[test]
fn merged_cells_are_not_flattened_away() {
    let markdown = text("multi_page_tables.pdf", Target::Markdown);
    // Colspan/rowspan cannot be expressed in GFM: the renderer must fall back to HTML and keep
    // the span attributes rather than dropping the spanned text.
    let has_spans = markdown.contains("colspan") || markdown.contains("rowspan");
    let has_html_table = markdown.contains("<table");
    assert!(
        has_spans && has_html_table,
        "the span table lost its colspan/rowspan (spans={has_spans}, html={has_html_table})"
    );
    assert!(markdown.contains("H1"), "merged row label was dropped");
}

#[test]
fn docx_output_has_native_tables_with_cell_properties() {
    let output = convert("multi_page_tables.pdf", Target::Docx);
    let document = zip_part(&output.bytes, "word/document.xml");
    let xml = String::from_utf8(document).expect("document.xml is UTF-8");
    assert_well_formed(&xml, "word/document.xml");

    let tables = xml.matches("<w:tbl>").count();
    let cells = xml.matches("<w:tc>").count();
    assert!(tables >= 2, "expected 2 native tables, got {tables}");
    assert!(
        cells >= 40,
        "expected >=40 table cells (31 rows of 5 plus the span table), got {cells}"
    );
    assert!(
        xml.contains("<w:gridSpan") && xml.contains("<w:vMerge"),
        "merged cells must be encoded as gridSpan/vMerge"
    );
    assert!(
        xml.contains("<w:shd"),
        "alternating row fills must survive as cell shading"
    );
    assert!(
        xml.contains("<w:tcBorders>") || xml.contains("<w:tblBorders>"),
        "cell borders must be drawn"
    );
}

// ---------------------------------------------------------------------------
// Images and vector graphics
// ---------------------------------------------------------------------------

#[test]
fn raster_images_are_extracted_and_embedded() {
    let output = convert("rich_media_layout.pdf", Target::Docx);
    let names = zip_names(&output.bytes);
    let media: Vec<&String> = names
        .iter()
        .filter(|name| name.starts_with("word/media/"))
        .collect();
    assert!(
        media.len() >= 2,
        "expected the block PNG and JPEG to be embedded, found {media:?}"
    );
    for name in &media {
        let bytes = zip_part(&output.bytes, name);
        assert!(!bytes.is_empty(), "{name} is an empty part");
    }
    let document = String::from_utf8(zip_part(&output.bytes, "word/document.xml")).expect("xml");
    assert!(
        document.contains("<w:drawing>") && document.contains("<wp:inline"),
        "images must be placed as inline drawings in the DOCX body"
    );

    // The same assets must survive a hop through the PDF exporter.
    let pdf = convert_document(
        FormatId::Docx,
        &output.bytes,
        Target::Pdf,
        &ConvertOptions::default(),
    )
    .expect("docx -> pdf");
    let pdf_text = String::from_utf8_lossy(&pdf.bytes).into_owned();
    let embedded = pdf_text.matches("/Subtype /Image").count();
    assert!(
        embedded >= 2,
        "the PDF exporter embedded {embedded} image(s); warnings: {:?}",
        pdf.report.warnings
    );
}

#[test]
fn vector_rules_and_callouts_are_detected() {
    let markdown = text("rich_media_layout.pdf", Target::Markdown);
    // Tinted boxes become quote/box callouts; their text must not vanish.
    assert!(markdown.contains("retention lifted"), "{markdown}");
    assert!(
        markdown.contains("filled, stroked background box"),
        "{markdown}"
    );
    // The stroked page frame and rules must not leak into the prose as glyph soup.
    assert!(!markdown.contains("\u{FFFD}"));
}

// ---------------------------------------------------------------------------
// Unicode
// ---------------------------------------------------------------------------

#[test]
fn typographic_symbols_are_never_dropped() {
    let plain = text("typographic_symbols.pdf", Target::Text);
    for symbol in [
        "\u{03b1}", "\u{03b2}", "\u{03a9}", "\u{03c0}", "\u{2211}", "\u{221a}", "\u{00b1}",
        "\u{2192}", "\u{21d2}", "\u{2264}", "\u{2265}", "\u{221e}", "\u{20ac}", "\u{2014}",
        "\u{00e9}", "\u{00df}", "\u{00e6}",
    ] {
        assert!(
            plain.contains(symbol),
            "character U+{:04X} ({symbol}) was dropped from the plain-text export",
            symbol.chars().next().map(u32::from).unwrap_or(0)
        );
    }
    assert!(
        !plain.contains('\u{FFFD}'),
        "replacement characters mean the font mapping failed"
    );
    // Zero-width characters are sanitizer fodder, not content.
    assert!(!plain.contains('\u{200b}'), "ZWNJ survived sanitization");
    assert!(!plain.contains('\u{feff}'), "BOM survived sanitization");

    // The code block survives in reading order, and its indented line is not glued to the
    // previous one.
    let markdown = text("typographic_symbols.pdf", Target::Markdown);
    let head = markdown.find("def summarize(values):").unwrap_or_else(|| {
        panic!("code block header lost:\n{markdown}");
    });
    let tail = markdown
        .find("return total / len(values)")
        .unwrap_or_else(|| panic!("indented code line lost:\n{markdown}"));
    assert!(head < tail, "code lines reordered");
}

// ---------------------------------------------------------------------------
// Layout: columns
// ---------------------------------------------------------------------------

#[test]
fn columns_linearise_left_to_right() {
    let plain = text("mixed_columns.pdf", Target::Text);
    let positions: Vec<usize> = [
        "Left column",
        "Right column",
        "First column",
        "Second column",
        "Third column",
    ]
    .iter()
    .map(|needle| {
        plain.find(needle).unwrap_or_else(|| {
            panic!(
                "`{needle}` missing from:\n{}",
                &plain[..plain.len().min(3000)]
            )
        })
    })
    .collect();
    assert!(
        positions[0] < positions[1],
        "two-column page interleaved: left at {}, right at {}",
        positions[0],
        positions[1]
    );
    assert!(
        positions[2] < positions[3] && positions[3] < positions[4],
        "three-column page is out of reading order: {positions:?}"
    );
    // The banner and endnotes stay single-column and are not swallowed by a column band.
    assert!(plain.contains("Annual Review"), "banner title lost");
    assert!(plain.contains("Endnotes"), "endnotes heading lost");
}

// ---------------------------------------------------------------------------
// Sanitization
// ---------------------------------------------------------------------------

#[test]
fn assistant_slop_is_removed_without_losing_content() {
    let output = convert("ai_slop_samples.pdf", Target::Markdown);
    let markdown = String::from_utf8(output.bytes).expect("markdown is UTF-8");
    for slop in [
        "As an AI language model",
        "I hope this helps",
        "This response was generated by",
        "feel free to ask if you have any questions",
    ] {
        assert!(
            !markdown.contains(slop),
            "boilerplate `{slop}` survived sanitization (warnings: {:?}):\n{markdown}",
            output.report.warnings
        );
    }
    for content in [
        "six-week pilot",
        "Support tickets per thousand orders fell",
        "Licensing cost 14,000 euros",
        "Pilot programme results",
        "Costs",
    ] {
        assert!(
            markdown.contains(content),
            "genuine content `{content}` was removed:\n{markdown}"
        );
    }
    assert!(
        output
            .report
            .warnings
            .iter()
            .any(|warning| warning.contains("sanitizer")),
        "the report should mention what the sanitizer did: {:?}",
        output.report.warnings
    );
}

// ---------------------------------------------------------------------------
// Package validity and cross-target smoke tests
// ---------------------------------------------------------------------------

#[test]
fn docx_package_is_valid_openxml() {
    let output = convert("mixed_columns.pdf", Target::Docx);
    let names = zip_names(&output.bytes);
    for required in [
        "[Content_Types].xml",
        "_rels/.rels",
        "word/document.xml",
        "word/styles.xml",
        "word/_rels/document.xml.rels",
    ] {
        assert!(
            names.iter().any(|name| name == required),
            "missing {required} in {names:?}"
        );
    }
    for name in &names {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let bytes = zip_part(&output.bytes, name);
            let xml = String::from_utf8(bytes).unwrap_or_else(|_| panic!("{name} is not UTF-8"));
            assert_well_formed(&xml, name);
        }
    }
    let document = String::from_utf8(zip_part(&output.bytes, "word/document.xml")).expect("xml");
    assert!(
        document.contains("<w:sectPr") && document.contains("<w:cols"),
        "multi-column sections must be expressed as numbered column sections"
    );
}

#[test]
fn every_fixture_converts_to_every_target() {
    let fixtures = [
        "multi_page_tables.pdf",
        "rich_media_layout.pdf",
        "typographic_symbols.pdf",
        "mixed_columns.pdf",
        "ai_slop_samples.pdf",
    ];
    let mut failures: Vec<String> = Vec::new();
    for name in fixtures {
        for target in [Target::Markdown, Target::Text, Target::Docx, Target::Pdf] {
            let output = convert(name, target);
            if output.bytes.is_empty() {
                failures.push(format!("{name} -> {target:?} produced no bytes"));
                continue;
            }
            if output.report.engine.starts_with("ir-") && !output.report.lossy {
                failures.push(format!(
                    "{name} -> {target:?} claims lossless for a re-render"
                ));
            }
            match target {
                Target::Docx => assert!(
                    output.bytes.starts_with(b"PK"),
                    "{name} -> DOCX is not a zip"
                ),
                // A PDF source converts to itself, keeping its own header, so only the
                // `%PDF-` magic is guaranteed here.
                Target::Pdf => assert!(
                    output.bytes.starts_with(b"%PDF-"),
                    "{name} -> PDF is not a PDF"
                ),
                Target::Markdown | Target::Text => {
                    let text = String::from_utf8(output.bytes).expect("utf-8");
                    if text.trim().is_empty() {
                        failures.push(format!("{name} -> {target:?} produced blank text"));
                    }
                    if text.contains('\u{FFFD}') {
                        failures.push(format!("{name} -> {target:?} contains U+FFFD"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn pdf_output_is_self_describing() {
    // Convert *into* PDF from a non-PDF source: a PDF source is its own target and is returned
    // byte-for-byte, which would never exercise the exporter.
    let markdown = b"# Findings\n\nCafe naive - \xe2\x88\x91\n\n| Item | Total |\n| --- | ---: |\n| Widgets | 1204 |\n";
    let output = convert_document(
        FormatId::Markdown,
        markdown,
        Target::Pdf,
        &ConvertOptions::default(),
    )
    .expect("markdown -> pdf");
    let text = String::from_utf8_lossy(&output.bytes).into_owned();
    assert!(text.starts_with("%PDF-1.7"));
    assert!(text.contains("/FontFile2"), "fonts must be embedded");
    assert!(text.contains("/ToUnicode"), "text must stay copyable");
    assert!(text.contains("startxref") && text.trim_end().ends_with("%%EOF"));
    // The identity conversion of a PDF must stay byte-identical: no re-render.
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
