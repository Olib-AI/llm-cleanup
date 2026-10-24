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

/// Clean only destinations belonging to actual `Link` events. Images, code, raw HTML, and
/// malformed `](...)` text never produce such an event, so they cannot be changed by this
/// structural pass. Reference definitions are intentionally left for a separate grammar.
fn structural_url_edits(src: &str) -> Vec<(Range<usize>, String)> {
    let mut edits = Vec::new();
    for (event, range) in Parser::new_ext(src, options()).into_offset_iter() {
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
    edits.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.end.cmp(&b.0.end)));
    edits.dedup_by(|a, b| a.0 == b.0);
    edits
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

        let meta = DocMeta {
            format: FormatId::Markdown,
            newline: detect_newline(src),
            had_bom: src.starts_with('\u{FEFF}'),
        };

        let mut spans = Vec::new();
        let mut protected_depth = 0u32;

        for (event, range) in Parser::new_ext(src, options()).into_offset_iter() {
            match event {
                Event::Start(tag) if is_protected_start(&tag) => protected_depth += 1,
                Event::End(tag) if is_protected_end(&tag) => {
                    protected_depth = protected_depth.saturating_sub(1);
                }
                Event::Text(_) if protected_depth == 0 => {
                    let text = src[range.clone()].to_string();
                    spans.push(ProseSpan::new(SpanAnchor::Bytes(range), text));
                }
                _ => {}
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
        let mut out: Vec<ContainerFinding> = structural_url_edits(src)
            .into_iter()
            .map(|(range, _)| ContainerFinding {
                rule_id: "markers.ai-tracking-urls.container".to_string(),
                message: format!(
                    "AI-provider attribution in Markdown link destination at bytes {}..{}",
                    range.start, range.end
                ),
                scrubbable: true,
                applied: false,
            })
            .collect();
        out.extend(inline_header_lists(src));
        out
    }

    fn scrub_container(&self, _doc: &Document, rendered: &[u8]) -> Option<Vec<u8>> {
        let src = std::str::from_utf8(rendered).ok()?;
        let edits = structural_url_edits(src);
        if edits.is_empty() {
            return None;
        }
        splice(rendered, &edits).ok()
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        // Structural fingerprint: every non-prose event is Debug-formatted (which captures
        // link URLs, heading levels, list markers, code/HTML content, etc.), while prose
        // Text events collapse to a single placeholder. Any change to structure — including
        // a replacement that accidentally introduces Markdown syntax — changes this string.
        let src = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::Encoding(format!("markdown is not valid UTF-8: {e}")))?;
        let mut out = String::new();
        for (event, _) in Parser::new_ext(src, options()).into_offset_iter() {
            match event {
                Event::Text(_) => out.push('\u{00B7}'), // prose excluded from the fingerprint
                other => out.push_str(&canonicalize_structural_urls(&format!("{other:?}"))),
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
