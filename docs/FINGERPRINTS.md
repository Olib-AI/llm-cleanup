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
| 2026-06/07 | Claude Code 2.1.91–2.1.196 encodes proxy/timezone classifications in its `Today's date is YYYY-MM-DD.` system-prompt line: U+2019, U+02BC or U+02B9 replaces the apostrophe, and `YYYY/MM/DD` carries the timezone bit. Removed in 2.1.197, but captured prompts and transcripts retain it | Exact standalone lines restored to `Today's date is YYYY-MM-DD.` at `light` by `markers.claude-prompt-fingerprint`; all three apostrophe variants and the independent date-separator bit are covered, while canonical lines and ordinary date prose do not match |
| 2025-02+ | ChatGPT stores citations as Private-Use-Area wrappers (`U+E200 cite U+E202 turn0search0 U+E201`, plus `U+E203`/`U+E204`/`U+E206` span markers); they survive copy-paste into Word, and the bare `turn0search0` IDs show up once the wrappers are lost | Flagged at `light` (the fix is a real citation), validated cite/filecite envelopes removed at `aggressive` (`markers.private-use-citations*`, `markers.turn-citations*`); unknown PUA payloads stay report-only |
| 2026-10 | Current citation exports also use `turn0block1` / `turn0url1`, visible `【cite|…】` / `【filecite|…】` envelopes with optional line locators, and legacy `(oaicite:N){index=N}` / `contentReference[oaicite:N]` wrappers | Generic ASCII-kind turn parsing and bounded envelope grammars cover new source kinds without a model-name list; meaningful dagger/line citations remain report-only |
| 2025 | Kobak et al. (*Science Advances*): excess-vocabulary measurement over 15.1M PubMed abstracts — `delves` r=28, `underscores` r=13.8, `showcasing` r=10.7; WikiProject AI Cleanup maintains the editor-side watch list | Word packs re-weighted to the measured evidence; all of it *flagged*, never auto-rewritten |
| 2026-08-02 | EU AI Act Article 50 transparency obligations take effect | Providers add provenance — see below |
| 2026-08-14 | Anthropic announces a text watermark (a SynthID-Text variant) for models released from 2026-08-02 onward, all surfaces including Claude Code and the API | Out of scope — statistical |
| 2026-09/10 | Graphite "AI Tells" study (10k human vs 90k model articles, nine families): ~13,000 tells, 65% unique to one family. Em-dash use collapses (Claude Opus 5.5 −99%, GPT-6 Astra −88%, Gemini 3.1 Pro nearly zero) while new framing tells appear | Em-dash rules kept (older models, pasted archives) but re-ranked; new phrase packs added for the 2026 tells |
| 2026-10-05 | OpenAI announces **textGrain**, a statistical watermark, for ChatGPT/Codex in the EU | Out of scope — statistical |
| 2026-09/10 | "Sneaky Bits" / ASCII-smuggling techniques mainstreamed (Microsoft Security blog): invisible payloads encoded in Unicode tags, variation selectors, and two-invisible-character binary runs | Tag/VS removal at `light`; zero-width *runs* and stray joiners in Latin text flagged at `standard` and removed at `aggressive` (`unicode.zero-width.*`) |
| 2026-10 | Pangram's nine-signal study (Oct 2026) measures Markdown in plain text at 12× the human rate — bold `**` 43×, headings 23×, inline code 5× — with bullets at 9× (28 vs 3 per 10k words); sloptells counts emoji-as-structure at 32× | New `structure` family: ATX-heading residue in plain text/DOCX, Title Case headings, and inline-header vertical lists (three or more bold-label items), all flag-only |
| 2026-10 | Botes et al. (512,970 abstracts): ~16% of 2025 abstracts show LLM traces, but `delve`, `intricate`, `showcasing`, `commendable` and `noteworthy` *declined* through 2025; Graphite's Oct 2026 corpus finds Claude Opus 5.5 down 99% on em dashes yet carrying 2,548 other tells | Word lists kept (they still describe archives) but re-ranked; five new phrasing packs cover the 2026 residue — notability boilerplate, source-availability hedges, optimistic endings, social-post register, letter formulas |
| 2026-10 | Encoding damage dominates pipeline output: UTF-8 decoded as Windows-1252 (`â€™`, `â€œ`) and copy-as-source HTML entities (`&nbsp;`, `&mdash;`) | New `encoding` family repairs both deterministically (`typo.mojibake.repair`, `typo.html-entities.decode`) and reports raw HTML syntax (`markers.html-tags`) |
| 2026-10 | Chat UIs print chrome around the answer — the "can make mistakes" disclaimer, role headers (`You said:`), buttons and progress lines — and it survives copy-paste | New `chat-ui` family: disclaimer flagged from `light` and removed at `aggressive`; labels and progress lines flagged at `standard` |
| 2026-10 (ongoing) | Agentic coding tools (Claude Code, Codex CLI, Cursor, Copilot, Devin, Jules, Aider, Windsurf) append attribution trailers by default; copied terminal output can also contain raw CSI/OSC/DCS sequences | Trailers covered at `light` (flag), including trailer *patterns* naming models released after this catalogue (`markers.agent-trailer-lines`); complete ANSI sequences removed at `light` without swallowing malformed suffixes |
| 2026-10 (ongoing) | AI-generated `.docx` files carry python-docx / JS-`docx` metadata defaults (creator, description, 2013 template date) that Word never clears; detector write-ups enumerate them | `docProps` fingerprints reported at every level; matched author/program fields scrubbed at `aggressive` (see *DOCX coverage*) |

## Per-family signatures

| Family | Models reviewed | Characteristic residue | Rules |
| ------ | --------------- | ---------------------- | ----- |
| OpenAI | GPT-5.5, GPT-5.6 Sol, GPT-6 Astra, o3/o4-mini | `U+202F`, stray ZWSP/ZWNJ, `【9†source】` / `【45†L75-L83】`, PUA citation wrappers (`U+E200…U+E201`), visible `【cite|…】` / `【filecite|…】`, bare `turn0search0`/`turn0block0`/`turn0url0` IDs (any numbers), legacy `oaicite` envelopes, AI-provider UTM/referrer parameters on links, harmony tokens (`<\|start\|>`, `<\|channel\|>`, `<\|message\|>`), unknown `<\|…\|>` tokens (pattern rule), leaked tool-call scaffolding, corrective framing ("rather than", "not simply") | `typo.invisibles.strip`, `unicode.spaces.normalize`, `markers.citation-*`, `markers.private-use-citations*`, `markers.turn-citations*`, `markers.chat-tokens*`, `markers.tool-calls`, `phrasal.negative-parallelism` |
| Anthropic | Opus 5/5.5, Sonnet 5, Haiku 4.5, Fable 5.1, Mythos 5.1 | Em/en dashes (older models), `<antThinking>`, `antml:`, leaked `<invoke>`/`<function_calls>` scaffolding, superlatives ("the single most", "dependable"), Claude Code trailers (incl. unlisted future model names, by pattern), `Claude-Session:`, Claude Code 2.1.91–2.1.196 prompt-date steganography (U+2019/U+02BC/U+02B9 and slash-separated dates) | `typo.em-dash.to-comma`, `typo.en-dash.to-hyphen`, `markers.reasoning-*`, `markers.agent-trailers`, `markers.agent-trailer-lines`, `markers.claude-prompt-fingerprint`, `markers.tool-calls`, `phrasal.claude-residue`, `phrasal.superlative-framing` |
| Google | Gemini 3 Pro, 3.1 Pro, Flash, AI Overviews | Clipboard ZWSP/NBSP from Docs/Keep, footnote markers, formal transitions ("furthermore", "moreover"); Gemma tokens (`<start_of_turn>`, `<\|turn\|>`, `<\|think\|>`) from self-hosted output | `typo.invisibles.strip`, `unicode.spaces.normalize`, `markers.chat-tokens*`, `phrasal.gemini-affirmations`, `phrasal.formal-transitions` |
| Meta | Llama 4 family, Meta AI | `<\|begin_of_text\|>`, `<\|start_header_id\|>`, Llama 4 `<\|header_start\|>` / `<\|eot\|>` / reasoning tokens, `I'm Meta AI`, assistant disclaimers | `markers.chat-tokens*`, `markers.model-identifiers`, `phrasal.model-self-reference` |
| xAI | Grok 4, Grok 5 | `<grok:render`, self-identification, emoji-heavy bullets, measured causal/empirical connective overuse | `markers.citation-artifacts`, `markers.model-identifiers`, `style.emoji.*`, `lexical.grok-tics` |
| Mistral | Mistral Large 3, Le Chat | `[INST]`, `<<SYS>>`, `[SYSTEM_PROMPT]`, `[AVAILABLE_TOOLS]`, `[TOOL_CALLS]`, "Le Chat" leakage | `markers.chat-tokens`, `markers.tool-calls`, `markers.model-identifiers` |
| DeepSeek | V4 Flash / Pro, V4.1, V3.x, R1 | Fullwidth `，（）：` and CJK `、。：；` inside English, detokenizer `Ġ`/`▁` residue, visible ` thinking` traces, `<\|begin▁of▁sentence\|>`, `｜end▁of▁sentence｜` | `typo.fullwidth.normalize`, `typo.cjk-punctuation.normalize`, `typo.detokenizer.normalize`, `markers.reasoning-*`, `markers.chat-tokens*` |
| Alibaba | Qwen3.8-Max, Qwen3.8-27B, Qwen Code | Fullwidth forms, CJK punctuation, `<\|im_end\|>`, always-on thinking output, "Qwen Code" footers | `typo.fullwidth.normalize`, `typo.cjk-punctuation.normalize`, `markers.chat-tokens*`, `markers.agent-trailers` |
| Moonshot | Kimi K2.5, K2.6, K3 | Fullwidth-pipe sentence tokens, always-on reasoning dumps | `markers.chat-tokens*`, `markers.reasoning-block`, `markers.model-identifiers` |
| Zhipu | GLM-5, GLM-5.1 | Fullwidth/CJK punctuation, `[gMASK]<sop>`, `<\|observation\|>`, reasoning traces | `typo.fullwidth.normalize`, `typo.cjk-punctuation.normalize`, `markers.chat-tokens*`, `markers.reasoning-trace` |
| Microsoft | Copilot (Word, Edge, VS Code) | Assistant disclaimers kept in the document, `utm_source=copilot.com`, raw Markdown pasted into Word, unfilled `[Your Name]` placeholders, document metadata (→ DOCX generators row) | `phrasal.machine-residue`, `markers.agent-trailers`, `markers.citation-artifacts`, `structural.markdown-tells`, `style.placeholder.residue` |
| Perplexity | Sonar, Sonar Pro | Dense numbered citations copied without links, answer-engine signposting | `markers.citation-artifacts`, `phrasal.formal-transitions` |
| Amazon | Nova Pro / Lite / Micro | Self-identification, assistant disclaimers | `markers.model-identifiers`, `phrasal.machine-residue` |
| Agentic tools | Claude Code, Codex CLI, Cursor, Copilot, Devin, Jules, Aider, Windsurf, Gemini CLI | `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`, `🤖 Generated with [Claude Code]`, `Claude-Session:` — literal and by trailer pattern (human names never match); Claude Code 2.1.91–2.1.196 prompt-date steganography; leaked tool-call scaffolding; decorative emoji, raw Markdown | `markers.agent-trailers`, `markers.agent-trailer-lines`, `markers.claude-prompt-fingerprint`, `markers.tool-calls`, `style.*` |
| DOCX generators | python-docx, JS `docx`, Apache POI, docx4j (under ChatGPT code interpreter, Claude/Gemini doc features, custom pipelines) | `docProps` fields: creator `python-docx` / `Un-named`, description `generated by python-docx`, model-name Application stamps, python-docx template creation date `2013-12-23T23:15:00Z` | container finding `container.docx-generator-metadata` (report always; author/program fields scrubbed at `aggressive`) |
| Chat UIs | ChatGPT, Claude, Gemini, Copilot, Grok, Perplexity, DeepSeek, Meta AI, Kimi, Le Chat | `ChatGPT can make mistakes. Check important info.`, `Claude can make mistakes. Please double-check responses.`, `You said:`, `ChatGPT said:`, `Copy code`, `Regenerate response`, `Searching the web` | `markers.chat-ui-disclaimer`, `markers.chat-ui-disclaimer.strip`, `markers.chat-ui-residue` |
| Document structure | Any model | Raw Markdown in a non-Markdown container (`**`, `## `, fences, `- [ ]`), Title Case headings, three or more bold-label list items (`- **Speed:** faster`), decorative emoji openers, emoji as structure. In Markdown the adapter scans the source for list runs in its container pass; literal markup in `.txt`/DOCX is caught by the prose rule; real Word bold is invisible to a prose rule | `structural.markdown-tells`, `structural.heading-emoji`, `style.markdown.residue`, `style.emoji.opener`, `style.emoji.density`, `style.heading.title-case`, `style.list.inline-headers` |
| Encoding damage | Any model | `â€™`/`â€œ` mojibake (single, double and triple encoded), `&nbsp;`/`&mdash;`/`&#8217;` entities, literal `<p>`, `<br>`, `<span style=…>` | `typo.mojibake.repair`, `typo.html-entities.decode`, `markers.html-tags` |
| All (lexical flags) | Any model | Measured post-ChatGPT excess vocabulary and phrasing (Graphite, Kobak et al., WikiProject AI Cleanup): overused verbs/nouns/adjectives, copula avoidance ("serves as"), trailing ‑ing clauses, academic/marketing formulas, promotional puffery, professional disclaimers, rhetorical pivots, throat-clearing, sycophancy, fake authority | `lexical.*`, `phrasal.*` (flag-only, always) |

## DOCX coverage

DOCX is a package, not a single file, and fingerprints hide outside the body. The adapter
cleans `<w:t>` prose in **all** of `word/document.xml`, `word/header*.xml`,
`word/footer*.xml`, `word/footnotes.xml`, `word/endnotes.xml` and `word/comments.xml`
(multi-run paragraphs joined exactly as in the body; untouched parts are copied
byte-for-byte). XML entities are decoded only for rule matching, with an explicit offset map
back to the original `<w:t>` bytes, so escaped chat tokens such as `&lt;|im_end|&gt;` are
cleaned without reserializing the run. Signatures placed in a header or footnote are cleaned
the same as body text.

External hyperlink relationships in `.rels` are inspected separately. At `aggressive`, exact
AI-provider `utm_source`/`utm_medium`/`utm_campaign` and `referrer`/`referer` values are
removed while other query parameters, fragments, internal relationships, and non-hyperlink
targets remain unchanged. The provider list covers ChatGPT/OpenAI, Copilot, Perplexity,
Claude/Anthropic, Gemini, Grok/xAI, DeepSeek, Kimi/Moonshot, Meta AI, Mistral/Le Chat, Qwen,
GLM/z.ai, and the Poe/You.com/Phind answer engines; a generic `utm_source=newsletter` is
ordinary analytics and is left alone. Markdown inline-link destinations and autolinks get the same
structural treatment; code, raw HTML, and image ranges are excluded.

Package metadata is reported separately as a *container finding*: python-docx / JS-`docx` /
model stamps in `docProps/core.xml` (creator, lastModifiedBy, description) and
`docProps/app.xml` (Application) are flagged at every level, and the matched fields are
emptied at `aggressive`. The reference-template **pair** that generator libraries copy into
`app.xml` — `Microsoft Macintosh Word` + `14.0000` (python-docx) and `Microsoft Word 12.0.0`
+ `12.0000` (pandoc) — is reported report-only: a genuine Word for Mac 2011 file carries the
same Application string, so neither value alone nor the pair justifies rewriting a field we
cannot attribute. The python-docx default-template creation date is **report-only** —
the tool never invents a date. Signed provenance (C2PA) is untouched.

## Character families

| Class | Code points | Level | Action |
| ----- | ----------- | ----- | ------ |
| Zero-width / non-rendering | `U+200B`, `U+2060`, `U+00AD`, `U+FEFF`, `U+180E`, `U+034F`, `U+2061`–`U+2065`, `U+206A`–`U+206F`, `U+FFF0`–`U+FFF8`, `U+FFF9`–`U+FFFB`, `U+1BCA0`–`U+1BCA3`, Mongolian free variation selectors `U+180B`–`U+180D` + `U+180F`, musical format controls `U+1D173`–`U+1D182`, Egyptian hieroglyph format controls `U+13430`–`U+1343F` | light | removed |
| C0/C1 controls & noncharacters | `U+0000`–`U+001F`, `U+007F`–`U+009F` (tab/newline kept), `U+FDD0`–`U+FDEF`, `U+*FFFE`/`U+*FFFF`, `U+FFFC`; `U+FFFD` flagged, never stripped (it marks already-lost data) | light | removed / flagged |
| ANSI / ECMA-48 terminal sequences | Complete CSI, OSC, DCS, SOS, PM, APC and other bounded ESC sequences; malformed/unterminated strings are not allowed to consume trailing prose | light | removed |
| Private Use Area | runs of any PUA (flag); ChatGPT's citation block `U+E200`–`U+E2FF` | light (flag) / aggressive (validated envelopes and sibling residue) | flagged / selectively removed |
| Combining-mark runs ("Zalgo") | ≥3 combining marks on one base — threshold sits above Vietnamese legitimacy | aggressive | flagged |
| Zero-width joiner runs | `U+200C`/`U+200D` *inside Latin words* (Sneaky-Bits style encoding); Arabic/Indic/emoji contexts untouched | standard (flag) / aggressive (remove) | flagged at Standard; the same narrowly identified contexts are removed at Aggressive |
| Claude Code prompt-date fingerprint | Exact standalone `Today[U+2019/U+02BC/U+02B9]s date is YYYY-MM-DD.` lines and the `YYYY/MM/DD` timezone-bit variant from Claude Code 2.1.91–2.1.196 | light | restored to canonical ASCII; canonical lines and non-exact prose untouched |
| No-break spaces | `U+00A0`, `U+202F` | light | → `U+0020` |
| Unicode tags / reserved format controls | `U+E0000`–`U+E00FF`, `U+E01F0`–`U+E0FFF` (default-ignorable plane-14 ranges; variation selectors `U+E0100`–`U+E01EF` are handled separately) | light | removed |
| Smuggled variation selectors | `U+FE00`–`U+FE0F`, `U+E0100`–`U+E01EF` (when not styling an emoji) | light | removed |
| Blank filler glyphs | `U+115F`, `U+1160`, `U+17B4`, `U+17B5`, `U+2800`, `U+3164`, `U+FFA0` | standard | removed |
| Exotic spaces / separators | `U+1680`, `U+2000`–`U+200A`, `U+205F`, `U+3000`, `U+2028`, `U+2029` | standard | → `U+0020` |
| Bidi overrides (Trojan Source) | `U+202D`, `U+202E` | standard | removed |
| Bidi controls / marks | `U+202A`–`U+202C`, `U+2066`–`U+2069`, `U+200E`, `U+200F`, `U+061C` | aggressive | flagged |
| Homoglyph mixing | Latin + Cyrillic/Greek/Armenian/Cherokee lookalikes in one word (digits ignored when finding the word edge) | aggressive | flagged |
| Curly quotes & guillemets | `U+2018`–`U+201F`, `U+00AB`, `U+00BB`, `U+2039`, `U+203A`, `U+02BC`, primes `U+2032`/`U+2033` used as quotes, acute accent `U+00B4` used as an apostrophe | standard | → ASCII |
| Ellipsis & leaders | `U+2026`, `U+22EF`, two-dot leader `U+2025` | standard | → `...` / `..` |
| Typographic ligatures | `U+FB00`–`U+FB06` (ﬀ ﬁ ﬂ ﬃ ﬄ ﬅ ﬆ) — Word's "typographic ligatures" and PDF copy-out | standard | → `ff`, `fi`, `fl`, `ffi`, `ffl`, `st` |
| En dash | `U+2013` | standard | → `, ` / `-` |
| Hyphen family | `U+2010`, `U+2011`, `U+2012`; minus sign `U+2212` only when digit-adjacent | standard | → `-` |
| Em dash family | `U+2014`, `U+2015`, `U+2E3A`, `U+2E3B`, `U+FE58` | aggressive | → `, ` / `--` |
| Fullwidth ASCII | `U+FF01`–`U+FF5E` | aggressive | → ASCII |
| CJK punctuation in non-CJK prose | `U+3001`, `U+3002`, `U+FF0E`…, i.e. `、。：；？！（）：` etc. (context-guarded; genuine CJK text untouched) | aggressive | → ASCII |
| Detokenizer space markers | `Ġ` (`U+0120`), `▁` (`U+2581`) standing for spaces in tokenizer dumps — context-guarded so Maltese `Ġ/Ċ` survive | aggressive | → space |
| Detokenized spacing | space before `.,;:!?` `%,` spaced `.`, tokeniser-style apostrophes — French narrow-space style is exempt | aggressive | flagged |
| Repeated punctuation | `!!!`, `???`, `!!?` runs (`...` exempt) | aggressive | flagged |

Deliberately preserved: `U+200C`/`U+200D` in Arabic, Indic and emoji sequences (only their
Latin-word/invisible-run contexts are removed at `aggressive`), emoji base characters, and
variation selectors on a genuine emoji base. Unknown PUA citation payloads and meaningful
legacy dagger/line citations are likewise report-only.

## Encoding repair

Text that was mis-decoded upstream is *repaired*, not merely flagged, because the damage is
deterministic. `typo.mojibake.repair` maps every Windows-1252 character back to the byte that
produced it and re-decodes the run as UTF-8, iterating until the text stops changing — so
double- and triple-encoded forms (`Ã¢â‚¬â„¢`) heal in one pass. `café` and `© 2026` never
match: a run only starts on a CP1252 lead byte (`0xC2`–`0xF4`) followed by a continuation
byte (`0x80`–`0xBF`), and decoded output that would contain control characters, noncharacters
or `U+FFFD` is rejected. `typo.html-entities.decode` decodes the curated named entities and
the numeric `&#…;`/`&#x…;` forms, refuses to double-decode `&amp;lt;`, and leaves unknown
entities alone (they may be deliberate documentation). `markers.html-tags` reports raw HTML
element syntax and is report-only, because a document may legitimately quote markup.

Every repaired character goes through the same level-aware punctuation mapping as text that
arrived intact, so the result is idempotent at each level: `â€”` becomes `—` at `standard`
and `--` at `aggressive`.

## Out of scope, by design

The Claude Code prompt-date character fingerprint above is **not** Anthropic's newer Claude text
watermark. Anthropic states that the latter adds no hidden characters: it is a statistical pattern
in word choice, so deleting Unicode cannot remove it.

| Technique | Owner | Why it cannot be removed here |
| --------- | ----- | ----------------------------- |
| SynthID-Text | Google Gemini; basis of Anthropic's 2026 watermark | Statistical: lives in token choice, no characters to strip |
| textGrain | OpenAI (EU ChatGPT/Codex, Oct 2026) | Statistical, same reason |
| C2PA / Content Credentials | Claude file exports, OpenAI images and audio, Google media | Cryptographically *signed* provenance manifests. (The unsigned `docProps` author/program fields ARE covered — see *DOCX coverage* — but we do not strip signed claims.) |

## Keeping this current

- `.cargo/audit.toml` and this file are reviewed every release (see `SECURITY.md`).
- `catalog_references_known_rules` fails the test suite if the catalogue names a rule that does
  not exist, so documentation cannot drift ahead of the implementation.
- New phrases go in `templates/core.yaml` (data, no code change). New *characters* or *machine
  artifacts* need a rule in `crates/cleanup-rules`, plus a row here and in `catalog.rs`.
