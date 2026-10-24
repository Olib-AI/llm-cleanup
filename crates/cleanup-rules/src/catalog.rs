//! The signature catalogue: which models we know about, what they leave behind, and which
//! rule catches it.
//!
//! This is the "coverage" record of the project — the answer to *"is my model covered?"*
//! It is data, not heuristics, so it can be shown to a user (`aiclean coverage`), diffed
//! between releases, and audited for staleness. Every claim here is grounded in provider
//! documentation, published research, or observed output; the review date is
//! [`CATALOG_REVIEWED`].
//!
//! Two things are deliberately **not** in this table, because no character- or phrase-level
//! tool can touch them:
//!
//! * statistical token watermarks (Google **SynthID**; Anthropic's watermark for models
//!   released on/after 2026-08-02; OpenAI **textGrain**, EU rollout announced 2026-10-05);
//! * file-level provenance (C2PA / Content Credentials) attached to exported images, audio
//!   and some office documents.
//!
//! Both are named in [`OUT_OF_SCOPE`] so users are not misled about what cleaning does.

/// When this catalogue was last reviewed against the vendor landscape.
pub const CATALOG_REVIEWED: &str = "2026-10-24";

/// One model family and the fingerprints we attribute to it.
#[derive(Debug, Clone, Copy)]
pub struct SignatureFamily {
    /// Stable identifier used by `aiclean coverage --provider <id>`.
    pub id: &'static str,
    /// Vendor / product.
    pub provider: &'static str,
    /// Models this row was reviewed against.
    pub models: &'static str,
    /// The residue we look for, in one sentence.
    pub signatures: &'static str,
    /// Comma-separated rule ids that implement it.
    pub rules: &'static str,
}

/// Fingerprints that are out of reach for deterministic character cleanup, with the reason.
#[derive(Debug, Clone, Copy)]
pub struct OutOfScope {
    pub technique: &'static str,
    pub owner: &'static str,
    pub why: &'static str,
}

pub const OUT_OF_SCOPE: &[OutOfScope] = &[
    OutOfScope {
        technique: "SynthID-Text (statistical token watermark)",
        owner: "Google Gemini (since 2024), and the basis of Anthropic's 2026 watermark",
        why: "Lives in the probability distribution over word choices, not in characters. \
              Copying, pasting and re-typing keep it; only a substantial rewrite degrades it.",
    },
    OutOfScope {
        technique: "textGrain (statistical token watermark)",
        owner: "OpenAI ChatGPT / Codex, EU rollout announced 2026-10-05",
        why: "Same as above: no hidden characters, so a character cleaner cannot remove it.",
    },
    OutOfScope {
        technique: "C2PA / Content Credentials",
        owner: "Claude file exports, OpenAI images and audio, Google media",
        why: "Cryptographically signed file metadata. Stripping it is a container-level \
              operation and a separate feature request (it is not prose).",
    },
];

const CATALOG: &[SignatureFamily] = &[
    SignatureFamily {
        id: "openai",
        provider: "OpenAI — ChatGPT, Codex, API",
        models: "GPT-5.5, GPT-5.6 Sol, GPT-6 Astra, o3/o4-mini",
        signatures: "U+202F narrow no-break space and stray invisibles from long answers; \
                     chat-UI citation plumbing (【9†source】, 【45†L75-L83】, turn0search, \
                     oai_citation); gpt-oss harmony control tokens; corrective framing \
                     (\"rather than\", \"not simply\")",
        rules: "typo.invisibles.strip, unicode.spaces.normalize, markers.citation-artifacts, \
                markers.citation-brackets, markers.chat-tokens, markers.model-identifiers, \
                phrasal.negative-parallelism",
    },
    SignatureFamily {
        id: "anthropic",
        provider: "Anthropic — Claude",
        models: "Opus 5 / 5.5, Sonnet 5, Haiku 4.5, Fable 5.1, Mythos 5.1",
        signatures: "Em and en dashes (heavily reduced from Opus 5.5 onward, still present in \
                     older models); superlative framing (\"the single most\", \"dependable\") \
                     measured at 100x+ human rate; extended-thinking markup; Claude Code \
                     commit and PR trailers; a statistical watermark on models released \
                     on/after 2026-08-02 (out of scope)",
        rules: "typo.em-dash.to-comma, typo.en-dash.to-hyphen, markers.reasoning-block, \
                markers.reasoning-trace, markers.agent-trailers, markers.model-identifiers, \
                phrasal.claude-residue, phrasal.superlative-framing",
    },
    SignatureFamily {
        id: "google",
        provider: "Google — Gemini, AI Overviews, Workspace",
        models: "Gemini 3 Pro / 3.1 Pro / Flash",
        signatures: "Zero-width and non-breaking spaces picked up from the Docs/Keep \
                     clipboard, footnote-style citation markers, formal transitions \
                     (\"furthermore\", \"moreover\"); SynthID watermark (out of scope)",
        rules: "typo.invisibles.strip, unicode.spaces.normalize, \
                phrasal.gemini-affirmations, phrasal.formal-transitions",
    },
    SignatureFamily {
        id: "meta",
        provider: "Meta — Llama, Meta AI",
        models: "Llama 4 family, Meta AI",
        signatures: "Llama 3/4 chat-template tokens, first-person \"I'm Meta AI\" identity, \
                     assistant disclaimers",
        rules: "markers.chat-tokens, markers.chat-tokens.strip, markers.model-identifiers, \
                phrasal.model-self-reference",
    },
    SignatureFamily {
        id: "xai",
        provider: "xAI — Grok",
        models: "Grok 4, Grok 5",
        signatures: "Inline render directives (<grok:render), self-identification, emoji-heavy \
                     bullet formatting",
        rules: "markers.citation-artifacts, markers.model-identifiers, style.emoji.opener, \
                style.emoji.density",
    },
    SignatureFamily {
        id: "mistral",
        provider: "Mistral AI — Le Chat, Mistral Large",
        models: "Mistral Large 3, Le Chat",
        signatures: "Mistral/community [INST] and <<SYS>> instruction markers, product-name \
                     leakage (\"Le Chat\")",
        rules: "markers.chat-tokens, markers.model-identifiers",
    },
    SignatureFamily {
        id: "deepseek",
        provider: "DeepSeek",
        models: "DeepSeek V4 Flash / V4 Pro / V4.1 Flash, V3.x, R1",
        signatures: "Fullwidth sentence-final punctuation inside English text; reasoning \
                     traces that leak as visible \"thinking\"; DeepSeek sentence tokens \
                     (<|begin▁of▁sentence|>, ｜end▁of▁sentence｜)",
        rules: "typo.fullwidth.normalize, markers.chat-tokens, markers.chat-tokens.strip, \
                markers.reasoning-trace, markers.reasoning-block, \
                markers.model-identifiers",
    },
    SignatureFamily {
        id: "alibaba",
        provider: "Alibaba — Qwen",
        models: "Qwen3.8-Max, Qwen3.8-27B, Qwen Code",
        signatures: "Fullwidth parentheses/colons, chat-template tokens, always-on thinking \
                     output that can be pasted into the document",
        rules: "typo.fullwidth.normalize, markers.chat-tokens, markers.agent-trailers, \
                markers.model-identifiers",
    },
    SignatureFamily {
        id: "moonshot",
        provider: "Moonshot AI — Kimi",
        models: "Kimi K2.5, K2.6, K3",
        signatures: "Sentence tokens with fullwidth pipes, always-on reasoning dumps, \
                     self-identification",
        rules: "markers.chat-tokens, markers.reasoning-block, markers.model-identifiers",
    },
    SignatureFamily {
        id: "zhipu",
        provider: "Zhipu AI — GLM",
        models: "GLM-5, GLM-5.1",
        signatures: "Fullwidth punctuation, chat-template tokens, reasoning traces",
        rules: "typo.fullwidth.normalize, markers.chat-tokens, markers.reasoning-trace",
    },
    SignatureFamily {
        id: "microsoft",
        provider: "Microsoft — Copilot (Word, Edge, VS Code)",
        models: "Copilot (GPT-5.6 family under the hood)",
        signatures: "Assistant disclaimers kept in the document, Copilot attribution, \
                     Office metadata naming the assistant as author (metadata is out of \
                     scope), raw Markdown pasted into Word",
        rules: "phrasal.machine-residue, phrasal.sycophancy, markers.agent-trailers, \
                structural.markdown-tells, style.markdown.residue",
    },
    SignatureFamily {
        id: "perplexity",
        provider: "Perplexity — Sonar, Comet",
        models: "Sonar, Sonar Pro",
        signatures: "Dense per-claim numbered citations copied without their links, \
                     answer-engine signposting",
        rules: "markers.citation-artifacts, phrasal.formal-transitions",
    },
    SignatureFamily {
        id: "amazon",
        provider: "Amazon — Nova",
        models: "Nova Pro / Lite / Micro",
        signatures: "Self-identification, assistant disclaimers",
        rules: "markers.model-identifiers, phrasal.machine-residue",
    },
    SignatureFamily {
        id: "agentic-coding",
        provider: "Agentic coding tools — Claude Code, Codex CLI, Cursor, Copilot, Devin, \
                    Jules, Aider, Windsurf, Gemini CLI",
        models: "2026 releases",
        signatures: "Attribution trailers in commits, PR bodies and changelogs \
                     (Co-Authored-By: Claude, 🤖 Generated with [Claude Code], \
                     Claude-Session:), decorative emoji and raw Markdown in generated docs",
        rules: "markers.agent-trailers, style.emoji.opener, style.emoji.density, \
                style.markdown.residue, structural.markdown-tells",
    },
    SignatureFamily {
        id: "unicode",
        provider: "All providers — invisible-character vectors",
        models: "Any model, any version (character-level smuggling)",
        signatures: "Unicode tag characters, variation-selector runs, filler glyphs \
                     (U+3164, U+FFA0, U+2800), bidi overrides (Trojan Source), mixed-script \
                     homoglyphs",
        rules: "typo.invisibles.strip, unicode.smuggled.strip, unicode.fillers.strip, \
                unicode.spaces.normalize, unicode.bidi.strip-overrides, unicode.bidi.flag, \
                unicode.confusables.flag",
    },
    SignatureFamily {
        id: "typography",
        provider: "All providers — punctuation and typography",
        models: "Any model, any version (office-style smart punctuation)",
        signatures: "Curly quotes and guillemets, ellipsis, em/en dashes, hyphen family, \
                     fullwidth ASCII forms, double spaces around punctuation",
        rules: "typo.quotes.straighten, typo.ellipsis.expand, typo.em-dash.to-comma, \
                typo.en-dash.to-hyphen, typo.hyphens.normalize, typo.fullwidth.normalize",
    },
];

/// Every model family in the catalogue, newest review first (see [`CATALOG_REVIEWED`]).
pub fn coverage() -> &'static [SignatureFamily] {
    CATALOG
}
