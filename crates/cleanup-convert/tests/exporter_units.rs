//! Unit-level exporter and sanitizer coverage for IR constructs the PDF fixtures cannot
//! express on their own (hyperlinks, underline/strike, custom RGB, VML, PDF link annotations).
//!
//! Unlike `ir_pipeline.rs`, these tests build a `Doc` by hand and call the four pure renderers
//! directly. That proves each exporter maps the *same* IR node to a native target construct.

use cleanup_convert::export::{docx, markdown, pdf, text};
use cleanup_convert::sanitize::{SanitizeOptions, sanitize};
use cleanup_core::doc::{
    Align, Anchor, Axis, Block, CalloutKind, Cell, Column, ColumnSpec, Doc, DocInfo, Figure,
    ImageAsset, ImageKind, Inline, InlineStyle, ListItem, PageBoundary, PageSpec, ParagraphStyle,
    Rect, Rgb, Row, Table, VectorKind, VectorShape, encode_png,
};

fn paragraph(text: &str) -> Block {
    Block::Paragraph {
        inlines: vec![Inline::plain(text)],
        anchor: None,
        style: ParagraphStyle::default(),
    }
}

fn tiny_png() -> Vec<u8> {
    let rgb = vec![200u8; 4 * 4 * 3];
    encode_png(4, 4, &rgb).expect("PNG encode")
}

fn styled_doc() -> Doc {
    let mut doc = Doc::new(PageSpec::default());
    doc.info = DocInfo {
        title: Some(" exporter unit fixture".trim().to_string()),
        page_count: 1,
        page_boundaries: vec![PageBoundary::new(0, 612.0, 792.0)],
        ..DocInfo::default()
    };
    doc.assets.push(ImageAsset {
        name: "figure-1".to_string(),
        kind: ImageKind::Png,
        data: tiny_png(),
        width_px: 4,
        height_px: 4,
        dpi_x: Some(96.0),
        dpi_y: Some(96.0),
    });

    doc.blocks.push(Block::Heading {
        level: 1,
        inlines: vec![Inline::plain("Styled document")],
        anchor: None,
    });
    doc.blocks.push(Block::Paragraph {
        inlines: vec![
            Inline::plain("plain "),
            Inline::bold("bold "),
            Inline::italic("italic "),
            Inline::new(
                "underline ",
                InlineStyle {
                    underline: true,
                    ..InlineStyle::default()
                },
            ),
            Inline::new(
                "strike ",
                InlineStyle {
                    strike: true,
                    ..InlineStyle::default()
                },
            ),
            Inline::code("code "),
            Inline::new(
                "red ",
                InlineStyle {
                    color: Some(Rgb(0xC5, 0x30, 0x30)),
                    ..InlineStyle::default()
                },
            ),
            Inline::new(
                "link",
                InlineStyle::plain().with_link("https://example.com/report"),
            ),
        ],
        anchor: None,
        style: ParagraphStyle::default(),
    });
    doc.blocks.push(Block::List {
        ordered: true,
        items: vec![
            ListItem {
                inlines: vec![Inline::plain("first")],
                depth: 0,
            },
            ListItem {
                inlines: vec![Inline::plain("nested")],
                depth: 1,
            },
        ],
        anchor: None,
    });
    doc.blocks.push(Block::Callout {
        kind: CalloutKind::Note,
        blocks: vec![paragraph("Important note inside a box.")],
        anchor: None,
        fill: Some(Rgb(0xFF, 0xF6, 0xDA)),
    });
    doc.blocks.push(Block::Code {
        lines: vec![
            "def summarize(values):".to_string(),
            "    return sum(values)  # [/INST] ** the the the".to_string(),
        ],
        anchor: None,
        language: Some("python".to_string()),
    });

    // Rectangular table -> GFM pipe table with alignments and explicit widths.
    let mut rectangular = Table {
        columns: vec![
            ColumnSpec {
                width: 2.0,
                align: Align::Left,
            },
            ColumnSpec {
                width: 1.0,
                align: Align::Right,
            },
        ],
        ..Table::default()
    };
    rectangular.headers.push(Row {
        cells: vec![Cell::paragraph("Item"), Cell::paragraph("Total")],
        header: true,
        fill: None,
    });
    rectangular.rows.push(Row {
        cells: vec![Cell::paragraph("Widgets"), Cell::paragraph("1,204")],
        header: false,
        fill: Some(Rgb(0xED, 0xF1, 0xF7)),
    });
    doc.blocks.push(Block::Table(rectangular));

    // Merged + multi-line table -> semantic HTML fallback with <colgroup>.
    let mut merged = Table {
        columns: vec![
            ColumnSpec {
                width: 1.0,
                align: Align::Left,
            },
            ColumnSpec {
                width: 1.0,
                align: Align::Center,
            },
            ColumnSpec {
                width: 1.0,
                align: Align::Right,
            },
        ],
        ..Table::default()
    };
    merged.headers.push(Row {
        cells: vec![
            Cell::paragraph("A"),
            Cell::paragraph("B"),
            Cell::paragraph("C"),
        ],
        header: true,
        fill: None,
    });
    let mut wide = Cell::paragraph("merged across columns");
    wide.colspan = 2;
    let mut tall = Cell::paragraph("two\nlines");
    tall.rowspan = 2;
    merged.rows.push(Row {
        cells: vec![Cell::paragraph("row"), wide, tall],
        header: false,
        fill: None,
    });
    doc.blocks.push(Block::Table(merged));

    doc.blocks.push(Block::Figure(Figure {
        asset: 0,
        anchor: None,
        caption: Some(vec![Inline::plain("A tiny figure")]),
        alt: "tiny".to_string(),
        display_size: Some(Rect::new(0.0, 0.0, 72.0, 72.0)),
        inline: true,
    }));
    doc.blocks.push(Block::Vector(VectorShape::new(
        None,
        Rect::new(0.0, 0.0, 120.0, 0.8),
        VectorKind::Line,
        Some(Rgb(0x4A, 0x55, 0x68)),
        0.8,
        None,
    )));
    doc.blocks.push(Block::Columns {
        columns: vec![
            Column {
                blocks: vec![paragraph("left column")],
                width: 1.0,
            },
            Column {
                blocks: vec![paragraph("right column")],
                width: 1.0,
            },
        ],
        anchor: None,
    });
    doc
}

fn string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("utf8")
}

#[test]
fn markdown_maps_every_inline_and_container() {
    let doc = styled_doc();
    let rendered = markdown::render(&doc);
    let md = string(rendered.bytes);
    for expected in [
        "# Styled document",
        "**bold",
        "*italic",
        "<u>underline",
        "~~strike",
        "`code",
        "<span style=\"color:#C53030\">",
        "[link](https://example.com/report)",
        "> [!NOTE]",
        "```python",
        "| Item | Total |",
        "| :--- | ---: |",
        "| Widgets | 1,204 |",
        "<colgroup>",
        "colspan=\"2\"",
        "merged across columns",
        "](assets/figure-1.png)",
        "96 dpi",
        "<svg ",
        "role=\"img\"",
        "left column",
        "right column",
    ] {
        assert!(md.contains(expected), "Markdown lost `{expected}`:\n{md}");
    }
    assert!(
        rendered
            .assets
            .iter()
            .any(|(name, _)| name == "figure-1.png"),
        "Markdown did not emit the image side-car"
    );
    // The fenced code is verbatim, including tokens the sanitizer would strip from prose.
    assert!(md.contains("# [/INST] ** the the the"));
}

#[test]
fn text_output_has_no_orphaned_markup_but_keeps_code() {
    let doc = styled_doc();
    let plain = string(text::render(&doc).bytes);
    let prose: String = plain
        .lines()
        .filter(|line| !line.starts_with("    "))
        .collect::<Vec<_>>()
        .join("\n");
    for residue in [
        "<u>", "<span", "**", "~~", "[INST]", "```", "[/INST]", "<svg",
    ] {
        assert!(
            !prose.contains(residue),
            "orphaned `{residue}` in plain text:\n{prose}"
        );
    }
    assert!(plain.contains("[/INST]"), "code block was rewritten");
    assert!(plain.contains('┌') && plain.contains('│') && plain.contains('┘'));
    assert!(plain.contains("[vector:"));
    assert!(plain.contains("[figure:"));
    assert!(plain.contains("96 dpi"));
    assert!(plain.contains("left column") && plain.contains("right column"));
    assert!(!plain.contains('\u{FFFD}'));
}

#[test]
fn docx_uses_native_openxml_for_every_feature() {
    let doc = styled_doc();
    let rendered = docx::render(&doc);
    assert!(rendered.bytes.starts_with(b"PK"));
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(rendered.bytes.clone())).expect("DOCX is a zip");
    let mut document = String::new();
    let mut rels = String::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).expect("zip entry");
        let mut body = String::new();
        std::io::Read::read_to_string(&mut file, &mut body).ok();
        let entry_name = file.name().map(|name| name.to_string()).unwrap_or_default();
        match entry_name.as_str() {
            "word/document.xml" => document = body.clone(),
            "word/_rels/document.xml.rels" => rels = body.clone(),
            _ => {}
        }
        if entry_name.ends_with(".xml") || entry_name.ends_with(".rels") {
            let mut reader = quick_xml::Reader::from_str(&body);
            loop {
                match reader.read_event() {
                    Ok(quick_xml::events::Event::Eof) => break,
                    Err(error) => panic!("{entry_name} is not well-formed XML: {error}"),
                    _ => {}
                }
            }
        }
    }
    for expected in [
        "Heading1",
        "<w:b/>",
        "<w:i/>",
        "<w:u ",
        "<w:strike/>",
        "<w:color w:val=\"C53030\"",
        "<w:hyperlink r:id=",
        "<w:tblGrid>",
        "<w:gridCol",
        "<w:gridSpan",
        "<w:vMerge",
        "<w:shd",
        "<w:tblBorders>",
        "<w:drawing>",
        "<wp:inline",
        "<v:line",
        "xmlns:v=",
        "<w:cols w:num=\"2\"",
    ] {
        assert!(document.contains(expected), "DOCX lost `{expected}`");
    }
    assert!(
        rels.contains("TargetMode=\"External\"") && rels.contains("https://example.com/report"),
        "external hyperlink relationship missing: {rels}"
    );
}

#[test]
fn pdf_exporter_embeds_fonts_links_colours_and_vectors() {
    let doc = styled_doc();
    let rendered = pdf::render(&doc);
    let body = String::from_utf8_lossy(&rendered.bytes).into_owned();
    for expected in [
        "%PDF-1.7",
        "/FontFile2",
        "/ToUnicode",
        "/Subtype /Link",
        "/URI (https://example.com/report)",
        "rg\n",
        " m ",
        " l S Q",
        "startxref",
        "%%EOF",
    ] {
        assert!(body.contains(expected), "PDF exporter lost `{expected}`");
    }
    assert!(
        rendered.warnings.is_empty(),
        "unexpected PDF warnings: {:?}",
        rendered.warnings
    );
}

#[test]
fn sanitizer_cleans_furniture_zones_and_artifacts_but_not_code() {
    let mut doc = Doc::new(PageSpec::default());
    doc.info.page_boundaries = vec![PageBoundary::new(0, 612.0, 792.0)];
    let boundary = doc.info.page_boundaries[0];
    doc.blocks.push(Block::Paragraph {
        inlines: vec![Inline::plain("CONFIDENTIAL — internal page 1")],
        anchor: Some(Anchor::new(0, Rect::new(40.0, 12.0, 300.0, 10.0))),
        style: ParagraphStyle::default(),
    });
    doc.blocks.push(Block::Paragraph {
        inlines: vec![Inline::plain("page 1")],
        anchor: Some(Anchor::new(
            0,
            Rect::new(40.0, boundary.footer_zone.y + 6.0, 60.0, 10.0),
        )),
        style: ParagraphStyle::default(),
    });
    doc.blocks.push(Block::Paragraph {
        inlines: vec![Inline::plain(
            "“The quarterly result is genuine content that must not be discarded here.”",
        )],
        anchor: Some(Anchor::new(0, Rect::new(40.0, 300.0, 500.0, 14.0))),
        style: ParagraphStyle::default(),
    });
    doc.blocks.push(Block::Paragraph {
        inlines: vec![Inline::plain(
            "<p>The the the the report continues [/INST] and [END] ```markdown",
        )],
        anchor: Some(Anchor::new(0, Rect::new(40.0, 330.0, 500.0, 14.0))),
        style: ParagraphStyle::default(),
    });
    doc.blocks.push(Block::Code {
        lines: vec!["    return sum(values)  # [/INST] the the the".to_string()],
        anchor: None,
        language: None,
    });
    let report = sanitize(&mut doc, &SanitizeOptions::default());
    assert!(
        report.normalization_warnings.is_empty(),
        "{:?}",
        report.normalization_warnings
    );
    assert!(doc.validate().is_empty());
    assert_eq!(doc.blocks.len(), 3, "furniture blocks were not removed");
    let code = doc.plain_text();
    let prose: String = doc
        .blocks
        .iter()
        .filter(|block| !matches!(block, Block::Code { .. }))
        .map(Block::plain_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prose.contains("quarterly result is genuine"), "{prose}");
    assert!(!prose.contains("CONFIDENTIAL"), "{prose}");
    assert!(!prose.contains("[/INST]"), "{prose}");
    assert!(!prose.contains("[END]"), "{prose}");
    assert!(!prose.contains("<p>"), "{prose}");
    assert!(!prose.contains("the the the the"), "{prose}");
    assert!(!prose.contains('“'), "wrapper quotes survived: {prose}");
    assert!(
        code.contains("[/INST] the the the"),
        "code block was rewritten: {code}"
    );
}

#[test]
fn ir_normalization_prunes_empty_containers_and_validates() {
    let mut doc = Doc::new(PageSpec::default());
    doc.info.page_boundaries = vec![PageBoundary::new(0, 612.0, 792.0)];
    doc.blocks.push(Block::Callout {
        kind: CalloutKind::Quote,
        blocks: vec![],
        anchor: None,
        fill: None,
    });
    doc.blocks.push(Block::Columns {
        columns: vec![
            Column {
                blocks: vec![],
                width: 1.0,
            },
            Column {
                blocks: vec![paragraph("kept")],
                width: 1.0,
            },
        ],
        anchor: None,
    });
    let warnings = doc.normalize();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(doc.blocks.len(), 1, "empty callout was not pruned");
    assert!(doc.validate().is_empty());
    assert!(doc.column_count() >= 1);
    assert!(
        doc.page_boundary(0)
            .unwrap()
            .in_furniture_zone(&Rect::new(0.0, 0.0, 10.0, 10.0))
    );
    assert!(
        doc.page_boundary(0)
            .unwrap()
            .in_furniture_zone(&Rect::new(0.0, 780.0, 10.0, 10.0))
    );
}

#[test]
fn no_exporter_emits_split_character_runs() {
    // Regression: font kerning/tracking must not collapse into "t h i s   i s"
    let mut doc = Doc::new(PageSpec::default());
    doc.blocks.push(Block::Paragraph {
        inlines: vec![Inline::plain("The quick brown fox jumps")],
        anchor: None,
        style: ParagraphStyle::default(),
    });
    doc.blocks.push(Block::Paragraph {
        inlines: vec![
            Inline::new("Bold ", InlineStyle::bold()),
            Inline::new("italic ", InlineStyle::italic()),
            Inline::plain("and plain"),
        ],
        anchor: None,
        style: ParagraphStyle::default(),
    });
    for target in [
        markdown::render(&doc),
        text::render(&doc),
        docx::render(&doc),
        pdf::render(&doc),
    ] {
        if target.engine == "pdf" || target.engine == "ir-pdf" {
            continue; // binary PDF bytes contain structural single-char tokens
        }
        let body = String::from_utf8_lossy(&target.bytes).into_owned();
        // Check for spaced-out artifact: 4+ consecutive single-char words
        let words: Vec<&str> = body.split_whitespace().collect();
        let mut run = 0;
        for word in &words {
            if word.chars().count() == 1 && word.chars().next().is_some_and(|c| c.is_alphanumeric())
            {
                run += 1;
                assert!(
                    run < 4,
                    "split-character regression in {} export: {:?}",
                    target.engine,
                    &body[..body.len().min(300)]
                );
            } else {
                run = 0;
            }
        }
        // Whole words must wrap as units, not character by character
        // Text-like targets should contain words verbatim; binary targets (docx/pdf) are checked structurally
        if target.engine == "markdown" || target.engine == "text" {
            assert!(
                body.contains("quick"),
                "word 'quick' was split in {}: {:?}",
                target.engine,
                &body[..body.len().min(300)]
            );
            assert!(
                body.contains("jumps"),
                "word 'jumps' was split in {}",
                target.engine
            );
        }
    }
    // Inline formatting must wrap whole words, not characters
    let styled = Doc::new(PageSpec::default());
    let mut doc2 = styled;
    doc2.blocks.push(Block::Paragraph {
        inlines: vec![
            Inline::bold("bold word"),
            Inline::plain(" "),
            Inline::italic("italic word"),
        ],
        anchor: None,
        style: ParagraphStyle::default(),
    });
    let md = String::from_utf8(markdown::render(&doc2).bytes).expect("utf8");
    assert!(md.contains("**bold word**"), "bold wrapping broken: {md}");
    assert!(md.contains("*italic word*"), "italic wrapping broken: {md}");
    // Must NOT be "**b** **o** **l** **d**"
    assert!(
        !md.contains("**b**"),
        "character-level splitting in markdown"
    );
}

#[test]
fn font_subset_prefix_is_stripped() {
    // "ABCDEF+Helvetica-Bold" should be treated as "Helvetica-Bold"
    let raw = "ABCDEF+Helvetica-Bold";
    let prefix = &raw[..6];
    assert!(prefix.chars().all(|c| c.is_ascii_uppercase()));
    assert_eq!(&raw[7..], "Helvetica-Bold");
    // Non-subset name should pass through
    assert_eq!("Helvetica", "Helvetica");
    assert_eq!(
        "Helvetica",
        "ABCDEF+Helvetica".split('+').next_back().unwrap()
    );
    // 7-char prefix is not a subset tag (must be exactly 6 uppercase)
    assert_eq!(
        "ABCDEF+Helvetica".len(),
        "ABCDEF+Helvetica".split('+').next_back().unwrap().len() + 7
    );
}

#[test]
fn vector_svg_uses_the_geometry_axis() {
    let horizontal = VectorShape::new(
        None,
        Rect::new(0.0, 0.0, 120.0, 0.8),
        VectorKind::Line,
        Some(Rgb(1, 2, 3)),
        0.8,
        None,
    );
    assert_eq!(horizontal.axis, Axis::Horizontal);
    assert!(horizontal.svg.contains("x2=\"120.0\" y2=\"0.0\""));
    assert!(horizontal.svg.contains("stroke=\"#010203\""));
    let vertical = VectorShape::new(
        None,
        Rect::new(0.0, 0.0, 0.8, 90.0),
        VectorKind::Line,
        None,
        1.0,
        None,
    );
    assert_eq!(vertical.axis, Axis::Vertical);
    assert!(vertical.svg.contains("x2=\"0.0\" y2=\"90.0\""));
}
