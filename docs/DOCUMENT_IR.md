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
            │               dropped statements are removed at block/cell │
            │               granularity, coordinates of survivors kept   │
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

## IR shape

`cleanup_core::doc` is split into three layers:

* `model` — raw page geometry (`LayoutDoc`, `TextRun`, `RuleSegment`, `FillRect`,
  `ImagePlacement`) and the interpreted IR (`Doc`, `Block`, `Inline`, `Table`, `Row`,
  `Cell`, `Figure`, `Callout`, `Columns`). Raw geometry is what the PDF reader produces;
  the IR is what everything downstream consumes.
* `analyze` — pure, deterministic layout analysis. It is unit-tested against synthetic
  `LayoutDoc`s, so the hard geometry rules are testable without a PDF.
* `png` — a small PNG encoder (for extracted raster XObjects) and decoder (for embedding
  the same assets back into PDF output). No image crate, no system libraries.

## Exporters

| Target | Fidelity strategy |
| --- | --- |
| Markdown | GFM tables when the grid is rectangular; semantic HTML `<table>` with `colspan`/`rowspan` when cells are merged or multi-paragraph. Images are written to `./assets/` and referenced relatively. |
| Plain text | Linearised reading order (columns are emitted column-by-column, never interleaved), box-drawing tables, `[figure: …]` markers. |
| DOCX | Hand-written OpenXML: `styles.xml` with real heading/quote/code styles, `w:tbl` with `w:tblBorders`/`w:tcMar`/`w:gridSpan`/`w:vMerge`, inline `wp:inline` drawings against `word/media/*` parts, `w:sectPr` with `w:cols` for multi-column regions, page geometry taken from the source page. |
| PDF | Native byte-splice cleaning for PDF→PDF (layout untouched by construction); the IR renderer re-typesets other sources with an embedded font, rules, tables and raster images. |

## Module map (as built)

| Path | Role |
| --- | --- |
| `cleanup-core/src/doc/model.rs` | The IR types. `LayoutDoc`/`PageLayout`/`TextRun`/`RuleSegment`/`FillRect`/`ImagePlacement` are the raw geometry; `Doc`/`Block`/`Table`/`Cell`/`Figure`/`Callout` are the interpreted structure. Every block keeps an `Option<Anchor>`. |
| `cleanup-core/src/doc/raster.rs` | Dependency-free PNG encode/decode and PNG/JPEG dimension probing, so an extracted raster needs no image crate. |
| `cleanup-core/src/doc/analyze.rs` | Pure `LayoutDoc → (Doc, Vec<String>)`: words→lines, column bands, headings, lists, code, callouts, captions, rule-cluster tables (with merged cells and alignment voting), cross-page table stitching, running furniture. |
| `cleanup-formats/src/pdf/layout.rs` | The spatial PDF reader: text state (`Tc`/`Tw`/`Tz`/`TL`/`Ts`), `Tm`/`Td`/`TD`/`T*`, paths, clipping, Form XObjects, images (JPEG passthrough, otherwise PNG), `/Rotate` warnings. |
| `cleanup-convert/src/input.rs` | Readers for Markdown (pulldown-cmark), HTML and DOCX (quick-xml), plain text, and PDF (via the two modules above). `input::build` is the single entry point. |
| `cleanup-convert/src/sanitize.rs` | The only mutation point: shared rule catalogue over block text plus block-level boilerplate removal. |
| `cleanup-convert/src/export/{markdown,text,docx,pdf}.rs` | One renderer per target; each is `&Doc → Rendered`. |
| `cleanup-convert/src/lib.rs` | `convert_document` wires reader → sanitizer → exporter and returns bytes, side-car assets and a report. |

## Reading-order and grid heuristics worth remembering

* **A baseline is not always one line.** Runs that share a baseline are merged only while the
  horizontal gap stays under ~1.6 em; a wider gap is a column gutter, and merging across it is
  exactly how two-column pages used to interleave.
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

`scripts/generate_fixtures.py` writes five deterministic PDFs into
`crates/cleanup-convert/tests/fixtures/` (reportlab + pillow; the PDFs are committed so `cargo test`
needs no Python). `crates/cleanup-convert/tests/ir_pipeline.rs` then asserts, per fixture, that
table cells and spans survive, that rasters are extracted and re-embedded, that the DOCX package is
well-formed OpenXML, that columns linearise left-to-right, that assistant slop is removed while
genuine content stays, and that no Unicode character is dropped.
