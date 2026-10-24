use cleanup_core::ir::{DocMeta, Document, FormatId, ProseSpan, SpanAnchor, detect_newline};
use cleanup_core::splice::{render_byte_spliced, splice};
use cleanup_core::{ContainerFinding, CoreError, EditSet, Format, Result};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

/// Markdown adapter. Uses `pulldown-cmark`'s offset iterator purely as a *locator*:
/// it yields `(Event, Range)` where a `Text` event's range is its exact byte span in the
/// source. We collect those ranges as prose spans and never run a Markdown serializer.
pub struct Markdown;

fn options() -> Options {
    let mut o = Options::empty();
    o.insert(Options::ENABLE_TABLES);
    o.insert(Options::ENABLE_STRIKETHROUGH);
    o.insert(Options::ENABLE_FOOTNOTES);
    o.insert(Options::ENABLE_TASKLISTS);
    o
}

/// Text inside these contexts is NOT editable prose (code stays code; image alt text is
/// structural metadata). Inline code (`Event::Code`) and HTML are separate events that are
/// never `Event::Text`, so they're excluded automatically.
fn is_protected_start(tag: &Tag) -> bool {
    matches!(tag, Tag::CodeBlock(_) | Tag::Image { .. })
}
fn is_protected_end(tag: &TagEnd) -> bool {
    matches!(tag, TagEnd::CodeBlock | TagEnd::Image)
}

/// Extract the raw destination from a parser-confirmed inline link or autolink. The parser
/// decides whether the construct is really a link (including its multiline grammar); this
/// function only locates bytes inside that already-validated range.
fn destination_in_link(src: &str, link: &Range<usize>) -> Option<Range<usize>> {
    let raw = src.get(link.clone())?;
    if raw.starts_with('<') {
        let start = link.start + 1;
        let end = start + src.get(start..link.end)?.find('>')?;
        return Some(start..end);
    }

    let relative = raw.rfind("](")?;
    let open_paren = link.start + relative + 1;
    let mut cursor = open_paren + 1;
    while cursor < link.end && matches!(src.as_bytes()[cursor], b' ' | b'\t' | b'\r' | b'\n') {
        cursor += 1;
    }
    if src.as_bytes().get(cursor) == Some(&b'<') {
        let start = cursor + 1;
        let end = start + src.get(start..link.end)?.find('>')?;
        return (end <= link.end).then_some(start..end);
    }

    let start = cursor;
    while cursor < link.end
        && !matches!(src.as_bytes()[cursor], b' ' | b'\t' | b'\r' | b'\n' | b')')
    {
        cursor += 1;
    }
    (start < cursor).then_some(start..cursor)
}

/// Clean destinations belonging to actual `Link` events as well as reference-style link
/// definitions (`[id]: https://...`) outside fenced code blocks. Images, code, raw HTML, and
/// malformed `](...)` text never match, so they cannot be changed by this structural pass.
fn structural_url_edits(src: &str, frontmatter_end: usize) -> Vec<(Range<usize>, String)> {
    let mut edits = Vec::new();
    for (event, range) in Parser::new_ext(src, options()).into_offset_iter() {
        if range.start < frontmatter_end {
            continue;
        }
        let Event::Start(Tag::Link { dest_url, .. }) = event else {
            continue;
        };
        // An empty destination with a title must not have its title mistaken for a URL.
        if dest_url.is_empty() {
            continue;
        }
        let Some(destination) = destination_in_link(src, &range) else {
            continue;
        };
        let Some(replacement) = cleanup_core::url::clean_ai_tracking_url(&src[destination.clone()])
        else {
            continue;
        };
        edits.push((destination, replacement));
    }
    edits.extend(reference_definition_url_edits(src, frontmatter_end));
    edits.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.end.cmp(&b.0.end)));
    edits.dedup_by(|a, b| a.0 == b.0);
    edits
}

fn reference_definition_url_edits(
    src: &str,
    frontmatter_end: usize,
) -> Vec<(Range<usize>, String)> {
    let mut edits = Vec::new();
    let mut fenced = false;
    let mut offset = 0usize;
    for line in src.split('\n') {
        let trimmed_start = line.trim_start();
        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            fenced = !fenced;
        } else if !fenced && offset >= frontmatter_end {
            let indent = line.len() - trimmed_start.len();
            if indent <= 3
                && trimmed_start.starts_with('[')
                && !trimmed_start.starts_with("[^")
                && let Some(close_bracket) = trimmed_start.find("]:")
            {
                let after_colon = close_bracket + 2;
                let rest = &trimmed_start[after_colon..];
                let ws = rest.len() - rest.trim_start().len();
                let url_part = rest.trim_start().trim_end_matches('\r');
                let (url_str, url_start_in_line) = if let Some(inner) = url_part.strip_prefix('<')
                    && let Some(gt) = inner.find('>')
                {
                    (&inner[..gt], indent + after_colon + ws + 1)
                } else {
                    let end = url_part
                        .find(|c: char| c.is_ascii_whitespace())
                        .unwrap_or(url_part.len());
                    (&url_part[..end], indent + after_colon + ws)
                };
                if (url_str.starts_with("https://") || url_str.starts_with("http://"))
                    && let Some(cleaned) = cleanup_core::url::clean_ai_tracking_url(url_str)
                {
                    let abs_start = offset + url_start_in_line;
                    edits.push((abs_start..(abs_start + url_str.len()), cleaned));
                }
            }
        }
        offset += line.len() + 1;
    }
    edits
}

/// Detect YAML (`---`) or TOML (`+++`) front matter at the top of a Markdown file, returning
/// `(open_end, body_end, full_frontmatter_end)` when well-formed.
fn locate_frontmatter(src: &str) -> Option<(usize, usize, usize)> {
    let start = if src.starts_with('\u{FEFF}') { 3 } else { 0 };
    let rest = &src[start..];
    let delim = if rest.starts_with("---\n") || rest.starts_with("---\r\n") {
        "---"
    } else if rest.starts_with("+++\n") || rest.starts_with("+++\r\n") {
        "+++"
    } else {
        return None;
    };
    let first_nl = rest.find('\n')?;
    let body_start = start + first_nl + 1;
    let mut cursor = body_start;
    for line in src[body_start..].split('\n') {
        let bare = line.trim_end_matches('\r').trim_end();
        if bare == delim {
            let close_line_end = (cursor + line.len() + 1).min(src.len());
            return Some((body_start, cursor, close_line_end));
        }
        cursor += line.len() + 1;
        if cursor - body_start > 65_536 {
            return None;
        }
    }
    None
}

const FRONTMATTER_AI_KEYS: &[&str] = &[
    "ai_generated",
    "ai-generated",
    "aigenerated",
    "ai_assisted",
    "ai-assisted",
    "ai_disclosure",
    "ai-disclosure",
    "ai_model",
    "ai-model",
    "llm_model",
    "llm-model",
    "ai_provider",
    "c2pa",
    "c2pa_manifest",
    "content_credentials",
];

const FRONTMATTER_AI_GENERATORS: &[&str] = &[
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
    "notebooklm",
    "manus",
    "genspark",
];

fn inspect_frontmatter_and_comments(
    src: &str,
) -> (Vec<ContainerFinding>, Vec<(Range<usize>, String)>) {
    let mut findings = Vec::new();
    let mut drop_ranges: Vec<Range<usize>> = Vec::new();

    if let Some((body_start, body_end, full_end)) = locate_frontmatter(src) {
        let fm_body = &src[body_start..body_end];
        for rel_range in crate::c2pa::find_structured_text_manifests(fm_body) {
            let abs = (body_start + rel_range.start)..(body_start + rel_range.end);
            findings.push(ContainerFinding {
                rule_id: "container.c2pa-manifest".to_string(),
                message: "Markdown front matter carries a C2PA structured-text manifest"
                    .to_string(),
                scrubbable: true,
                applied: false,
            });
            drop_ranges.push(abs);
        }

        let mut line_offset = body_start;
        for line in fm_body.split('\n') {
            let next_offset = (line_offset + line.len() + 1).min(body_end);
            let overlaps = drop_ranges
                .iter()
                .any(|r| r.start < next_offset && line_offset < r.end);
            if !overlaps && let Some(msg) = classify_frontmatter_line(line) {
                findings.push(ContainerFinding {
                    rule_id: "container.markdown-frontmatter".to_string(),
                    message: msg,
                    scrubbable: true,
                    applied: false,
                });
                drop_ranges.push(line_offset..next_offset);
            }
            line_offset = next_offset;
        }

        // If every non-blank line inside front matter is being removed, drop the empty
        // `--- ... ---` wrapper as well instead of leaving an empty metadata block.
        let mut remaining_nonblank = false;
        let mut check_off = body_start;
        for line in fm_body.split('\n') {
            let next_off = (check_off + line.len() + 1).min(body_end);
            let dropped = drop_ranges
                .iter()
                .any(|r| r.start <= check_off && next_off <= r.end);
            if !dropped && !line.trim().is_empty() {
                remaining_nonblank = true;
                break;
            }
            check_off = next_off;
        }
        if !remaining_nonblank && !drop_ranges.is_empty() {
            drop_ranges.retain(|r| r.end <= body_start || r.start >= full_end);
            let fm_start = if src.starts_with('\u{FEFF}') { 3 } else { 0 };
            let mut end = full_end;
            if src.as_bytes().get(end..end + 2) == Some(b"\r\n") {
                end += 2;
            } else if src.as_bytes().get(end) == Some(&b'\n') {
                end += 1;
            }
            drop_ranges.push(fm_start..end);
        }
    }

    // Outside fenced/indented code blocks, check HTML comments (`Event::Html`) for C2PA
    // structured-text manifests and AI generator provenance comments, and check non-code
    // prose lines for SNOW-style trailing tab/space steganography (which CommonMark strips
    // before emitting `Event::Text` spans).
    let mut protected_ranges: Vec<Range<usize>> = Vec::new();
    let prose_start = locate_frontmatter(src).map_or(0, |(_, _, full_end)| full_end);
    for (event, range) in Parser::new_ext(src, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) | Event::Code(_) | Event::InlineHtml(_) => {
                protected_ranges.push(range);
            }
            Event::Html(_) => {
                protected_ranges.push(range.clone());
                let raw = &src[range.clone()];
                let c2pa_ranges = crate::c2pa::find_structured_text_manifests(raw);
                if !c2pa_ranges.is_empty() {
                    for rel in c2pa_ranges {
                        let abs = (range.start + rel.start)..(range.start + rel.end);
                        if !drop_ranges
                            .iter()
                            .any(|r| r.start < abs.end && abs.start < r.end)
                        {
                            findings.push(ContainerFinding {
                                rule_id: "container.c2pa-manifest".to_string(),
                                message:
                                    "Markdown HTML comment carries a C2PA structured-text manifest"
                                        .to_string(),
                                scrubbable: true,
                                applied: false,
                            });
                            drop_ranges.push(abs);
                        }
                    }
                } else if let Some(msg) = crate::html::classify_ai_html_comment(raw.trim())
                    && !drop_ranges
                        .iter()
                        .any(|r| r.start < range.end && range.start < r.end)
                {
                    findings.push(ContainerFinding {
                        rule_id: "container.html-ai-metadata".to_string(),
                        message: msg,
                        scrubbable: true,
                        applied: false,
                    });
                    drop_ranges.push(range);
                }
            }
            _ => {}
        }
    }

    let bytes = src.as_bytes();
    let mut line_start = prose_start;
    while line_start < bytes.len() {
        let rel_nl = bytes[line_start..].iter().position(|&b| b == b'\n');
        let line_break_end = rel_nl.map_or(bytes.len(), |n| line_start + n + 1);
        let mut content_end = rel_nl.map_or(bytes.len(), |n| line_start + n);
        if content_end > line_start && bytes[content_end - 1] == b'\r' {
            content_end -= 1;
        }
        let mut trimmed_end = content_end;
        while trimmed_end > line_start && matches!(bytes[trimmed_end - 1], b' ' | b'\t') {
            trimmed_end -= 1;
        }
        if trimmed_end < content_end
            && bytes[line_start..trimmed_end]
                .iter()
                .any(|b| !b.is_ascii_whitespace())
            && bytes[trimmed_end..content_end].contains(&b'\t')
        {
            let ws_range = trimmed_end..content_end;
            let overlaps_protected = protected_ranges
                .iter()
                .any(|r| r.start < ws_range.end && ws_range.start < r.end);
            let overlaps_drop = drop_ranges
                .iter()
                .any(|r| r.start < ws_range.end && ws_range.start < r.end);
            if !overlaps_protected && !overlaps_drop {
                findings.push(ContainerFinding {
                    rule_id: "unicode.whitespace-stego".to_string(),
                    message: "SNOW-style trailing tab/space steganography on Markdown line"
                        .to_string(),
                    scrubbable: true,
                    applied: false,
                });
                drop_ranges.push(ws_range);
            }
        }
        line_start = line_break_end;
    }

    drop_ranges.sort_by_key(|r| (r.start, r.end));
    let edits = drop_ranges
        .into_iter()
        .map(|r| (r, String::new()))
        .collect();
    (findings, edits)
}

fn classify_frontmatter_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (raw_key, raw_val) = trimmed
        .split_once(':')
        .or_else(|| trimmed.split_once('='))?;
    let key = raw_key
        .trim()
        .trim_matches(['"', '\''])
        .to_ascii_lowercase();
    let val = raw_val
        .trim()
        .trim_matches(['"', '\'', ' '])
        .to_ascii_lowercase();
    if FRONTMATTER_AI_KEYS.contains(&key.as_str()) && val != "false" && val != "no" {
        return Some(format!(
            "Markdown front matter key {key:?} declares AI provenance"
        ));
    }
    if matches!(
        key.as_str(),
        "generator" | "author" | "created_by" | "created-by" | "tool" | "source"
    ) && let Some(gen_name) = FRONTMATTER_AI_GENERATORS.iter().find(|g| val.contains(**g))
    {
        return Some(format!(
            "Markdown front matter {key:?} names AI tool {gen_name:?}"
        ));
    }
    None
}

fn canonicalize_structural_urls(text: &str) -> String {
    let replacements = cleanup_core::url::find_ai_tracking_urls(text);
    if replacements.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for replacement in replacements {
        out.push_str(&text[cursor..replacement.range.start]);
        out.push_str(&replacement.replacement);
        cursor = replacement.range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Is this source line a list item whose content opens with a bold label?
///
/// Handles `- **Speed:** faster`, `* **Cost**: lower`, `1. **Plan:** …`. The label must be
/// closed on the same line and followed by a colon, a dash, a space or the end of the line.
fn bold_label_item(line: &str) -> bool {
    let line = line.trim_end_matches('\r');
    let trimmed = line.trim_start();
    let body = if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        rest
    } else {
        let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        let rest = &trimmed[digits..];
        match rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            Some(rest) => rest,
            None => return false,
        }
    };
    let Some(after) = body.strip_prefix("**") else {
        return false;
    };
    let Some(close) = after.find("**") else {
        return false;
    };
    let label = &after[..close];
    if label.trim().is_empty() || label.chars().count() > 80 || label.contains('`') {
        return false;
    }
    matches!(
        after[close + 2..].chars().next(),
        None | Some(':') | Some('.') | Some(' ') | Some('-') | Some('\u{2014}')
    )
}

/// Runs of three or more bold-label list items. The shape — a bullet whose lead phrase is
/// bold, followed by a colon or dash — is the "inline-header vertical list", one of the
/// most-cited 2026 formatting tells, and it is a *Markdown* fingerprint: the prose reader
/// never sees the bullets, the `**`, or the line breaks, so the rule layer cannot recover
/// it. Reported from the container pass, flag-only, because it is a formatting habit and
/// not machine residue.
fn inline_header_lists(src: &str) -> Vec<ContainerFinding> {
    const MIN_ITEMS: usize = 3;
    let mut out = Vec::new();
    let mut fenced = false;
    let mut run = 0usize;
    let mut run_start = 0usize;
    let mut run_end = 0usize;
    let mut offset = 0usize;
    let flush = |run: &mut usize, start: usize, end: usize, out: &mut Vec<ContainerFinding>| {
        if *run >= MIN_ITEMS {
            out.push(ContainerFinding {
                rule_id: "style.list.inline-headers".to_string(),
                message: format!(
                    "{} consecutive bold-label list items (inline-header list) at bytes \
                     {start}..{end}",
                    *run
                ),
                scrubbable: false,
                applied: false,
            });
        }
        *run = 0;
    };
    for line in src.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            flush(&mut run, run_start, run_end, &mut out);
        } else if !fenced && bold_label_item(line) {
            if run == 0 {
                run_start = offset;
            }
            run += 1;
            run_end = offset + line.trim_end().len();
        } else {
            flush(&mut run, run_start, run_end, &mut out);
        }
        offset += line.len() + 1;
    }
    flush(&mut run, run_start, run_end, &mut out);
    out
}

impl Format for Markdown {
    fn id(&self) -> FormatId {
        FormatId::Markdown
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("markdown is not valid UTF-8: {e}")))?;

        let had_bom = src.starts_with('\u{FEFF}') && !crate::text::starts_with_c2pa_vs_wrapper(src);
        let meta = DocMeta {
            format: FormatId::Markdown,
            newline: detect_newline(src),
            had_bom,
        };

        let frontmatter_end = locate_frontmatter(src).map_or(0, |(_, _, end)| end);
        let mut spans: Vec<ProseSpan> = Vec::new();
        let mut protected_depth = 0u32;
        let mut can_merge_prev_text = false;

        for (event, range) in Parser::new_ext(src, options()).into_offset_iter() {
            match event {
                Event::Start(tag) => {
                    if is_protected_start(&tag) {
                        protected_depth += 1;
                    }
                    can_merge_prev_text = false;
                }
                Event::End(tag) => {
                    if is_protected_end(&tag) {
                        protected_depth = protected_depth.saturating_sub(1);
                    }
                    can_merge_prev_text = false;
                }
                Event::Text(_) if protected_depth == 0 && range.start >= frontmatter_end => {
                    if can_merge_prev_text
                        && let Some(prev) = spans.last_mut()
                        && let SpanAnchor::Bytes(prev_range) = &mut prev.anchor
                        && prev_range.end == range.start
                    {
                        prev_range.end = range.end;
                        prev.text.push_str(&src[range]);
                    } else {
                        let text = src[range.clone()].to_string();
                        spans.push(ProseSpan::new(SpanAnchor::Bytes(range), text));
                        can_merge_prev_text = true;
                    }
                }
                _ => {
                    can_merge_prev_text = false;
                }
            }
        }

        Ok(Document::new(bytes.to_vec(), spans, meta))
    }

    fn render(&self, doc: &Document, edits: &EditSet) -> Result<Vec<u8>> {
        render_byte_spliced(doc, edits)
    }

    fn container_findings(&self, doc: &Document) -> Vec<ContainerFinding> {
        let Ok(src) = std::str::from_utf8(&doc.source) else {
            return Vec::new();
        };
        let frontmatter_end = locate_frontmatter(src).map_or(0, |(_, _, end)| end);
        let (mut out, _) = inspect_frontmatter_and_comments(src);
        out.extend(
            structural_url_edits(src, frontmatter_end)
                .into_iter()
                .map(|(range, _)| ContainerFinding {
                    rule_id: "markers.ai-tracking-urls.container".to_string(),
                    message: format!(
                        "AI-provider attribution in Markdown link destination at bytes {}..{}",
                        range.start, range.end
                    ),
                    scrubbable: true,
                    applied: false,
                }),
        );
        out.extend(inline_header_lists(&src[frontmatter_end..]));
        out
    }

    fn scrub_container(&self, _doc: &Document, rendered: &[u8]) -> Option<Vec<u8>> {
        let src = std::str::from_utf8(rendered).ok()?;
        let frontmatter_end = locate_frontmatter(src).map_or(0, |(_, _, end)| end);
        let (_, mut edits) = inspect_frontmatter_and_comments(src);
        edits.extend(structural_url_edits(src, frontmatter_end));
        edits.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.end.cmp(&b.0.end)));
        edits.dedup_by(|a, b| a.0 == b.0);
        if edits.is_empty() {
            return None;
        }
        splice(rendered, &edits).ok()
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        // Structural fingerprint: every non-prose event is Debug-formatted (which captures
        // link URLs, heading levels, list markers, code/HTML content, etc.), while contiguous
        // prose Text runs collapse to a single placeholder. Any change to structure — including
        // a replacement that accidentally introduces Markdown syntax — changes this string.
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("markdown is not valid UTF-8: {e}")))?;
        let frontmatter_end = locate_frontmatter(src).map_or(0, |(_, _, end)| end);
        let mut out = String::new();
        let mut prev_was_text = false;
        for (event, _) in Parser::new_ext(&src[frontmatter_end..], options()).into_offset_iter() {
            match event {
                Event::Text(_) => {
                    if !prev_was_text {
                        out.push('\u{00B7}');
                        prev_was_text = true;
                    }
                }
                other => {
                    prev_was_text = false;
                    out.push_str(&canonicalize_structural_urls(&format!("{other:?}")));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists(src: &str) -> Vec<ContainerFinding> {
        inline_header_lists(src)
    }

    #[test]
    fn detects_runs_of_bold_label_items() {
        let src = "- **Speed:** faster\n- **Cost:** lower\n- **Risk:** managed\n";
        let found = lists(src);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule_id, "style.list.inline-headers");
        assert!(!found[0].scrubbable);
    }

    #[test]
    fn ignores_short_runs_plain_bullets_and_code_fences() {
        assert!(lists("- **Speed:** faster\n- **Cost:** lower\n").is_empty());
        assert!(lists("- faster\n- lower\n- safer\n").is_empty());
        assert!(lists("```\n- **A:** b\n- **C:** d\n- **E:** f\n```\n").is_empty());
        // A run ends at a blank line; a later run is reported separately.
        let two_runs =
            "- **A:** b\n- **C:** d\n- **E:** f\n\nProse.\n\n- **G:** h\n- **I:** j\n- **K:** l\n";
        assert_eq!(lists(two_runs).len(), 2);
    }

    #[test]
    fn numbered_and_star_bullets_count() {
        let src = "1. **Plan:** write it\n2. **Build:** do it\n3. **Ship:** send it";
        assert_eq!(lists(src).len(), 1);
    }
}
