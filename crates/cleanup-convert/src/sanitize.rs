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
    /// Keep source typography (curly quotes, dashes, ellipses, ligatures) during conversion.
    /// The `typo.*` rules are AI-*cleaning* rules; applying them while converting to another
    /// container would silently rewrite legitimate Unicode, which is a fidelity regression.
    /// Hidden/zero-width characters are still removed by the `unicode.*` rules.
    pub preserve_typography: bool,
}

impl Default for SanitizeOptions {
    fn default() -> Self {
        SanitizeOptions {
            level: CleanupLevel::Standard,
            drop_boilerplate: true,
            preserve_typography: true,
        }
    }
}

/// What the IR sanitizer did.
#[derive(Debug, Default, Clone)]
pub struct SanitizeReport {
    pub edits_applied: usize,
    pub blocks_removed: usize,
    pub applied_by_rule: Vec<(String, usize)>,
    /// Structural IR warnings found after filtering (empty containers, orphaned assets, …).
    /// Normally empty; surfaced so a caller can spot an analyser regression.
    pub normalization_warnings: Vec<String>,
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
        self.applied_by_rule
            .sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    }
}

/// Sanitize a document in place.
///
/// Order matters: furniture-zone filtering runs first (a running header is not prose), then the
/// shared rule catalogue and the IR-specific artifact passes, then `Doc::normalize` prunes the
/// containers those deletions emptied and re-checks every anchor. Exporters therefore always
/// see a consistent tree.
pub fn sanitize(doc: &mut Doc, options: &SanitizeOptions) -> SanitizeReport {
    let rules = cleanup_rules::builtin_rules();
    let active: Vec<&dyn Rule> = rules
        .iter()
        .filter(|rule| {
            !options.preserve_typography
                || !rule.id().starts_with("typo.")
                // Invisible/zero-width characters are artifacts, not typography: they must be
                // removed even when the source's quotes, dashes and ellipses are preserved.
                || rule.id() == "typo.invisibles.strip"
        })
        .map(|rule| rule.as_ref())
        .collect();
    let cx = Ctx::new(options.level);
    let mut report = SanitizeReport::default();
    if options.drop_boilerplate {
        filter_furniture_zones(doc, &mut report);
    }
    sanitize_blocks(&mut doc.blocks, &active, &cx, options, &mut report);
    for warning in doc.normalize() {
        report.normalization_warnings.push(warning);
    }
    report
}

fn sanitize_blocks(
    blocks: &mut Vec<Block>,
    rules: &[&dyn Rule],
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
            Block::Figure(_) | Block::Vector(_) | Block::Divider(_) => {}
        }
    }

    if options.drop_boilerplate {
        let mut removed = 0usize;
        blocks.retain(|block| {
            let text = block.plain_text();
            let drop = matches!(
                block,
                Block::Paragraph { .. } | Block::Heading { .. } | Block::Callout { .. }
            ) && (is_pure_boilerplate(&text) || is_short_disclaimer(&text));
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
    rules: &[&dyn Rule],
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
        if !on_char_boundary(&text, finding.range.start)
            || !on_char_boundary(&text, finding.range.end)
        {
            continue;
        }
        edits.push((
            finding.range.clone(),
            replacement.clone(),
            finding.rule_id.clone(),
        ));
    }
    let contains_code = inlines.iter().any(|inline| inline.style.is_code());
    if cx.level >= CleanupLevel::Standard {
        for (range, rule_id) in boilerplate_sentence_ranges(&text) {
            if on_char_boundary(&text, range.start) && on_char_boundary(&text, range.end) {
                edits.push((range, String::new(), rule_id.to_string()));
            }
        }
        // Disclaimers are content-free wherever they appear, so they are removed from the middle
        // of a block too — readers merge wrap-adjacent paragraphs, and a disclaimer that lands
        // inside one is still a disclaimer.
        for range in disclaimer_ranges(&text) {
            if on_char_boundary(&text, range.start) && on_char_boundary(&text, range.end) {
                edits.push((range, String::new(), "ir.disclaimer".to_string()));
            }
        }
        // Zero-width/invisible characters are always artifacts in prose, regardless of the
        // typography-preserving setting. The shared `typo.invisibles.strip` rule removes most;
        // ZWNJ/ZWJ/word-joiner need an explicit IR pass at Standard level.
        if !contains_code {
            for range in invisible_ranges(&text) {
                edits.push((range, String::new(), "ir.invisible".to_string()));
            }
        }
        // Chat-template / markup residue is never prose. Code-bearing blocks are exempt: their
        // brackets, backticks and asterisks are intentional syntax.
        if !contains_code {
            for range in format_token_ranges(&text) {
                if on_char_boundary(&text, range.start) && on_char_boundary(&text, range.end) {
                    edits.push((range, String::new(), "ir.format-token".to_string()));
                }
            }
            for range in wrapper_quote_ranges(&text) {
                if on_char_boundary(&text, range.start) && on_char_boundary(&text, range.end) {
                    edits.push((range, String::new(), "ir.wrapper-quotes".to_string()));
                }
            }
            for range in repetition_ranges(&text) {
                if on_char_boundary(&text, range.start) && on_char_boundary(&text, range.end) {
                    edits.push((range, String::new(), "ir.repetition".to_string()));
                }
            }
        }
    }
    if edits.is_empty() {
        return;
    }
    // Longest edit wins at equal start offsets, matching the byte-splice pipeline. Otherwise a
    // one-character wrapper quote at offset 0 would shadow a whole preamble sentence.
    edits.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(b.0.end.cmp(&a.0.end)));
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
        for (index, window) in starts.windows(2).enumerate() {
            if offset <= window[1] {
                return (index, offset.saturating_sub(window[0]));
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
// Formatting tokens, wrapper quotes and hallucinated repetition
// ---------------------------------------------------------------------------

/// Literal serialization artifacts that only appear when a chat response was pasted into a
/// document without rendering it. None of these can be legitimate prose punctuation.
const FORMAT_TOKENS: &[&str] = &[
    "<|im_start|>",
    "<|im_end|>",
    "<|endoftext|>",
    "[/INST]",
    "[INST]",
    "<<SYS>>",
    "<</SYS>>",
    "[END]",
    "[START]",
    "```markdown",
    "```",
    "~~~",
    "</s>",
    "<s>",
    "**",
    "`",
];

const FURNITURE_WORDS: &[&str] = &[
    "confidential",
    "internal",
    "draft",
    "copyright",
    "all rights reserved",
    "page",
];

fn looks_like_html_tag(inner: &str) -> bool {
    let trimmed = inner.trim();
    let name: String = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic() || *ch == '/' || *ch == '!' || *ch == '-')
        .collect();
    let name = name
        .trim_start_matches('/')
        .trim_start_matches('!')
        .trim_start_matches('-')
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "p" | "div"
            | "span"
            | "br"
            | "hr"
            | "b"
            | "i"
            | "em"
            | "strong"
            | "code"
            | "pre"
            | "ul"
            | "ol"
            | "li"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "a"
            | "img"
            | "table"
            | "thead"
            | "tbody"
            | "tr"
            | "td"
            | "th"
    )
}

/// Byte ranges of formatting/invisible characters that never carry meaning in prose.
fn invisible_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    text.char_indices()
        .filter(|(_, ch)| {
            matches!(
                *ch,
                '\u{200B}'..='\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}'
                    | '\u{FEFF}'
                    | '\u{00AD}'
            )
        })
        .map(|(offset, ch)| offset..offset + ch.len_utf8())
        .collect()
}

/// Byte ranges of chat-template tokens and orphaned HTML tags in a prose block.
fn format_token_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    for token in FORMAT_TOKENS {
        let mut from = 0usize;
        while let Some(rel) = text[from..].find(token) {
            let start = from + rel;
            let end = start + token.len();
            ranges.push(start..end);
            from = end;
        }
    }
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'<' {
            let limit = (index + 96).min(bytes.len());
            let end = bytes[index + 1..limit]
                .iter()
                .position(|byte| *byte == b'>')
                .map(|rel| index + 1 + rel);
            if let Some(end) = end
                && looks_like_html_tag(&text[index + 1..end])
            {
                ranges.push(index..end + 1);
                index = end + 1;
                continue;
            }
        }
        index += 1;
    }
    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    ranges
}

/// Strip an artificial pair of quotes wrapped around an entire paragraph.
///
/// Deliberately conservative: the block must be a real sentence (six or more words), must have
/// no other quote characters inside, and the quotes must be a matching pair at the very start
/// and end. A genuine short quotation or a quotation with an embedded citation is preserved.
fn wrapper_quote_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut forward = text.char_indices().filter(|(_, ch)| !ch.is_whitespace());
    let mut backward = text
        .char_indices()
        .rev()
        .filter(|(_, ch)| !ch.is_whitespace());
    let (Some((start, first)), Some((end, last))) = (forward.next(), backward.next()) else {
        return Vec::new();
    };
    if end <= start {
        return Vec::new();
    }
    let matched = matches!(
        (first, last),
        ('"', '"') | ('“', '”') | ('‘', '’') | ('\'', '\'')
    );
    if !matched {
        return Vec::new();
    }
    let inner = &text[start + first.len_utf8()..end];
    let has_inner_quote = inner
        .chars()
        .any(|ch| matches!(ch, '"' | '“' | '”' | '‘' | '’' | '\''));
    if has_inner_quote || inner.split_whitespace().count() < 6 {
        return Vec::new();
    }
    vec![start..start + first.len_utf8(), end..end + last.len_utf8()]
}

#[derive(Debug)]
struct Token {
    start: usize,
    end: usize,
    normalized: String,
}

fn word_tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for (offset, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some((start, word)) = current.take() {
                out.push(Token {
                    start,
                    end: offset,
                    normalized: normalize_word(&word),
                });
            }
        } else {
            let entry = current.get_or_insert_with(|| (offset, String::new()));
            entry.1.push(ch);
        }
    }
    if let Some((start, word)) = current {
        out.push(Token {
            start,
            end: text.len(),
            normalized: normalize_word(&word),
        });
    }
    out
}

fn normalize_word(word: &str) -> String {
    word.trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase()
}

/// Collapse runs of three or more identical words, and three or more repeats of a two-word
/// phrase — the classic loop a chat model falls into when it loses context.
fn repetition_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let tokens = word_tokens(text);
    let mut ranges = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        let mut stop = index + 1;
        while stop < tokens.len()
            && !tokens[index].normalized.is_empty()
            && tokens[stop].normalized == tokens[index].normalized
        {
            stop += 1;
        }
        if stop - index >= 3 {
            ranges.push(tokens[index].end..tokens[stop - 1].end);
        }
        index = stop;
    }
    if tokens.len() >= 6 {
        for index in 0..=tokens.len() - 6 {
            let first = &tokens[index].normalized;
            let second = &tokens[index + 1].normalized;
            if first.is_empty() || second.is_empty() {
                continue;
            }
            let repeats = (0..3).all(|step| {
                tokens[index + step * 2].normalized == *first
                    && tokens[index + step * 2 + 1].normalized == *second
            });
            if repeats {
                ranges.push(tokens[index + 2].start..tokens[index + 5].end);
            }
        }
    }
    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    ranges
}

// ---------------------------------------------------------------------------
// Header/footer zone filtering
// ---------------------------------------------------------------------------

fn furniture_key(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            out.push('#');
        } else if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        } else if ch.is_whitespace() && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

fn carries_prose(block: &Block) -> bool {
    matches!(
        block,
        Block::Heading { .. } | Block::Paragraph { .. } | Block::List { .. }
    )
}

fn looks_like_furniture(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let words = key.split_whitespace().count();
    if words > 14 {
        return false;
    }
    if FURNITURE_WORDS.iter().any(|word| key.contains(word)) {
        return true;
    }
    key.chars()
        .all(|ch| ch.is_ascii_digit() || ch == '#' || ch.is_whitespace() || ch == '©')
}

/// Remove text that lives entirely inside a recorded header/footer band.
///
/// The spatial analyser already strips *repeated* running furniture from the line stream. This
/// pass is the IR-level safety net for single-page documents, first/last pages and readers that
/// produce their own anchors: a short line in a furniture band is dropped only when it repeats
/// elsewhere, was recognised as running furniture by the analyser, or looks like a page stamp.
/// Survivors keep their anchors, so reading order and hierarchy are untouched.
fn filter_furniture_zones(doc: &mut Doc, report: &mut SanitizeReport) {
    if doc.info.page_boundaries.is_empty() {
        return;
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for block in doc.walk() {
        if !carries_prose(block) || block.anchor().is_none() {
            continue;
        }
        let key = furniture_key(&block.plain_text());
        if key.split_whitespace().count() <= 14 {
            *counts.entry(key).or_default() += 1;
        }
    }
    let repeated: Vec<String> = counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(key, _)| key)
        .collect();
    let known: Vec<String> = doc
        .info
        .running_headers
        .iter()
        .chain(doc.info.running_footers.iter())
        .map(|text| furniture_key(text))
        .collect();
    let boundaries = doc.info.page_boundaries.clone();
    prune_furniture_blocks(&mut doc.blocks, &boundaries, &repeated, &known, report);
}

fn prune_furniture_blocks(
    blocks: &mut Vec<Block>,
    boundaries: &[cleanup_core::doc::PageBoundary],
    repeated: &[String],
    known: &[String],
    report: &mut SanitizeReport,
) {
    blocks.retain_mut(|block| {
        match block {
            Block::Callout { blocks, .. } => {
                prune_furniture_blocks(blocks, boundaries, repeated, known, report);
            }
            Block::Columns { columns, .. } => {
                for column in columns.iter_mut() {
                    prune_furniture_blocks(&mut column.blocks, boundaries, repeated, known, report);
                }
            }
            Block::Table(table) => {
                for row in table.headers.iter_mut().chain(table.rows.iter_mut()) {
                    for cell in row.cells.iter_mut() {
                        prune_furniture_blocks(
                            &mut cell.blocks,
                            boundaries,
                            repeated,
                            known,
                            report,
                        );
                    }
                }
            }
            _ => {}
        }
        if !carries_prose(block) {
            return true;
        }
        let Some(anchor) = block.anchor() else {
            return true;
        };
        let Some(boundary) = boundaries.get(anchor.page) else {
            return true;
        };
        if !boundary.in_furniture_zone(&anchor.rect) {
            return true;
        }
        let key = furniture_key(&block.plain_text());
        let drop = !key.is_empty()
            && (repeated.contains(&key) || known.contains(&key) || looks_like_furniture(&key));
        if drop {
            report.blocks_removed += 1;
        }
        !drop
    });
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
    "here is your summary",
    "below you will find",
    "prepared in the format you requested",
    "in the format you requested",
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
    "here is the summary",
    "here is your summary",
    "below you will find",
    "below is the summary",
    "here is the breakdown",
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

/// A short block that carries an explicit disclaimer phrase is boilerplate even when the
/// sentence splitter does not isolate the phrase (merged paragraphs, unusual punctuation).
fn is_short_disclaimer(text: &str) -> bool {
    let normalized = normalized(text);
    if normalized.split_whitespace().count() > 24 {
        return false;
    }
    DISCLAIMER_SENTENCES
        .iter()
        .chain(PURE_BOILERPLATE.iter())
        .any(|pattern| normalized.contains(*pattern))
}

/// Ranges of sentences in `text` that are pure disclaimers.
///
/// A lone sentence is left to [`is_pure_boilerplate`]: dropping the only sentence in a block is
/// block removal, not sentence trimming.
fn disclaimer_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let sentences = sentence_ranges(text);
    if sentences.len() < 2 {
        return Vec::new();
    }
    sentences
        .into_iter()
        .filter(|range| is_disclaimer_sentence(&text[range.clone()]))
        .collect()
}

/// True if this single sentence is a disclaimer rather than document content.
fn is_disclaimer_sentence(sentence: &str) -> bool {
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
    DISCLAIMER_SENTENCES
        .iter()
        .any(|pattern| normalized.starts_with(*pattern))
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
    for warning in &report.normalization_warnings {
        target.warnings.push(format!("IR sanitizer: {warning}"));
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
