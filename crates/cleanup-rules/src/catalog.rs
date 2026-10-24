//! The signature catalogue: which models we know about, what they leave behind, and which
//! rule catches it.
//!
//! This is the "coverage" record of the project — the answer to *"is my model covered?"*
//! It is data, not heuristics, so it can be shown to a user (`aiclean coverage`), diffed
//! between releases, and audited for staleness. Every claim here is grounded in provider
//! documentation, published research, or observed output; the review date is
//! [`CATALOG_REVIEWED`].
//!
//! Three hard cryptographic or physical limits remain in [`OUT_OF_SCOPE`] so users are
//! never misled about what deterministic local processing can certify:
//!
//! * **Keyed statistical watermark certification** (`SynthID-Text`, `textGrain`): opting into
//!   `--dewatermark` disrupts consecutive 5-gram tournament windows via truth-preserving
//!   canonicalizations, but without the vendor's secret key no offline tool can verify
//!   complete watermark removal.
//! * **Pixel- and audio-domain imperceptible watermarks** (`SynthID-Image`, `SynthID-Audio`,
//!   `Tree-Ring`): lossless C2PA/XMP chunk stripping preserves bit-exact pixels and therefore
//!   leaves pixel-embedded signals untouched.
//! * **External soft-binding registries**: cloud perceptual-hash databases remain able to
//!   match an asset after embedded C2PA manifests are stripped.

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
        technique: "Keyed statistical watermark certification (SynthID-Text, textGrain)",
        owner: "Google Gemini, Anthropic Claude (models from 2026-08-02+), OpenAI ChatGPT / Codex",
        why: "Opt-in `--dewatermark` deterministically disrupts overlapping 5-gram token \
              tournament windows, degrading windowed detection scores, but without the \
              vendor's secret key no offline tool can verify or certify zero residual signal.",
    },
    OutOfScope {
        technique: "Pixel- and audio-domain imperceptible watermarks (SynthID-Image / Audio)",
        owner: "Google Imagen / Veo / Lyria, Meta AudioSeal, diffusion latent watermarks",
        why: "Lives in pixel DCT/wavelet coefficients or audio waveforms rather than metadata \
              chunks. Because `aiclean` strips C2PA/XMP chunks losslessly without re-encoding \
              pixels, imperceptible media watermarks remain intact.",
    },
    OutOfScope {
        technique: "External C2PA soft-binding / perceptual-hash registries",
        owner: "Cloud provenance lookup services",
        why: "Embedded C2PA manifests (`caBX`, `APP11`, `C2PA`, `META-INF/*.c2pa`, text \
              wrappers) are removed locally, but an external registry that previously indexed \
              a perceptual hash of the file can still match it out-of-band.",
    },
];

const CATALOG: &[SignatureFamily] = &[
    SignatureFamily {
        id: "openai",
        provider: "OpenAI — ChatGPT, Codex, API",
        models: "GPT-5.5, GPT-5.6 Sol, GPT-6 Astra, o3/o4-mini",
        signatures: "U+202F narrow no-break space and stray invisibles from long answers; \
                     chat-UI citation plumbing in all its forms — bracket citations \
                     (【9†source】, 【45†L75-L83】), the Private-Use-Area wrappers the UI \
                     actually stores (U+E200…U+E201/U+E202, incl. cited-span markers \
                     U+E203/U+E204/U+E206), visible cite/filecite envelopes, bare \
                     turn0search0/turn0block0/turn0url0-style IDs at any turn/source number, \
                     oai_citation and legacy oaicite envelopes, and AI-provider tracking \
                     parameters left on cited links; gpt-oss harmony control tokens; leaked \
                     tool-call/function scaffolding; corrective framing (\"rather than\", \
                     \"not simply\")",
        rules: "typo.invisibles.strip, unicode.spaces.normalize, markers.citation-artifacts, \
                markers.citation-brackets, markers.private-use-citations, \
                markers.private-use-citations.strip, markers.turn-citations, \
                markers.turn-citations.strip, markers.citation-brackets.strip, \
                markers.legacy-citations.strip, markers.ai-tracking-urls, \
                markers.ai-tracking-urls.strip, markers.chat-tokens, \
                markers.chat-tokens.pattern, markers.chat-tokens.strip, markers.tool-calls, \
                markers.model-identifiers, phrasal.negative-parallelism",
    },
    SignatureFamily {
        id: "anthropic",
        provider: "Anthropic — Claude",
        models: "Opus 5 / 5.5, Sonnet 5, Haiku 4.5, Fable 5.1, Mythos 5.1",
        signatures: "Em and en dashes (heavily reduced from Opus 5.5 onward, still present in \
                     older models); superlative framing (\"the single most\", \"dependable\") \
                     measured at 100x+ human rate; extended-thinking markup and leaked \
                     tool-use scaffolding (<invoke>, <function_calls>); Claude Code commit \
                     and PR trailers, incl. versioned Co-Authored-By lines for unreleased \
                     model names (matched by pattern); Claude Code 2.1.91–2.1.196's hidden \
                     prompt-date fingerprint (U+2019/U+02BC/U+02B9 apostrophes and slash-separated \
                     dates), restored to canonical ASCII at light; a statistical watermark on \
                     models released on/after 2026-08-02 (out of scope)",
        rules: "typo.em-dash.to-comma, typo.en-dash.to-hyphen, markers.reasoning-block, \
                markers.reasoning-trace, markers.agent-trailers, markers.agent-trailer-lines, \
                markers.claude-prompt-fingerprint, markers.tool-calls, \
                markers.model-identifiers, phrasal.claude-residue, \
                phrasal.superlative-framing",
    },
    SignatureFamily {
        id: "google",
        provider: "Google — Gemini, NotebookLM, AI Studio, Workspace",
        models: "Gemini 3 Pro / 3.1 Pro / Flash, NotebookLM",
        signatures: "Zero-width and non-breaking spaces picked up from the Docs/Keep \
                     clipboard; Gemini / AI Studio / NotebookLM grounding citation markers \
                     ([cite_start], [cite_end], [cite: 1, 2]); formal transitions \
                     (\"furthermore\", \"moreover\"); Gemma chat tokens (<start_of_turn>, \
                     <|turn|>, <|think|>, <|channel|>thought) in self-hosted output; \
                     SynthID-Text watermark (opt-in disruption via --dewatermark)",
        rules: "typo.invisibles.strip, unicode.spaces.normalize, markers.gemini-citations, \
                markers.gemini-citations.strip, markers.chat-tokens, \
                markers.chat-tokens.pattern, phrasal.gemini-affirmations, \
                phrasal.formal-transitions",
    },
    SignatureFamily {
        id: "meta",
        provider: "Meta — Llama, Meta AI",
        models: "Llama 4 family, Meta AI",
        signatures: "Llama 3/4 chat-template tokens, including Llama 4 header/eot and \
                     reasoning-thinking serialization, first-person \"I'm Meta AI\" identity, \
                     assistant disclaimers",
        rules: "markers.chat-tokens, markers.chat-tokens.strip, markers.chat-tokens.pattern, \
                markers.model-identifiers, phrasal.model-self-reference",
    },
    SignatureFamily {
        id: "xai",
        provider: "xAI — Grok",
        models: "Grok 4, Grok 5",
        signatures: "Inline render directives (<grok:render), self-identification, emoji-heavy \
                     bullet formatting; measured overuse of causal/empirical connectives in \
                     AI-generated Wikipedia text (\"empirically\", \"correlate\", \"causally\")",
        rules: "markers.citation-artifacts, markers.model-identifiers, style.emoji.opener, \
                style.emoji.density, lexical.grok-tics",
    },
    SignatureFamily {
        id: "mistral",
        provider: "Mistral AI — Le Chat, Mistral Large",
        models: "Mistral Large 3, Le Chat",
        signatures: "Mistral/community [INST] and <<SYS>> instruction markers, V7 template \
                     sections ([SYSTEM_PROMPT], [AVAILABLE_TOOLS], [TOOL_CALLS]), product-name \
                     leakage (\"Le Chat\")",
        rules: "markers.chat-tokens, markers.tool-calls, markers.model-identifiers",
    },
    SignatureFamily {
        id: "deepseek",
        provider: "DeepSeek",
        models: "DeepSeek V4 Flash / V4 Pro / V4.1 Flash, V3.x, R1",
        signatures: "Fullwidth and CJK punctuation inside English text (、。：；？！), \
                     visible detokenizer space markers (Ġ, ▁) from tokenizer dumps; \
                     reasoning traces that leak as visible \"thinking\"; DeepSeek sentence \
                     tokens (<|begin▁of▁sentence|>, ｜end▁of▁sentence｜)",
        rules: "typo.fullwidth.normalize, typo.cjk-punctuation.normalize, \
                typo.detokenizer.normalize, markers.chat-tokens, markers.chat-tokens.strip, \
                markers.chat-tokens.pattern, markers.reasoning-trace, \
                markers.reasoning-block, markers.model-identifiers",
    },
    SignatureFamily {
        id: "alibaba",
        provider: "Alibaba — Qwen",
        models: "Qwen3.8-Max, Qwen3.8-27B, Qwen Code",
        signatures: "Fullwidth parentheses/colons and CJK punctuation in English text, \
                     ChatML tokens, always-on thinking output that can be pasted into the \
                     document",
        rules: "typo.fullwidth.normalize, typo.cjk-punctuation.normalize, \
                markers.chat-tokens, markers.chat-tokens.pattern, markers.agent-trailers, \
                markers.model-identifiers",
    },
    SignatureFamily {
        id: "moonshot",
        provider: "Moonshot AI — Kimi",
        models: "Kimi K2.5, K2.6, K3",
        signatures: "Sentence tokens with fullwidth pipes, always-on reasoning dumps, \
                     self-identification",
        rules: "markers.chat-tokens, markers.chat-tokens.pattern, markers.reasoning-block, \
                markers.model-identifiers",
    },
    SignatureFamily {
        id: "zhipu",
        provider: "Zhipu AI — GLM",
        models: "GLM-5, GLM-5.1",
        signatures: "Fullwidth and CJK punctuation, chat-template tokens ([gMASK]<sop>, \
                     <|observation|>), reasoning traces",
        rules: "typo.fullwidth.normalize, typo.cjk-punctuation.normalize, \
                markers.chat-tokens, markers.chat-tokens.pattern, markers.reasoning-trace",
    },
    SignatureFamily {
        id: "microsoft",
        provider: "Microsoft — Copilot (Word, Edge, VS Code)",
        models: "Copilot (GPT-5.6 family under the hood)",
        signatures: "Assistant disclaimers kept in the document, Copilot attribution, \
                     utm_source=copilot.com tracking parameters, raw Markdown pasted into \
                     Word, unfilled template placeholders ([Your Name], [INSERT …]); \
                     Office metadata naming the assistant as author (see docx-generators)",
        rules: "phrasal.machine-residue, phrasal.sycophancy, markers.agent-trailers, \
                markers.citation-artifacts, structural.markdown-tells, \
                style.markdown.residue, style.placeholder.residue",
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
                     Claude-Session:) — the literal ones and the same patterns naming \
                     models released after this catalogue was written; leaked tool-call \
                     scaffolding; Claude Code 2.1.91–2.1.196 prompt-date steganography, \
                     normalized by exact line grammar; ANSI terminal escapes copied from agent \
                     output; decorative emoji and raw Markdown in generated docs",
        rules: "markers.agent-trailers, markers.agent-trailer-lines, markers.tool-calls, \
                markers.claude-prompt-fingerprint, unicode.ansi.strip, \
                style.emoji.opener, style.emoji.density, style.markdown.residue, \
                structural.markdown-tells",
    },
    SignatureFamily {
        id: "chat-ui",
        provider: "All chat UIs — the interface chrome around the answer",
        models: "ChatGPT, Claude, Gemini, Copilot, Grok, Perplexity, DeepSeek, Meta AI, \
                 Kimi, Le Chat (2026 interface copy)",
        signatures: "The disclaimer line printed beneath every answer (\"ChatGPT can make \
                     mistakes. Check important info.\") and the labels around it — role \
                     headers (\"You said:\", \"ChatGPT said:\"), buttons (\"Copy code\", \
                     \"Regenerate response\"), progress lines (\"Searching the web\", \
                     \"Reading sources\") — all of which survive a careless copy/paste into \
                     Word, Markdown or a note app",
        rules: "markers.chat-ui-disclaimer, markers.chat-ui-disclaimer.strip, \
                markers.chat-ui-residue",
    },
    SignatureFamily {
        id: "structure",
        provider: "All providers — document shape and formatting habits",
        models: "Any model, any version (RLHF formatting defaults; measured 9–43x the \
                 human rate in 2026 corpora)",
        signatures: "Raw Markdown in plain text or DOCX (bold, ATX heading markers, fences, \
                     task boxes), decorative emoji openers and emoji used as structure, \
                     Title Case headings, and runs of three or more bold-label list items \
                     (\"- **Speed:** faster\") — the inline-header vertical list. The \
                     Markdown adapter recovers list runs from the source in its container \
                     pass; literal markup in plain text and DOCX hits the prose rule. Real \
                     Word bold is invisible to a prose rule and is left alone",
        rules: "structural.markdown-tells, structural.heading-emoji, \
                style.markdown.residue, style.emoji.opener, style.emoji.density, \
                style.heading.title-case, style.list.inline-headers",
    },
    SignatureFamily {
        id: "encoding",
        provider: "All providers — encoding damage in the delivery path",
        models: "Any model, any version: UTF-8 decoded as Windows-1252 upstream, copy-as-source \
                 out of a chat UI or web page",
        signatures: "Mojibake from a middleware that decoded UTF-8 as Windows-1252 \
                     (â€™, â€œ, Ã©, and double- or triple-encoded forms such as \
                     Ã¢â‚¬â„¢); HTML entities left when an answer is copied as source \
                     (&nbsp;, &mdash;, &#8217;, &amp;lt;); raw HTML tags in prose \
                     (<p>, <br>, <strong>, <span style=…>)",
        rules: "typo.mojibake.repair, typo.html-entities.decode, markers.html-tags",
    },
    SignatureFamily {
        id: "unicode",
        provider: "All providers — invisible-character vectors",
        models: "Any model, any version (character-level smuggling)",
        signatures: "Unicode tag characters, variation-selector runs, filler glyphs \
                     (U+3164, U+FFA0, U+2800), bidi overrides (Trojan Source), mixed-script \
                     homoglyphs (Greek, Cyrillic, Armenian, Cherokee lookalikes); C0/C1 \
                     control characters and noncharacters; Private-Use-Area residue; \
                     combining-mark (Zalgo) runs; zero-width runs encoding hidden bits \
                     (\"Sneaky Bits\") and stray joiners inside Latin words; reserved \
                     default-ignorable Unicode ranges and complete ANSI terminal sequences",
        rules: "typo.invisibles.strip, unicode.smuggled.strip, unicode.fillers.strip, \
                unicode.spaces.normalize, unicode.bidi.strip-overrides, unicode.bidi.flag, \
                unicode.confusables.flag, unicode.controls.strip, unicode.ansi.strip, \
                unicode.private-use.flag, unicode.private-use.strip, unicode.combining.flag, \
                unicode.zero-width.flag, unicode.zero-width.strip, \
                unicode.whitespace-stego.flag, unicode.whitespace-stego.strip, \
                unicode.math-styled.flag, unicode.math-styled.normalize",
    },
    SignatureFamily {
        id: "typography",
        provider: "All providers — punctuation and typography",
        models: "Any model, any version (office-style smart punctuation)",
        signatures: "Curly quotes, guillemets, primes and acute accents used as apostrophes, \
                     ellipsis and two-dot leader, em/en dashes, hyphen family, typographic \
                     ligatures (ﬁ ﬂ ﬀ), fullwidth ASCII forms, CJK punctuation in non-CJK \
                     text, detokenizer space markers and detokenized spacing (space before \
                     punctuation), repeated sentence punctuation",
        rules: "typo.quotes.straighten, typo.ligatures.expand, typo.ellipsis.expand, \
                typo.em-dash.to-comma, typo.en-dash.to-hyphen, typo.hyphens.normalize, \
                typo.fullwidth.normalize, typo.cjk-punctuation.normalize, \
                typo.detokenizer.normalize, style.spacing.detokenized, \
                style.punctuation.repeated",
    },
    SignatureFamily {
        id: "lexical",
        provider: "All providers — measured excess vocabulary and phrasing",
        models: "Any model, any version (flag-only; evidence-weighted, never auto-rewritten)",
        signatures: "Post-ChatGPT excess words measured on 15.1M PubMed abstracts (delve, \
                     showcase, underscore, tapestry — Kobak et al., Science Advances 2025), \
                     the WikiProject AI Cleanup watch list, copula avoidance (\"serves \
                     as\" for \"is\"), negative parallelism, trailing -ing clauses, academic \
                     and marketing formulas, promotional puffery, professional disclaimers, \
                     rhetorical pivots, throat-clearing and sycophancy",
        rules: "lexical.overused-verbs, lexical.overused-nouns, lexical.inflated-adjectives, \
                lexical.llm-tics, lexical.hedges, phrasal.copula-avoidance, \
                phrasal.negative-parallelism, phrasal.trailing-participles, \
                phrasal.academic-formulas, phrasal.marketing-formulas, \
                phrasal.promotional-puffery, phrasal.professional-disclaimer, \
                phrasal.rhetorical-pivots, phrasal.throat-clearing, phrasal.sycophancy, \
                phrasal.fake-authority, phrasal.formal-transitions",
    },
    SignatureFamily {
        id: "docx-generators",
        provider: "DOCX generator libraries — python-docx, JS docx, Apache POI, docx4j",
        models: "Underlies ChatGPT code interpreter, Claude/Gemini document generation, \
                 most custom AI pipelines",
        signatures: "Package metadata, not prose: creator / lastModifiedBy / description \
                     stamped by a generation library or model (\"python-docx\", \"generated \
                     by python-docx\", the JS library's \"Un-named\" creator), Application \
                     naming the tool, and python-docx's 2013-12-23 default-template \
                     creation date — none of which Word clears on resave. The reference \
                     template's Application/AppVersion pair (python-docx: Microsoft \
                     Macintosh Word 14.0000; pandoc: Microsoft Word 12.0.0 12.0000) is \
                     reported report-only, because a genuine Word for Mac 2011 file carries \
                     the same strings. Reported as container findings at every level; the \
                     matched fields are scrubbed at aggressive. External hyperlink targets in `.rels` are also reported \
                     and have exact AI-provider attribution parameters removed at aggressive; \
                     prose in headers, footers, footnotes, endnotes and comments is cleaned \
                     by the ordinary prose rules.",
        // Implemented by the DOCX adapter's container.findings hook
        // (container.docx-generator-metadata), which is not a prose rule.
        rules: "",
    },
    SignatureFamily {
        id: "c2pa-provenance",
        provider: "C2PA 2.x / Content Credentials & AI container provenance",
        models: "OpenAI DALL-E / GPT-Image, Claude file & artifact exports, Adobe Firefly, \
                 Google Imagen, Microsoft Designer, c2pa-rs / c2patool pipelines",
        signatures: "ISO/IEC 19566-5 JUMBF Content Credentials manifests (`c2pa` UUID \
                     63327061-0011-0010-8000-00AA00389B71) and IPTC `trainedAlgorithmicMedia` \
                     metadata across PNG (`caBX`, `iTXt`/`tEXt` XMP, SD/ComfyUI chunks), \
                     JPEG (single and multi-segment `APP11` `JP` XTBox and `APP1` XMP), \
                     WebP (`C2PA` and `XMP ` RIFF chunks), SVG (`<metadata>`), HTML \
                     (`<link rel=\"c2pa-manifest\">`, `<script type=\"application/c2pa\">`, \
                     AI `<meta>` tags), Markdown YAML/TOML front matter, DOCX packages \
                     (`META-INF/*.c2pa`, embedded `word/media/*`, `docProps/custom.xml`), \
                     C2PA 2.4 Structured Text banners, and C2PA Unstructured Text \
                     (`U+FEFF` + `C2PATXT\\0` variation-selector binary frames)",
        rules: "markers.c2pa-text-manifest, markers.c2pa-structured-manifest, \
                markers.c2pa-structured-manifest.strip",
    },
    SignatureFamily {
        id: "statistical-watermarks",
        provider: "Statistical token watermarks — opt-in n-gram window disruption",
        models: "Google SynthID-Text (Gemini, Claude 5.x from Aug 2026), OpenAI textGrain",
        signatures: "Keyed pseudorandom biases over 5-gram token windows (`H = 4` preceding \
                     tokens in SynthID-Text). Dormant on standard runs; when `--dewatermark` \
                     is passed, acyclic truth-preserving phrasal, discourse, and closed-class \
                     canonicalizations shift token offsets and invalidate consecutive 5-gram \
                     windows while shielding quotations, URLs, numbers, and identifiers",
        rules: "watermark.ngram-disrupt",
    },
];

/// Every model family in the catalogue, newest review first (see [`CATALOG_REVIEWED`]).
pub fn coverage() -> &'static [SignatureFamily] {
    CATALOG
}
