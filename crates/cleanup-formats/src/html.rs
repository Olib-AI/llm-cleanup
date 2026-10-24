//! Lossless byte-splicing HTML adapter (`.html`, `.htm`).
//!
//! HTML documents exported from chat interfaces, Claude Artifacts, Notion/Google Docs, CMS
//! editors, and static-site pipelines carry fingerprints in two places:
//!
//! 1. **Body prose** inside `<p>`, `<li>`, `<td>`, headings, and inline spans (`<span>`, `<em>`,
//!    `<strong>`, `<a>`), including tokens escaped as `&lt;|im_end|&gt;` or zero-width code
//!    points encoded as `&#x200B;` / `&#8203;`.
//! 2. **Document metadata and attributes**: C2PA 2.x `<link rel="c2pa-manifest">` and
//!    `<script type="application/c2pa">` bindings, C2PA structured-text comments, `<meta>` AI
//!    provenance declarations (`name="generator"`, `name="ai-generated"`, IPTC
//!    `trainedAlgorithmicMedia`), and AI attribution query parameters on `<a href="…">` links.
//!
//! Like [`Docx`](crate::Docx), this adapter never re-serializes a DOM: untouched tags,
//! indentation, attribute quoting, and entities remain byte-for-byte identical.

use crate::c2pa;
use cleanup_core::ir::{
    DecodedSegment, DocMeta, Document, FormatId, ProseSpan, SpanAnchor, detect_newline,
};
use cleanup_core::url::clean_ai_tracking_url;
use cleanup_core::{
    ContainerFinding, CoreError, Edit, EditSet, Format, Result, render_byte_spliced,
};
use std::ops::Range;

#[derive(Debug, Default, Clone, Copy)]
pub struct Html;

const RAW_TEXT_ELEMENTS: &[&str] = &["script", "style", "textarea", "xmp"];

const PROTECTED_ELEMENTS: &[&str] = &[
    "head", "title", "script", "style", "textarea", "xmp", "code", "pre", "kbd", "samp", "var",
    "svg", "math", "noscript", "template",
];

const BLOCK_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "caption",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "html",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
    "br",
];

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

const HTML_AI_GENERATORS: &[&str] = &[
    "chatgpt",
    "openai",
    "claude",
    "anthropic",
    "gemini",
    "copilot",
    "perplexity",
    "deepseek",
    "grok",
    "qwen",
    "kimi",
    "cursor",
    "windsurf",
    "lovable",
    "v0.dev",
    "bolt.new",
    "genspark",
    "manus",
    "notebooklm",
];

#[derive(Debug, Clone)]
enum HtmlToken {
    Tag {
        range: Range<usize>,
        name_lower: String,
        is_close: bool,
        self_closing: bool,
    },
    Comment(Range<usize>),
    Markup(Range<usize>),
    RawProtected(Range<usize>),
    Text(Range<usize>),
}

#[derive(Debug, Clone)]
struct AttrSpan {
    name_lower: String,
    value: String,
    value_range: Range<usize>,
}

fn scan_html(src: &str) -> Vec<HtmlToken> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        if src[cursor..].starts_with("<!--") {
            let end = src[cursor + 4..]
                .find("-->")
                .map_or(bytes.len(), |rel| cursor + 4 + rel + 3);
            tokens.push(HtmlToken::Comment(cursor..end));
            cursor = end;
            continue;
        }
        if src[cursor..].starts_with("<![CDATA[") {
            let end = src[cursor + 9..]
                .find("]]>")
                .map_or(bytes.len(), |rel| cursor + 9 + rel + 3);
            tokens.push(HtmlToken::Markup(cursor..end));
            cursor = end;
            continue;
        }
        if src[cursor..].starts_with("<!") || src[cursor..].starts_with("<?") {
            let end = scan_to_tag_close(bytes, cursor + 2);
            tokens.push(HtmlToken::Markup(cursor..end));
            cursor = end;
            continue;
        }
        if let Some((tag_end, name_lower, is_close, self_closing)) = parse_tag_at(src, cursor) {
            tokens.push(HtmlToken::Tag {
                range: cursor..tag_end,
                name_lower: name_lower.clone(),
                is_close,
                self_closing,
            });
            cursor = tag_end;
            if !is_close && !self_closing && RAW_TEXT_ELEMENTS.contains(&name_lower.as_str()) {
                let raw_end = find_raw_text_close(src, cursor, &name_lower);
                if raw_end > cursor {
                    tokens.push(HtmlToken::RawProtected(cursor..raw_end));
                    cursor = raw_end;
                }
            }
            continue;
        }

        // Plain text up to the next valid markup opener.
        let text_start = cursor;
        cursor += 1;
        while cursor < bytes.len() {
            if bytes[cursor] == b'<'
                && (src[cursor..].starts_with("<!--")
                    || src[cursor..].starts_with("<!")
                    || src[cursor..].starts_with("<?")
                    || parse_tag_at(src, cursor).is_some())
            {
                break;
            }
            cursor += 1;
        }
        tokens.push(HtmlToken::Text(text_start..cursor));
    }

    tokens
}

fn scan_to_tag_close(bytes: &[u8], mut i: usize) -> usize {
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'>' {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn parse_tag_at(src: &str, pos: usize) -> Option<(usize, String, bool, bool)> {
    let bytes = src.as_bytes();
    if bytes.get(pos) != Some(&b'<') {
        return None;
    }
    let mut i = pos + 1;
    let is_close = if bytes.get(i) == Some(&b'/') {
        i += 1;
        true
    } else {
        false
    };
    if !bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    let name_start = i;
    while bytes
        .get(i)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(*b, b'-' | b':' | b'_'))
    {
        i += 1;
    }
    let name_lower = src[name_start..i].to_ascii_lowercase();
    if let Some(&next) = bytes.get(i)
        && !next.is_ascii_whitespace()
        && next != b'>'
        && next != b'/'
    {
        return None;
    }
    let end = scan_to_tag_close(bytes, i);
    if bytes.get(end.saturating_sub(1)) != Some(&b'>') {
        return None;
    }
    let before_gt = src[i..end - 1].trim_end();
    let self_closing =
        !is_close && (before_gt.ends_with('/') || VOID_ELEMENTS.contains(&name_lower.as_str()));
    Some((end, name_lower, is_close, self_closing))
}

fn find_raw_text_close(src: &str, from: usize, tag_name_lower: &str) -> usize {
    let bytes = src.as_bytes();
    let mut i = from;
    while let Some(rel) = src[i..].find("</") {
        let cand = i + rel;
        let name_at = cand + 2;
        let name_end = name_at + tag_name_lower.len();
        if let Some(slice) = src.get(name_at..name_end)
            && slice.eq_ignore_ascii_case(tag_name_lower)
            && bytes
                .get(name_end)
                .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
        {
            return cand;
        }
        i = cand + 2;
    }
    bytes.len()
}

/// Parse attributes inside `<tag ...>` (given the whole tag range in `src`).
fn parse_attributes(src: &str, tag_range: &Range<usize>) -> Vec<AttrSpan> {
    let tag_str = &src[tag_range.clone()];
    let bytes = tag_str.as_bytes();
    let mut i = 1usize;
    if bytes.get(i) == Some(&b'/') {
        return Vec::new();
    }
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/'
    {
        i += 1;
    }

    let mut attrs = Vec::new();
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len()
            || bytes[i] == b'>'
            || (bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'>'))
        {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let name_lower = tag_str[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            attrs.push(AttrSpan {
                name_lower,
                value: String::new(),
                value_range: (tag_range.start + i)..(tag_range.start + i),
            });
            continue;
        }
        i += 1; // '='
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if matches!(bytes[i], b'"' | b'\'') {
            let quote = bytes[i];
            i += 1;
            let val_start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let val_end = i;
            if i < bytes.len() && bytes[i] == quote {
                i += 1;
            }
            attrs.push(AttrSpan {
                name_lower,
                value: tag_str[val_start..val_end].to_string(),
                value_range: (tag_range.start + val_start)..(tag_range.start + val_end),
            });
        } else {
            let val_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                i += 1;
            }
            let val_end = i;
            attrs.push(AttrSpan {
                name_lower,
                value: tag_str[val_start..val_end].to_string(),
                value_range: (tag_range.start + val_start)..(tag_range.start + val_end),
            });
        }
    }
    attrs
}

/// Decode HTML syntax entities (`&lt;`, `&gt;`, `&amp;`, `&quot;`, `&apos;`) and numeric
/// character references (`&#8203;`, `&#x200B;`), leaving named typographic entities such as
/// `&mdash;` intact so [`DecodeHtmlEntities`](../../cleanup_rules/repair/struct.DecodeHtmlEntities.html)
/// can report and normalize them with rule attribution while `<p>AT&amp;T</p>` stays untouched.
fn decode_html_syntax_run(raw: &str, base_offset: usize) -> (String, DecodedSegment) {
    let mut text = String::with_capacity(raw.len());
    let mut boundaries = Vec::with_capacity(raw.len() + 1);
    let bytes = raw.as_bytes();
    let mut cursor = 0usize;

    while cursor < raw.len() {
        if bytes[cursor] == b'&'
            && let Some(semi_rel) = raw[cursor + 1..].find(';')
        {
            let entity_end = cursor + 1 + semi_rel + 1;
            let entity = &raw[cursor + 1..entity_end - 1];
            if let Some(decoded) = decode_syntax_entity(entity) {
                let mut ch_buf = [0u8; 4];
                let rendered = decoded.encode_utf8(&mut ch_buf);
                boundaries.push(cursor);
                for _ in 1..rendered.len() {
                    boundaries.push(entity_end);
                }
                text.push_str(rendered);
                cursor = entity_end;
                continue;
            }
        }
        let ch = raw[cursor..].chars().next().expect("cursor < raw.len()");
        for offset in 0..ch.len_utf8() {
            boundaries.push(cursor + offset);
        }
        text.push(ch);
        cursor += ch.len_utf8();
    }
    boundaries.push(raw.len());
    (
        text,
        DecodedSegment {
            source: base_offset..(base_offset + raw.len()),
            boundaries,
        },
    )
}

fn decode_syntax_entity(entity: &str) -> Option<char> {
    if entity.eq_ignore_ascii_case("lt") {
        return Some('<');
    }
    if entity.eq_ignore_ascii_case("gt") {
        return Some('>');
    }
    if entity.eq_ignore_ascii_case("amp") {
        return Some('&');
    }
    if entity.eq_ignore_ascii_case("quot") {
        return Some('"');
    }
    if entity.eq_ignore_ascii_case("apos") {
        return Some('\'');
    }
    let code = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        entity.strip_prefix('#')?.parse::<u32>().ok()?
    };
    char::from_u32(code)
}

fn escape_html_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn flush_block_segments(
    spans: &mut Vec<ProseSpan>,
    buf_text: &mut String,
    buf_segs: &mut Vec<DecodedSegment>,
) {
    if !buf_segs.is_empty() && buf_text.chars().any(|c| !c.is_whitespace()) {
        spans.push(ProseSpan::new(
            SpanAnchor::DecodedSegments(std::mem::take(buf_segs)),
            std::mem::take(buf_text),
        ));
    } else {
        buf_segs.clear();
        buf_text.clear();
    }
}

impl Format for Html {
    fn id(&self) -> FormatId {
        FormatId::Html
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let had_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("HTML is not valid UTF-8: {e}")))?;
        let newline = detect_newline(src);
        let body_offset = if had_bom { 3 } else { 0 };

        let tokens = scan_html(&src[body_offset..]);
        let mut spans = Vec::new();
        let mut protected_stack: Vec<String> = Vec::new();
        let mut buf_text = String::new();
        let mut buf_segs: Vec<DecodedSegment> = Vec::new();

        for tok in tokens {
            match tok {
                HtmlToken::Tag {
                    name_lower,
                    is_close,
                    self_closing,
                    ..
                } => {
                    if BLOCK_ELEMENTS.contains(&name_lower.as_str())
                        || PROTECTED_ELEMENTS.contains(&name_lower.as_str())
                    {
                        flush_block_segments(&mut spans, &mut buf_text, &mut buf_segs);
                    }
                    if PROTECTED_ELEMENTS.contains(&name_lower.as_str()) {
                        if is_close {
                            if let Some(pos) =
                                protected_stack.iter().rposition(|n| n == &name_lower)
                            {
                                protected_stack.truncate(pos);
                            }
                        } else if !self_closing {
                            protected_stack.push(name_lower);
                        }
                    }
                }
                HtmlToken::RawProtected(_) => {
                    flush_block_segments(&mut spans, &mut buf_text, &mut buf_segs);
                }
                HtmlToken::Comment(_) | HtmlToken::Markup(_) => {}
                HtmlToken::Text(rel_range) if protected_stack.is_empty() => {
                    let abs_start = body_offset + rel_range.start;
                    let raw = &src[abs_start..body_offset + rel_range.end];
                    let (decoded, seg) = decode_html_syntax_run(raw, abs_start);
                    buf_text.push_str(&decoded);
                    buf_segs.push(seg);
                }
                HtmlToken::Text(_) => {}
            }
        }
        flush_block_segments(&mut spans, &mut buf_text, &mut buf_segs);

        Ok(Document::new(
            bytes.to_vec(),
            spans,
            DocMeta {
                format: FormatId::Html,
                newline,
                had_bom,
            },
        ))
    }

    fn render(&self, doc: &Document, edits: &EditSet) -> Result<Vec<u8>> {
        if edits.edits.is_empty() {
            return Ok(doc.source.clone());
        }
        let escaped = EditSet {
            edits: edits
                .edits
                .iter()
                .map(|edit| Edit {
                    span_index: edit.span_index,
                    range: edit.range.clone(),
                    replacement: escape_html_text(&edit.replacement),
                    rule_id: edit.rule_id.clone(),
                })
                .collect(),
        };
        render_byte_spliced(doc, &escaped)
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("HTML is not valid UTF-8: {e}")))?;
        let body = src.strip_prefix('\u{FEFF}').unwrap_or(src);
        let mut out = String::new();
        for tok in scan_html(body) {
            match tok {
                HtmlToken::Tag {
                    range,
                    name_lower,
                    is_close,
                    self_closing,
                } => {
                    out.push('<');
                    if is_close {
                        out.push('/');
                    }
                    out.push_str(&name_lower);
                    for attr in parse_attributes(body, &range) {
                        out.push(' ');
                        out.push_str(&attr.name_lower);
                        out.push('=');
                        if attr.name_lower == "href"
                            && let Some(cleaned) = clean_ai_tracking_url(&attr.value)
                        {
                            out.push_str(&cleaned);
                        } else {
                            out.push_str(&attr.value);
                        }
                    }
                    if self_closing {
                        out.push('/');
                    }
                    out.push('>');
                }
                HtmlToken::RawProtected(r) => {
                    out.push_str("raw:");
                    out.push_str(&body[r]);
                    out.push('\n');
                }
                HtmlToken::Markup(r) => {
                    out.push_str("decl:");
                    out.push_str(&body[r]);
                    out.push('\n');
                }
                HtmlToken::Comment(_) | HtmlToken::Text(_) => {}
            }
        }
        Ok(out)
    }

    fn container_findings(&self, doc: &Document) -> Vec<ContainerFinding> {
        let Ok(src) = std::str::from_utf8(&doc.source) else {
            return Vec::new();
        };
        inspect_or_scrub_html_container(src, false).0
    }

    fn scrub_container(&self, _doc: &Document, rendered: &[u8]) -> Option<Vec<u8>> {
        let src = std::str::from_utf8(rendered).ok()?;
        inspect_or_scrub_html_container(src, true).1
    }
}

#[derive(Debug)]
enum HtmlReplacement {
    Drop(Range<usize>),
    Replace(Range<usize>, String),
}

fn inspect_or_scrub_html_container(
    src: &str,
    build_scrubbed: bool,
) -> (Vec<ContainerFinding>, Option<Vec<u8>>) {
    let tokens = scan_html(src);
    let mut findings = Vec::new();
    let mut edits: Vec<HtmlReplacement> = Vec::new();

    // 1. C2PA structured-text comment blocks (`<!-- -----BEGIN C2PA MANIFEST----- ... -->`).
    for range in c2pa::find_structured_text_manifests(src) {
        findings.push(ContainerFinding {
            rule_id: "container.c2pa-manifest".to_string(),
            message: "HTML comment carries a C2PA structured-text manifest".to_string(),
            scrubbable: true,
            applied: false,
        });
        edits.push(HtmlReplacement::Drop(range));
    }

    let mut idx = 0usize;
    while idx < tokens.len() {
        match &tokens[idx] {
            HtmlToken::Tag {
                range,
                name_lower,
                is_close: false,
                ..
            } => {
                let attrs = parse_attributes(src, range);
                if name_lower == "link" && has_c2pa_rel(&attrs) {
                    findings.push(ContainerFinding {
                        rule_id: "container.c2pa-manifest".to_string(),
                        message:
                            "HTML <link rel=\"c2pa-manifest\"> references an external C2PA manifest"
                                .to_string(),
                        scrubbable: true,
                        applied: false,
                    });
                    edits.push(HtmlReplacement::Drop(expand_standalone_line(
                        src,
                        range.clone(),
                    )));
                } else if name_lower == "script" && has_c2pa_script_type(&attrs) {
                    let mut end = range.end;
                    if let Some(HtmlToken::RawProtected(raw_r)) = tokens.get(idx + 1) {
                        end = raw_r.end;
                        if let Some(HtmlToken::Tag {
                            range: close_r,
                            name_lower: close_name,
                            is_close: true,
                            ..
                        }) = tokens.get(idx + 2)
                            && close_name == "script"
                        {
                            end = close_r.end;
                            idx += 2;
                        }
                    } else if let Some(HtmlToken::Tag {
                        range: close_r,
                        name_lower: close_name,
                        is_close: true,
                        ..
                    }) = tokens.get(idx + 1)
                        && close_name == "script"
                    {
                        end = close_r.end;
                        idx += 1;
                    }
                    findings.push(ContainerFinding {
                        rule_id: "container.c2pa-manifest".to_string(),
                        message:
                            "HTML <script type=\"application/c2pa\"> embeds a C2PA manifest store"
                                .to_string(),
                        scrubbable: true,
                        applied: false,
                    });
                    edits.push(HtmlReplacement::Drop(expand_standalone_line(
                        src,
                        range.start..end,
                    )));
                } else if name_lower == "meta"
                    && let Some(msg) = classify_ai_meta_tag(&attrs)
                {
                    findings.push(ContainerFinding {
                        rule_id: "container.html-ai-metadata".to_string(),
                        message: msg,
                        scrubbable: true,
                        applied: false,
                    });
                    edits.push(HtmlReplacement::Drop(expand_standalone_line(
                        src,
                        range.clone(),
                    )));
                } else if name_lower == "a" {
                    for attr in attrs {
                        if attr.name_lower == "href"
                            && let Some(cleaned) = clean_ai_tracking_url(&attr.value)
                        {
                            findings.push(ContainerFinding {
                                rule_id: "markers.ai-tracking-urls.container".to_string(),
                                message: "HTML <a href> carries AI-provider attribution parameter"
                                    .to_string(),
                                scrubbable: true,
                                applied: false,
                            });
                            edits.push(HtmlReplacement::Replace(attr.value_range, cleaned));
                        }
                    }
                }
            }
            HtmlToken::Comment(range) => {
                let overlaps_c2pa = edits.iter().any(|e| match e {
                    HtmlReplacement::Drop(r) | HtmlReplacement::Replace(r, _) => {
                        r.start < range.end && range.start < r.end
                    }
                });
                if !overlaps_c2pa && let Some(msg) = classify_ai_html_comment(&src[range.clone()]) {
                    findings.push(ContainerFinding {
                        rule_id: "container.html-ai-metadata".to_string(),
                        message: msg,
                        scrubbable: true,
                        applied: false,
                    });
                    edits.push(HtmlReplacement::Drop(expand_standalone_line(
                        src,
                        range.clone(),
                    )));
                }
            }
            _ => {}
        }
        idx += 1;
    }

    if edits.is_empty() || !build_scrubbed {
        return (findings, None);
    }

    edits.sort_by_key(|e| match e {
        HtmlReplacement::Drop(r) | HtmlReplacement::Replace(r, _) => (r.start, r.end),
    });

    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut cursor = 0usize;
    for edit in edits {
        match edit {
            HtmlReplacement::Drop(r) => {
                if r.start < cursor {
                    continue;
                }
                out.extend_from_slice(&bytes[cursor..r.start]);
                cursor = r.end;
            }
            HtmlReplacement::Replace(r, repl) => {
                if r.start < cursor {
                    continue;
                }
                out.extend_from_slice(&bytes[cursor..r.start]);
                out.extend_from_slice(repl.as_bytes());
                cursor = r.end;
            }
        }
    }
    if cursor < bytes.len() {
        out.extend_from_slice(&bytes[cursor..]);
    }
    if out == bytes {
        (findings, None)
    } else {
        (findings, Some(out))
    }
}

fn has_c2pa_rel(attrs: &[AttrSpan]) -> bool {
    attrs.iter().any(|a| {
        a.name_lower == "rel"
            && a.value
                .split_ascii_whitespace()
                .any(|tok| tok.eq_ignore_ascii_case("c2pa-manifest"))
    })
}

fn has_c2pa_script_type(attrs: &[AttrSpan]) -> bool {
    attrs.iter().any(|a| {
        a.name_lower == "type"
            && (a.value.eq_ignore_ascii_case("application/c2pa")
                || a.value
                    .eq_ignore_ascii_case("application/x-c2pa-manifest-store"))
    })
}

fn classify_ai_meta_tag(attrs: &[AttrSpan]) -> Option<String> {
    let key = attrs
        .iter()
        .find(|a| matches!(a.name_lower.as_str(), "name" | "property" | "itemprop"))?
        .value
        .to_ascii_lowercase();
    let content = attrs
        .iter()
        .find(|a| a.name_lower == "content")
        .map(|a| a.value.as_str())
        .unwrap_or("");
    let content_lower = content.to_ascii_lowercase();

    if matches!(
        key.as_str(),
        "ai-generated"
            | "ai_generated"
            | "ai-content-declaration"
            | "ai-model"
            | "llm-model"
            | "c2pa"
            | "c2pa:manifest"
            | "dcterms:provenance"
    ) {
        return Some(format!(
            "HTML <meta name={key:?}> declares AI provenance ({content:?})"
        ));
    }
    if content_lower.contains("trainedalgorithmicmedia")
        || content_lower.contains("compositewithtrainedalgorithmicmedia")
    {
        return Some(format!(
            "HTML <meta name={key:?}> carries IPTC AI digitalSourceType"
        ));
    }
    if matches!(key.as_str(), "generator" | "author" | "creator")
        && let Some(gen_name) = HTML_AI_GENERATORS
            .iter()
            .find(|g| content_lower.contains(**g))
    {
        return Some(format!(
            "HTML <meta name={key:?}> names AI tool {gen_name:?}"
        ));
    }
    None
}

pub(crate) fn classify_ai_html_comment(comment_raw: &str) -> Option<String> {
    let inner = comment_raw
        .strip_prefix("<!--")?
        .strip_suffix("-->")?
        .trim();
    if inner.is_empty() || inner.len() > 280 {
        return None;
    }
    let lower = inner.to_ascii_lowercase();
    let has_provenance_verb = lower.starts_with("generated by ")
        || lower.starts_with("generated with ")
        || lower.starts_with("created by ")
        || lower.starts_with("created with ")
        || lower.starts_with("exported from ")
        || lower.starts_with("ai-generated")
        || lower.starts_with("c2pa:");
    if !has_provenance_verb {
        return None;
    }
    if lower.starts_with("ai-generated") || lower.starts_with("c2pa:") {
        return Some(format!("HTML comment declares AI provenance: {inner:?}"));
    }
    HTML_AI_GENERATORS
        .iter()
        .find(|g| lower.contains(**g))
        .map(|gen_name| format!("HTML comment names AI generator {gen_name:?}"))
}

fn expand_standalone_line(src: &str, range: Range<usize>) -> Range<usize> {
    let line_start = src[..range.start].rfind('\n').map_or(0, |n| n + 1);
    if !src[line_start..range.start]
        .bytes()
        .all(|b| matches!(b, b' ' | b'\t'))
    {
        return range;
    }
    let rest = &src.as_bytes()[range.end..];
    let mut i = 0usize;
    while i < rest.len() && matches!(rest[i], b' ' | b'\t') {
        i += 1;
    }
    if rest.get(i..i + 2) == Some(b"\r\n") {
        line_start..(range.end + i + 2)
    } else if rest.get(i) == Some(&b'\n') {
        line_start..(range.end + i + 1)
    } else {
        range
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_html_preserves_ampersand_entity_byte_for_byte() {
        let html = Html;
        let raw = b"<!DOCTYPE html>\n<html><body><p>AT&amp;T &amp; Sons</p></body></html>\n";
        let doc = html.parse(raw).unwrap();
        assert_eq!(doc.spans.len(), 1);
        assert_eq!(doc.spans[0].text, "AT&T & Sons");
        let out = html.render(&doc, &EditSet::default()).unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn html_container_scrubs_c2pa_link_ai_meta_and_href_tracking() {
        let html = Html;
        let raw = b"<!DOCTYPE html>\n<html>\n<head>\n  <meta name=\"generator\" content=\"ChatGPT\">\n  <link rel=\"c2pa-manifest\" href=\"/manifest.c2pa\">\n</head>\n<body>\n  <p><a href=\"https://example.com/post?a=1&amp;utm_source=chatgpt.com\">Source</a></p>\n</body>\n</html>\n";
        let doc = html.parse(raw).unwrap();
        let findings = html.container_findings(&doc);
        assert_eq!(findings.len(), 3);
        let scrubbed = html.scrub_container(&doc, raw).expect("should scrub HTML");
        let text = std::str::from_utf8(&scrubbed).unwrap();
        assert!(!text.contains("c2pa-manifest"));
        assert!(!text.contains("ChatGPT"));
        assert!(text.contains("href=\"https://example.com/post?a=1\""));
    }
}
