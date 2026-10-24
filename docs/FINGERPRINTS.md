# AI fingerprint catalogue — October 2026

What this build looks for, which model families it attributes to, and what it deliberately
does **not** attempt. Reviewed **2026-10-24**.

The authoritative machine-readable version of this table is
`crates/cleanup-rules/src/catalog.rs`; print it with `aiclean coverage`.

## Two ground rules

1. **Character- and phrase-level residue only.** If a signal is statistical (a watermark in
   word choice), no character cleaner can remove it, and we say so rather than implying
   otherwise.
2. **Never rewrite meaning.** Anything that is ordinary English is *flagged*, never replaced.
   Only machine artifacts (control tokens, invisible characters, smart punctuation) are edited.

## What changed in 2026

| Date | Change | Effect on this tool |
| ---- | ------ | ------------------- |
| 2024 (ongoing) | Google **SynthID-Text** marks Gemini output | Out of scope — statistical |
| 2025-04 | OpenAI o3/o4-mini emit `U+202F` and stray invisibles; OpenAI calls it a training quirk, not a watermark | Covered at `light` (invisibles) |
| 2026-08-02 | EU AI Act Article 50 transparency obligations take effect | Providers add provenance — see below |
| 2026-08-14 | Anthropic announces a text watermark (a SynthID-Text variant) for models released from 2026-08-02 onward, all surfaces including Claude Code and the API | Out of scope — statistical |
| 2026-09/10 | Graphite "AI Tells" study (10k human vs 90k model articles, nine families): ~13,000 tells, 65% unique to one family. Em-dash use collapses (Claude Opus 5.5 −99%, GPT-6 Astra −88%, Gemini 3.1 Pro nearly zero) while new framing tells appear | Em-dash rules kept (older models, pasted archives) but re-ranked; new phrase packs added for the 2026 tells |
| 2026-10-05 | OpenAI announces **textGrain**, a statistical watermark, for ChatGPT/Codex in the EU | Out of scope — statistical |
| 2026-10 (ongoing) | Agentic coding tools (Claude Code, Codex CLI, Cursor, Copilot, Devin, Jules, Aider, Windsurf) append attribution trailers by default | Covered at `light` (flag) |

## Per-family signatures

| Family | Models reviewed | Characteristic residue | Rules |
| ------ | --------------- | ---------------------- | ----- |
| OpenAI | GPT-5.5, GPT-5.6 Sol, GPT-6 Astra, o3/o4-mini | `U+202F`, stray ZWSP/ZWNJ, `【9†source】` / `【45†L75-L83】`, `turn0search`, `oai_citation`, harmony tokens (`<\|start\|>`, `<\|channel\|>`, `<\|message\|>`), corrective framing ("rather than", "not simply") | `typo.invisibles.strip`, `unicode.spaces.normalize`, `markers.citation-*`, `markers.chat-tokens*`, `phrasal.negative-parallelism` |
| Anthropic | Opus 5/5.5, Sonnet 5, Haiku 4.5, Fable 5.1, Mythos 5.1 | Em/en dashes (older models), `<antThinking>`, `antml:`, superlatives ("the single most", "dependable"), Claude Code trailers, `Claude-Session:` | `typo.em-dash.to-comma`, `typo.en-dash.to-hyphen`, `markers.reasoning-*`, `markers.agent-trailers`, `phrasal.claude-residue`, `phrasal.superlative-framing` |
| Google | Gemini 3 Pro, 3.1 Pro, Flash, AI Overviews | Clipboard ZWSP/NBSP from Docs/Keep, footnote markers, formal transitions ("furthermore", "moreover") | `typo.invisibles.strip`, `unicode.spaces.normalize`, `phrasal.gemini-affirmations`, `phrasal.formal-transitions` |
| Meta | Llama 4 family, Meta AI | `<\|begin_of_text\|>`, `<\|start_header_id\|>`, `I'm Meta AI`, assistant disclaimers | `markers.chat-tokens*`, `markers.model-identifiers`, `phrasal.model-self-reference` |
| xAI | Grok 4, Grok 5 | `<grok:render`, self-identification, emoji-heavy bullets | `markers.citation-artifacts`, `markers.model-identifiers`, `style.emoji.*` |
| Mistral | Mistral Large 3, Le Chat | `[INST]`, `<<SYS>>`, "Le Chat" leakage | `markers.chat-tokens`, `markers.model-identifiers` |
| DeepSeek | V4 Flash / Pro, V4.1, V3.x, R1 | Fullwidth `，（）：` inside English, visible ` thinking` traces, `<\|begin▁of▁sentence\|>`, `｜end▁of▁sentence｜` | `typo.fullwidth.normalize`, `markers.reasoning-*`, `markers.chat-tokens*` |
| Alibaba | Qwen3.8-Max, Qwen3.8-27B, Qwen Code | Fullwidth forms, `<\|im_end\|>`, always-on thinking output, "Qwen Code" footers | `typo.fullwidth.normalize`, `markers.chat-tokens`, `markers.agent-trailers` |
| Moonshot | Kimi K2.5, K2.6, K3 | Fullwidth-pipe sentence tokens, always-on reasoning dumps | `markers.chat-tokens`, `markers.reasoning-block`, `markers.model-identifiers` |
| Zhipu | GLM-5, GLM-5.1 | Fullwidth punctuation, chat tokens, reasoning traces | `typo.fullwidth.normalize`, `markers.chat-tokens`, `markers.reasoning-trace` |
| Microsoft | Copilot (Word, Edge, VS Code) | Assistant disclaimers kept in the document, raw Markdown pasted into Word, Office metadata naming the assistant (metadata out of scope → see below) | `phrasal.machine-residue`, `markers.agent-trailers`, `structural.markdown-tells` |
| Perplexity | Sonar, Sonar Pro | Dense numbered citations copied without links, answer-engine signposting | `markers.citation-artifacts`, `phrasal.formal-transitions` |
| Amazon | Nova Pro / Lite / Micro | Self-identification, assistant disclaimers | `markers.model-identifiers`, `phrasal.machine-residue` |
| Agentic tools | Claude Code, Codex CLI, Cursor, Copilot, Devin, Jules, Aider, Windsurf, Gemini CLI | `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`, `🤖 Generated with [Claude Code]`, `Claude-Session:`, decorative emoji, raw Markdown | `markers.agent-trailers`, `style.*` |

## Character families

| Class | Code points | Level | Action |
| ----- | ----------- | ----- | ------ |
| Zero-width / non-rendering | `U+200B`, `U+2060`, `U+00AD`, `U+FEFF`, `U+180E`, `U+034F`, `U+2061`–`U+2064`, `U+206A`–`U+206F`, `U+FFF9`–`U+FFFB`, `U+1BCA0`–`U+1BCA3` | light | removed |
| No-break spaces | `U+00A0`, `U+202F` | light | → `U+0020` |
| Unicode tag characters | `U+E0000`–`U+E007F` | light | removed |
| Smuggled variation selectors | `U+FE00`–`U+FE0F`, `U+E0100`–`U+E01EF` (when not styling an emoji) | light | removed |
| Blank filler glyphs | `U+115F`, `U+1160`, `U+17B4`, `U+17B5`, `U+2800`, `U+3164`, `U+FFA0` | standard | removed |
| Exotic spaces / separators | `U+1680`, `U+2000`–`U+200A`, `U+205F`, `U+3000`, `U+2028`, `U+2029` | standard | → `U+0020` |
| Bidi overrides (Trojan Source) | `U+202D`, `U+202E` | standard | removed |
| Bidi controls / marks | `U+202A`–`U+202C`, `U+2066`–`U+2069`, `U+200E`, `U+200F`, `U+061C` | aggressive | flagged |
| Homoglyph mixing | Latin + Cyrillic/Greek in one word | aggressive | flagged |
| Curly quotes & guillemets | `U+2018`–`U+201F`, `U+00AB`, `U+00BB`, `U+2039`, `U+203A`, `U+02BC` | standard | → ASCII |
| Ellipsis | `U+2026`, `U+22EF` | standard | → `...` |
| En dash | `U+2013` | standard | → `, ` / `-` |
| Hyphen family | `U+2010`, `U+2011`, `U+2012`, `U+2212` | standard | → `-` |
| Em dash family | `U+2014`, `U+2015`, `U+2E3A`, `U+2E3B`, `U+FE58` | aggressive | → `, ` / `--` |
| Fullwidth ASCII | `U+FF01`–`U+FF5E` | aggressive | → ASCII |

Deliberately preserved: `U+200C`/`U+200D` (ZWNJ/ZWJ — required in Arabic, Indic and emoji
sequences), emoji base characters, and variation selectors on a genuine emoji base.

## Out of scope, by design

| Technique | Owner | Why it cannot be removed here |
| --------- | ----- | ----------------------------- |
| SynthID-Text | Google Gemini; basis of Anthropic's 2026 watermark | Statistical: lives in token choice, no characters to strip |
| textGrain | OpenAI (EU ChatGPT/Codex, Oct 2026) | Statistical, same reason |
| C2PA / Content Credentials | Claude file exports, OpenAI images and audio, Google media | Signed container metadata, not prose — a separate feature |
| Office/Copilot document metadata | `docProps/core.xml`, `docProps/app.xml` | Container-level fields; a separate feature (tracked, not claimed) |

## Keeping this current

- `.cargo/audit.toml` and this file are reviewed every release (see `SECURITY.md`).
- `catalog_references_known_rules` fails the test suite if the catalogue names a rule that does
  not exist, so documentation cannot drift ahead of the implementation.
- New phrases go in `templates/core.yaml` (data, no code change). New *characters* or *machine
  artifacts* need a rule in `crates/cleanup-rules`, plus a row here and in `catalog.rs`.
