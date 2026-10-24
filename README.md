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

llm-cleanup strips the tell-tale residue that large language models leave behind (em dashes, curly quotes, the narrow no-break space, invisible "smuggling" characters, chat-template tokens, citation plumbing, agent attribution trailers, overused wording) from Markdown, plain text, and Word documents. It does this without breaking a single byte of your formatting: headings, bold, italic, lists, tables, links, code blocks, images, and shapes are preserved exactly. There is no network call and no LLM at runtime.

The rule set is reviewed against the model landscape each release. Coverage as of **October 2026** spans GPT-5.5/5.6/6, Claude Opus 5.x and Fable/Mythos 5.1, Gemini 3.x, Llama 4, Grok 4/5, Mistral Large 3, DeepSeek V4, Qwen3.8, Kimi K2.5–K3, GLM-5, Copilot, Perplexity Sonar and the 2026 crop of coding agents. Run `aiclean coverage` to print it, or read [docs/FINGERPRINTS.md](docs/FINGERPRINTS.md) for the sources.

## Why llm-cleanup

- **Formatting-safe by construction.** It never re-serializes your document. It locates the prose, cleans only that text, and splices the result back into the original bytes. If nothing matches, the output is byte-for-byte identical to the input.
- **Offline and deterministic.** No network, no model, no telemetry. The same input always produces the same output.
- **Real document support.** Markdown, plain text, and Word (.docx). DOCX keeps your images, shapes, tables, and styles intact while cleaning the text inside the runs.
- **Three cleanup levels.** Light (invisible-character hygiene only), Standard (plus visible punctuation and safe phrasing), and Aggressive (plus opt-in rewrites and stylistic flags).
- **Cross-format conversion.** Clean a .docx and save it as Markdown or text, or turn Markdown into a Word document, in one step.
- **CLI and desktop app.** A fast command-line tool (`aiclean`) and a native desktop app (`aiclean-gui`) that share the same engine.

## What it cleans

**Invisible and "smuggling" characters.** Zero-width spaces, the narrow no-break space (`U+202F`, a ChatGPT tell), the math-mode invisibles (`U+2061`–`U+2064`) that leak out of reasoning traces, Unicode tag characters, stray variation selectors, blank filler glyphs (`U+3164`, `U+FFA0`, `U+2800`), exotic Unicode spaces, and bidirectional override controls (the "Trojan Source" vector). Mixed-script homoglyph words are reported.

**Typographic tells.** Em dashes, en dashes, curly quotes and guillemets, the ellipsis character, the hyphen family (`U+2010`–`U+2012`, the minus sign), and CJK fullwidth punctuation.

**Machine residue.** Chat-template control tokens (`<|im_start|>`, `<|eot_id|>`, `｜end▁of▁sentence｜`, `[INST]`, `antml:`), chat-UI citation plumbing (`【9†source】`, `【45†L75-L83】`, `turn0search`, `oai_citation`), leaked reasoning blocks (`<think>…</think>`), and coding-agent attribution trailers (`Co-Authored-By: Claude … <noreply@anthropic.com>`, `🤖 Generated with [Claude Code]`).

**Wording and style, flagged for your review.** Overused vocabulary, negative parallelism ("not just X, but Y"), superlative framing ("the single most"), sycophancy ("great question!"), unearned authority ("studies show"), decorative heading emoji, and raw Markdown left in a Word or text file. These are *reported*, never rewritten: single "AI-sounding" words are ordinary English, and changing them would change what you wrote.

### What it cannot remove

Statistical token watermarks live in *which words the model chose*, not in characters, so no text cleanup can remove them: Google **SynthID** (Gemini), Anthropic's watermark on Claude models released from 2026-08-02, and OpenAI **textGrain** (EU ChatGPT/Codex, announced 2026-10-05). Cryptographically signed **C2PA / Content Credentials** are file metadata rather than prose, and Office "author" metadata (`docProps/`) is a separate, not-yet-implemented feature. We would rather say this plainly than imply a cleaner defeats them.

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
```

Preview the changes without writing anything:

```bash
aiclean diff report.docx
```

Convert while cleaning (the output extension picks the target format):

```bash
aiclean clean report.docx -o report.md      # clean, then convert to Markdown
aiclean clean notes.md --to docx            # clean, then convert to Word
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

Each format is parsed only to locate the editable prose. Rules run over that prose, producing edits that are spliced back into the original bytes. After writing, the tool re-parses the output and asserts the structure is unchanged, so a corrupt or reflowed document is never produced. For DOCX, only the text inside the `w:t` runs is touched; every other part of the package, including images and shapes, is copied through byte for byte.

## License

MIT. Copyright (c) 2026 Olib AI. See [LICENSE](LICENSE).
