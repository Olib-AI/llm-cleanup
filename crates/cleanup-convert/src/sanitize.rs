//! The AI-sanitization layer, run on the IR rather than on source bytes.
//!
//! Two mechanisms cooperate:
//!
//! 1. **The shared rule catalogue** (`cleanup_rules::builtin_rules`) runs over every prose-bearing
//!    block — headings, paragraphs, list items and table cells — while code blocks, figure
//!    captions' alt text and structural data are left alone. Because the rules see the *text* of a
//!    block and not the file, a phrase split across runs, spans or pages is now matched as one
//!    string.
//! 2. **Block-level boilerplate removal**, which the byte-splice path deliberately cannot do: a
//!    paragraph that is only a chat preamble, a disclaimer, or an "I hope this helps" closer is
//!    removed from the document instead of being left as an empty shell. Survivors keep their
//!    anchors, so layout coordinates stay valid for everything that is exported.

use crate::ConvertReport;
use cleanup_core::doc::{Block, Doc, Inline, inlines_text};
use cleanup_core::ir::{ProseSpan, SpanAnchor};
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};
use std::collections::BTreeMap;

/// What the IR sanitizer is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SanitizeOptions {
    pub level: CleanupLevel,
    /// Remove blocks that consist only of assistant boilerplate.
    pub drop_boilerplate: bool,
}

impl Default for SanitizeOptions {
    fn default() -> Self {
        SanitizeOptions {
            level: CleanupLevel::Standard,
            drop_boilerplate: true,
        }
    }
}

/// What the IR sanitizer did.
#[derive(Debug, Default, Clone)]
pub struct SanitizeReport {
    pub edits_applied: usize,
    pub blocks_removed: usize,
    pub applied_by_rule: Vec<(String, usize)>,
}

impl SanitizeReport {
    pub fn changed(&self) -> bool {
        self.edits_applied > 0 || self.blocks_removed > 0
    }

    fn record(&mut self, rule_id: &str, count: usize) {
        self.edits_applied += count;
        let mut map: BTreeMap<&str, usize> = self
            .applied_by_rule
            .iter()
            .map(|(id, value)| (id.as_str(), *value))
            .collect();
        *map.entry(rule_id).or_default() += count;
        self.applied_by_rule = map
            .into_iter()
            .map(|(id, value)| (id.to_string(), value))
            .collect();
        self.applied_by_rule.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    }
}

/// Sanitize a document in place.
pub fn sanitize(doc: &mut Doc, options: &SanitizeOptions) -> SanitizeReport {
    let rules = cleanup_rules::builtin_rules();
    let cx = Ctx::new(options.level);
    let mut report = SanitizeReport::default();
    sanitize_blocks(&mut doc.blocks, &rules, &cx, options, &mut report);
    report
}

fn sanitize_blocks(
    blocks: &mut Vec<Block>,
    rules: &[Box<dyn Rule>],
    cx: &Ctx,
    options: &SanitizeOptions,
    report: &mut SanitizeReport,
) {
    for block in blocks.iter_mut() {
        match block {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => {
                sanitize_inlines(inlines, rules, cx, report);
            }
            Block::List { items, .. } => {
                for item in items.iter_mut() {
                    sanitize_inlines(&mut item.inlines, rules, cx, report);
                }
                items.retain(|item| !inlines_text(&item.inlines).trim().is_empty());
            }
            Block::Code { .. } => {
                // Code is never rewritten: indentation and inline code are content, not slop.
            }
            Block::Callout { blocks, .. } => {
                sanitize_blocks(blocks, rules, cx, options, report);
            }
            Block::Columns { columns, .. } => {
                for column in columns.iter_mut() {
                    sanitize_blocks(&mut column.blocks, rules, cx, options, report);
                }
            }
            Block::Table(table) => {
                for row in table.headers.iter_mut().chain(table.rows.iter_mut()) {
                    for cell in row.cells.iter_mut() {
                        sanitize_blocks(&mut cell.blocks, rules, cx, options, report);
                    }
                }
            }
            Block::Figure(_) | Block::Divider(_) => {}
        }
    }

    if options.drop_boilerplate {
        let mut removed = 0usize;
        blocks.retain(|block| {
            let text = block.plain_text();
            let drop = matches!(
                block,
                Block::Paragraph { .. } | Block::Heading { .. } | Block::Callout { .. }
            ) && is_pure_boilerplate(&text);
            if drop {
                removed += 1;
            }
            !drop
        });
        report.blocks_removed += removed;
    }
    blocks.retain(|block| !block.is_empty() || matches!(block, Block::Table(_) | Block::Figure(_)));
}

fn sanitize_inlines(
    inlines: &mut Vec<Inline>,
    rules: &[Box<dyn Rule>],
    cx: &Ctx,
    report: &mut SanitizeReport,
) {
    if inlines.is_empty() || inlines.iter().all(|inline| inline.style.code) {
        return;
    }
    let text = inlines_text(inlines);
    if text.trim().is_empty() {
        return;
    }
    let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text.clone());
    let mut findings: Vec<Finding> = Vec::new();
    for rule in rules {
        if !cx.level.includes(rule.min_level()) {
            continue;
        }
        findings.extend(rule.detect(0, &span, cx));
    }
    let mut edits: Vec<(std::ops::Range<usize>, String, String)> = Vec::new();
    for finding in &findings {
        let Some(replacement) = &finding.replacement else {
            continue;
        };
        // A replacement with a newline would change block structure; those are reported by the
        // native path and skipped here for the same reason.
        if replacement.contains(['\n', '\r']) {
            continue;
        }
        if !on_char_boundary(&text, finding.range.start) || !on_char_boundary(&text, finding.range.end)
        {
            continue;
        }
        edits.push((finding.range.clone(), replacement.clone(), finding.rule_id.clone()));
    }
    if cx.level >= CleanupLevel::Standard {
        for (range, rule_id) in boilerplate_sentence_ranges(&text) {
            if on_char_boundary(&text, range.start) && on_char_boundary(&text, range.end) {
                edits.push((range, String::new(), rule_id));
            }
        }
    }
    if edits.is_empty() {
        return;
    }
    edits.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.end.cmp(&b.0.end)));
    let mut accepted: Vec<(std::ops::Range<usize>, String, String)> = Vec::new();
    let mut cursor = 0usize;
    for edit in edits {
        if edit.0.start < cursor || edit.0.end > text.len() || edit.0.start > edit.0.end {
            continue;
        }
        cursor = edit.0.end;
        accepted.push(edit);
    }
    if accepted.is_empty() {
        return;
    }
    let applied = apply_to_inlines(inlines, &accepted);
    if applied == 0 {
        return;
    }
    for (_, _, rule_id) in &accepted {
        report.record(rule_id, 1);
    }
}

fn on_char_boundary(text: &str, index: usize) -> bool {
    text.is_char_boundary(index)
}

/// Apply `(range, replacement, rule)` edits to an inline sequence.
///
/// Edits are expressed against the concatenation of the inline texts; the replacement is written
/// into the inline where the edit starts and the covered tail of later inlines is trimmed — the
/// same "first run wins" rule the DOCX and PDF byte-splicers use, so styling of untouched inlines
/// survives.
fn apply_to_inlines(
    inlines: &mut Vec<Inline>,
    edits: &[(std::ops::Range<usize>, String, String)],
) -> usize {
    let mut pieces: Vec<String> = inlines.iter().map(|inline| inline.text.clone()).collect();
    let mut starts: Vec<usize> = Vec::with_capacity(pieces.len() + 1);
    let mut offset = 0usize;
    for piece in &pieces {
        starts.push(offset);
        offset += piece.len();
    }
    starts.push(offset);

    let locate = |starts: &[usize], offset: usize| -> (usize, usize) {
        if starts.len() < 2 {
            return (0, 0);
        }
        let last = starts.len() - 2;
        for index in 0..=last {
            if offset <= starts[index + 1] {
                return (index, offset.saturating_sub(starts[index]));
            }
        }
        (last, offset.saturating_sub(starts[last]))
    };

    for (range, replacement, _) in edits.iter().rev() {
        let (first, local_start) = locate(&starts, range.start);
        let (last, local_end) = locate(&starts, range.end);
        if first == last {
            let piece = &mut pieces[first];
            let start = local_start.min(piece.len());
            let end = local_end.min(piece.len()).max(start);
            piece.replace_range(start..end, replacement);
        } else {
            let piece = &mut pieces[first];
            let start = local_start.min(piece.len());
            piece.truncate(start);
            piece.push_str(replacement);
            for piece in pieces.iter_mut().take(last + 1).skip(first + 1) {
                piece.clear();
            }
            let piece = &mut pieces[last];
            let end = local_end.min(piece.len());
            piece.replace_range(0..end, "");
        }
    }
    for (inline, piece) in inlines.iter_mut().zip(pieces.iter()) {
        inline.text = piece.clone();
    }
    inlines.retain(|inline| inline.is_break() || !inline.text.is_empty());
    edits.len()
}

// ---------------------------------------------------------------------------
// Boilerplate
// ---------------------------------------------------------------------------

/// Phrases that only ever appear because a chat model wrote around the content. Matching one is
/// enough to delete a *whole block* only when the block is nothing else.
const PURE_BOILERPLATE: &[&str] = &[
    "as an ai language model",
    "as a large language model",
    "i'm unable to continue",
    "i cannot continue this conversation",
    "as of my last knowledge update",
    "as of my knowledge cutoff",
    "i don't have access to real-time",
    "i do not have access to real-time",
    "i don't have the ability to browse",
    "this response was generated by",
    "this document was generated by",
    "generated by chatgpt",
    "generated with chatgpt",
    "generated by claude",
    "generated by an ai",
    "this is a hypothetical scenario",
    "please note that this is a hypothetical",
    "i hope this helps",
    "hope this helps!",
    "let me know if you'd like me to",
    "let me know if you would like me to",
    "let me know if you need anything else",
    "feel free to ask if you have any questions",
    "if you have any further questions, feel free",
    "would you like me to expand on any",
];

/// Openers that are removed as *sentences* from the start of an otherwise valid paragraph.
const PREAMBLE_SENTENCES: &[&str] = &[
    "certainly!",
    "certainly,",
    "sure!",
    "sure,",
    "of course!",
    "of course,",
    "absolutely!",
    "great question!",
    "good question!",
    "here's a comprehensive overview",
    "here is a comprehensive overview",
    "below is a comprehensive overview",
    "here's an overview",
    "here is an overview",
    "here's a summary",
    "here is a summary",
    "i'd be happy to help",
    "i would be happy to help",
    "happy to help",
    "as requested, here",
];

/// Closers removed from the end of a paragraph.
const CLOSER_SENTENCES: &[&str] = &[
    "i hope this helps",
    "hope this helps",
    "let me know if you'd like me to expand",
    "let me know if you would like me to expand",
    "let me know if you have any questions",
    "let me know if you need anything else",
    "feel free to reach out if you have any questions",
    "feel free to ask if you have any questions",
    "if you have any further questions, feel free to ask",
    "would you like me to expand on any section",
    "would you like me to expand on any of these",
];

fn normalized(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_whitespace() {
            space = !out.is_empty();
        } else if ch.is_ascii_punctuation() || ch == '\u{2019}' || ch == '\u{2018}' {
            // Normalise curly quotes to their straight form so patterns only use ASCII.
            if ch == '\u{2019}' || ch == '\u{2018}' {
                out.push('\'');
            } else {
                out.push(ch);
            }
            space = false;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// Disclaimers and meta-commentary that carry no document content. Removing one of these
/// sentences never loses information a reader came for.
const DISCLAIMER_SENTENCES: &[&str] = &[
    "as an ai language model",
    "as a large language model",
    "this document is for informational purposes only",
    "for informational purposes only",
    "this is for informational purposes only",
    "this response was generated by",
    "this document was generated by",
    "this content was generated by",
    "please note that this is a hypothetical",
    "this is a hypothetical scenario",
    "i cannot provide legal advice",
    "i cannot give legal advice",
    "i am not a lawyer",
    "no warranty is provided",
    "always consult a professional",
    "the numbers may not reflect your environment",
];

/// True if this single sentence is assistant chatter rather than content.
fn is_slop_sentence(sentence: &str) -> bool {
    let normalized = normalized(sentence);
    if normalized.is_empty() {
        return false;
    }
    if PURE_BOILERPLATE
        .iter()
        .any(|pattern| normalized.contains(pattern))
    {
        return true;
    }
    PREAMBLE_SENTENCES
        .iter()
        .chain(CLOSER_SENTENCES.iter())
        .chain(DISCLAIMER_SENTENCES.iter())
        .any(|pattern| normalized.starts_with(*pattern))
}

/// True when the block carries no content of its own.
///
/// The test is deliberately conservative: a block is dropped only when *every* sentence in it
/// is chatter, or when it is a single short phrase that is chatter outright. A paragraph that
/// opens with a disclaimer and then says something real keeps its real sentence — the sentence
/// trimmer, not this function, handles that case.
fn is_pure_boilerplate(text: &str) -> bool {
    let normalized = normalized(text);
    if normalized.is_empty() {
        return false;
    }
    if PURE_BOILERPLATE
        .iter()
        .any(|pattern| normalized.contains(pattern))
    {
        return normalized.split_whitespace().count() <= 16;
    }
    let sentences = sentence_ranges(text);
    if sentences.is_empty() {
        return false;
    }
    sentences
        .iter()
        .all(|range| is_slop_sentence(&text[range.clone()]))
}

/// Sentence ranges at the start/end of `text` that are assistant chatter.
fn boilerplate_sentence_ranges(text: &str) -> Vec<(std::ops::Range<usize>, &'static str)> {
    let mut out = Vec::new();
    let sentences = sentence_ranges(text);
    if sentences.len() >= 2 {
        let first = &text[sentences[0].clone()];
        let normalized_first = normalized(first);
        if let Some(pattern) = PREAMBLE_SENTENCES
            .iter()
            .find(|pattern| normalized_first.starts_with(**pattern))
            && normalized_first.len() <= pattern.len() + 40
        {
            out.push((sentences[0].clone(), "ir.preamble"));
            let _ = pattern;
        }
    }
    if sentences.len() >= 2 {
        let last_index = sentences.len() - 1;
        let last = &text[sentences[last_index].clone()];
        let normalized_last = normalized(last);
        if CLOSER_SENTENCES
            .iter()
            .any(|pattern| normalized_last.starts_with(*pattern))
            && normalized_last.len() <= 160
        {
            out.push((sentences[last_index].clone(), "ir.closer"));
        }
    }
    out
}

/// Byte ranges of the sentences in `text`, including their trailing whitespace.
fn sentence_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start = 0usize;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (index, (offset, ch)) in chars.iter().enumerate() {
        let is_end = matches!(ch, '.' | '!' | '?');
        if !is_end {
            continue;
        }
        // The terminator must be followed by whitespace or end-of-text to count.
        let next = chars.get(index + 1).map(|(_, c)| *c);
        if next.is_some_and(|c| !c.is_whitespace()) {
            continue;
        }
        let mut end = offset + ch.len_utf8();
        while end < text.len() {
            let mut advance = 0usize;
            for (inner_offset, inner) in chars.iter() {
                if *inner_offset >= end {
                    if inner.is_whitespace() {
                        advance = inner.len_utf8();
                    }
                    break;
                }
            }
            if advance == 0 {
                break;
            }
            end += advance;
        }
        ranges.push(start..end);
        start = end;
    }
    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges.retain(|range| !text[range.clone()].trim().is_empty());
    ranges
}

/// Fold the IR sanitizer's report into the converter's user-facing report.
pub(crate) fn merge_into(report: &SanitizeReport, target: &mut ConvertReport) {
    if !report.changed() {
        return;
    }
    target.warnings.push(format!(
        "IR sanitizer: {} edit(s) across {} rule(s), {} boilerplate block(s) removed",
        report.edits_applied,
        report.applied_by_rule.len(),
        report.blocks_removed
    ));
    if let Some((rule, count)) = report.applied_by_rule.first() {
        target.warnings.push(format!(
            "IR sanitizer: most active rule `{rule}` ({count} edit(s))"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::doc::{Doc, PageSpec};

    fn doc_with(text: &str) -> Doc {
        let mut doc = Doc::new(PageSpec::default());
        doc.blocks.push(Block::Paragraph {
            inlines: vec![Inline::plain(text)],
            anchor: None,
            style: cleanup_core::doc::ParagraphStyle::default(),
        });
        doc
    }

    #[test]
    fn chat_preamble_block_is_removed_entirely() {
        let mut doc = doc_with("Certainly! Here is the summary you asked for.");
        doc.blocks.push(Block::Paragraph {
            inlines: vec![Inline::plain("Revenue grew 12% year over year.")],
            anchor: None,
            style: cleanup_core::doc::ParagraphStyle::default(),
        });
        let report = sanitize(&mut doc, &SanitizeOptions::default());
        assert_eq!(report.blocks_removed, 1);
        assert_eq!(doc.blocks.len(), 1);
        assert!(doc.plain_text().contains("Revenue grew"));
    }

    #[test]
    fn trailing_closer_is_trimmed_but_content_survives() {
        let mut doc = doc_with(
            "Revenue grew 12% year over year. Costs stayed flat. I hope this helps, let me know if you have any questions.",
        );
        sanitize(&mut doc, &SanitizeOptions::default());
        let text = doc.plain_text();
        assert!(text.contains("Revenue grew 12%"));
        assert!(!text.contains("I hope this helps"));
    }

    #[test]
    fn code_blocks_are_never_touched() {
        let mut doc = Doc::new(PageSpec::default());
        doc.blocks.push(Block::Code {
            lines: vec!["fn main() { /* Certainly! */ }".to_string()],
            anchor: None,
            language: None,
        });
        sanitize(&mut doc, &SanitizeOptions::default());
        assert_eq!(doc.blocks.len(), 1);
        assert!(doc.plain_text().contains("Certainly!"));
    }

    #[test]
    fn genuine_sentences_are_not_mistaken_for_boilerplate() {
        let text = "The panel recommended that the company expand into Norway.";
        assert!(!is_pure_boilerplate(text));
        assert!(boilerplate_sentence_ranges(text).is_empty());
    }

    #[test]
    fn sentence_ranges_cover_every_sentence() {
        let ranges = sentence_ranges("One. Two! Three? Four");
        assert_eq!(ranges.len(), 4);
        assert_eq!(&"One. Two! Three? Four"[ranges[0].clone()], "One. ");
        assert_eq!(&"One. Two! Three? Four"[ranges[3].clone()], "Four");
    }
}
