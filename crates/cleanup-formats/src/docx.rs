//! DOCX adapter.
//!
//! A `.docx` is a zip (OPC package). Cleanable prose lives in `<w:t>` text nodes, and not
//! only in the body: LLM generators (and Word users pasting LLM output) put fingerprints
//! in headers, footers, footnotes, endnotes and reviewer comments too, and detectors
//! scan all of them. We therefore treat every prose-bearing part — `word/document.xml`,
//! `word/header*.xml`, `word/footer*.xml`, `word/footnotes.xml`, `word/endnotes.xml`,
//! `word/comments.xml` — uniformly. We:
//!   1. extract each prose part and find the byte range of each `<w:t>`'s text content
//!      (the WHOLE inner content, so XML entities like `&amp;` stay intact),
//!   2. group the `<w:t>` of each paragraph (`<w:p>`) into ONE logical span so phrases split
//!      across runs are matched (the "multi-run problem"), splitting at `<w:tab>`/`<w:br>` so
//!      runs are never glued into false matches across a visual break. `Document::source`
//!      is the parts' XML concatenated (each span's segments carry its part's base offset),
//!   3. run the rules on that logical text and map each edit back onto the runs it covers
//!      (first run gets the replacement, later runs are trimmed) — every run keeps its `<w:rPr>`,
//!   4. rebuild the zip, splicing only the parts that received edits and copying every
//!      other part (images, shapes, styles, numbering, …) byte for byte, so formatting
//!      and embedded objects are preserved exactly.
//!
//! Package metadata is a fingerprint of its own: AI pipelines (ChatGPT's code
//! interpreter, Claude/Gemini document features) typically write files with libraries
//! such as python-docx or the JS `docx` package, whose default templates stamp
//! `docProps/core.xml`/`docProps/app.xml` with recognizable creators, descriptions and
//! template dates — and resaving in Word does *not* clear creator/description. We
//! report such metadata as container findings at every level, and (Aggressive only,
//! via [`Format::scrub_container`]) empty the matched creator/description/application
//! fields.

use cleanup_core::ir::{
    DecodedSegment, DocMeta, Document, FormatId, Newline, ProseSpan, SpanAnchor,
};
use cleanup_core::splice::render_byte_spliced;
use cleanup_core::{ContainerFinding, CoreError, Edit, EditSet, Format, Result};
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

/// Zip entry names, in archive order.
fn part_names(zip_bytes: &[u8]) -> Result<Vec<String>> {
    let mut archive = ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| CoreError::Parse(format!("not a valid .docx (zip) file: {e}")))?;
    (0..archive.len())
        .map(|i| {
            let f = archive
                .by_index_raw(i)
                .map_err(|e| CoreError::Parse(format!("zip read: {e}")))?;
            let name = f
                .name()
                .map_err(|e| CoreError::Parse(format!("zip entry name: {e}")))?;
            Ok(name.into_owned())
        })
        .collect()
}

/// Does this package part carry `<w:t>` prose?
fn is_prose_part(name: &str) -> bool {
    // Header/footer parts are numbered (header1.xml, footer2.xml, …); the notes and
    // comments parts have fixed names. Deliberately name-based: every mainstream
    // producer (Word, Google Docs, LibreOffice, python-docx, …) uses these names.
    fn numbered(name: &str, prefix: &str) -> bool {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(".xml"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }
    name == DOC_PART
        || matches!(
            name,
            "word/footnotes.xml" | "word/endnotes.xml" | "word/comments.xml"
        )
        || numbered(name, "word/header")
        || numbered(name, "word/footer")
}

/// All prose parts with their XML, in the canonical order used everywhere below: the
/// body first, then the rest alphabetically. Deterministic order matters — the
/// concatenated [`Document::source`] (and therefore every span offset) is built from it
/// independently in `parse`, `render` and `skeleton`. `word/document.xml` is required
/// (a `.docx` without a body is not a word-processing document at all).
fn prose_parts(zip_bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut names: Vec<String> = part_names(zip_bytes)?
        .into_iter()
        .filter(|n| is_prose_part(n))
        .collect();
    names.sort_by(|a, b| (a != DOC_PART, a).cmp(&(b != DOC_PART, b)));
    if !names.iter().any(|n| n == DOC_PART) {
        return Err(CoreError::Parse(format!(
            "{DOC_PART} not found in .docx: not a word-processing document"
        )));
    }
    let mut archive = ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| CoreError::Parse(format!("not a valid .docx (zip) file: {e}")))?;
    names
        .into_iter()
        .map(|name| {
            let mut buf = Vec::new();
            archive
                .by_name(&name)
                .map_err(|e| CoreError::Parse(format!("{name} not found in .docx: {e}")))?
                .read_to_end(&mut buf)?;
            Ok((name, buf))
        })
        .collect()
}

/// Concatenate the parts' XML into the single buffer spans are anchored into
/// (`\n`-separated, which no `<w:t>` content can straddle), returning each part's
/// base offset within it. This is the one place the layout is defined.
fn concat_parts(parts: &[(String, Vec<u8>)]) -> (Vec<u8>, Vec<usize>) {
    let mut source = Vec::new();
    let mut bases = Vec::with_capacity(parts.len());
    for (_, xml) in parts {
        bases.push(source.len());
        source.extend_from_slice(xml);
        source.push(b'\n');
    }
    (source, bases)
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
                "w:p" => current = Some(Vec::new()),
                // text content begins right after the `<w:t …>` start tag
                "w:t" => wt_start = Some(reader.buffer_position() as usize),
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                // A tab/line-break/carriage-return separates runs visually; flush the runs
                // accumulated so far as one span so a phrase never glues across the break.
                if matches!(e.name().as_ref(), "w:tab" | "w:br" | "w:cr")
                    && let Some(segs) = current.as_mut()
                    && !segs.is_empty()
                {
                    paragraphs.push(std::mem::take(segs));
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                "w:t" => {
                    if let Some(s) = wt_start.take()
                        && pre > s
                    {
                        push_seg(&mut paragraphs, &mut current, s..pre);
                    }
                }
                "w:p" => {
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

/// Decode one `<w:t>` body while retaining an exact map back to its XML bytes.
///
/// XML requires `<` to be escaped and generators commonly escape `>`, `&`, quotes, and
/// non-ASCII characters as numeric entities. Running rules over the raw body would therefore
/// miss exactly the angle-bracket tokens used by chat templates and reasoning traces. A
/// decoded character can occupy a different number of bytes from its entity, so a plain
/// `Segments` anchor is not sufficient here.
fn decode_xml_text(raw: &str, source: Range<usize>) -> (String, DecodedSegment) {
    fn entity_char(body: &str) -> Option<char> {
        match body {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "apos" => Some('\''),
            "quot" => Some('"'),
            _ => {
                let (digits, radix) = if let Some(hex) =
                    body.strip_prefix("#x").or_else(|| body.strip_prefix("#X"))
                {
                    (hex, 16)
                } else {
                    let decimal = body.strip_prefix('#')?;
                    (decimal, 10)
                };
                if digits.is_empty() {
                    return None;
                }
                u32::from_str_radix(digits, radix)
                    .ok()
                    .and_then(char::from_u32)
            }
        }
    }

    let mut decoded = String::with_capacity(raw.len());
    let mut boundaries = vec![0usize];
    let mut cursor = 0usize;
    while cursor < raw.len() {
        let mut consumed = 0usize;
        let mut ch = None;
        if raw.as_bytes()[cursor] == b'&'
            && let Some(relative_end) = raw[cursor + 1..].find(';')
        {
            let end = cursor + 1 + relative_end;
            // XML's named entities are short and numeric character references are bounded in
            // practice. The cap avoids scanning a huge run of ordinary text after a stray `&`.
            if end - cursor <= 16
                && let Some(decoded_char) = entity_char(&raw[cursor + 1..end])
            {
                ch = Some(decoded_char);
                consumed = end + 1 - cursor;
            }
        }
        if consumed == 0 {
            ch = raw[cursor..].chars().next();
            consumed = ch.expect("cursor is inside raw").len_utf8();
        }
        let ch = ch.expect("a source character or decoded entity");
        decoded.push(ch);
        // Interior bytes of a multi-byte decoded character are not legal edit boundaries.
        // Fill them with the entity's start; legal boundaries below map exactly.
        while boundaries.len() < decoded.len() {
            boundaries.push(cursor);
        }
        boundaries.push(cursor + consumed);
        cursor += consumed;
    }
    debug_assert_eq!(boundaries.len(), decoded.len() + 1);
    debug_assert_eq!(boundaries.last(), Some(&(source.end - source.start)));
    (decoded, DecodedSegment { source, boundaries })
}

/// Rebuild the zip from the original, replacing the given parts. Everything else
/// (images, shapes, styles, numbering, untouched prose parts, …) is copied verbatim.
fn rezip(original: &[u8], replacements: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    rezip_with_deletions(original, replacements, &[])
}

fn rezip_with_deletions(
    original: &[u8],
    replacements: &[(String, Vec<u8>)],
    deletions: &[String],
) -> Result<Vec<u8>> {
    let names = part_names(original)?;
    let mut archive = ZipArchive::new(Cursor::new(original))
        .map_err(|e| CoreError::Parse(format!("re-reading .docx zip failed: {e}")))?;

    let mut out = Vec::new();
    {
        let mut writer = ZipWriter::new(Cursor::new(&mut out));
        for (i, name) in names.iter().enumerate() {
            if deletions.iter().any(|d| d == name) {
                continue;
            }
            if let Some((_, bytes)) = replacements.iter().find(|(n, _)| n == name) {
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                writer
                    .start_file(name, opts)
                    .map_err(|e| CoreError::Parse(format!("zip write: {e}")))?;
                writer.write_all(bytes)?;
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

/// Structural fingerprint of prose-part XML (possibly several parts concatenated):
/// element names + attributes, with `<w:t>` text content excluded entirely (so editing
/// or emptying a run's text leaves the fingerprint unchanged).
fn xml_skeleton(xml: &[u8]) -> Result<String> {
    fn write_open(out: &mut String, e: &quick_xml::events::BytesStart, self_closing: bool) {
        out.push('<');
        out.push_str(e.name().as_ref());
        for a in e.attributes().flatten() {
            out.push(' ');
            out.push_str(a.key.as_ref());
            out.push('=');
            out.push_str(&a.value);
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
                if e.name().as_ref() == "w:t" {
                    in_wt = true;
                }
            }
            Ok(Event::Empty(e)) => write_open(&mut out, &e, true),
            Ok(Event::End(e)) => {
                if e.name().as_ref() == "w:t" {
                    in_wt = false;
                }
                out.push_str("</");
                out.push_str(e.name().as_ref());
                out.push('>');
            }
            Ok(Event::Text(t)) if !in_wt => {
                out.push_str(t.as_ref());
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(CoreError::Parse(format!("malformed document.xml: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

const CORE_PART: &str = "docProps/core.xml";
const APP_PART: &str = "docProps/app.xml";
const CUSTOM_PART: &str = "docProps/custom.xml";
const CONTENT_TYPES_PART: &str = "[Content_Types].xml";

/// Case-insensitive substrings that betray programmatic/AI generation in package
/// metadata fields. "python-docx" is what ChatGPT's code interpreter writes; the model
/// and vendor names appear when AI document features stamp themselves as author; the
/// library names mark the other common programmatic pipelines.
const GENERATOR_SIGNS: &[&str] = &[
    "python-docx",
    "docx-rs",
    "chatgpt",
    "openai",
    "gpt-",
    "claude",
    "anthropic",
    "gemini",
    "copilot",
    "deepseek",
    "qwen",
    "mistral",
    "perplexity",
    "grok",
    "kimi",
    "manus",
    "genspark",
    "notebooklm",
    "apache poi",
    "docx4j",
    "generated by ai",
    "ai-generated",
    "large language model",
    "c2pa",
    "content credentials",
];

const AI_CUSTOM_PROP_NAMES: &[&str] = &[
    "aigenerated",
    "ai_generated",
    "ai-generated",
    "aidisclosure",
    "ai_disclosure",
    "ai-disclosure",
    "aimodel",
    "ai_model",
    "ai-model",
    "llm_model",
    "ai_provider",
    "c2pa",
    "c2pa_manifest",
    "contentcredentials",
    "digitalsourcetype",
];

/// python-docx's bundled default template was saved at this exact instant; every file
/// written without touching core properties still carries it (revision 1, zero editing
/// time) no matter when it was actually generated.
const PYTHON_DOCX_TEMPLATE_DATE: &str = "2013-12-23T23:15:00Z";

/// Inner-text byte ranges of the named leaf elements (matched by local name, so any
/// namespace prefix works), in document order — the metadata analogue of `paragraph_wt_ranges`.
fn element_text_ranges(xml: &[u8], wanted: &[&str]) -> Vec<(String, Range<usize>)> {
    let mut reader = Reader::from_reader(xml);
    let mut out = Vec::new();
    let mut open: Option<(String, usize)> = None;
    let mut buf = Vec::new();
    loop {
        let pre = reader.buffer_position() as usize;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let qname = e.name();
                let local_name = qname.local_name();
                let name: &str = local_name.as_ref();
                if wanted.contains(&name) {
                    open = Some((name.to_string(), reader.buffer_position() as usize));
                }
            }
            Ok(Event::End(e)) => {
                if let Some((name, start)) = open.take() {
                    if e.name().local_name().as_ref() == name.as_str() && pre >= start {
                        out.push((name, start..pre));
                    } else {
                        // Mismatched close (nested markup): keep waiting for ours.
                        open = Some((name, start));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// One generator-metadata hit: `(field, value, text range, scrubbable)`.
type MetaHit = (String, String, Range<usize>, bool);

/// Scan one `docProps` part for generator fingerprints. Pure text-field matching —
/// the fields below are the ones Word shows in File ▸ Properties and the ones
/// detector tooling reads.
fn metadata_hits(part: &str, xml: &[u8]) -> Vec<MetaHit> {
    let fields: &[&str] = if part == CORE_PART {
        &["creator", "lastModifiedBy", "description", "created"]
    } else {
        &["Application", "AppVersion"]
    };
    let mut found: Vec<(String, String, Range<usize>)> = Vec::new();
    for (field, range) in element_text_ranges(xml, fields) {
        let Ok(raw) = std::str::from_utf8(&xml[range.clone()]) else {
            continue;
        };
        let value = raw.trim();
        if value.is_empty() {
            continue;
        }
        found.push((field, value.to_string(), range));
    }
    // Generator libraries copy the Office version of the reference document they ship with
    // instead of the version of Word actually in use. The *pair* is the fingerprint, and it
    // is only reportable: a genuine Word for Mac 2011 file carries the same Application, so
    // neither value alone — nor the pair — justifies rewriting a field we cannot attribute.
    let field_value = |name: &str| {
        found
            .iter()
            .find(|(field, _, _)| field == name)
            .map(|(_, value, _)| value.as_str())
    };
    let library_template = matches!(
        (field_value("Application"), field_value("AppVersion")),
        (Some("Microsoft Macintosh Word"), Some("14.0000"))
            | (Some("Microsoft Word 12.0.0"), Some("12.0000"))
    );
    let mut hits = Vec::new();
    for (field, value, range) in found {
        let lower = value.to_lowercase();
        if field == "created" {
            // Template-date fingerprint: report-only (we never invent a date).
            if value == PYTHON_DOCX_TEMPLATE_DATE {
                hits.push((field, value, range, false));
            }
        } else if field == "Application" && library_template {
            hits.push((field, value, range, false));
        } else if field == "AppVersion" {
            // Only the Application/AppVersion pair is meaningful; a version string on its
            // own is the user's Word build.
            continue;
        } else if GENERATOR_SIGNS.iter().any(|s| lower.contains(s))
            || (field == "creator" && lower == "un-named")
        {
            hits.push((field, value, range, true));
        }
    }
    hits
}

#[derive(Debug)]
struct RelationshipUrlHit {
    part: String,
    range: Range<usize>,
    replacement: String,
}

/// Find an attribute's raw value range inside a relationship start/empty tag. Keeping the
/// range, rather than reserializing the XML, preserves namespace prefixes, attribute order,
/// whitespace, and every unrelated relationship byte.
fn raw_attribute_range(tag: &[u8], wanted: &[u8]) -> Option<Range<usize>> {
    let mut cursor = 0usize;
    while cursor < tag.len() {
        if tag[cursor].is_ascii_whitespace() || matches!(tag[cursor], b'<' | b'/' | b'>' | b'=') {
            cursor += 1;
            continue;
        }
        let name_start = cursor;
        while cursor < tag.len()
            && !tag[cursor].is_ascii_whitespace()
            && !matches!(tag[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        let name_end = cursor;
        while cursor < tag.len() && tag[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if tag.get(name_start..name_end) == Some(wanted) && tag.get(cursor) == Some(&b'=') {
            cursor += 1;
            while cursor < tag.len() && tag[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            let quote = *tag.get(cursor)?;
            if !matches!(quote, b'\'' | b'"') {
                return None;
            }
            let start = cursor + 1;
            let end = tag[start..].iter().position(|b| *b == quote)? + start;
            return Some(start..end);
        }
        // Continue at the next attribute separator. The name scan already stopped at a
        // delimiter, so this loop always makes progress.
    }
    None
}

fn relationship_tag_hit(xml: &[u8], tag_range: Range<usize>) -> Option<(Range<usize>, String)> {
    let tag = xml.get(tag_range.clone())?;
    let type_range = raw_attribute_range(tag, b"Type")?;
    let type_value = std::str::from_utf8(tag.get(type_range)?).ok()?;
    if !type_value.ends_with("/hyperlink") {
        return None;
    }
    if let Some(mode_range) = raw_attribute_range(tag, b"TargetMode") {
        let mode = std::str::from_utf8(tag.get(mode_range)?).ok()?;
        if !mode.eq_ignore_ascii_case("External") {
            return None;
        }
    }
    let target_range = raw_attribute_range(tag, b"Target")?;
    let target = std::str::from_utf8(tag.get(target_range.clone())?).ok()?;
    let replacement = cleanup_core::url::clean_ai_tracking_url(target)?;
    Some((
        (tag_range.start + target_range.start)..(tag_range.start + target_range.end),
        replacement,
    ))
}

/// External hyperlink targets live in `.rels`, outside `<w:t>` prose. Inspect them as
/// package structure and never touch internal relationships or non-hyperlink targets.
fn relationship_url_hits(zip_bytes: &[u8]) -> Vec<RelationshipUrlHit> {
    let Ok(names) = part_names(zip_bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in names.into_iter().filter(|name| name.ends_with(".rels")) {
        let Ok(xml) = read_part(zip_bytes, &name) else {
            continue;
        };
        let mut reader = Reader::from_reader(xml.as_slice());
        let mut buf = Vec::new();
        loop {
            let pre = reader.buffer_position() as usize;
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e))
                    if e.name().local_name().as_ref() == "Relationship" =>
                {
                    let end = reader.buffer_position() as usize;
                    if let Some((range, replacement)) = relationship_tag_hit(&xml, pre..end) {
                        out.push(RelationshipUrlHit {
                            part: name.clone(),
                            range,
                            replacement,
                        });
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }
    out
}

/// Read a part if present; metadata inspection is advisory and must never fail a clean.
fn read_part_opt(zip_bytes: &[u8], part: &str) -> Option<Vec<u8>> {
    read_part(zip_bytes, part).ok()
}

impl Format for Docx {
    fn id(&self) -> FormatId {
        FormatId::Docx
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let parts = prose_parts(bytes)?;
        let (source, bases) = concat_parts(&parts);
        let mut spans = Vec::new();
        for ((name, xml), base) in parts.iter().zip(bases) {
            for segs in paragraph_wt_ranges(xml)? {
                let mut text = String::new();
                let mut decoded_segments = Vec::with_capacity(segs.len());
                for r in segs {
                    let raw = std::str::from_utf8(&xml[r.clone()]).map_err(|e| {
                        CoreError::Encoding(format!("{name} text is not UTF-8: {e}"))
                    })?;
                    let source = (r.start + base)..(r.end + base);
                    let (decoded, segment) = decode_xml_text(raw, source);
                    text.push_str(&decoded);
                    decoded_segments.push(segment);
                }
                spans.push(ProseSpan::new(
                    SpanAnchor::DecodedSegments(decoded_segments),
                    text,
                ));
            }
        }
        let meta = DocMeta {
            format: FormatId::Docx,
            newline: Newline::Lf,
            had_bom: false,
        };
        let mut doc = Document::new(source, spans, meta);
        doc.aux = bytes.to_vec();
        Ok(doc)
    }

    fn render(&self, doc: &Document, edits: &EditSet) -> Result<Vec<u8>> {
        // Escape replacements so spliced text can never make any part's XML malformed.
        let escaped: Vec<Edit> = edits
            .edits
            .iter()
            .map(|e| Edit {
                span_index: e.span_index,
                range: e.range.clone(),
                replacement: xml_escape(&e.replacement),
                rule_id: e.rule_id.clone(),
            })
            .collect();

        // Re-derive the part layout from the original zip (the same deterministic order
        // `parse` used), then splice each part on its own so offsets stay part-local.
        let parts = prose_parts(&doc.aux)?;
        let (_, bases) = concat_parts(&parts);
        // Global span index -> part ordinal, via each span's first segment.
        let span_part: Vec<usize> = doc
            .spans
            .iter()
            .map(|span| {
                let start = match &span.anchor {
                    SpanAnchor::Segments(segs) => segs.first().map_or(0, |r| r.start),
                    SpanAnchor::DecodedSegments(segs) => segs.first().map_or(0, |s| s.source.start),
                    SpanAnchor::Bytes(r) => r.start,
                };
                // The part whose [base, base+len) window contains the span.
                parts
                    .iter()
                    .zip(&bases)
                    .position(|((_, xml), base)| start >= *base && start < base + xml.len())
                    .unwrap_or(0)
            })
            .collect();

        let mut replacements = Vec::new();
        for (pi, ((name, xml), base)) in parts.iter().zip(&bases).enumerate() {
            // This part's spans (in global order) with ranges shifted part-local,
            // and its edits renumbered accordingly.
            let mut local_spans: Vec<ProseSpan> = Vec::new();
            let mut renumber: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();
            for (gi, span) in doc.spans.iter().enumerate() {
                if span_part[gi] != pi {
                    continue;
                }
                let anchor = match &span.anchor {
                    SpanAnchor::Segments(segs) => SpanAnchor::Segments(
                        segs.iter()
                            .map(|r| (r.start - base)..(r.end - base))
                            .collect(),
                    ),
                    SpanAnchor::DecodedSegments(segs) => SpanAnchor::DecodedSegments(
                        segs.iter()
                            .map(|segment| DecodedSegment {
                                source: (segment.source.start - base)..(segment.source.end - base),
                                boundaries: segment.boundaries.clone(),
                            })
                            .collect(),
                    ),
                    SpanAnchor::Bytes(r) => SpanAnchor::Bytes((r.start - base)..(r.end - base)),
                };
                renumber.insert(gi, local_spans.len());
                local_spans.push(ProseSpan::new(anchor, span.text.clone()));
            }
            let local_edits: Vec<Edit> = escaped
                .iter()
                .filter_map(|e| {
                    renumber.get(&e.span_index).map(|&li| Edit {
                        span_index: li,
                        range: e.range.clone(),
                        replacement: e.replacement.clone(),
                        rule_id: e.rule_id.clone(),
                    })
                })
                .collect();
            if local_edits.is_empty() {
                continue; // untouched part: raw-copied by rezip
            }
            let part_doc = Document::new(xml.clone(), local_spans, doc.meta.clone());
            let edited = render_byte_spliced(&part_doc, &EditSet { edits: local_edits })?;
            replacements.push((name.clone(), edited));
        }
        rezip(&doc.aux, &replacements)
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        // Cover every prose part, so the pipeline's self-check also proves the
        // header/footer/footnote/comment structure survived. For a zip this is the
        // skeleton of the same concatenated layout `parse` built `Document::source`
        // from; raw XML input (a lone part) is fingerprinted as-is.
        let xml = if bytes.starts_with(b"PK\x03\x04") {
            concat_parts(&prose_parts(bytes)?).0
        } else {
            bytes.to_vec()
        };
        xml_skeleton(&xml)
    }

    fn container_findings(&self, doc: &Document) -> Vec<ContainerFinding> {
        let mut out = Vec::new();
        for (part, message) in c2pa_package_parts(&doc.aux) {
            let _ = part;
            out.push(ContainerFinding {
                rule_id: "container.c2pa-manifest".to_string(),
                message,
                scrubbable: true,
                applied: false,
            });
        }
        for media_part in media_part_names(&doc.aux) {
            if let Some(bytes) = read_part_opt(&doc.aux, &media_part) {
                out.extend(crate::c2pa::inspect_media(&bytes, &media_part));
            }
        }
        for hit in relationship_url_hits(&doc.aux) {
            out.push(ContainerFinding {
                rule_id: "markers.ai-tracking-urls.container".to_string(),
                message: format!(
                    "{} external hyperlink target contains AI-provider attribution",
                    hit.part
                ),
                scrubbable: true,
                applied: false,
            });
        }
        for part in [CORE_PART, APP_PART] {
            let Some(xml) = read_part_opt(&doc.aux, part) else {
                continue;
            };
            for (field, value, _, scrubbable) in metadata_hits(part, &xml) {
                let message = if field == "created" {
                    format!(
                        "{part} creation date {value:?} is the python-docx default \
                         template stamp — the file was almost certainly generated \
                         programmatically (report-only; dates are never rewritten)"
                    )
                } else if field == "Application" && !scrubbable {
                    format!(
                        "{part} <Application> = {value:?} paired with the reference \
                         template's AppVersion is a generator-library default \
                         (python-docx / pandoc), not the user's Word build (report-only)"
                    )
                } else {
                    format!(
                        "{part} <{field}> = {value:?} matches a known AI/programmatic \
                         generator fingerprint; resaving in Word does not clear it"
                    )
                };
                out.push(ContainerFinding {
                    rule_id: "container.docx-generator-metadata".to_string(),
                    message,
                    scrubbable,
                    applied: false,
                });
            }
        }
        if let Some(custom_xml) = read_part_opt(&doc.aux, CUSTOM_PART) {
            for (prop_name, prop_val, _) in custom_property_hits(&custom_xml) {
                out.push(ContainerFinding {
                    rule_id: "container.docx-ai-custom-props".to_string(),
                    message: format!(
                        "{CUSTOM_PART} custom property {prop_name:?} = {prop_val:?} declares AI provenance"
                    ),
                    scrubbable: true,
                    applied: false,
                });
            }
        }
        out
    }

    fn scrub_container(&self, _doc: &Document, rendered: &[u8]) -> Option<Vec<u8>> {
        let mut replacements = Vec::new();
        let deletions: Vec<String> = c2pa_package_parts(rendered)
            .into_iter()
            .map(|(part, _)| part)
            .collect();

        if !deletions.is_empty()
            && let Some(ct_xml) = read_part_opt(rendered, CONTENT_TYPES_PART)
            && let Some(cleaned_ct) = strip_content_type_overrides(&ct_xml, &deletions)
        {
            replacements.push((CONTENT_TYPES_PART.to_string(), cleaned_ct));
        }

        for media_part in media_part_names(rendered) {
            if let Some(bytes) = read_part_opt(rendered, &media_part)
                && let Some(scrubbed_media) = crate::c2pa::scrub_media(&bytes)
            {
                replacements.push((media_part, scrubbed_media));
            }
        }

        let mut relationship_parts: std::collections::BTreeMap<
            String,
            Vec<(Range<usize>, String)>,
        > = std::collections::BTreeMap::new();
        for hit in relationship_url_hits(rendered) {
            relationship_parts
                .entry(hit.part)
                .or_default()
                .push((hit.range, hit.replacement));
        }
        for (part, edits) in relationship_parts {
            // Relationship hits are offsets in the decompressed part, not in the zip. Apply
            // them to that part's XML before rezip; never splice them into `rendered` itself.
            let xml = read_part_opt(rendered, &part)?;
            let scrubbed = cleanup_core::splice(&xml, &edits).ok()?;
            replacements.push((part, scrubbed));
        }
        for part in [CORE_PART, APP_PART] {
            let Some(xml) = read_part_opt(rendered, part) else {
                continue;
            };
            let edits: Vec<(Range<usize>, String)> = metadata_hits(part, &xml)
                .into_iter()
                .filter(|(_, _, _, scrubbable)| *scrubbable)
                .map(|(_, _, range, _)| (range, String::new()))
                .collect();
            if edits.is_empty() {
                continue;
            }
            let scrubbed = cleanup_core::splice(&xml, &edits).ok()?;
            replacements.push((part.to_string(), scrubbed));
        }
        if let Some(custom_xml) = read_part_opt(rendered, CUSTOM_PART) {
            let edits: Vec<(Range<usize>, String)> = custom_property_hits(&custom_xml)
                .into_iter()
                .map(|(_, _, range)| (range, String::new()))
                .collect();
            if !edits.is_empty() {
                let scrubbed = cleanup_core::splice(&custom_xml, &edits).ok()?;
                replacements.push((CUSTOM_PART.to_string(), scrubbed));
            }
        }
        if replacements.is_empty() && deletions.is_empty() {
            return None;
        }
        rezip_with_deletions(rendered, &replacements, &deletions).ok()
    }
}

/// Inspect OOXML archive entries for C2PA Manifest Stores (`META-INF/content_credential.c2pa`
/// and validated `.c2pa` JUMBF sidecar entries).
fn c2pa_package_parts(zip_bytes: &[u8]) -> Vec<(String, String)> {
    let Ok(names) = part_names(zip_bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in names {
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".c2pa") {
            continue;
        }
        let Some(bytes) = read_part_opt(zip_bytes, &name) else {
            continue;
        };
        if let Some(summary) = crate::c2pa::inspect_jumbf_c2pa(&bytes) {
            out.push((name.clone(), summary.describe(&name)));
        } else if lower == "meta-inf/content_credential.c2pa" {
            out.push((
                name.clone(),
                format!("{name} carries an OOXML C2PA Content Credentials store"),
            ));
        }
    }
    out
}

fn media_part_names(zip_bytes: &[u8]) -> Vec<String> {
    part_names(zip_bytes)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| name.starts_with("word/media/"))
        .collect()
}

/// Inspect `docProps/custom.xml` for AI provenance custom properties (`AIGenerated`,
/// `AIDisclosure`, `ai_model`, `c2pa_manifest`, or custom values naming known AI tools).
/// Returns `(property_name, value, inner_value_range)`. Clearing the inner value text rather
/// than deleting the `<property pid="…">` node preserves ECMA-376 contiguous `pid` numbering.
fn custom_property_hits(xml: &[u8]) -> Vec<(String, String, Range<usize>)> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut current_prop: Option<String> = None;
    let mut value_open: Option<(String, usize)> = None;
    let mut out = Vec::new();

    loop {
        let pre = reader.buffer_position() as usize;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let qname = e.name();
                let local = qname.local_name();
                if local.as_ref() == "property" {
                    current_prop = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == "name")
                        .map(|a| a.value.to_string());
                } else if current_prop.is_some() {
                    value_open = Some((
                        local.as_ref().to_string(),
                        reader.buffer_position() as usize,
                    ));
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                let local = qname.local_name();
                if local.as_ref() == "property" {
                    current_prop = None;
                    value_open = None;
                } else if let (Some(prop_name), Some((val_tag, val_start))) =
                    (&current_prop, value_open.take())
                    && local.as_ref() == val_tag.as_str()
                    && pre >= val_start
                    && let Ok(raw_val) = std::str::from_utf8(&xml[val_start..pre])
                {
                    let trimmed = raw_val.trim();
                    if !trimmed.is_empty() && is_ai_custom_property(prop_name, trimmed) {
                        out.push((prop_name.clone(), trimmed.to_string(), val_start..pre));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn is_ai_custom_property(name: &str, value: &str) -> bool {
    let name_lower = name.to_ascii_lowercase();
    let val_lower = value.to_ascii_lowercase();
    if AI_CUSTOM_PROP_NAMES.iter().any(|k| *k == name_lower) {
        return true;
    }
    if val_lower.contains("trainedalgorithmicmedia")
        || val_lower.contains("compositewithtrainedalgorithmicmedia")
    {
        return true;
    }
    (name_lower.contains("generator")
        || name_lower.contains("author")
        || name_lower.contains("source")
        || name_lower.contains("model")
        || name_lower.contains("provenance"))
        && GENERATOR_SIGNS.iter().any(|sig| val_lower.contains(sig))
}

fn strip_content_type_overrides(xml: &[u8], deleted_parts: &[String]) -> Option<Vec<u8>> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();

    loop {
        let pre = reader.buffer_position() as usize;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) if e.name().local_name().as_ref() == "Override" => {
                let end = reader.buffer_position() as usize;
                let matches_deleted = e.attributes().flatten().any(|a| {
                    a.key.local_name().as_ref() == "PartName" && {
                        let pn = a.value.trim_start_matches('/');
                        deleted_parts.iter().any(|d| d == pn)
                    }
                });
                if matches_deleted {
                    edits.push((pre..end, String::new()));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    if edits.is_empty() {
        return None;
    }
    cleanup_core::splice(xml, &edits).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part_xml(root: &str, paras: &[&str]) -> String {
        let ps: String = paras
            .iter()
            .map(|t| format!("<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"))
            .collect();
        format!(
            r#"<?xml version="1.0"?><{root} xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{ps}</{root}>"#
        )
    }

    fn make_docx(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = ZipWriter::new(Cursor::new(&mut buf));
            for (name, xml) in entries {
                w.start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
                w.write_all(xml.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf
    }

    fn part_text(zip_bytes: &[u8], part: &str) -> String {
        String::from_utf8(read_part(zip_bytes, part).unwrap()).unwrap()
    }

    fn replace_in(text: &str, needle: &str) -> Range<usize> {
        let start = text.find(needle).unwrap();
        start..start + needle.len()
    }

    #[test]
    fn xml_entities_are_decoded_for_rules_without_losing_source_offsets() {
        let raw = "A&amp;B &lt;|im_end|&gt; &#x200B;";
        let (decoded, segment) = decode_xml_text(raw, 10..10 + raw.len());
        assert_eq!(decoded, "A&B <|im_end|> \u{200B}");
        assert_eq!(segment.decoded_len(), decoded.len());
        assert_eq!(segment.boundaries.first(), Some(&0));
        assert_eq!(segment.boundaries.last(), Some(&raw.len()));

        let doc = part_xml("w:document", &["&lt;|im_end|&gt;"]);
        let bytes = make_docx(&[(DOC_PART, &doc)]);
        let parsed = Docx.parse(&bytes).unwrap();
        assert_eq!(parsed.spans[0].text, "<|im_end|>");
        let output = Docx
            .render(
                &parsed,
                &EditSet {
                    edits: vec![Edit {
                        span_index: 0,
                        range: 0..parsed.spans[0].text.len(),
                        replacement: String::new(),
                        rule_id: "test".to_string(),
                    }],
                },
            )
            .unwrap();
        assert!(part_text(&output, DOC_PART).contains("<w:t></w:t>"));
    }

    #[test]
    fn prose_parts_cover_headers_and_footnotes() {
        let doc = part_xml("w:document", &["Body text."]);
        let hdr = part_xml("w:hdr", &["Header text."]);
        let ftn = r#"<?xml version="1.0"?><w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1"><w:p><w:r><w:t>Footnote text.</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
        let bytes = make_docx(&[
            (DOC_PART, &doc),
            ("word/header1.xml", &hdr),
            ("word/footnotes.xml", ftn),
            ("word/styles.xml", "<w:styles/>"),
        ]);
        let fmt = Docx;
        let parsed = fmt.parse(&bytes).unwrap();
        let texts: Vec<&str> = parsed.spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["Body text.", "Footnote text.", "Header text."]);

        // Edit the header span only; the other parts must round-trip byte-identical.
        let (hi, hspan) = parsed
            .spans
            .iter()
            .enumerate()
            .find(|(_, s)| s.text == "Header text.")
            .unwrap();
        let edits = EditSet {
            edits: vec![Edit {
                span_index: hi,
                range: replace_in(&hspan.text, "Header"),
                replacement: "[clean]".to_string(),
                rule_id: "test".to_string(),
            }],
        };
        let out = fmt.render(&parsed, &edits).unwrap();
        assert!(part_text(&out, "word/header1.xml").contains("[clean] text."));
        assert_eq!(read_part(&out, DOC_PART).unwrap(), doc.as_bytes());
        assert_eq!(
            read_part(&out, "word/footnotes.xml").unwrap(),
            ftn.as_bytes()
        );
        // Structure fingerprint is unchanged by a pure text edit, across all parts.
        assert_eq!(fmt.skeleton(&bytes).unwrap(), fmt.skeleton(&out).unwrap());
        assert_eq!(
            fmt.skeleton(&parsed.source).unwrap(),
            fmt.skeleton(&bytes).unwrap()
        );
    }

    #[test]
    fn comments_and_endnotes_are_prose_parts() {
        let doc = part_xml("w:document", &["Body."]);
        let comments = r#"<?xml version="1.0"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="0"><w:p><w:r><w:t>Reviewer note.</w:t></w:r></w:p></w:comment></w:comments>"#;
        let bytes = make_docx(&[
            (DOC_PART, &doc),
            ("word/comments.xml", comments),
            ("word/header12.xml", &part_xml("w:hdr", &["H12."])),
            (
                "word/headerSettings.xml",
                &part_xml("w:hdr", &["Not prose."]),
            ),
        ]);
        let parsed = Docx.parse(&bytes).unwrap();
        let texts: Vec<&str> = parsed.spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["Body.", "Reviewer note.", "H12."]);
    }

    #[test]
    fn generator_metadata_flagged_and_scrubbed() {
        let doc = part_xml("w:document", &["Some body."]);
        let core = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/"><dc:creator>python-docx</dc:creator><cp:lastModifiedBy>python-docx</cp:lastModifiedBy><dc:description>generated by python-docx</dc:description><dcterms:created>2013-12-23T23:15:00Z</dcterms:created></cp:coreProperties>"#;
        let app = r#"<?xml version="1.0"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>ChatGPT</Application></Properties>"#;
        let bytes = make_docx(&[(DOC_PART, &doc), (CORE_PART, core), (APP_PART, app)]);
        let fmt = Docx;
        let parsed = fmt.parse(&bytes).unwrap();

        let findings = fmt.container_findings(&parsed);
        assert_eq!(findings.len(), 5, "{findings:?}");
        assert!(findings.iter().all(|f| !f.applied));
        // Only the template-date hit is report-only.
        assert_eq!(findings.iter().filter(|f| f.scrubbable).count(), 4);

        let rendered = fmt.render(&parsed, &EditSet::default()).unwrap();
        let scrubbed = fmt.scrub_container(&parsed, &rendered).unwrap();
        let core_out = part_text(&scrubbed, CORE_PART);
        assert!(core_out.contains("<dc:creator></dc:creator>"), "{core_out}");
        assert!(core_out.contains("<cp:lastModifiedBy></cp:lastModifiedBy>"));
        assert!(core_out.contains("<dc:description></dc:description>"));
        // The date is never rewritten.
        assert!(core_out.contains("2013-12-23T23:15:00Z"));
        let app_out = part_text(&scrubbed, APP_PART);
        assert!(app_out.contains("<Application></Application>"), "{app_out}");
        // Body prose untouched by a metadata scrub.
        assert_eq!(read_part(&scrubbed, DOC_PART).unwrap(), doc.as_bytes());
    }

    /// A generator library copies its reference template's Office version into app.xml.
    /// The pair is reported (so the user knows the file was produced by a library) but is
    /// never scrubbed: a genuine Word for Mac 2011 file carries the same strings.
    #[test]
    fn library_reference_template_pair_is_reported_but_not_scrubbed() {
        let doc = part_xml("w:document", &["Some body."]);
        let app = r#"<?xml version="1.0"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Microsoft Macintosh Word</Application><AppVersion>14.0000</AppVersion></Properties>"#;
        let bytes = make_docx(&[(DOC_PART, &doc), (APP_PART, app)]);
        let parsed = Docx.parse(&bytes).unwrap();
        let findings = Docx.container_findings(&parsed);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(!findings[0].scrubbable);
        assert!(findings[0].message.contains("generator-library default"));
        assert!(Docx.scrub_container(&parsed, &bytes).is_none());

        // The real Word pair (a version the user's Word actually reported) stays silent.
        let real = r#"<?xml version="1.0"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Microsoft Office Word</Application><AppVersion>16.0000</AppVersion></Properties>"#;
        let bytes = make_docx(&[(DOC_PART, &doc), (APP_PART, real)]);
        let parsed = Docx.parse(&bytes).unwrap();
        assert!(Docx.container_findings(&parsed).is_empty());
    }

    #[test]
    fn application_alone_is_not_a_library_default() {
        // Pandoc's Application string without the matching AppVersion is ambiguous (Word
        // 2007 wrote a similar value), so it must not be reported as a library default.
        let doc = part_xml("w:document", &["Some body."]);
        let app = r#"<?xml version="1.0"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Microsoft Word 12.0.0</Application><AppVersion>16.0000</AppVersion></Properties>"#;
        let bytes = make_docx(&[(DOC_PART, &doc), (APP_PART, app)]);
        let parsed = Docx.parse(&bytes).unwrap();
        assert!(Docx.container_findings(&parsed).is_empty());
    }

    #[test]
    fn human_metadata_is_not_flagged() {
        let doc = part_xml("w:document", &["Some body."]);
        let core = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>Jane Doe</dc:creator><dc:description>Quarterly budget notes</dc:description></cp:coreProperties>"#;
        let app = r#"<?xml version="1.0"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Microsoft Office Word</Application></Properties>"#;
        let bytes = make_docx(&[(DOC_PART, &doc), (CORE_PART, core), (APP_PART, app)]);
        let parsed = Docx.parse(&bytes).unwrap();
        assert!(Docx.container_findings(&parsed).is_empty());
        assert!(Docx.scrub_container(&parsed, &bytes).is_none());
    }
}
