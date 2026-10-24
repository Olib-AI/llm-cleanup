<p align="center">
  <img src="crates/cleanup-gui/ui/icons/app-icon.svg" width="84" height="84" alt="llm-cleanup logo">
</p>
<h1 align="center">llm-cleanup</h1>
<p align="center">
  <strong>Remove AI-generated fingerprints from your documents. Offline, deterministic, and formatting-safe.</strong>
</p>
<p align="center">
  Built by <a href="https://www.olib.ai">Olib AI</a>
</p>

---

llm-cleanup strips the tell-tale residue that large language models and AI export pipelines leave behind (em dashes, curly quotes, the narrow no-break space, invisible "smuggling" characters, SNOW trailing whitespace steganography, Mathematical Alphanumeric fake-bold styling, chat-template tokens, ChatGPT and Gemini/NotebookLM citation plumbing, C2PA / Content Credentials manifests, agent attribution trailers, overused wording) from Markdown, plain text, HTML, Word documents (`.docx`), and standalone images (`.png`, `.jpg`, `.webp`, `.svg`). Language-neutral cleanup works on any script, and review-only French and Spanish phrasing cues are enabled alongside English so mixed-language documents are handled in one pass. It does this without breaking a single byte of your formatting or re-encoding image pixels: headings, bold, italic, lists, tables, links, code blocks, scripts, shapes, and pixel streams are preserved exactly. There is no network call and no LLM at runtime.

The rule set is reviewed against the model landscape each release. Coverage as of **October 2026** spans GPT-5.5/5.6/6, Claude Opus 5.x and Fable/Mythos 5.1, Gemini 3.x, Llama 4, Grok 4/5, Mistral Large 3, DeepSeek V4, Qwen3.8, Kimi K2.5–K3, GLM-5, Copilot, Perplexity Sonar and the 2026 crop of coding agents. Run `aiclean coverage` to print it, or read [docs/FINGERPRINTS.md](docs/FINGERPRINTS.md) for the sources.

## Why llm-cleanup

- **Formatting-safe by construction.** It never re-serializes your document. It locates the prose, cleans only that text, and splices the result back into the original bytes. If nothing matches, the output is byte-for-byte identical to the input.
- **Offline and deterministic.** No network, no model, no telemetry. The same input always produces the same output.
- **Real document & media support.** Markdown, plain text, HTML (`.html`/`.htm`), Word (`.docx`), and standalone media containers (`.png`, `.jpg`/`.jpeg`, `.webp`, `.svg`). DOCX keeps your shapes, tables, and styles intact while cleaning prose runs *and* losslessly scrubbing embedded media (`word/media/*`), `META-INF/*.c2pa` stores, and AI custom properties.
- **Three cleanup levels + opt-in watermark disruption.** Light (invisible-character and C2PA text-wrapper hygiene), Standard (plus visible punctuation and safe phrasing), Aggressive (plus machine-residue stripping and lossless C2PA/container metadata scrubbing), and opt-in `--dewatermark` (deterministic 5-gram tournament window disruption for statistical token watermarks).
- **Cross-format conversion & custom packs.** Supported document inputs (Markdown, plain text, HTML, or DOCX) can be converted to Markdown, plain text, DOCX, or PDF via the clean-then-convert workflow, and custom YAML/JSON rule packs (`--pack`) support `flag`, `replace`, and `delete` actions.
- **CLI and desktop app.** A fast command-line tool (`aiclean`) and a native desktop app (`aiclean-gui`) that share the same engine.

## What it cleans

**Invisible and "smuggling" characters.** Zero-width spaces, the narrow no-break space (`U+202F`, a ChatGPT tell), the math-mode invisibles (`U+2061`–`U+2064`) that leak out of reasoning traces, Unicode tag and reserved default-ignorable format characters, stray variation selectors (including the Mongolian and musical-notation format blocks), blank filler glyphs (`U+3164`, `U+FFA0`, `U+2800`), exotic Unicode spaces, raw C0/C1 control characters and "noncharacters" that are guaranteed never to appear in real text, complete ANSI/ECMA-48 terminal sequences, and bidirectional override controls (the "Trojan Source" vector). Mixed-script homoglyph words (Latin mixed with Cyrillic/Greek/Armenian/Cherokee lookalikes), Private-Use-Area runs, combining-mark ("Zalgo") runs, and zero-width runs that can encode hidden bits are reported; only the latter's narrowly identified Latin/invisible-run contexts are removed at Aggressive.

**Encoding damage.** UTF-8 that a middleware decoded as Windows-1252 arrives as mojibake (`â€™`, `â€œ`, `Ã©`), often double- or triple-encoded; `typo.mojibake.repair` maps it back to bytes and re-decodes it, iterating until stable, while `café` and `© 2026` are left alone. HTML entities (`&nbsp;`, `&mdash;`, `&#8217;`) left by a copy-as-source paste are decoded (without double-decoding `&amp;lt;`), and raw HTML tags (`<p>`, `<br>`, `<span style=…>`) in prose are reported. Repaired characters go through the same level-aware punctuation mapping as intact text, so the result is idempotent.

**Document shape.** Raw Markdown left in a Word or text file (`**bold**`, `## headings`, fences, `- [ ]`), Title Case headings, runs of three or more bold-label list items ("- **Speed:** faster" — the inline-header vertical list), and decorative or structural emoji. These are the most-measured tells of 2026 (Markdown in plain text at 12× the human rate, bold at 43×, headings at 23×, bullets at 9×) and are flagged, never rewritten.

**Chat-UI chrome.** The disclaimer line every assistant prints under its answer ("ChatGPT can make mistakes. Check important info."), role headers ("You said:", "ChatGPT said:"), buttons ("Copy code", "Regenerate response") and progress lines ("Searching the web") — all interface furniture that survives copy-paste. The disclaimer is flagged from Light and removed at Aggressive.

**Typographic tells.** Em dashes, en dashes, curly quotes and guillemets, primes and acute accents used as quotes, typographic ligatures (ﬁ ﬂ), the ellipsis character and two-dot leader, the hyphen family (`U+2010`–`U+2012`, the minus sign), CJK fullwidth punctuation, CJK punctuation in English text, tokenizer spacing artifacts (`Ġ`, `▁`, and space-before-punctuation "detokenized" text), and repeated `!!!`/`???` punctuation.

**Machine residue.** Chat-template control tokens — the known ones (`<|im_start|>`, `<|eot_id|>`, `<start_of_turn>`, `｜end▁of▁sentence｜`, `[INST]`, `antml:`) and *unknown* `<|…|>` / fullwidth-pipe tokens caught by pattern — chat-UI citation plumbing in every form the UIs store it (`【9†source】`, `【45†L75-L83】`, the invisible Private-Use wrappers around `turn0search0`-style IDs, `oai_citation`, `utm_source=chatgpt.com` left on cited links), leaked reasoning blocks (`<think>…</think>`) and tool-call scaffolding (`<invoke>`, `[TOOL_CALLS]`), unfilled template placeholders (`[Your Name]`, `[INSERT SOURCE]`), and coding-agent attribution trailers (`Co-Authored-By: Claude … <noreply@anthropic.com>`, `🤖 Generated with [Claude Code]`) — including the same patterns naming models released after this build. The exact Claude Code 2.1.91–2.1.196 prompt-date steganography (`Today’/ʼ/ʹs date is …`, U+2019/U+02BC/U+02B9, plus slash-separated dates) is restored to canonical ASCII at Light; clean lines and ordinary date prose are left alone.

**DOCX, HTML, Markdown front matter, and C2PA / Content Credentials.** Word documents are cleaned everywhere prose can hide: body, headers, footers, footnotes, endnotes, and reviewer comments. At the container layer, `llm-cleanup` validates ISO/IEC 19566-5 JUMBF superboxes (`c2pa` UUID `63327061-0011-0010-8000-00AA00389B71`) and AI provenance across formats, reporting them at every level and scrubbing them losslessly at Aggressive:
- **Images & SVG (`.png`, `.jpg`, `.webp`, `.svg`, and `word/media/*` inside `.docx`)**: PNG `caBX` chunks, AI XMP `iTXt`/`tEXt` packets (IPTC `trainedAlgorithmicMedia`), Stable Diffusion `parameters` and ComfyUI node-graph chunks; JPEG single- and multi-segment `APP11` (`JP` XTBox) JUMBF stores and `APP1` XMP/Extended XMP (leaving camera EXIF and post-`SOS` entropy scan bytes bit-exact); WebP `C2PA` and AI `XMP ` RIFF chunks (updating `RIFF` size and `VP8X` flags); SVG `<metadata>` and `<?xpacket?>` packets.
- **DOCX packages**: `META-INF/content_credential.c2pa` stores (plus `[Content_Types].xml` overrides), generator stamps in `docProps/{core,app}.xml`, EU AI Act / tool-stamped custom properties in `docProps/custom.xml` (`AIGenerated`, `AIDisclosure`, `ai_model`), and external `.rels` tracking URLs.
- **HTML, Markdown & text**: HTML `<link rel="c2pa-manifest">`, `<script type="application/c2pa">`, AI `<meta>` declarations, and `<a href>` tracking parameters; Markdown YAML/TOML AI front matter and reference-link tracking URLs; C2PA 2.4 Structured Text (`-----BEGIN C2PA MANIFEST-----`) blocks; and C2PA Unstructured Text (`U+FEFF` + `C2PATXT\0` variation-selector binary frames, stripped whole at Light).

**Wording and style, flagged for your review.** Overused vocabulary — weighted by the published evidence (Kobak et al.'s 15.1M-abstract excess-word measurement, the WikiProject AI Cleanup watch list, Graphite's "AI Tells" corpus), not vibes — plus copula avoidance ("serves as" for "is"), negative parallelism ("not just X, but Y"), trailing "-ing" clauses, superlative framing ("the single most"), sycophancy ("great question!"), unearned authority ("studies show"), canned notability ("has garnered attention"), source-availability hedges ("based on the information available"), "despite the challenges" endings, social-post hooks, letter formulas, promotional puffery and marketing formulas. French and Spanish packs also flag common assistant formulas and vocabulary (for example, "il est important de noter" and "cabe destacar"). On standard runs these are *reported*, never rewritten: single "AI-sounding" words are ordinary language, and changing them would change what you wrote.

**Opt-in statistical watermark disruption (`--dewatermark`).** Token-sampling watermarks — Google **SynthID-Text** (Gemini, and adopted on Claude 5.x models from August 2026) and OpenAI **textGrain** — bias word choices over 5-gram token windows (`H = 4` preceding context tokens). Standard runs never rewrite prose words; passing `--dewatermark` applies a strictly acyclic, idempotent table of truth-preserving phrasal compressions, discourse connectors, and closed-class canonicalizations (while shielding direct quotations, URLs, numbers, and code/identifiers) and reports the exact fraction of 5-gram windows invalidated.

### What it still cannot remove or certify

- **Keyed statistical watermark certification (`SynthID-Text`, `textGrain`).** `--dewatermark` deterministically breaks overlapping 5-gram tournament windows and degrades windowed score accumulation, *but without the vendor's secret key no offline tool can verify or certify zero residual signal*, especially on very short or tightly constrained passages.
- **Pixel- and audio-domain imperceptible watermarks (`SynthID-Image`, `SynthID-Audio`, `AudioSeal`, `Tree-Ring`).** Because `aiclean` strips C2PA and XMP chunks *losslessly* without decoding or re-encoding pixels, signals embedded directly in pixel/DCT coefficients or audio waveforms remain intact.
- **External soft-binding registries.** Stripping embedded C2PA manifests removes all in-file provenance, but a remote registry that previously indexed a perceptual hash of the asset can still match it out-of-band.

## Install

Download a prebuilt binary from the [Releases](https://github.com/Olib-AI/llm-cleanup/releases) page, or build from source:

```bash
git clone https://github.com/Olib-AI/llm-cleanup.git
cd llm-cleanup
cargo build --release
# binaries land in target/release/: aiclean (CLI) and aiclean-gui (desktop app)
```

Building requires **Rust 1.99.1**. The pin lives in `rust-toolchain.toml`, so `cargo` picks it
up automatically; run `rustup toolchain install 1.99.1` once if you use rustup.

### Windows

`aiclean.exe` is a single self-contained executable — no runtime, no sidecar files.

`aiclean-gui.exe` is built against the dynamic MSVC runtime, so it needs the Microsoft Visual
C++ runtime. The Windows download already contains it: `vcruntime140.dll`,
`vcruntime140_1.dll`, `msvcp140.dll` and the rest of the redistributable set are deployed
**app-local** next to the executable, so the app starts on a clean Windows install with no
redistributable prompt and no administrator rights. Keep the folder together: if you move
`aiclean-gui.exe`, copy the `.dll` files with it. See `WINDOWS-RUNTIME.txt` inside the zip.

If you build the GUI yourself, `scripts/package-windows.ps1 -Verify` produces the same layout
and fails loudly if the runtime is missing.

## Usage

Clean a file (writes a `.cleaned` copy next to it by default):

```bash
aiclean clean report.docx --level standard
aiclean clean notes.md --level aggressive
aiclean clean hero.png --level aggressive          # losslessly strip C2PA caBX & AI XMP chunks
aiclean clean draft.md --level aggressive --dewatermark
aiclean clean draft.md --pack custom-rules.yaml
```

Preview the changes without writing anything:

```bash
aiclean diff report.docx
```

Convert while cleaning (the output extension picks the target format):

```bash
aiclean clean report.docx -o report.md      # clean, then convert to Markdown
aiclean clean notes.md --to docx            # clean, then convert to Word
aiclean clean report.docx -o report.pdf     # clean, then create a text-only PDF
aiclean convert report.docx -o report.txt   # convert only, no cleaning
```

List the active rules for a level:

```bash
aiclean rules --level aggressive
```

See which models and fingerprints this build knows about (and what it deliberately cannot do):

```bash
aiclean coverage
aiclean coverage --provider anthropic
```

### Desktop app

Launch `aiclean-gui` (or open `llm-cleanup.app` on macOS). Choose a file, pick a cleanup level and an output format, review exactly what changed, and save.

## Security

- The workspace denies `unsafe` (`unsafe_code = "deny"` in `Cargo.toml`); there is no `unsafe` block in the tree.
- `cargo audit` runs on every pull request, every push to `main`, and **daily** against the
  live RustSec advisory database, so a newly published advisory fails the build even when
  nothing changed here. Run it locally with `scripts/audit.sh`.
- Ignored advisories are triaged individually, and only when the affected code path is
  unreachable from this workspace. Every ignore records its reason and owner in
  `.cargo/audit.toml`; the full table is in [SECURITY.md](SECURITY.md).
- Dependabot watches the `cargo` ecosystem weekly. Parser crates (`quick-xml`, `zip`,
  `pulldown-cmark`) are grouped into their own pull request for review.
- The toolchain is pinned to Rust 1.99.1 in `rust-toolchain.toml`, and CI verifies that the
  pin and the compiler that ran the audit are the same version.

## How it works

Each format is parsed only to locate the editable prose. Rules run over that prose, producing edits that are spliced back into the original bytes. After writing, the tool re-parses the output and asserts the structure is unchanged, so a corrupt or reflowed document is never produced. For DOCX, only the text inside the `w:t` runs is touched — in every prose-bearing part of the package (body, headers, footers, footnotes, endnotes, comments) — and every other part, including images and shapes, is copied through byte for byte. Package metadata fingerprints are reported separately from the prose and, at Aggressive, the matched metadata fields are emptied after the structural check has passed.

## License

MIT. Copyright (c) 2026 Olib AI. See [LICENSE](LICENSE).
