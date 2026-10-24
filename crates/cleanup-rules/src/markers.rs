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
        let matcher = AhoCorasick::builder()
            .ascii_case_insensitive(true)
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
///   `<|eot_id|>`, `<|python_tag|>`
/// * Qwen / DeepSeek / GLM: `<|im_end|>`, `<|endoftext|>`, `<|▁begin▁of▁sentence|>`,
///   `｜end▁of▁sentence｜` (fullwidth pipe variant)
/// * Mistral / community instruction formats: `[INST]`, `[/INST]`, `<<SYS>>`, `<</SYS>>`
/// * Anthropic extended-thinking markup: `<antThinking>`, `</antThinking>`, `antml:`
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

// ---------------------------------------------------------------------------
// Chat-UI citation artifacts
// ---------------------------------------------------------------------------

/// Citation plumbing the chat UIs leave behind when you copy an answer.
///
/// * ChatGPT web search / deep research: `【9†source】`, `【45†L75-L83】`, and the
///   `oai_citation:` / `turn0search0` / `turn0file0` marker families that the copy handler
///   leaks in some locales and clients.
/// * Legacy `contentReference[oaicite:0]` markers from GPT-4 browsing.
/// * Shared-link tracking parameters appended by the copy button (`utm_source=chatgpt.com`).
/// * Claude / Grok inline render directives: `<grok:render`, `citeturn`.
const CITATION_MARKERS: &[&str] = &[
    "\u{2020}source\u{3011}",
    "\u{2020}L",
    "oai_citation",
    "contentReference",
    "oaicite",
    "citeturn0",
    "turn0search",
    "turn0file",
    "turn0news",
    "utm_source=chatgpt.com",
    "utm_source=openai",
    "utm_medium=chatgpt",
    "grok:render",
    "smart_search_thinking",
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
            let looks_like_citation = inner.contains('\u{2020}')
                || inner.contains("source")
                || inner.contains("web")
                || inner.to_ascii_lowercase().contains("turn");
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
    "qwen3.8",
    "qwen3-max",
    "kimi-k3",
    "kimi-k2",
    "glm-5",
    "sonar-pro",
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
    const OPENERS: &[&str] = &["<think", "<thinking>", "```thinking", "```think"];
    const CLOSERS: &[&str] = &["</think", "</thinking>", "```"];

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
}
