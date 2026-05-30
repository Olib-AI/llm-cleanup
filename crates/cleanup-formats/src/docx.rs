//! DOCX adapter.
//!
//! A `.docx` is a zip (OPC package). The cleanable prose lives in `<w:t>` text nodes inside
//! `word/document.xml`. We:
//!   1. extract `word/document.xml` and find the byte range of each `<w:t>`'s text content
//!      (the WHOLE inner content, so XML entities like `&amp;` stay intact),
//!   2. group the `<w:t>` of each paragraph (`<w:p>`) into ONE logical span so phrases split
//!      across runs are matched (the "multi-run problem"), splitting at `<w:tab>`/`<w:br>` so
//!      runs are never glued into false matches across a visual break,
//!   3. run the rules on that logical text and map each edit back onto the runs it covers
//!      (first run gets the replacement, later runs are trimmed) — every run keeps its `<w:rPr>`,
//!   4. rebuild the zip, copying every other part (images, shapes, styles, numbering, …) byte
//!      for byte, so formatting and embedded objects are preserved exactly.

use cleanup_core::ir::{DocMeta, Document, FormatId, Newline, ProseSpan, SpanAnchor};
use cleanup_core::splice::render_byte_spliced;
use cleanup_core::{CoreError, Edit, EditSet, Format, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::io::{Cursor, Read, Write};
use std::ops::Range;
use zip::{ZipArchive, ZipWriter};

pub struct Docx;

const DOC_PART: &str = "word/document.xml";

fn read_part(zip_bytes: &[u8], part: &str) -> Result<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| CoreError::Parse(format!("not a valid .docx (zip) file: {e}")))?;
    let mut file = archive
        .by_name(part)
        .map_err(|e| CoreError::Parse(format!("{part} not found in .docx: {e}")))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Escape the three characters that are significant in XML text content, so a replacement can
/// never make `document.xml` malformed. (Today's rules never emit these; this is defensive.)
fn xml_escape(s: &str) -> String {
    if !s.contains(['&', '<', '>']) {
        return s.to_string();
    }
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The byte range of each paragraph's `<w:t>` inner content, grouped per `<w:p>` and split at
/// `<w:tab>`/`<w:br>`/`<w:cr>` boundaries. Each range covers a whole `<w:t>` element's content
/// (entities included), so it round-trips losslessly.
fn paragraph_wt_ranges(xml: &[u8]) -> Result<Vec<Vec<Range<usize>>>> {
    let mut reader = Reader::from_reader(xml);
    let mut paragraphs: Vec<Vec<Range<usize>>> = Vec::new();
    let mut current: Option<Vec<Range<usize>>> = None;
    let mut wt_start: Option<usize> = None;
    let mut buf = Vec::new();

    let push_seg = |paragraphs: &mut Vec<Vec<Range<usize>>>,
                    current: &mut Option<Vec<Range<usize>>>,
                    range: Range<usize>| {
        match current.as_mut() {
            Some(segs) => segs.push(range),
            None => paragraphs.push(std::iter::once(range).collect()), // stray <w:t>
        }
    };

    loop {
        // Position before reading = start of the upcoming token. For an `End(w:t)` this is where
        // `</w:t>` begins, i.e. the end of the run's text content.
        let pre = reader.buffer_position() as usize;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"w:p" => current = Some(Vec::new()),
                // text content begins right after the `<w:t …>` start tag
                b"w:t" => wt_start = Some(reader.buffer_position() as usize),
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                // A tab/line-break/carriage-return separates runs visually; flush the runs
                // accumulated so far as one span so a phrase never glues across the break.
                if matches!(e.name().as_ref(), b"w:tab" | b"w:br" | b"w:cr")
                    && let Some(segs) = current.as_mut()
                    && !segs.is_empty()
                {
                    paragraphs.push(std::mem::take(segs));
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:t" => {
                    if let Some(s) = wt_start.take()
                        && pre > s
                    {
                        push_seg(&mut paragraphs, &mut current, s..pre);
                    }
                }
                b"w:p" => {
                    if let Some(segs) = current.take()
                        && !segs.is_empty()
                    {
                        paragraphs.push(segs);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(CoreError::Parse(format!("malformed document.xml: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    Ok(paragraphs)
}

/// Rebuild the zip from the original, replacing only `word/document.xml`. Everything else
/// (images, shapes, styles, numbering, headers/footers, …) is copied verbatim.
fn rezip(original: &[u8], doc_xml: &[u8]) -> Result<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(original))
        .map_err(|e| CoreError::Parse(format!("re-reading .docx zip failed: {e}")))?;
    let names: Vec<String> = (0..archive.len())
        .map(|i| {
            archive
                .by_index_raw(i)
                .map(|f| f.name().to_string())
                .map_err(|e| CoreError::Parse(format!("zip read: {e}")))
        })
        .collect::<Result<_>>()?;

    let mut out = Vec::new();
    {
        let mut writer = ZipWriter::new(Cursor::new(&mut out));
        for (i, name) in names.iter().enumerate() {
            if name == DOC_PART {
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                writer
                    .start_file(name, opts)
                    .map_err(|e| CoreError::Parse(format!("zip write: {e}")))?;
                writer.write_all(doc_xml)?;
            } else {
                let entry = archive
                    .by_index_raw(i)
                    .map_err(|e| CoreError::Parse(format!("zip read: {e}")))?;
                writer
                    .raw_copy_file(entry)
                    .map_err(|e| CoreError::Parse(format!("zip copy: {e}")))?;
            }
        }
        writer
            .finish()
            .map_err(|e| CoreError::Parse(format!("zip finish: {e}")))?;
    }
    Ok(out)
}

/// Structural fingerprint of `document.xml`: element names + attributes, with `<w:t>` text
/// content excluded entirely (so editing or emptying a run's text leaves the fingerprint
/// unchanged). Accepts either a full `.docx` (zip) or raw `document.xml`.
fn xml_skeleton(xml: &[u8]) -> Result<String> {
    fn write_open(out: &mut String, e: &quick_xml::events::BytesStart, self_closing: bool) {
        out.push('<');
        out.push_str(&String::from_utf8_lossy(e.name().as_ref()));
        for a in e.attributes().flatten() {
            out.push(' ');
            out.push_str(&String::from_utf8_lossy(a.key.as_ref()));
            out.push('=');
            out.push_str(&String::from_utf8_lossy(&a.value));
        }
        out.push_str(if self_closing { "/>" } else { ">" });
    }

    let mut reader = Reader::from_reader(xml);
    let mut out = String::new();
    let mut buf = Vec::new();
    let mut in_wt = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                write_open(&mut out, &e, false);
                if e.name().as_ref() == b"w:t" {
                    in_wt = true;
                }
            }
            Ok(Event::Empty(e)) => write_open(&mut out, &e, true),
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"w:t" {
                    in_wt = false;
                }
                out.push_str("</");
                out.push_str(&String::from_utf8_lossy(e.name().as_ref()));
                out.push('>');
            }
            Ok(Event::Text(t)) if !in_wt => {
                out.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(CoreError::Parse(format!("malformed document.xml: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

impl Format for Docx {
    fn id(&self) -> FormatId {
        FormatId::Docx
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let xml = read_part(bytes, DOC_PART)?;
        let paragraphs = paragraph_wt_ranges(&xml)?;
        let mut spans = Vec::with_capacity(paragraphs.len());
        for segs in paragraphs {
            let mut text = String::new();
            for r in &segs {
                let part = std::str::from_utf8(&xml[r.clone()]).map_err(|e| {
                    CoreError::Encoding(format!("document.xml text is not UTF-8: {e}"))
                })?;
                text.push_str(part);
            }
            spans.push(ProseSpan::new(SpanAnchor::Segments(segs), text));
        }
        let meta = DocMeta {
            format: FormatId::Docx,
            newline: Newline::Lf,
            had_bom: false,
        };
        let mut doc = Document::new(xml, spans, meta);
        doc.aux = bytes.to_vec();
        Ok(doc)
    }

    fn render(&self, doc: &Document, edits: &EditSet) -> Result<Vec<u8>> {
        // Escape replacements so spliced text can never make document.xml malformed.
        let escaped = EditSet {
            edits: edits
                .edits
                .iter()
                .map(|e| Edit {
                    span_index: e.span_index,
                    range: e.range.clone(),
                    replacement: xml_escape(&e.replacement),
                    rule_id: e.rule_id.clone(),
                })
                .collect(),
        };
        let edited_xml = render_byte_spliced(doc, &escaped)?;
        rezip(&doc.aux, &edited_xml)
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        let xml = if bytes.starts_with(b"PK\x03\x04") {
            read_part(bytes, DOC_PART)?
        } else {
            bytes.to_vec()
        };
        xml_skeleton(&xml)
    }
}
