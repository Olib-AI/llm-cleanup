//! Exact-string AI signatures: chat-template control tokens, chat-UI citation artifacts,
//! agentic-coding trailers, and model self-identifiers.
//!
//! These are different in kind from the lexical rules in `templates/core.yaml`. Lexical
//! rules guess at *style* and therefore only ever flag. The strings here are machine
//! artifacts that never belong in finished prose: `<|im_start|>` is a chat-template token,
//! `【45†L75-L83】` is a ChatGPT deep-research citation marker, `Co-Authored-By: Claude` is a
//! Claude Code commit trailer. They are matched as literals (no word-boundary heuristics,
//! because the tokens are punctuation-delimited) and split into two severities:
//!
//! * **flag** — reported for review, never edited. Used where the string could plausibly be
//!   quoted deliberately (a blog post *about* AI, a changelog, documentation).
//! * **strip** — removed at the aggressive level. Used only for strings that are unambiguously
//!   a serialization artifact, never author-chosen text.
//!
//! Catalogue and provenance: see `catalog.rs`, `README.md` § "What it cleans", and
//! `docs/FINGERPRINTS.md`.

use aho_corasick::AhoCorasick;
use cleanup_core::ir::ProseSpan;
use cleanup_core::{CleanupLevel, Ctx, Finding, Rule};

/// A rule over a fixed list of literal needles.
pub struct MarkerRule {
    id: &'static str,
    level: CleanupLevel,
    message: &'static str,
    /// `Some("")` removes the match; `None` reports it and leaves the text alone.
    strip: bool,
    matcher: AhoCorasick,
}

impl MarkerRule {
    fn new(
        id: &'static str,
        level: CleanupLevel,
        message: &'static str,
        strip: bool,
        patterns: &[&str],
    ) -> Self {
        // LeftmostLongest: several lists contain both a token and a longer token with the
        // same prefix (`<|start|>` vs a role token, `generated with claude code` vs the
        // bracketed footer form); the longer, more specific match is the honest report.
        let matcher = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::LeftmostLongest)
            .build(patterns)
            .expect("marker patterns are literal, valid aho-corasick patterns");
        MarkerRule {
            id,
            level,
            message,
            strip,
            matcher,
        }
    }

    fn find(&self, idx: usize, span: &ProseSpan) -> Vec<Finding> {
        let text = span.text.as_str();
        self.matcher
            .find_iter(text)
            .map(|m| Finding {
                rule_id: self.id.to_string(),
                span_index: idx,
                range: m.start()..m.end(),
                message: format!("{}: {:?}", self.message, &text[m.start()..m.end()]),
                replacement: if self.strip {
                    Some(String::new())
                } else {
                    None
                },
            })
            .collect()
    }
}

impl Rule for MarkerRule {
    fn id(&self) -> &str {
        self.id
    }
    fn min_level(&self) -> CleanupLevel {
        self.level
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        self.find(idx, span)
    }
}

// ---------------------------------------------------------------------------
// Chat-template control tokens
// ---------------------------------------------------------------------------

/// Tokens emitted by chat templates / special-token vocabularies. They leak into documents
/// whenever a prompt, a raw API response, a log, or a truncated stream is pasted into prose.
///
/// Covers, as of 2026-10:
/// * OpenAI / gpt-oss harmony: `<|im_start|>`, `<|im_end|>`, `<|start|>`, `<|channel|>`,
///   `<|message|>`, `<|constrain|>`, `<|call|>`, `<|endofprompt|>`, `<|python_tag|>`
/// * Llama 3/4: `<|begin_of_text|>`, `<|start_header_id|>`, `<|end_header_id|>`,
///   `<|eot_id|>`, `<|python_tag|>`, `<|finetune_right_pad_id|>`
/// * Google Gemma 2/3: `<start_of_turn>`, `<end_of_turn>`; Gemma 4 (2026): `<|turn>`,
///   `<turn|>`, `<|channel>`, `<channel|>`, `<|think|>`
/// * Microsoft Phi 3/4, Zephyr, GLM-4, OLMo role tokens: `<|system|>`, `<|user|>`,
///   `<|assistant|>`, `<|observation|>`; ChatGLM `[gMASK]`, `<sop>`, `<eop>`
/// * Cohere Command R/R+: `<|START_OF_TURN_TOKEN|>`, `<|END_OF_TURN_TOKEN|>` and the
///   `<|SYSTEM_TOKEN|>` / `<|USER_TOKEN|>` / `<|CHATBOT_TOKEN|>` role tokens
/// * Qwen / DeepSeek / GLM: `<|im_end|>`, `<|endoftext|>`, `<|▁begin▁of▁sentence|>`,
///   `｜end▁of▁sentence｜` (fullwidth pipe variant), DeepSeek `<｜User｜>` / `<｜Assistant｜>`,
///   Qwen-VL/Audio `<|vision_start|>` / `<|image_pad|>` / `<|audio_start|>` and friends
/// * Mistral / community instruction formats: `[INST]`, `[/INST]`, `<<SYS>>`, `<</SYS>>`,
///   `[SYSTEM_PROMPT]`, `[AVAILABLE_TOOLS]`, `[TOOL_CALLS]`, `[TOOL_RESULTS]`
/// * Anthropic extended-thinking markup: `<antThinking>`, `</antThinking>`, `antml:`
/// * Hugging Face tokenizer specials that leak from raw generations: `<bos>`, `<eos>`,
///   `<unk>`, `<pad>`, `<mask>`, `[UNK]`, `[PAD]`, `[MASK]`, `<s>` / `</s>`
/// * Detokenizer residue: SentencePiece `▁` (U+2581) and byte-level-BPE `Ġ` (U+0120) /
///   `Ċ` (U+010A), flagged here and normalized to spaces by `typo.detokenizer.normalize`
///
/// Anything with the `<|…|>` shape that is *not* enumerated here is caught generically by
/// [`SpecialTokenPatternRule`], so a model family we have never heard of still leaves a
/// recognizable fingerprint instead of passing silently.
const CHAT_TOKENS: &[&str] = &[
    "<|im_start|>",
    "<|im_end|>",
    "<|im_sep|>",
    "<|endoftext|>",
    "<|startoftext|>",
    "<|eot_id|>",
    "<|eom|>",
    "<|end|>",
    "<|begin_of_text|>",
    "<|start_header_id|>",
    "<|end_header_id|>",
    "<|start|>",
    "<|channel|>",
    "<|message|>",
    "<|constrain|>",
    "<|call|>",
    "<|return|>",
    "<|recipient|>",
    "<|content|>",
    "<|python_tag|>",
    "<|endofprompt|>",
    "<|fim_prefix|>",
    "<|fim_middle|>",
    "<|fim_suffix|>",
    "<|begin\u{2581}of\u{2581}sentence|>",
    "\u{FF5C}end\u{2581}of\u{2581}sentence\u{FF5C}",
    "<|reserved_special_token",
    "[INST]",
    "[/INST]",
    "<<SYS>>",
    "<</SYS>>",
    "<antThinking>",
    "</antThinking>",
    "antml:",
    "</s>",
    // Google Gemma 2/3 turn tokens, and the Gemma 4 (2026) turn/channel tokens.
    "<start_of_turn>",
    "<end_of_turn>",
    "<|turn>",
    "<turn|>",
    "<|channel>",
    "<channel|>",
    "<|think|>",
    // Role tokens shared by Phi 3/4, Zephyr, GLM-4, OLMo and many community templates.
    "<|system|>",
    "<|user|>",
    "<|assistant|>",
    "<|observation|>",
    "<|developer|>",
    "<|tool|>",
    // ChatGLM / GLM specials.
    "[gMASK]",
    "<sop>",
    "<eop>",
    // Cohere Command R family (matched case-insensitively, listed as shipped).
    "<|START_OF_TURN_TOKEN|>",
    "<|END_OF_TURN_TOKEN|>",
    "<|SYSTEM_TOKEN|>",
    "<|USER_TOKEN|>",
    "<|CHATBOT_TOKEN|>",
    // Llama padding/vision specials.
    "<|finetune_right_pad_id|>",
    "<|eom_id|>",
    "<|image|>",
    // DeepSeek: ASCII sentence tokens and the fullwidth-pipe forms, bare and in brackets,
    // plus the fullwidth role markers its chat template wraps around each turn.
    "<|begin_of_sentence|>",
    "<|end_of_sentence|>",
    "\u{FF5C}begin\u{2581}of\u{2581}sentence\u{FF5C}",
    "<\u{FF5C}begin\u{2581}of\u{2581}sentence\u{FF5C}>",
    "<\u{FF5C}end\u{2581}of\u{2581}sentence\u{FF5C}>",
    "<\u{FF5C}User\u{FF5C}>",
    "<\u{FF5C}Assistant\u{FF5C}>",
    // Qwen multimodal specials (Qwen-VL / Qwen-Audio generations pasted into documents).
    "<|vision_start|>",
    "<|vision_end|>",
    "<|image_pad|>",
    "<|video_pad|>",
    "<|audio_start|>",
    "<|audio_end|>",
    // Mistral tool/system sections.
    "[SYSTEM_PROMPT]",
    "[/SYSTEM_PROMPT]",
    "[AVAILABLE_TOOLS]",
    "[/AVAILABLE_TOOLS]",
    "[TOOL_CALLS]",
    "[TOOL_RESULTS]",
    // Hugging Face tokenizer specials.
    "<bos>",
    "<eos>",
    "<unk>",
    "<pad>",
    "<mask>",
    "<sep>",
    "<cls>",
    "<s>",
    "[UNK]",
    "[PAD]",
    "[MASK]",
    "[SEP]",
    "[CLS]",
    // Detokenizer residue (see the doc comment above): SentencePiece space marker and the
    // byte-level-BPE visible-space / newline glyphs. Flagged here at light; turned into
    // plain spaces at aggressive by `typo.detokenizer.normalize`.
    "\u{2581}",
    "\u{0120}",
    "\u{010A}",
];

/// Unambiguous subset that is safe to delete outright at the aggressive level: every entry
/// is a serialization token no author types on purpose.
const CHAT_TOKENS_STRIPPABLE: &[&str] = &[
    "<|im_start|>",
    "<|im_end|>",
    "<|im_sep|>",
    "<|endoftext|>",
    "<|startoftext|>",
    "<|eot_id|>",
    "<|eom|>",
    "<|begin_of_text|>",
    "<|start_header_id|>",
    "<|end_header_id|>",
    "<|fim_prefix|>",
    "<|fim_middle|>",
    "<|fim_suffix|>",
    "<|begin\u{2581}of\u{2581}sentence|>",
    "\u{FF5C}end\u{2581}of\u{2581}sentence\u{FF5C}",
    "<|python_tag|>",
    "<|endofprompt|>",
    "<|channel|>",
    "<|message|>",
    "<|constrain|>",
    "<|call|>",
    "<antThinking>",
    "</antThinking>",
    // Additions (2026-10 refresh). The HF specials (`<bos>`, `[PAD]`, …) and the Mistral
    // bracket sections stay flag-only: angle/square-bracket tags are things a document
    // *about* tokenizers legitimately quotes, while the `<|…|>` and fullwidth-pipe forms
    // below are serialization syntax with no prose use.
    "<start_of_turn>",
    "<end_of_turn>",
    "<|turn>",
    "<turn|>",
    "<|channel>",
    "<channel|>",
    "<|think|>",
    "<|system|>",
    "<|user|>",
    "<|assistant|>",
    "<|observation|>",
    "<|START_OF_TURN_TOKEN|>",
    "<|END_OF_TURN_TOKEN|>",
    "<|SYSTEM_TOKEN|>",
    "<|USER_TOKEN|>",
    "<|CHATBOT_TOKEN|>",
    "<|finetune_right_pad_id|>",
    "<|eom_id|>",
    "<|begin_of_sentence|>",
    "<|end_of_sentence|>",
    "\u{FF5C}begin\u{2581}of\u{2581}sentence\u{FF5C}",
    "<\u{FF5C}begin\u{2581}of\u{2581}sentence\u{FF5C}>",
    "<\u{FF5C}end\u{2581}of\u{2581}sentence\u{FF5C}>",
    "<\u{FF5C}User\u{FF5C}>",
    "<\u{FF5C}Assistant\u{FF5C}>",
    "<|vision_start|>",
    "<|vision_end|>",
    "<|image_pad|>",
    "<|video_pad|>",
    "<|audio_start|>",
    "<|audio_end|>",
    "[gMASK]",
    "<sop>",
    "<eop>",
];

/// Flag chat-template control tokens for review.
pub fn chat_tokens() -> MarkerRule {
    MarkerRule::new(
        "markers.chat-tokens",
        CleanupLevel::Light,
        "chat-template control token (raw model serialization, never finished prose)",
        false,
        CHAT_TOKENS,
    )
}

/// Remove the unambiguous chat-template control tokens.
pub fn chat_tokens_strip() -> MarkerRule {
    MarkerRule::new(
        "markers.chat-tokens.strip",
        CleanupLevel::Aggressive,
        "removed chat-template control token",
        true,
        CHAT_TOKENS_STRIPPABLE,
    )
}

/// Flag anything with the *shape* of a special token — `<|…|>` (and the fullwidth-pipe
/// `｜…▁…｜` form) — that the literal lists do not already know. Model families ship new
/// token vocabularies faster than any catalogue can be re-reviewed; the delimiters are the
/// durable fingerprint even when the token name is not. Flag-only even at aggressive: an
/// unrecognized token might be quoted deliberately, and the enumerated strippable subset
/// already covers the tokens we are sure about.
pub struct SpecialTokenPatternRule;

impl SpecialTokenPatternRule {
    /// Longest token interior we are willing to call a special token. Real interiors are
    /// short (`finetune_right_pad_id`); a paragraph of prose between pipes is a table.
    const MAX_INNER: usize = 64;

    fn is_known(candidate: &str) -> bool {
        CHAT_TOKENS
            .iter()
            .chain(TOOL_CALL_MARKERS.iter())
            .any(|k| k.eq_ignore_ascii_case(candidate) || k.contains(candidate))
    }
}

impl Rule for SpecialTokenPatternRule {
    fn id(&self) -> &str {
        "markers.chat-tokens.pattern"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();

        // ASCII form: <|name|>
        let mut from = 0usize;
        while let Some(rel) = text[from..].find("<|") {
            let start = from + rel;
            let inner_start = start + 2;
            let Some(close_rel) = text[inner_start..].find("|>") else {
                break;
            };
            let inner = &text[inner_start..inner_start + close_rel];
            let end = inner_start + close_rel + 2;
            let plausible = !inner.is_empty()
                && inner.len() <= Self::MAX_INNER
                && inner
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if plausible && !Self::is_known(&text[start..end]) {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..end,
                    message: format!(
                        "unrecognized special-token-shaped marker {:?} (chat-template \
                         serialization from a model family not in the catalogue)",
                        &text[start..end]
                    ),
                    replacement: None,
                });
            }
            from = end;
        }

        // Fullwidth-pipe form: ｜name▁with▁markers｜ (DeepSeek-style). Requiring the
        // SentencePiece marker or an underscore inside keeps ordinary Japanese title
        // separators (`作品｜作者`) out of the report.
        let mut from = 0usize;
        while let Some(rel) = text[from..].find('\u{FF5C}') {
            let start = from + rel;
            let inner_start = start + '\u{FF5C}'.len_utf8();
            let Some(close_rel) = text[inner_start..].find('\u{FF5C}') else {
                break;
            };
            let inner = &text[inner_start..inner_start + close_rel];
            let end = inner_start + close_rel + '\u{FF5C}'.len_utf8();
            let plausible = !inner.is_empty()
                && inner.chars().count() <= Self::MAX_INNER
                && (inner.contains('\u{2581}') || inner.contains('_'))
                && !inner.chars().any(|c| c.is_whitespace() || c.is_control());
            if plausible && !Self::is_known(&text[start..end]) {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..end,
                    message: format!(
                        "unrecognized fullwidth-pipe special token {:?} (chat-template \
                         serialization, DeepSeek-style)",
                        &text[start..end]
                    ),
                    replacement: None,
                });
            }
            from = end;
        }

        out.sort_by_key(|f| (f.range.start, f.range.end));
        out
    }
}

// ---------------------------------------------------------------------------
// Chat-UI citation artifacts
// ---------------------------------------------------------------------------

/// Citation plumbing the chat UIs leave behind when you copy an answer.
///
/// * ChatGPT web search / deep research: `【9†source】`, `【45†L75-L83】`, and the
///   `oai_citation:` marker family that the copy handler leaks in some locales and clients.
///   The `turn<digits>search<digits>` family is pattern-matched by [`TurnCitationRule`]
///   (any turn/source number, not just `turn0…`), and the Private-Use-Area citation
///   wrappers the UI actually stores are handled by [`PrivateUseCitationRule`].
/// * Legacy `contentReference[oaicite:0]` markers from GPT-4 browsing.
/// * Shared-link tracking parameters chatbots append to cited links
///   (`utm_source=chatgpt.com`, `utm_source=copilot.com`, `referrer=grok.com`) and
///   share-link URLs pasted into a document, which name the tool outright.
/// * Claude artifact tags and Grok inline render directives (`<grok:render`).
const CITATION_MARKERS: &[&str] = &[
    "\u{2020}source\u{3011}",
    "\u{2020}L",
    "oai_citation",
    "contentReference",
    "oaicite",
    "utm_source=chatgpt.com",
    "utm_source=chatgpt",
    "utm_source=openai",
    "utm_medium=chatgpt",
    "utm_source=copilot.com",
    "utm_source=copilot",
    "utm_source=perplexity",
    "referrer=grok.com",
    "chatgpt.com/share/",
    "claude.ai/share/",
    "g.co/gemini/share",
    "grok:render",
    "smart_search_thinking",
    "antartifact",
];

/// Flag chat-UI citation artifacts.
pub fn citation_artifacts() -> MarkerRule {
    MarkerRule::new(
        "markers.citation-artifacts",
        CleanupLevel::Light,
        "chat-UI citation artifact (copy/paste residue, not part of the answer)",
        false,
        CITATION_MARKERS,
    )
}

/// Bracket citation markers such as `【45†L75-L83】` where the enclosing CJK brackets and an
/// inner dagger/`source`/`web` word identify a ChatGPT research citation. Kept separate from
/// [`MarkerRule`] because the content between the brackets varies per citation.
pub struct CitationBracketRule;

impl CitationBracketRule {
    /// Longest bracket group we are willing to inspect. Real markers are short
    /// (`【4:0†source】`); anything longer is prose that happens to use CJK brackets.
    const MAX_INNER: usize = 48;
}

impl Rule for CitationBracketRule {
    fn id(&self) -> &str {
        "markers.citation-brackets"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        for (start, ch) in text.char_indices() {
            if ch != '\u{3010}' {
                continue; // 【
            }
            let inner_start = start + ch.len_utf8();
            let Some(close_rel) = text[inner_start..].find('\u{3011}') else {
                continue; // 【
            };
            let close = inner_start + close_rel;
            let inner = &text[inner_start..close];
            if inner.chars().count() > Self::MAX_INNER {
                continue;
            }
            let inner_lower = inner.to_ascii_lowercase();
            let looks_like_citation = inner.contains('\u{2020}')
                || inner_lower.contains("source")
                || inner_lower.contains("web")
                || inner_lower.contains("turn")
                || inner_lower.contains("cite");
            if !looks_like_citation {
                continue;
            }
            let end = close + '\u{3011}'.len_utf8();
            out.push(Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                range: start..end,
                message: format!("ChatGPT research citation marker {inner:?} left in the text"),
                replacement: None,
            });
        }
        out
    }
}

/// ChatGPT citations in the form the interface actually stores them: wrapped in Unicode
/// Private Use Area characters — `U+E200` opens, `U+E202` separates, `U+E201` closes —
/// around a payload like `cite`, `filecite`, `entity` or `url` plus opaque source ids
/// (`\u{E200}cite\u{E202}turn0search11\u{E202}turn0search0\u{E201}`). First catalogued in
/// pasted Wikipedia edits in February 2025; OpenAI's developer citation guide documents
/// the same delimiter scheme. In the chat UI this renders as a clickable source pill.
/// Everywhere else it is a box glyph (or invisible) followed by plumbing — the single
/// most specific copy/paste fingerprint a document can carry.
///
/// Flagged at `light` (restore the source by hand), removed whole at `aggressive`.
/// Leftover sibling characters from the same block (`U+E203`/`U+E204`/`U+E206` bracket
/// cited spans) are removed by `unicode.private-use.strip`.
pub struct PrivateUseCitationRule {
    strip: bool,
}

impl PrivateUseCitationRule {
    /// Longest wrapped payload we are willing to treat as one citation. Real markers list
    /// a handful of source ids; anything longer is two markers (or not a marker at all).
    const MAX_SPAN_BYTES: usize = 512;
}

impl Rule for PrivateUseCitationRule {
    fn id(&self) -> &str {
        if self.strip {
            "markers.private-use-citations.strip"
        } else {
            "markers.private-use-citations"
        }
    }
    fn min_level(&self) -> CleanupLevel {
        if self.strip {
            CleanupLevel::Aggressive
        } else {
            CleanupLevel::Light
        }
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(rel) = text[from..].find('\u{E200}') {
            let start = from + rel;
            let after_open = start + '\u{E200}'.len_utf8();
            // A complete marker ends at the next U+E201 with no newline in between. A
            // truncated stream can drop the closer; then only the orphan opener itself
            // is reported (the bare `turn…` ids after it are `TurnCitationRule`'s job).
            let end = match text[after_open..].find('\u{E201}') {
                Some(close_rel) => {
                    let close = after_open + close_rel;
                    let candidate = close + '\u{E201}'.len_utf8();
                    if candidate - start <= Self::MAX_SPAN_BYTES
                        && !text[start..candidate].contains(['\n', '\r'])
                    {
                        candidate
                    } else {
                        after_open
                    }
                }
                None => after_open,
            };
            out.push(Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                range: start..end,
                message: if self.strip {
                    "removed Private-Use-Area citation marker (ChatGPT source pill \
                     plumbing, U+E200..U+E201)"
                        .to_string()
                } else {
                    "Private-Use-Area citation marker (ChatGPT source pill copied without \
                     its link; restore the source or remove the marker)"
                        .to_string()
                },
                replacement: if self.strip {
                    Some(String::new())
                } else {
                    None
                },
            });
            from = end;
        }
        out
    }
}

/// Flag ChatGPT's Private-Use-Area citation markers for review.
pub fn private_use_citations() -> PrivateUseCitationRule {
    PrivateUseCitationRule { strip: false }
}

/// Remove ChatGPT's Private-Use-Area citation markers outright.
pub fn private_use_citations_strip() -> PrivateUseCitationRule {
    PrivateUseCitationRule { strip: true }
}

/// The same citation ids with their Private Use delimiters lost in transit — the plain
/// gibberish form `citeturn0search0`, `turn12search3turn34view7` or
/// `fileciteturn0file0L20-L28` that survives when an answer is copied through a channel
/// that drops unassigned characters (or read out of the conversation API). This pattern
/// generalization replaces the old fixed literals (`turn0search`, `citeturn0`), which
/// only ever matched the *first* tool call of an answer.
///
/// Flagged at `light` so the author can restore the missing source; removed at
/// `aggressive`, where the user has opted into losing the placeholder.
pub struct TurnCitationRule {
    strip: bool,
}

impl TurnCitationRule {
    const KINDS: &[&str] = &["search", "file", "news", "image", "view", "video", "audio"];

    /// Parse one or more `turn<digits><kind><digits>` groups (with an optional
    /// `L<digits>-L<digits>` line range after a group) starting at `pos` in the
    /// ASCII-lowercased text. Returns the end offset of the last complete group.
    fn parse_groups(lower: &str, mut cursor: usize) -> Option<usize> {
        let bytes = lower.as_bytes();
        let mut end: Option<usize> = None;
        while lower[cursor..].starts_with("turn") {
            let mut i = cursor + 4;
            let digits_start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == digits_start {
                break;
            }
            let Some(mut j) = Self::KINDS
                .iter()
                .find(|k| lower[i..].starts_with(*k))
                .map(|k| i + k.len())
            else {
                break;
            };
            let digits2_start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j == digits2_start {
                break;
            }
            // Optional source line range, e.g. `turn0file0L20-L28`.
            if j < bytes.len() && bytes[j] == b'l' {
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 {
                    if k < bytes.len() && bytes[k] == b'-' {
                        let mut m = k + 1;
                        if m < bytes.len() && bytes[m] == b'l' {
                            m += 1;
                            let range_digits = m;
                            while m < bytes.len() && bytes[m].is_ascii_digit() {
                                m += 1;
                            }
                            if m > range_digits {
                                k = m;
                            }
                        }
                    }
                    j = k;
                }
            }
            end = Some(j);
            cursor = j;
        }
        end
    }

    fn scan(text: &str) -> Vec<std::ops::Range<usize>> {
        // `to_ascii_lowercase` preserves byte offsets, so ranges map back onto `text`.
        let lower = text.to_ascii_lowercase();
        let bytes = lower.as_bytes();
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find("turn") {
            let pos = from + rel;
            // Include an immediately preceding `cite` / `filecite` prefix so the report
            // (and the aggressive deletion) covers the whole leak, not its tail. A
            // preceding alphanumeric otherwise means `turn` is mid-word ("return",
            // "nocturne") and not plumbing at all.
            let mut start = pos;
            if pos >= 4 && lower.is_char_boundary(pos - 4) && &lower[pos - 4..pos] == "cite" {
                start = pos - 4;
                if start >= 4
                    && lower.is_char_boundary(start - 4)
                    && &lower[start - 4..start] == "file"
                {
                    start -= 4;
                }
            }
            let blocked_by_word = start > 0 && {
                let prev = bytes[start - 1];
                prev.is_ascii_alphanumeric() || prev == b'_'
            };
            if blocked_by_word {
                from = pos + 4;
                continue;
            }
            match Self::parse_groups(&lower, pos) {
                Some(end) => {
                    out.push(start..end);
                    from = end;
                }
                None => from = pos + 4,
            }
        }
        out
    }
}

impl Rule for TurnCitationRule {
    fn id(&self) -> &str {
        if self.strip {
            "markers.turn-citations.strip"
        } else {
            "markers.turn-citations"
        }
    }
    fn min_level(&self) -> CleanupLevel {
        if self.strip {
            CleanupLevel::Aggressive
        } else {
            CleanupLevel::Light
        }
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        Self::scan(text)
            .into_iter()
            .map(|range| Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                message: format!(
                    "{} ChatGPT citation id {:?} (the source link was lost in copy/paste)",
                    if self.strip { "removed" } else { "leaked" },
                    &text[range.clone()]
                ),
                range,
                replacement: if self.strip {
                    Some(String::new())
                } else {
                    None
                },
            })
            .collect()
    }
}

/// Flag bare `citeturn…` / `turn<digits><kind><digits>` citation ids for review.
pub fn turn_citations() -> TurnCitationRule {
    TurnCitationRule { strip: false }
}

/// Remove bare `citeturn…` citation ids outright.
pub fn turn_citations_strip() -> TurnCitationRule {
    TurnCitationRule { strip: true }
}

// ---------------------------------------------------------------------------
// Agentic coding trailers
// ---------------------------------------------------------------------------

/// Attribution blocks that AI coding agents append to commits, pull requests, changelogs and
/// READMEs. All are flag-only: a project may legitimately *document* that it uses these tools.
///
/// Sources (2026): Claude Code's `attribution` setting (`Co-Authored-By: Claude …`,
/// `🤖 Generated with [Claude Code]`), its remote-session `Claude-Session:` trailer, Cursor /
/// Copilot / Codex / Devin / Jules / Aider footers.
const AGENT_TRAILERS: &[&str] = &[
    "generated with claude code",
    "generated with [claude code]",
    "co-authored-by: claude",
    "noreply@anthropic.com",
    "claude-session:",
    "claude.com/claude-code",
    "claude.ai/code/session",
    "generated by claude",
    "made with cursor",
    "generated with cursor",
    "cursor agent",
    "co-authored-by: cursor",
    "generated by copilot",
    "copilot workspace",
    "github copilot",
    "co-authored-by: copilot",
    "openai codex",
    "codex cli",
    "generated with codex",
    "generated by devin",
    "co-authored-by: devin",
    "google jules",
    "gemini cli",
    "qwen code",
    "generated with aider",
    "aider.chat",
    "generated with windsurf",
    "windsurf agent",
    "generated by replit agent",
    "claude cowork",
    "claude tag",
    // 2025–2026 additions: bot account names as they appear in `Author:` lines, exported
    // changelogs and PR bodies pasted into documents, plus the second wave of editors.
    "copilot-swe-agent",
    "claude[bot]",
    "cursor[bot]",
    "devin-ai-integration",
    "gemini-code-assist",
    "[bot]@users.noreply.github.com",
    "co-authored-by: github copilot",
    "co-authored-by: openai codex",
    "generated with github copilot",
    "generated with cline",
    "roo code",
    "continue.dev",
    "sourcegraph cody",
    "generated by cody",
    "amazon q developer",
    "generated by amazon q",
    "jetbrains ai assistant",
    "augment code",
    "codeium",
    "sweep ai",
    "created with lovable",
    "built with v0",
    "generated by bolt",
    "claude.com/code/session",
    "generated by chatgpt",
    "generated with chatgpt",
    "written by chatgpt",
    "drafted by claude",
    "written by claude",
];

/// Flag agentic-coding attribution trailers.
pub fn agent_trailers() -> MarkerRule {
    MarkerRule::new(
        "markers.agent-trailers",
        CleanupLevel::Light,
        "AI coding-agent attribution trailer (reveals the tool that produced the text)",
        false,
        AGENT_TRAILERS,
    )
}

/// Flag a whole git-trailer-style attribution *line* — `Co-Authored-By:`, `Generated-By:`
/// or `Assisted-By:` naming an AI agent — that the literal list above does not cover.
/// The literal list ages quickly (every model release renames the byline:
/// `Co-Authored-By: GPT-5.6-Codex <…>`); the trailer shape does not. Lines the literal
/// rule already reports are skipped so the same trailer is never counted twice, and a
/// *human* co-author line matches nothing: an agent name must appear on the line.
pub struct AgentTrailerLineRule;

impl AgentTrailerLineRule {
    const PREFIXES: &[&str] = &["co-authored-by:", "generated-by:", "assisted-by:"];
    const AGENT_NAMES: &[&str] = &[
        "claude",
        "chatgpt",
        "gpt-",
        "openai",
        "copilot",
        "cursor",
        "codex",
        "aider",
        "windsurf",
        "gemini",
        "anthropic",
        "amazon q",
        "codeium",
        "tabnine",
        "sweep",
        "deepseek",
        "qwen",
        "[bot]",
        // Deliberately absent: Devin, Cody, Jules, Kimi, Cline. Each is also an ordinary
        // human first name (and "cline" hides inside "Jacqueline"), so a *human*
        // co-author line would false-positive. Their product-specific strings are in
        // the literal list above ("generated by devin", "google jules", …).
    ];
    /// Trailers are one short line; cap the scan so a `Co-Authored-By:` mentioned in a
    /// paragraph does not swallow the paragraph.
    const MAX_LINE_BYTES: usize = 200;
}

impl Rule for AgentTrailerLineRule {
    fn id(&self) -> &str {
        "markers.agent-trailer-lines"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let lower = text.to_ascii_lowercase();
        let mut out = Vec::new();
        for prefix in Self::PREFIXES {
            let mut from = 0usize;
            while let Some(rel) = lower[from..].find(*prefix) {
                let start = from + rel;
                let mut end = text[start..]
                    .find('\n')
                    .map_or(text.len(), |n| start + n)
                    .min(start + Self::MAX_LINE_BYTES);
                while end < text.len() && !text.is_char_boundary(end) {
                    end -= 1;
                }
                let line_lower = &lower[start..end];
                let already_known = AGENT_TRAILERS.iter().any(|lit| line_lower.contains(lit));
                let names_agent = Self::AGENT_NAMES.iter().any(|n| line_lower.contains(n));
                if !already_known && names_agent {
                    let trimmed_end = start + text[start..end].trim_end().len();
                    out.push(Finding {
                        rule_id: self.id().to_string(),
                        span_index: idx,
                        range: start..trimmed_end,
                        message: format!(
                            "AI-agent attribution trailer line: {:?}",
                            &text[start..trimmed_end]
                        ),
                        replacement: None,
                    });
                }
                from = end.max(start + prefix.len());
            }
        }
        out.sort_by_key(|f| (f.range.start, f.range.end));
        out
    }
}

// ---------------------------------------------------------------------------
// Tool-call and agent-scaffolding leakage
// ---------------------------------------------------------------------------

/// Fragments of the tool-calling machinery that agent transcripts and API logs leak into
/// documents: function-call XML (Anthropic's `antml:` form *without* the namespace
/// prefix, as community clients serialize it), Harmony/Gemma tool sections, tool-use ids,
/// and harness scaffolding such as Claude Code's `<system-reminder>` injections. Like
/// chat tokens these are machinery, never prose, but they are flag-only: a post *about*
/// tool calling quotes them on purpose.
const TOOL_CALL_MARKERS: &[&str] = &[
    "<tool_call>",
    "</tool_call>",
    "<tool_calls>",
    "</tool_calls>",
    "<|tool_call|>",
    "<|tool_response|>",
    "<function_calls>",
    "</function_calls>",
    "<invoke name=",
    "<parameter name=",
    "\"tool_calls\":",
    "\"function_call\":",
    "tool_use_id",
    "toolu_",
    "<system-reminder>",
    "</system-reminder>",
    "<system_reminder>",
    "<|plugin|>",
    "<start_code>",
    "<end_code>",
];

/// Flag leaked tool-call / agent-scaffolding fragments.
pub fn tool_calls() -> MarkerRule {
    MarkerRule::new(
        "markers.tool-calls",
        CleanupLevel::Light,
        "tool-call or agent-scaffolding fragment (raw transcript machinery, not prose)",
        false,
        TOOL_CALL_MARKERS,
    )
}

// ---------------------------------------------------------------------------
// Assistant self-identification / model identifiers
// ---------------------------------------------------------------------------

/// Assistant-identity leakage: the model naming itself, its maker, or its model string.
/// This is the single most reliable textual tell that a passage was pasted straight out of a
/// chat window, and it is model-version specific, so the list is refreshed each release.
///
/// Families covered (October 2026): OpenAI GPT-5.x / GPT-6 "Astra"; Anthropic Claude Opus 5.x,
/// Sonnet 5, Haiku 4.5, Fable 5.1, Mythos 5.1; Google Gemini 3.x; Meta Llama 4 / Meta AI;
/// xAI Grok 4/5; Mistral Large 3 / Le Chat; DeepSeek V4.x; Alibaba Qwen3.8; Moonshot Kimi K3;
/// Zhipu GLM-5; Amazon Nova; Microsoft Copilot; Perplexity Sonar.
const MODEL_IDENTIFIERS: &[&str] = &[
    // Model names, product strings and assistant self-identification. Generic
    // disclaimers ("as an AI", "my knowledge cutoff", …) deliberately live in
    // `templates/core.yaml` (`phrasal.machine-residue` and friends) so the same
    // phrase is not reported twice by two different rule families.
    "gpt-5",
    "gpt-6",
    "gpt-4o",
    "chatgpt-4o",
    "o4-mini",
    "claude-opus",
    "claude-sonnet",
    "claude-haiku",
    "claude-3-",
    "claude-4-",
    "gemini-3",
    "gemini-2.5",
    "gemini-pro",
    "llama-4",
    "llama-3",
    "grok-4",
    "grok-5",
    "mistral-large",
    "deepseek-v4",
    "deepseek-v3",
    "deepseek-r1",
    "deepseek v4",
    "qwen3",
    "qwen2.5",
    "kimi-k3",
    "kimi-k2",
    "kimi k2",
    "glm-5",
    "glm-4",
    "sonar-pro",
    // Open-weight and second-wave families whose raw outputs now turn up in documents
    // via local/agentic tooling (Ollama, LM Studio, open agent harnesses).
    "gemma-4",
    "gemma-3",
    "phi-4",
    "phi-3",
    "command-r",
    "command-a",
    "mixtral",
    "magistral",
    "llama 4",
    "claude-fable",
    "claude-mythos",
    "gpt-oss",
    "minimax-m",
    "ernie-",
    "hunyuan",
    "chatgpt-5",
    "i am gemma",
    "i'm gemma",
    "i was created by",
    "i was developed by",
    "i was built by",
    "as a language model developed",
    "developed by openai",
    "developed by anthropic",
    "created by openai",
    "trained by anthropic",
    "i am an ai",
    "i'm an ai",
    "i am claude",
    "i'm claude",
    "claude, an ai assistant",
    "anthropic's claude",
    "made by anthropic",
    "i am gemini",
    "i'm gemini",
    "google's gemini",
    "i am chatgpt",
    "i'm chatgpt",
    "i am grok",
    "i'm grok",
    "i am meta ai",
    "i'm meta ai",
    "i am deepseek",
    "i'm deepseek",
    "i am qwen",
    "i'm qwen",
    "i am kimi",
    "i'm kimi",
    "i am copilot",
    "i'm copilot",
    "i am perplexity",
    "i'm perplexity",
    "le chat",
    "my training cutoff",
    "i cannot browse the internet",
    "i was trained by",
    "this model was trained",
    "textgrain",
    "synthid",
    "c2pa",
    "content credentials",
    "ai-generated content notice",
];

/// Flag assistant self-identification and leaked model identifiers.
pub fn model_identifiers() -> MarkerRule {
    MarkerRule::new(
        "markers.model-identifiers",
        CleanupLevel::Aggressive,
        "model self-identification or leaked model identifier",
        false,
        MODEL_IDENTIFIERS,
    )
}

/// Flag reasoning/thinking-trace leakage: the scaffolding a reasoning model leaves when its
/// trace is pasted or when a transcript is copied out of `--verbose` output.
pub struct ReasoningTraceRule;

impl ReasoningTraceRule {
    const LITERALS: &[&str] = &[
        "<think",
        "</think",
        "thinking...",
        "let me think",
        "let's think step by step",
        "step-by-step reasoning",
        "chain of thought",
        "reasoning_content",
        "here's my thinking",
        "here is my thinking",
        "let me work through",
        "thought for ",
        "reasoning trace",
        // 2025–2026 reasoning surfaces: alternate block tags (incl. Anthropic's classic
        // `<scratchpad>`), the Kimi/Qwen fence triangles, and chat-UI thinking panels
        // copied as text ("Show thinking", the "Thought for Xs" disclosure header).
        "<reasoning>",
        "</reasoning>",
        "<scratchpad>",
        "</scratchpad>",
        "<reflection>",
        "</reflection>",
        "\u{25C1}think\u{25B7}",
        "show thinking",
        "extended thinking",
        "thinking process:",
        "internal reasoning:",
    ];
}

impl Rule for ReasoningTraceRule {
    fn id(&self) -> &str {
        "markers.reasoning-trace"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let lower = text.to_ascii_lowercase();
        let mut out = Vec::new();
        for needle in Self::LITERALS {
            let mut from = 0usize;
            while let Some(rel) = lower[from..].find(*needle) {
                let start = from + rel;
                let end = start + needle.len();
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..end,
                    message: format!(
                        "reasoning-trace leak ({:?}): raw thinking should not be in finished prose",
                        &text[start..end]
                    ),
                    replacement: None,
                });
                from = end.max(start + 1);
            }
        }
        out.sort_by_key(|f| (f.range.start, f.range.end));
        out
    }
}

/// Flag a full ```` ```think ```` / `<think>`-style reasoning block, including its body, so the
/// user can delete the whole thing in one gesture. The range covers from the opening marker
/// to the closing marker (or the end of the span when it is unterminated).
pub struct ReasoningBlockRule;

impl ReasoningBlockRule {
    const OPENERS: &[&str] = &[
        "<think",
        "<think>",
        "<thinking>",
        "```thinking",
        "```think",
        "<reasoning>",
        "```reasoning",
        "<scratchpad>",
        "<antthinking>",
        "\u{25C1}think\u{25B7}",
    ];
    // Both the bare and the angle-bracket-terminated spelling are listed: the tie-break in
    // `earliest` prefers the longest match, so the `>` is included in the reported range, and
    // a truncated closer (stream cut off mid-tag) still terminates the block.
    const CLOSERS: &[&str] = &[
        "</think",
        "</think>",
        "</thinking",
        "</thinking>",
        "```",
        "</reasoning>",
        "</scratchpad>",
        "</antthinking>",
        "\u{25C1}/think\u{25B7}",
    ];

    /// The earliest needle at or after `from`, preferring the *longest* needle when two match
    /// at the same offset. Without the length tie-break `</think` would win over `</thinking>`
    /// and the block range would stop one character short of the closing `>`.
    fn earliest<'a>(haystack: &str, needles: &[&'a str], from: usize) -> Option<(usize, &'a str)> {
        needles
            .iter()
            .filter_map(|n| haystack[from..].find(*n).map(|p| (from + p, *n)))
            .min_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.len().cmp(&a.1.len())))
    }
}

impl Rule for ReasoningBlockRule {
    fn id(&self) -> &str {
        "markers.reasoning-block"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Standard
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let lower = text.to_ascii_lowercase();
        let mut out = Vec::new();
        let mut search_from = 0usize;
        while let Some((open_at, opener)) = Self::earliest(&lower, Self::OPENERS, search_from) {
            let body_start = open_at + opener.len();
            let close = Self::earliest(&lower, Self::CLOSERS, body_start);
            let (end, next) = match close {
                Some((at, closer)) => (at + closer.len(), at + closer.len()),
                None => (text.len(), text.len()),
            };
            out.push(Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                range: open_at..end,
                message: "reasoning block leaked into the document (raw thinking)".to_string(),
                replacement: None,
            });
            search_from = next;
            if search_from >= lower.len() {
                break;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::Ctx;
    use cleanup_core::ir::SpanAnchor;

    fn detect(rule: &dyn Rule, text: &str) -> Vec<Finding> {
        let span = ProseSpan::new(SpanAnchor::Bytes(0..text.len()), text);
        rule.detect(0, &span, &Ctx::new(CleanupLevel::Aggressive))
    }

    #[test]
    fn finds_chat_tokens_and_strips_them() {
        let text = "answer<|im_end|> and <|begin_of_text|>more";
        let flags = detect(&chat_tokens(), text);
        assert_eq!(flags.len(), 2);
        assert!(flags.iter().all(|f| f.replacement.is_none()));

        let strips = detect(&chat_tokens_strip(), text);
        assert_eq!(strips.len(), 2);
        assert!(strips.iter().all(|f| f.replacement.as_deref() == Some("")));
    }

    #[test]
    fn finds_citation_markers() {
        let text = "The report【45†L75-L83】says so, and【9†source】too.";
        let found = detect(&citation_artifacts(), text);
        assert!(found.len() >= 2, "expected dagger citations, got {found:?}");
        assert!(found.iter().all(|f| f.range.start < f.range.end));
    }

    #[test]
    fn citation_brackets_ignore_prose_brackets() {
        let rule = CitationBracketRule;
        assert!(detect(&rule, "A【note】inside prose.").is_empty());
        let hits = detect(&rule, "Claim【4:0†source】.");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            &"Claim【4:0†source】."[hits[0].range.clone()],
            "【4:0†source】"
        );
    }

    #[test]
    fn finds_agent_trailers() {
        let text = "Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>";
        let found = detect(&agent_trailers(), text);
        assert!(found.len() >= 2);
    }

    #[test]
    fn finds_model_identifiers() {
        let found = detect(&model_identifiers(), "I'm Claude, an AI assistant.");
        assert!(!found.is_empty());
    }

    #[test]
    fn reasoning_block_covers_the_whole_trace() {
        let text = "Answer. <think>hmm, 2+2</think>Done.";
        let found = detect(&ReasoningBlockRule, text);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].range.clone()], "<think>hmm, 2+2</think>");
    }

    #[test]
    fn unterminated_reasoning_block_runs_to_the_end() {
        let text = "Answer. <think>still going";
        let found = detect(&ReasoningBlockRule, text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.end, text.len());
    }

    #[test]
    fn finds_gemma_phi_and_glm_tokens() {
        let text = "<start_of_turn>user hi<end_of_turn> <|system|>s<|user|>u [gMASK]<sop>";
        let flags = detect(&chat_tokens(), text);
        assert_eq!(flags.len(), 6, "got {flags:?}");
        let strips = detect(&chat_tokens_strip(), text);
        assert_eq!(strips.len(), 6, "got {strips:?}");
    }

    #[test]
    fn pattern_rule_flags_unknown_special_tokens_only() {
        let text = "known <|im_end|> unknown <|frobnicate_9|> prose <| not a token |> done";
        let found = detect(&SpecialTokenPatternRule, text);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(&text[found[0].range.clone()], "<|frobnicate_9|>");
    }

    #[test]
    fn pattern_rule_flags_unknown_fullwidth_tokens_not_japanese_separators() {
        let token = "a \u{FF5C}begin\u{2581}of\u{2581}everything\u{FF5C} b";
        let found = detect(&SpecialTokenPatternRule, token);
        assert_eq!(found.len(), 1, "got {found:?}");
        let title = "Title\u{FF5C}Author Name";
        assert!(detect(&SpecialTokenPatternRule, title).is_empty());
    }

    #[test]
    fn private_use_citations_flagged_then_stripped_whole() {
        let text =
            "Water boils at 100 C\u{E200}cite\u{E202}turn0search11\u{E202}turn0search0\u{E201}.";
        let flags = detect(&private_use_citations(), text);
        assert_eq!(flags.len(), 1);
        assert!(flags[0].replacement.is_none());
        assert_eq!(
            &text[flags[0].range.clone()],
            "\u{E200}cite\u{E202}turn0search11\u{E202}turn0search0\u{E201}"
        );
        let strips = detect(&private_use_citations_strip(), text);
        assert_eq!(strips.len(), 1);
        assert_eq!(strips[0].replacement.as_deref(), Some(""));
        assert_eq!(strips[0].range, flags[0].range);
    }

    #[test]
    fn turn_citations_match_any_turn_and_source_numbers() {
        for (text, expect) in [
            ("Claim citeturn0search0 here.", "citeturn0search0"),
            (
                "Claim turn12search3turn34view7 here.",
                "turn12search3turn34view7",
            ),
            (
                "Cell fileciteturn0file0L20-L28 done.",
                "fileciteturn0file0L20-L28",
            ),
            ("Claim turn3image12 here.", "turn3image12"),
            // Multibyte characters immediately before the marker must not panic the
            // `cite` prefix look-behind (byte slicing on a non-boundary).
            ("\u{2764}\u{00E9}turn0search0", "turn0search0"),
        ] {
            let found = detect(&turn_citations(), text);
            assert_eq!(found.len(), 1, "text {text:?} got {found:?}");
            assert_eq!(&text[found[0].range.clone()], expect);
        }
    }

    #[test]
    fn turn_citations_ignore_ordinary_words() {
        for text in [
            "Please return0search forms to the desk.",
            "The nocturne38search was never a word.",
            "Turn left, then search the drawer.",
            "A turn0searching glance.",
        ] {
            assert!(
                detect(&turn_citations(), text).is_empty(),
                "false positive in {text:?}"
            );
        }
    }

    #[test]
    fn agent_trailer_lines_catch_unlisted_models_but_not_humans() {
        let agent = "Fix bug.\n\nCo-Authored-By: GPT-9.9-Hyperion <bot@example.com>\n";
        // "Hyperion" is unlisted, but the `gpt-` prefix gives the line away.
        let found = detect(&AgentTrailerLineRule, agent);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert!(
            agent[found[0].range.clone()].starts_with("Co-Authored-By:"),
            "whole line reported"
        );

        let human = "Fix bug.\n\nCo-Authored-By: Jacqueline Cody <jacqueline@example.com>\n";
        assert!(detect(&AgentTrailerLineRule, human).is_empty());

        // Already covered by the literal list: not double-counted by the line rule.
        let known = "Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>";
        assert!(detect(&AgentTrailerLineRule, known).is_empty());
    }

    #[test]
    fn finds_tool_call_leakage() {
        let text = "before <tool_call>{\"x\":1}</tool_call> and <system-reminder>hi";
        let found = detect(&tool_calls(), text);
        assert_eq!(found.len(), 3, "got {found:?}");
    }
}
