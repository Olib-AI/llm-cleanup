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

use aho_corasick::{AhoCorasick, MatchKind};
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
///   `<|eot_id|>`, `<|python_tag|>`, `<|finetune_right_pad_id|>`; Llama 4 also uses
///   `<|header_start|>`, `<|header_end|>`, `<|eot|>` and reasoning-thinking delimiters
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
    "<|end_of_text|>",
    "<|start_header_id|>",
    "<|end_header_id|>",
    // Llama 4's renamed headers and end-of-turn token, plus its reasoning, Python and
    // multimodal serialization tokens (Meta's models/llama4/tokenizer.py).
    "<|header_start|>",
    "<|header_end|>",
    "<|eot|>",
    "<|step|>",
    "<|python_start|>",
    "<|python_end|>",
    "<|finetune_right_pad|>",
    "<|image_start|>",
    "<|image_end|>",
    "<|tile_x_separator|>",
    "<|tile_y_separator|>",
    "<|patch|>",
    "<|reasoning_thinking_start|>",
    "<|reasoning_thinking_end|>",
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
    // --- 2026-10 second refresh: token vocabularies of the current model crop ---------
    // Kimi K2.5 / K3 (Moonshot AI): role tokens and the tool-call section framing its
    // published chat template wraps around every function call.
    "<|im_system|>",
    "<|im_user|>",
    "<|im_assistant|>",
    "<|im_tool|>",
    "<|media_begin|>",
    "<|media_content|>",
    "<|media_end|>",
    // DeepSeek V3.2 / V4: fullwidth-pipe role, tool and FIM tokens. The V3.1 template
    // already used the same pipe shape for sentence boundaries, so new kinds follow the
    // same pattern instead of inventing a separate family.
    "<\u{FF5C}System\u{FF5C}>",
    "<\u{FF5C}Tool\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}calls\u{2581}begin\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}calls\u{2581}end\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}call\u{2581}begin\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}call\u{2581}end\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}sep\u{FF5C}>",
    "<\u{FF5C}\u{2581}pad\u{2581}\u{FF5C}>",
    // Zhipu GLM-4.6 / GLM-5: the box markers its tool template emits around arguments.
    "<|begin_of_box|>",
    "<|end_of_box|>",
    // Llama 4.5 / Llama 4 instruct: the JSON tool payload markers added to the 2026
    // tokenizer alongside the Python ones.
    "<|json_start|>",
    "<|json_end|>",
    // Qwen3.8-VL / Qwen-VL grounding markers.
    "<|object_ref_start|>",
    "<|object_ref_end|>",
    "<|box_start|>",
    "<|box_end|>",
    "<|quad_start|>",
    "<|quad_end|>",
    // Gemma 4 multimodal serialization.
    "<|image>",
    "<|audio>",
    "<|video>",
    // Phi-4-reasoning / Phi-5 thinking delimiters.
    "<|thinking|>",
    "<|/thinking|>",
    // IBM Granite 4 chat template.
    "<|start_of_role|>",
    "<|end_of_role|>",
    // Mistral V7 / Magistral reasoning and tool sections (bracketed, like [INST]).
    "[THINK]",
    "[/THINK]",
    "[/TOOL_CALLS]",
    "[ARGS]",
    "[IMG]",
    "[/IMG]",
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
    "<|end_of_text|>",
    "<|start_header_id|>",
    "<|end_header_id|>",
    "<|header_start|>",
    "<|header_end|>",
    "<|eot|>",
    "<|step|>",
    "<|python_start|>",
    "<|python_end|>",
    "<|finetune_right_pad|>",
    "<|image_start|>",
    "<|image_end|>",
    "<|tile_x_separator|>",
    "<|tile_y_separator|>",
    "<|patch|>",
    "<|reasoning_thinking_start|>",
    "<|reasoning_thinking_end|>",
    "<|start|>",
    "<|return|>",
    "<|recipient|>",
    "<|content|>",
    "<|end|>",
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
    // 2026-10 second refresh. Everything here is `<|...|>` or fullwidth-pipe syntax —
    // serialization delimiters with no prose use — so it is safe to delete outright.
    "<|im_system|>",
    "<|im_user|>",
    "<|im_assistant|>",
    "<|im_tool|>",
    "<|media_begin|>",
    "<|media_content|>",
    "<|media_end|>",
    "<\u{FF5C}System\u{FF5C}>",
    "<\u{FF5C}Tool\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}calls\u{2581}begin\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}calls\u{2581}end\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}call\u{2581}begin\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}call\u{2581}end\u{FF5C}>",
    "<\u{FF5C}tool\u{2581}sep\u{FF5C}>",
    "<\u{FF5C}\u{2581}pad\u{2581}\u{FF5C}>",
    "<|begin_of_box|>",
    "<|end_of_box|>",
    "<|json_start|>",
    "<|json_end|>",
    "<|object_ref_start|>",
    "<|object_ref_end|>",
    "<|box_start|>",
    "<|box_end|>",
    "<|quad_start|>",
    "<|quad_end|>",
    "<|image>",
    "<|audio>",
    "<|video>",
    "<|thinking|>",
    "<|/thinking|>",
    "<|start_of_role|>",
    "<|end_of_role|>",
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
/// durable fingerprint even when the token name is not. Unknown shapes remain report-only at
/// every level: angle-bracket text can be intentional, and only reviewed token names are
/// safe to delete automatically.
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
    // 2026 additions: the Anthropic Messages API serializes document citations as
    // `<cite index="…">` around the quoted span, and the newer chat UIs expose a
    // `[citation:…]` envelope when a source pill is copied as text. Both are plumbing, not
    // the author's words, so they are reported from light.
    "<cite index=",
    "[citation:",
    // Share links that name the tool that produced the answer.
    "gemini.google.com/share/",
    "grok.com/share/",
    "chat.openai.com/share/",
    "copilot.microsoft.com/share/",
    "chat.deepseek.com/share/",
    "kimi.com/share/",
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

/// Report AI-provider attribution parameters in visible HTTP(S) URLs at every level.
pub struct AiTrackingUrlFlagRule;
impl Rule for AiTrackingUrlFlagRule {
    fn id(&self) -> &str {
        "markers.ai-tracking-urls"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        cleanup_core::url::find_ai_tracking_urls(&span.text)
            .into_iter()
            .map(|url| Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                message: "AI-provider attribution parameter in cited URL".to_string(),
                range: url.range,
                replacement: None,
            })
            .collect()
    }
}

/// Remove AI-provider attribution parameters from visible HTTP(S) URLs while preserving the
/// cited page, unrelated analytics parameters, and fragments. Structural destinations are
/// handled by the Markdown and DOCX adapters through the same core implementation.
pub struct AiTrackingUrlStripRule;

impl Rule for AiTrackingUrlStripRule {
    fn id(&self) -> &str {
        "markers.ai-tracking-urls.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        cleanup_core::url::find_ai_tracking_urls(&span.text)
            .into_iter()
            .map(|url| Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                message: "removed AI-provider attribution parameter from cited URL".to_string(),
                range: url.range,
                replacement: Some(url.replacement),
            })
            .collect()
    }
}

/// Bracket citation markers such as `【45†L75-L83】` where the enclosing CJK brackets and an
/// inner dagger/`source`/`web` word identify a ChatGPT research citation. Kept separate from
/// [`MarkerRule`] because the content between the brackets varies per citation.
pub struct CitationBracketRule;

impl CitationBracketRule {
    /// Longest bracket group we are willing to inspect. Real markers are short
    /// (`【4:0†source】`); anything longer is prose that happens to use CJK brackets.
    const MAX_INNER: usize = 512;
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

fn is_line_locator(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix('l') else {
        return false;
    };
    if let Some((start, end)) = rest.split_once("-l") {
        !start.is_empty()
            && !end.is_empty()
            && start.bytes().all(|b| b.is_ascii_digit())
            && end.bytes().all(|b| b.is_ascii_digit())
    } else {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
    }
}

fn is_turn_source_id(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    TurnCitationRule::parse_groups(&lower, 0) == Some(value.len())
}

/// Strict subset of bracket markers that is unambiguously machine-generated and can be
/// deleted at aggressive. The broader flag rule intentionally remains more permissive.
fn is_structured_citation_bracket(inner: &str) -> bool {
    // Dagger/line citations can carry a meaningful source reference after copy/paste. Keep
    // those report-only; only the explicitly machine-labelled pipe envelopes are disposable.
    if inner.contains('\u{2020}') {
        return false;
    }

    let lower = inner.to_ascii_lowercase();
    if !lower.contains('|') {
        return is_turn_source_id(&lower);
    }
    let parts: Vec<&str> = lower.split('|').collect();
    if !parts
        .first()
        .is_some_and(|family| matches!(*family, "cite" | "filecite"))
    {
        return false;
    }
    let mut source_ids = 0usize;
    for (index, part) in parts.iter().enumerate().skip(1) {
        if is_turn_source_id(part) {
            source_ids += 1;
        } else if index + 1 == parts.len() && is_line_locator(part) {
            // A single optional locator may follow the source IDs.
        } else {
            return false;
        }
    }
    source_ids > 0
}

/// Remove modern visible `【cite|…】` / `【filecite|…】` envelopes and bare turn IDs.
/// Unlike a literal replacement, this covers arbitrary turn/source numbers and line locators
/// while leaving meaningful dagger citations, ordinary CJK brackets, and `【url|anchor|…】`
/// content intact.
pub struct CitationBracketStripRule;

impl Rule for CitationBracketStripRule {
    fn id(&self) -> &str {
        "markers.citation-brackets.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(relative) = text[from..].find('\u{3010}') {
            let start = from + relative;
            let inner_start = start + '\u{3010}'.len_utf8();
            let Some(close_relative) = text[inner_start..].find('\u{3011}') else {
                break;
            };
            let close = inner_start + close_relative;
            let end = close + '\u{3011}'.len_utf8();
            let inner = &text[inner_start..close];
            if inner.chars().count() <= CitationBracketRule::MAX_INNER
                && is_structured_citation_bracket(inner)
            {
                out.push(Finding {
                    rule_id: self.id().to_string(),
                    span_index: idx,
                    range: start..end,
                    message: format!("removed ChatGPT citation marker {inner:?}"),
                    replacement: Some(String::new()),
                });
            }
            from = end;
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

    /// Validate the envelope before deleting it. Other applications may use the same Private
    /// Use code points, and non-citation families such as `entity` / `url` can contain visible
    /// anchor content that must not be discarded with the wrapper.
    fn is_strippable_marker(marker: &str) -> bool {
        if !marker.starts_with('\u{E200}') || !marker.ends_with('\u{E201}') {
            return false;
        }
        let inner_start = '\u{E200}'.len_utf8();
        let inner_end = marker.len() - '\u{E201}'.len_utf8();
        let parts: Vec<&str> = marker[inner_start..inner_end].split('\u{E202}').collect();
        if !parts
            .first()
            .is_some_and(|family| matches!(*family, "cite" | "filecite"))
        {
            return false;
        }
        let mut source_ids = 0usize;
        for (index, part) in parts.iter().enumerate().skip(1) {
            let valid_source = !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
            if !valid_source {
                return false;
            }
            if index + 1 == parts.len() && source_ids > 0 && is_line_locator(part) {
                continue;
            }
            source_ids += 1;
        }
        source_ids > 0
    }
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
            let complete = end > after_open;
            if self.strip && (!complete || !Self::is_strippable_marker(&text[start..end])) {
                from = end;
                continue;
            }
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
    /// Parse one or more `turn<digits><kind><digits>` groups (with an optional
    /// `L<digits>-L<digits>` line range after a group) starting at `pos` in the
    /// ASCII-lowercased text. Returns the end offset of the last complete group.
    ///
    /// OpenAI's citation grammar is `turn\d+\w+\d+`, not a closed list. In particular,
    /// current file/block/URL citations use `turn0file1`, `turn0block1`, and `turn0url1`.
    /// Restricting the kind to ASCII letters keeps that forward-compatible without accepting
    /// arbitrary punctuation or prose.
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
            let kind_start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i == kind_start || i - kind_start > 32 {
                break;
            }
            let mut j = i;
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
        if let Some(end) = end
            && end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
        {
            // Do not cut a citation-shaped prefix out of a longer ordinary identifier.
            return None;
        }
        end
    }

    fn enclosed(text: &str, start: usize, end: usize, open: char, close: char) -> bool {
        text[..start].rfind(open).is_some_and(|open_at| {
            text[open_at..]
                .find(close)
                .is_some_and(|relative| open_at + relative >= end)
        })
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
                    // Whole-envelope rules own complete PUA and CJK citation markers. In
                    // particular, do not nibble a turn ID out of an unknown `entity` / `url`
                    // payload that the validating envelope rule deliberately preserved.
                    if Self::enclosed(text, start, end, '\u{E200}', '\u{E201}')
                        || Self::enclosed(text, start, end, '\u{3010}', '\u{3011}')
                    {
                        from = end;
                        continue;
                    }
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

/// Remove the complete legacy browsing citation envelope, not just the word `oaicite`.
/// Handles `::contentReference[oaicite:0]{index=0}`, the same form without the leading
/// `::` / `contentReference`, and the `(oaicite:0){index=0}` copy variant.
pub struct LegacyCitationStripRule;

impl Rule for LegacyCitationStripRule {
    fn id(&self) -> &str {
        "markers.legacy-citations.strip"
    }
    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Aggressive
    }
    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let lower = text.to_ascii_lowercase();
        let bytes = lower.as_bytes();
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(relative) = lower[from..].find("oaicite:") {
            let name_start = from + relative;
            let digits_start = name_start + "oaicite:".len();
            let mut cursor = digits_start;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if cursor == digits_start || name_start == 0 {
                from = digits_start;
                continue;
            }
            let (open, close) = match bytes[name_start - 1] {
                b'[' => (name_start - 1, b']'),
                b'(' => (name_start - 1, b')'),
                _ => {
                    from = digits_start;
                    continue;
                }
            };
            if bytes.get(cursor) != Some(&close) {
                from = digits_start;
                continue;
            }
            cursor += 1;
            let mut start = open;
            if open >= "contentReference".len() && lower[..open].ends_with("contentreference") {
                start = open - "contentReference".len();
                if start >= 2 && &lower[start - 2..start] == "::" {
                    start -= 2;
                }
            }
            if lower[cursor..].starts_with("{index=") {
                let index_digits = cursor + "{index=".len();
                let mut end = index_digits;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > index_digits && bytes.get(end) == Some(&b'}') {
                    cursor = end + 1;
                } else {
                    // A malformed suffix is not a complete legacy envelope. Leave the whole
                    // construct untouched rather than creating a new orphaned `{index=…`.
                    from = cursor;
                    continue;
                }
            }
            out.push(Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                range: start..cursor,
                message: "removed legacy ChatGPT browsing citation envelope".to_string(),
                replacement: Some(String::new()),
            });
            from = cursor;
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Claude Code prompt steganography
// ---------------------------------------------------------------------------

/// Normalize the hidden fingerprint embedded by Claude Code 2.1.91–2.1.196 in its
/// `Today's date is YYYY-MM-DD.` system-prompt line. Those releases replaced the ASCII
/// apostrophe with U+2019, U+02BC or U+02B9 to encode proxy classifications, and used
/// `YYYY/MM/DD` to encode a timezone classification. The mechanism was rolled back in
/// 2.1.197, but captured prompts and agent transcripts still carry it.
///
/// This is deliberately an exact, line-anchored grammar rather than a global quote/date
/// rewrite. A clean ASCII line produces no finding; a marked line is restored to its
/// canonical ASCII form at every level, including Light. This is separate from the
/// statistical SynthID-Text watermark used by newer Claude models: that watermark adds
/// no characters and cannot be removed by this or any other Unicode cleanup rule.
pub struct ClaudePromptFingerprintRule;

impl Rule for ClaudePromptFingerprintRule {
    fn id(&self) -> &str {
        "markers.claude-prompt-fingerprint"
    }

    fn min_level(&self) -> CleanupLevel {
        CleanupLevel::Light
    }

    fn detect(&self, idx: usize, span: &ProseSpan, _cx: &Ctx) -> Vec<Finding> {
        let text = span.text.as_str();
        let bytes = text.as_bytes();
        let mut out = Vec::new();

        for (start, _) in text.match_indices("Today") {
            // The fingerprint is a standalone system-prompt line. Requiring a line boundary
            // avoids reclassifying an ordinary sentence that merely contains the same words.
            if start > 0 && !matches!(bytes[start - 1], b'\n' | b'\r') {
                continue;
            }

            let apostrophe_at = start + "Today".len();
            let Some(apostrophe) = text[apostrophe_at..].chars().next() else {
                continue;
            };
            if !matches!(apostrophe, '\'' | '\u{2019}' | '\u{02BC}' | '\u{02B9}') {
                continue;
            }

            let suffix_at = apostrophe_at + apostrophe.len_utf8();
            const SUFFIX: &str = "s date is ";
            if !text[suffix_at..].starts_with(SUFFIX) {
                continue;
            }
            let date_at = suffix_at + SUFFIX.len();
            if bytes.len() < date_at + 11 {
                continue;
            }

            let date = &bytes[date_at..date_at + 10];
            let separator = date[4];
            if !matches!(separator, b'-' | b'/') || date[7] != separator {
                continue;
            }
            let digits_are_ascii = date
                .iter()
                .enumerate()
                .all(|(offset, byte)| matches!(offset, 4 | 7) || byte.is_ascii_digit());
            if !digits_are_ascii || bytes[date_at + 10] != b'.' {
                continue;
            }

            let end = date_at + 11;
            if end < bytes.len() && !matches!(bytes[end], b'\n' | b'\r') {
                continue;
            }
            if apostrophe == '\'' && separator == b'-' {
                continue; // Canonical line: no hidden signal to remove.
            }

            let mut replacement = String::with_capacity(end - start);
            replacement.push_str("Today's date is ");
            for (offset, byte) in date.iter().enumerate() {
                replacement.push(if matches!(offset, 4 | 7) {
                    '-'
                } else {
                    char::from(*byte)
                });
            }
            replacement.push('.');

            out.push(Finding {
                rule_id: self.id().to_string(),
                span_index: idx,
                range: start..end,
                message: format!(
                    "normalized Claude Code hidden prompt fingerprint (apostrophe U+{:04X})",
                    apostrophe as u32
                ),
                replacement: Some(replacement),
            });
        }
        out
    }
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
    // 2026 agent crop. Each string is a product-specific attribution that a human has no
    // reason to type into a document; the generic `Co-Authored-By:` shape is handled by
    // `markers.agent-trailer-lines`, so only the distinctive spellings live here.
    "noreply@ampcode.com",
    "ampcode.com",
    "generated by amp",
    "generated by kiro",
    "kiro.dev",
    "google antigravity",
    "antigravity agent",
    "warp.dev/agent",
    "warp agent",
    "factory.ai",
    "factory droid",
    "openhands",
    "jetbrains junie",
    "junie agent",
    "qoder",
    "codebuddy",
    "trae agent",
    "made with manus",
    "built with manus",
    "genspark agent",
    "base44",
    "same.new",
    "firebase studio",
    "opencode.ai",
    "co-authored-by: codex",
    "co-authored-by: gemini",
    "co-authored-by: amp",
    "co-authored-by: kiro",
    "codex-session:",
    "gemini-session:",
    "made with claude",
    "built with claude",
    "generated with lovable",
    "powered by gemini",
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
        "ampcode",
        "kiro",
        "antigravity",
        "openhands",
        "qoder",
        "codebuddy",
        "junie",
        "manus",
        "genspark",
        "factory.ai",
        "warp.dev",
        "opencode",
        "base44",
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
    // 2026 tool-call framing: Kimi/Qwen section envelopes, DeepSeek call markers, the
    // Anthropic XML shape as community clients serialize it, and the JSON keys that leak
    // when a raw API payload is pasted.
    "<|tool_calls_section_begin|>",
    "<|tool_calls_section_end|>",
    "<|tool_call_begin|>",
    "<|tool_call_argument_begin|>",
    "<|tool_call_end|>",
    "<tool_response>",
    "</tool_response>",
    "<tool_result>",
    "</tool_result>",
    "<function=",
    "<parameter>",
    "</parameter>",
    "recipient=functions.",
    "\"tool_use_id\":",
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
    // 2026 model names, vendors and codenames as they appear in pasted chat output.
    "gpt-5.5",
    "gpt-5.6",
    "gpt-6 astra",
    "claude opus 5",
    "claude opus",
    "claude sonnet 5",
    "claude haiku 4.5",
    "fable 5",
    "mythos 5",
    "gemini 3",
    "grok 5",
    "kimi k2.5",
    "kimi-k2.5",
    "glm 5",
    "qwen3.8",
    "qwen 3.8",
    "mistral large",
    "llama 4.5",
    "llama-4.5",
    "amazon nova",
    "nova pro",
    "i am llama",
    "i'm llama",
    "i am mistral",
    "i'm mistral",
    "as an ai model",
    "as a helpful assistant",
    "my last knowledge update",
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
        // 2026 reasoning surfaces: channel-scoped thinking tokens, the progress lines a
        // reasoning UI prints while it works, and the giveaway second-person framing of a
        // leaked planning trace ("The user wants…").
        "<|thinking|>",
        "<|/thinking|>",
        "<|reasoning|>",
        "<|/reasoning|>",
        "the user wants",
        "the user asks",
        "the user is asking",
        "the user's request",
        "let me reconsider",
        "let me double-check",
        "let me verify",
        "internal monologue",
        "chain-of-thought",
        "here's my plan",
        "here is my plan",
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
        "<|thinking|>",
        "<|reasoning|>",
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
        "<|/thinking|>",
        "<|/reasoning|>",
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

// ---------------------------------------------------------------------------
// Chat-UI chrome
// ---------------------------------------------------------------------------

/// The disclaimer every assistant UI prints beneath an answer. It is interface furniture,
/// not part of the document: it survives "copy" into Word, Markdown and note apps, and it
/// names the product outright. Reported from light; removed at aggressive.
const UI_DISCLAIMERS: &[&str] = &[
    "ChatGPT can make mistakes. Check important info.",
    "ChatGPT can make mistakes. Check important info. See Cookie Preferences.",
    "Claude can make mistakes. Please double-check responses.",
    "Claude can make mistakes. Please double-check responses. Open in Claude.",
    "Gemini can make mistakes, so double-check it.",
    "Gemini can make mistakes. Check important info.",
    "Copilot uses AI. Check for mistakes.",
    "Perplexity can make mistakes. Check important info.",
    "Grok can make mistakes. Check important info.",
    "DeepSeek can make mistakes. Check important info.",
    "Meta AI can make mistakes.",
    "Le Chat can make mistakes. Check important info.",
];

/// Interface labels that only make sense inside a chat window: role headers, buttons and
/// progress lines. Flag-only — unlike the disclaimer, a document may legitimately quote a
/// transcript or describe a UI.
const UI_RESIDUE: &[&str] = &[
    "you said:",
    "chatgpt said:",
    "claude responded:",
    "gemini said:",
    "copilot said:",
    "grok said:",
    "perplexity said:",
    "deepseek said:",
    "meta ai said:",
    "copy code",
    "regenerate response",
    "read aloud",
    "edit message",
    "share conversation",
    "searching the web",
    "reading sources",
    "analyzing sources",
    "browsing the web",
    "check important info",
    "can make mistakes",
    "open in claude",
    "open in chatgpt",
    "start a new chat",
    "temporary chat",
];

/// Flag the assistant-UI disclaimer line for review.
pub fn chat_ui_disclaimers() -> MarkerRule {
    MarkerRule::new(
        "markers.chat-ui-disclaimer",
        CleanupLevel::Light,
        "assistant-UI disclaimer line (interface furniture, not document content)",
        false,
        UI_DISCLAIMERS,
    )
}

/// Remove the assistant-UI disclaimer line outright.
pub fn chat_ui_disclaimers_strip() -> MarkerRule {
    MarkerRule::new(
        "markers.chat-ui-disclaimer.strip",
        CleanupLevel::Aggressive,
        "removed assistant-UI disclaimer line",
        true,
        UI_DISCLAIMERS,
    )
}

/// Flag chat-window labels and progress lines that were copied along with the answer.
pub fn chat_ui_residue() -> MarkerRule {
    MarkerRule::new(
        "markers.chat-ui-residue",
        CleanupLevel::Standard,
        "chat-window label or progress line (copied UI chrome)",
        false,
        UI_RESIDUE,
    )
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
    fn claude_prompt_fingerprint_normalizes_every_encoded_variant() {
        for apostrophe in ['\u{2019}', '\u{02BC}', '\u{02B9}'] {
            for separator in ['-', '/'] {
                let line = format!("Today{apostrophe}s date is 2026{separator}06{separator}30.");
                let text = format!("context before\n{line}\ncontext after");
                let found = detect(&ClaudePromptFingerprintRule, &text);
                assert_eq!(found.len(), 1, "text {text:?}: {found:?}");
                assert_eq!(
                    found[0].replacement.as_deref(),
                    Some("Today's date is 2026-06-30.")
                );
                assert_eq!(&text[found[0].range.clone()], line);
            }
        }

        // The timezone bit can be present even when the apostrophe is canonical.
        let found = detect(&ClaudePromptFingerprintRule, "Today's date is 2026/06/30.");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].replacement.as_deref(),
            Some("Today's date is 2026-06-30.")
        );
    }

    #[test]
    fn claude_prompt_fingerprint_ignores_clean_lines_and_ordinary_prose() {
        for text in [
            "Today's date is 2026-06-30.",
            "Today’s date is June 30, 2026.",
            "See Today’s date is 2026/06/30. for the original prompt.",
            "Today’s date is 2026/06/30. trailing prose",
            "Today’s date is not really a date.",
        ] {
            assert!(
                detect(&ClaudePromptFingerprintRule, text).is_empty(),
                "unexpected fingerprint in {text:?}"
            );
        }
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
    fn covers_2026_token_vocabularies() {
        let text = "<|im_user|>q<|im_assistant|>a <|tool_call_begin|>x<|tool_call_end|> \
                    <|begin_of_box|>y<|end_of_box|> <\u{FF5C}tool\u{2581}sep\u{FF5C}> [THINK]hmm[/THINK]";
        let flags = detect(&chat_tokens(), text);
        assert!(flags.len() >= 6, "got {flags:?}");
        for token in [
            "<|im_user|>",
            "<|begin_of_box|>",
            "<|json_start|>",
            "<|video>",
        ] {
            let strips = detect(&chat_tokens_strip(), token);
            assert_eq!(strips.len(), 1, "not stripped: {token}");
            assert_eq!(strips[0].replacement.as_deref(), Some(""));
        }
    }

    #[test]
    fn chat_ui_disclaimers_flag_then_strip() {
        let text = "Answer text.\n\nChatGPT can make mistakes. Check important info.\n";
        let flags = detect(&chat_ui_disclaimers(), text);
        assert_eq!(flags.len(), 1, "{flags:?}");
        assert!(flags[0].replacement.is_none());
        let strips = detect(&chat_ui_disclaimers_strip(), text);
        assert_eq!(strips.len(), 1);
        assert_eq!(strips[0].replacement.as_deref(), Some(""));
    }

    #[test]
    fn chat_ui_residue_is_flagged_only() {
        let text = "You said: hello\nChatGPT said: hi";
        let found = detect(&chat_ui_residue(), text);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|f| f.replacement.is_none()));
    }

    #[test]
    fn reasoning_trace_covers_planning_framing() {
        let text = "The user wants a summary, so let me reconsider the structure.";
        let found = detect(&ReasoningTraceRule, text);
        assert!(found.len() >= 2, "{found:?}");
    }

    #[test]
    fn citations_cover_anthropic_cite_tags() {
        let found = detect(
            &citation_artifacts(),
            "as stated <cite index=\"1-2\">there</cite>",
        );
        assert_eq!(found.len(), 1, "{found:?}");
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

    #[test]
    fn turn_citations_cover_current_block_and_url_sources() {
        for text in [
            "Source turn0block1 here.",
            "Source turn12url4 here.",
            "Source fileciteturn3file2L8-L13 here.",
        ] {
            let found = detect(&turn_citations(), text);
            assert_eq!(found.len(), 1, "{text:?}: {found:?}");
            assert_eq!(found[0].range.start, 7);
        }
        assert!(detect(&turn_citations(), "A turn0search0suffix stays text.").is_empty());
    }

    #[test]
    fn visible_citation_envelopes_strip_only_machine_labelled_forms() {
        let modern = "Claim【cite|turn0search1|turn2url0】 and【filecite|turn0file1|L8-L13】.";
        let found = detect(&CitationBracketStripRule, modern);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|f| f.replacement.as_deref() == Some("")));

        let preserved = "Keep【12†L20-L24】 and【url|anchor|turn0url1】 and【note|turn0url1】.";
        assert!(detect(&CitationBracketStripRule, preserved).is_empty());
        assert_eq!(detect(&CitationBracketRule, "【12†L20-L24】").len(), 1);
    }

    #[test]
    fn pua_strip_validates_family_and_payload() {
        let unknown = "\u{E200}entity\u{E202}turn0search0\u{E201}";
        assert_eq!(detect(&private_use_citations(), unknown).len(), 1);
        assert!(detect(&private_use_citations_strip(), unknown).is_empty());
        let malformed = "\u{E200}cite\u{E202}not a source\u{E201}";
        assert!(detect(&private_use_citations_strip(), malformed).is_empty());
        let locator = "\u{E200}filecite\u{E202}turn0file1\u{E202}L8-L13\u{E201}";
        assert_eq!(detect(&private_use_citations_strip(), locator).len(), 1);
    }

    #[test]
    fn legacy_citation_envelopes_are_removed_whole() {
        for text in [
            "before ::contentReference[oaicite:12]{index=12} after",
            "before (oaicite:3){index=3} after",
            "before contentReference[oaicite:0] after",
        ] {
            let found = detect(&LegacyCitationStripRule, text);
            assert_eq!(found.len(), 1, "{text:?}: {found:?}");
            assert!(text[found[0].range.clone()].contains("oaicite:"));
            assert!(!text[found[0].range.clone()].contains(" after"));
        }
        assert!(detect(&LegacyCitationStripRule, "ordinary oaicite: text").is_empty());
        assert!(detect(&LegacyCitationStripRule, "(oaicite:1){index=broken}").is_empty());
        assert!(detect(&LegacyCitationStripRule, "(oaicite:1){index=1").is_empty());
    }

    #[test]
    fn unknown_special_tokens_stay_report_only_at_aggressive() {
        let text = "a<|future_token_9|>b";
        let found = detect(&SpecialTokenPatternRule, text);
        assert_eq!(found.len(), 1);
        assert!(found[0].replacement.is_none());
    }

    #[test]
    fn ai_url_rule_preserves_unrelated_query_parameters() {
        let text = "See https://example.test/?a=1&utm_source=chatgpt.com&b=2#part.";
        let found = detect(&AiTrackingUrlStripRule, text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].replacement.as_deref(),
            Some("https://example.test/?a=1&b=2#part")
        );
        assert!(
            detect(
                &AiTrackingUrlStripRule,
                "https://example.test/?utm_source=newsletter"
            )
            .is_empty()
        );
    }
}
