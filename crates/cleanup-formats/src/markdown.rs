use cleanup_core::ir::{DocMeta, Document, FormatId, ProseSpan, SpanAnchor, detect_newline};
use cleanup_core::splice::render_byte_spliced;
use cleanup_core::{CoreError, EditSet, Format, Result};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

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
                other => out.push_str(&format!("{other:?}")),
            }
        }
        Ok(out)
    }
}
