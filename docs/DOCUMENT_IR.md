# Document IR: why cross-format conversion used to collapse layout

This document is the design record for the re-architected conversion pipeline. It is
deliberately explicit about **root causes**, because the fix is not "add more CSS" — it is
"stop throwing the layout away before rendering".

## Root causes of the old collapses

| Symptom | Root cause in the pre-IR pipeline |
| --- | --- |
| Headings became paragraphs | `cleanup-formats::Pdf::parse` yields `ProseSpan`s whose only structure is *which content stream they came from*. Font size, weight and position are parsed by the PDF adapter but discarded once the span is built, so the converter cannot know that a line was 24 pt bold. |
| Columns were interleaved | Reading order was "stream order". PDF producers emit text in whatever order they painted it; two-column pages therefore produced `left-1, right-1, left-2, right-2`. Nothing in the pipeline ever looked at `Tm/Td` coordinates. |
| Tables became soup | There was no table model at all. `/Length`-delimited rules (`re … f`) and text alignment were never inspected, so cells were flattened into prose lines and cell boundaries were lost as whitespace. |
| Multi-page tables split | Same as above, plus: nothing correlated the last table on page *n* with the first table on page *n+1*, so a repeated header row was emitted as a second, unrelated table. |
| Images vanished | `Docx`/`Markdown`/`Text` exporters only ever received `Vec<String>` paragraphs. Image XObjects were reachable from the PDF adapter but had no representation in `Document`, and the DOCX exporter had no `word/media` writer. |
| Typography collapsed to plain runs | The Markdown→DOCX converter emitted `Paragraph::new().add_run(Run::new().add_text(..))` — no `w:pPr`, no `w:tbl`, no `w:sectPr`, no styles part. Bold/italic were tracked in a flag but only applied to `Run`s; tables and images were explicitly dropped with a warning. |
| Unicode characters disappeared | Converters extracted text through `Vec<String>` join/filter paths, so runs split mid-grapheme (or in a different font) were re-joined without space logic, and characters outside the embedded font's `cmap` were silently replaced at PDF render time. |
| A whole page became one table cell | A page frame, callout card or code-block border is also a closed rectangle of vector rules. Any rule component with two vertical and two horizontal lines was accepted as a table grid, so the decorative box swallowed the page. |
| Two-column bodies still interleaved | A full-width banner (or a callout line straddling the gutter) filled the coverage bitmap used to find white corridors, so no gutter was found. Paint order also returned from the right column to the left; a negative horizontal gap was treated as "close enough" and the runs were merged into one interleaved line. |
| Table text was emitted twice | Detecting a grid never marked its text lines as consumed, so the same cell text appeared once as `w:tc`/`<td>` and once as a giant concatenated paragraph. Those duplicate blocks also sat between page fragments and prevented multi-page stitching. |
| Curly quotes, ellipses and en-dashes changed during conversion | The `typo.*` rules are AI-*cleaning* rules. Applying them while converting a typeset document rewrites legitimate Unicode; preserving typography must be a conversion option, not an accident. |

## What the new pipeline does

```
            ┌────────────────────────────────────────────────────────────┐
  PDF ─────►│ pdf::layout  (spatial reconstruction)                      │
  DOCX ────►│ doc::input   (OpenXML / Markdown / HTML / text readers)    │──► Doc (IR)
  MD/TXT/HTML                                                           │
            └────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
            ┌────────────────────────────────────────────────────────────┐
            │ doc::analyze  — reading order, columns, header/footer,     │
            │                 headings, lists, code, callouts, tables,   │
            │                 multi-page table stitching, figures        │
            └────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
            ┌────────────────────────────────────────────────────────────┐
            │ ir_sanitize — the whole rule catalogue runs on IR blocks;  │
            │               furniture-zone filtering, chat tokens,       │
            │               wrapper quotes and repetition collapse;      │
            │               code blocks are never rewritten              │
            └────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
            ┌────────────────────────────────────────────────────────────┐
            │ Doc::normalize — prune emptied containers, drop orphaned   │
            │                 figures, validate every anchor/asset/span  │
            └────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
      markdown / text / docx (OpenXML) / pdf exporters, all from the same IR
```

Key invariants:

1. **No text loss.** Every run of ink that carries a character ends up in some IR block.
   Sanitization may delete a character, but rendering may never drop one.
2. **Geometry survives analysis.** Every block keeps `anchor: Option<Rect>` — its bounding
   box in the source page — so exporters can lay out with the original intent, and tests
   can prove that a trimmed paragraph stayed where it was.
3. **Sanitization is the only mutation point.** Exporters are pure functions of the IR.
4. **The IR is self-describing.** `DocInfo.page_boundaries` records each source page's ink
   bounds, furniture zones and detected column count; `ImageAsset` records pixel dimensions
   *and* measured placement DPI; `InlineStyle` carries source RGB and hyperlink targets;
   `VectorShape` carries both geometry and a ready-to-emit inline `<svg>`.
5. **The IR re-validates itself after cleaning.** `Doc::normalize()` prunes emptied containers
   and `Doc::validate()` checks anchors, assets, column widths, spans and page order. Empty
   warnings are asserted by the integration suite for every fixture.

## IR shape

`cleanup_core::doc` is split into five layers:

* `model` — raw page geometry (`LayoutDoc`, `Glyph`, `TextRun`, `RuleSegment`, `FillRect`,
  `ImagePlacement`) and the interpreted IR (`Doc`, `Block`, `Inline`, `Table`, `Row`,
  `Cell`, `Figure`, `VectorShape`, `PageBoundary`, `Callout`, `Columns`). Raw geometry is
  what the PDF reader produces; the IR is what everything downstream consumes.
  `InlineStyle` carries bold/italic/underline/strike/code plus an optional RGB colour, a
  link target and the source typography (family, weight, size); `Figure.inline`
  distinguishes a baseline icon from an anchored block image.
* `cluster` — glyph → word reconstruction. The reader emits one `Glyph` per character code
  (advance box, baseline, font style, the font's own space advance); this module is the only
  place that decides where a word ends. A word break needs a gap wider than 75% of the font's
  space advance, avoiding false breaks on modest positive tracking while still detecting ordinary
  `TJ` word gaps. `Tc` tracking, `Tz` scaling and `TJ` kerning (all of which are folded into
  the advance, leaving a gap of zero) never split a word, while a common proportional-font
  `TJ` word gap (`-250`, i.e. 0.25 em) clears the cutoff. Smaller `-200` gaps are deliberately
  treated conservatively because geometry alone cannot distinguish them from wide tracking.
  A style change splits the *run* without
  inserting a space, which is what lets one word carry two fonts.
* `fontmap` — one interpretation of a font name: subset tags (`ABCDEF+`) stripped, style words
  (`-BoldOblique`, `,Italic`, `-Bd`) removed from the family, metric-compatible aliases folded
  (`ArialMT`/`NimbusSans`/`LiberationSans` → `Helvetica`), weight on the `100..=900` scale from
  the name / `/FontWeight` / `/StemV` / `ForceBold`, and the AFM advance tables that keep an
  unembedded base-14 face measurable.
* `analyze` — pure, deterministic layout analysis. It is unit-tested against synthetic
  `LayoutDoc`s, so the hard geometry rules are testable without a PDF.
* `png` — a small PNG encoder (for extracted raster XObjects) and decoder (for embedding
  the same assets back into PDF output). No image crate, no system libraries.

## Exporters

| Target | Fidelity strategy |
| --- | --- |
| Markdown | GFM tables when the grid is rectangular; semantic HTML `<table>` with `<colgroup>` widths, `colspan`/`rowspan` and cell fills when cells are merged or multi-paragraph. GFM alerts (`> [!NOTE]`) for callouts, inline `<span style="color:#…">`/`<u>` for colour and underline, inline `<svg>` for vector rules. Images are written to `./assets/` with a measured-DPI caption and referenced relatively. |
| Plain text | Linearised reading order (columns are emitted column-by-column, never interleaved), aligned box-drawing tables, `[figure: …]` / `[vector: …]` markers with DPI, and residual markup stripped from prose while code blocks stay verbatim. |
| DOCX | Hand-written OpenXML: `styles.xml` with real heading/quote/code styles, `w:tbl` with `w:tblBorders`/`w:tcMar`/`w:gridSpan`/`w:vMerge`/`w:tblHeader`, `w:hyperlink` + external relationships, `w:color`, VML shapes for vector rules, `wp:inline` for icons and `wp:anchor` for block images against `word/media/*` parts, `w:sectPr` with `w:cols` for multi-column regions, page geometry taken from the source page. |
| PDF | Native cleaning uses font metrics and compensating `TJ` adjustments to preserve following text positions, and rejects edits it cannot position safely; the IR renderer re-typesets other sources with embedded fonts, stroked vector paths, `rg` text colour, `/Annots` `/URI` link annotations, tables and raster images. |

## Module map (as built)

| Path | Role |
| --- | --- |
| `cleanup-core/src/doc/model.rs` | The IR types. `LayoutDoc`/`PageLayout`/`TextRun`/`RuleSegment`/`FillRect`/`ImagePlacement` are the raw geometry; `Doc`/`Block`/`Table`/`Cell`/`Figure`/`VectorShape`/`PageBoundary`/`Callout` are the interpreted structure. Every block keeps an `Option<Anchor>`; `Doc::normalize`/`validate` enforce the invariants. |
| `cleanup-core/src/doc/raster.rs` | Dependency-free PNG encode/decode and PNG/JPEG dimension probing, so an extracted raster needs no image crate. |
| `cleanup-core/src/doc/cluster.rs` | Glyph → word clustering with the geometry/space-advance rule, plus `gap_spaces`, the one function that turns a measured gap back into blank characters. Unit-tested without a PDF. |
| `cleanup-core/src/doc/fontmap.rs` | Font-name normalisation: subset tags, style words, metric-compatible aliases, the `100..=900` weight scale from `/FontWeight`/`/StemV`/`ForceBold`, and the AFM advance tables for unembedded base-14 faces. |
| `cleanup-core/src/doc/analyze.rs` | Pure `LayoutDoc → (Doc, Vec<String>)`: words→lines, column bands, headings, lists, code, callouts, captions, rule-cluster tables (with merged cells, alignment voting and internal-separator validation), vector shapes with inline SVG, cross-page table stitching, running furniture, page boundaries and placement DPI. |
| `cleanup-formats/src/pdf/layout.rs` | The spatial PDF reader: text state (`Tc`/`Tw`/`Tz`/`TL`/`Ts`), `Tm`/`Td`/`TD`/`T*`, paths, clipping, Form XObjects, images (JPEG passthrough, otherwise PNG), `/Rotate` warnings. |
| `cleanup-convert/src/input.rs` | Readers for Markdown (pulldown-cmark), HTML and DOCX (quick-xml), plain text, and PDF (via the two modules above). `input::build` is the single entry point. |
| `cleanup-convert/src/sanitize.rs` | The only mutation point: shared rule catalogue over block text, typography-preserving filtering, header/footer-zone removal, chat-token/wrapper-quote/repetition cleanup, block-level boilerplate removal, and `Doc::normalize`. |
| `cleanup-convert/src/export/{markdown,text,docx,pdf}.rs` | One renderer per target; each is `&Doc → Rendered`. |
| `cleanup-convert/src/lib.rs` | `convert_document` wires reader → sanitizer → exporter and returns bytes, side-car assets and a report. |

## Words, spaces and lines

A PDF has no words: it has glyphs, advances and a set of registers that change those advances.
The pipeline therefore reconstructs text in three explicit steps, and each step has exactly one
job.

1. **Glyphs (reader).** `Tj`, every `TJ` element, `'` and `"` all funnel into one emitter that
   appends a `Glyph` with the *advance box* (`x`, `advance`), the exact baseline
   (`page_height − (trm · (pen, rise))`), the declared font style, and the natural space advance
   of the font. `Tc`/`Tw`/`Tz` and `TJ` displacements are folded into the advance, which is why
   a tracked or kerned glyph still ends exactly where the next one starts.
2. **Words (cluster).** Glyphs are bucketed by baseline and ordered left to right, then joined
   while the gap to the previous advance box stays at or below 75% of the font's space advance.
   The more conservative cutoff tolerates positive per-glyph positioning used as character
   tracking without treating every displacement as a space; a typical proportional-font
   `0.25em` word gap still separates. A blank glyph is an unconditional separator; its advance is consumed and
   re-synthesised later. Results are whole words (`Kerning`, not `K e r n i n g`, and not `Kern ing`).
3. **Lines and gaps (analyze).** Words are grouped into lines by baseline (never by box centre,
   which drifts when a 8pt and a 14pt run share a line), and every gap is turned back into
   blank characters: `0` when the boxes touch, one when they do not, and — for monospaced lines
   — as many as the font's own advance accounts for, so a four-space indent stays four spaces.
   Only then are lines clustered into headings, paragraphs, list items, code blocks and table
   cells.

Regressions in any of the three steps have a signature: extra spaces between letters
(`t h i s`), missing spaces where words were separated by a `TJ` gap (`KerningTJarray`), or a
"word" that is really two columns interleaved. `tests/fixtures/kerning_and_typography.pdf`
covers all three: its third page is drawn with raw `TJ` arrays, `Tc`/`Tw` registers and one
`Tm` per glyph, and the integration suite asserts the exact sentences come back.

## Reading-order and grid heuristics worth remembering

* **A baseline is not always one line.** Runs that share a baseline are merged only while the
  horizontal gap stays under ~1.6 em **and is not negative**. A producer that paints the whole
  left column before the right one returns to a smaller x on the next line; a negative gap used
  to pass the "close enough" test and produce `left-1, right-1, left-2` in one line.
* **Only narrow lines may vote on a gutter.** Full-width banners and callout lines that straddle
  a column gap erase the white corridor in a coverage bitmap. Lines wider than 55% of the page's
  ink are excluded from gutter detection; a short final line of a banner is reclassified as
  full-width when it has no twin at the same height across a gutter.
* **A table needs internal separators.** `grid_separator` requires a horizontal rule between two
  rows spanning ≥50% of the grid width and a vertical rule between two columns spanning ≥40% of
  its height, plus at least three clustered column coordinates. A page frame, a rounded card and
  a code-block border have no internal separators and are rejected.
* **Grid text must be consumed.** After a table is built, every line whose centre falls inside
  the rule bounds is marked consumed, otherwise cell text is emitted a second time as prose and
  the duplicates block cross-page stitching.
* **Vector leftovers become nodes, not glyphs.** Meaningful rules that are not table grids or
  page-frame furniture are emitted as `Block::Vector` with a self-contained `<svg>`; they are
  excluded from callout boxing so a code frame never becomes a quote.
* **Columnar lines group by band, not by row.** Full-width lines (banners, section breaks) cut the
  page into bands; inside a band every columnar line is assigned to a gutter-derived span and the
  bands render as `Block::Columns`. Grouping row-by-row produced one `Columns` block per visual
  row, which linearised as left-right-left-right.
* **A rule grid may be open.** Tables drawn without a closing border stop short of their own text,
  so the column grid is extended over the words it contains; presence checks for inner rules are
  clipped to the ruled area, otherwise the last column would merge every row into one tall cell.
* **Disclaimers count anywhere in a block.** Leading-sentence trimming is not enough: readers merge
  wrap-adjacent paragraphs, so `DISCLAIMER_SENTENCES` are also removed from the middle of a
  multi-sentence block (never when the sentence is the block's only one).
* **DOCX stores images on empty elements.** `<a:blip r:embed="rId3"/>` is self-closing, so the
  reader has to inspect `Event::Empty` as well as `Event::Start` or every picture vanishes on the
  round trip.

## Fixtures and tests

`scripts/generate_fixtures.py` writes five canonical PDFs, four compatibility fixtures, and
`manifest.json` into `tests/fixtures/`. Install the pinned versions with
`python3 -m pip install -r scripts/fixture-requirements.txt`, then run
`python3 scripts/generate_fixtures.py --verify` to regenerate in a temporary directory and
compare without modifying committed files. The checked-in corpus was generated with Debian
`fonts-dejavu-core` 2.37-6 (regular, bold, mono and mono-bold). Different font versions can
change the PDFs; optional system italic fonts are intentionally not used by the generator.
The typography fixture draws genuine base-14 bold/oblique faces independently of them.
The PDFs and ground-truth manifest are committed, so `cargo test` needs no
Python or network access:

| Fixture | Ground truth asserted |
| --- | --- |
| `complex_tables.pdf` | 61 rows × 5 columns stitched across three pages (`Table.pages`), repeated header, a second grid with `colspan=3`, `rowspan=2` and multi-line cells, explicit `<colgroup>` widths, native DOCX `w:gridSpan`/`w:vMerge`/`w:tblHeader`. |
| `rich_media.pdf` | 900×500 PNG, 1200×700 JPEG, 320×180 PNG and a 48×24 inline icon, measured DPI, `wp:inline` + `wp:anchor`, VML/SVG vector rules, tinted callouts, relative `assets/` references. |
| `typographic_symbols.pdf` | Greek/Cyrillic/extended-Latin/math/arrow/currency/combining symbols unchanged in `.txt`, `.md` and `word/document.xml`; zero-width probes removed; source RGB colour tags preserved; monospaced code block intact. |
| `multi_column_mixed.pdf` | Four pages of 1→2→3→2→1 column transitions linearised left-to-right, marker order from the manifest, `PageBoundary.columns`, DOCX `w:cols` sections. |
| `ai_slop_samples.pdf` | Preambles, disclaimers, closers, repeated tokens, wrapper quotes and chat-template tokens removed from prose; every genuine numeric fact retained; the intentional code block (containing the same tokens) untouched. |

`crates/cleanup-convert/tests/ir_pipeline.rs` reads the manifest and asserts structure, package
validity, Unicode integrity, anchor stability after cleaning, native PDF byte-splice geometry
preservation, and that all five fixtures convert to all four targets.
`crates/cleanup-convert/tests/exporter_units.rs` builds a synthetic IR to cover hyperlinks,
underline/strike, RGB runs, VML shapes, PDF `/URI` annotations and sanitizer edge cases.
