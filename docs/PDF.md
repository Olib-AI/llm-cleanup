# PDF support

`llm-cleanup` cleans PDFs **natively**. Choosing “Same as input” (or `aiclean clean
report.pdf`) never flattens a PDF to text and re-typesets it: the page tree, page boxes,
fonts, vector drawing operators, annotations, links, outlines, form objects, and embedded
images stay in place while only the prose inside text-showing operators changes.

## How a PDF is cleaned

1. **Resolve the current file.** Classic cross-reference tables, cross-reference streams,
   hybrid (`XRefStm`) files, and incremental updates are followed newest-first; objects
   stored in object streams (`ObjStm`) are materialized. A damaged xref falls back to a
   sequential object scan.
2. **Locate prose.** Each page's content streams — plus reachable Form XObjects — are
   decoded (`FlateDecode` incl. PNG/TIFF predictors, `ASCIIHexDecode`, `ASCII85Decode`,
   `RunLengthDecode`). Strings shown by `Tj`, `TJ`, `'` and `"` become prose segments,
   grouped per `BT … ET` block the way DOCX groups `<w:t>` runs per paragraph, so a
   phrase split across several string operands is still matched as one phrase.
3. **Translate glyph codes.** Simple-font encodings (`WinAnsi`, `MacRoman`, and
   `Differences`) and `ToUnicode` CMaps (for composite/Identity-H fonts) map encoded
   bytes to Unicode for the shared rule set, with an exact byte-boundary map back into
   the original string operand — including octal/hex escapes.
4. **Edit without re-typesetting.** Replacements use the original font encoding. For each
   affected string, declared font advances and `Tc`/`Tw` are used to insert a compensating
   `TJ` displacement, so the following glyphs retain their original positions, including
   under `Tz` and transformed text matrices. The original `Tm`, `Td`, `Tf`, colour, path,
   and image operators are retained. A replacement wider than the removed text can still
   overlap adjacent ink; no reflow is attempted.
5. **Repack unchanged objects.** Unmodified objects — including image XObjects and embedded
   font streams — are copied verbatim; edited content streams and the xref are regenerated.
   With no edits, the output is byte-identical. The structural self-check fingerprints every
   object and the original content operators after restoring text-free templates for the
   generated compensations; original `TJ` adjustments are not ignored. Position regressions
   additionally compare neighboring text coordinates before and after editing.

## Metadata and embedded images

Reported at every level, and scrubbed at **Aggressive**:

- **Document information dictionary** (`/Title`, `/Author`, `/Creator`, `/Producer`,
  `/Subject`, `/Keywords`) values naming AI tools or generators are emptied.
- **XMP metadata streams** carrying C2PA / Content Credentials, IPTC
  `trainedAlgorithmicMedia`, or an AI creating-tool claim are replaced with an empty
  XMP packet (catalog and image `/Metadata` alike).
- **JPEG (`DCTDecode`) image XObjects** are inspected and scrubbed with the same
  lossless logic as standalone JPEGs (C2PA `APP11` JUMBF, AI XMP). Other image
  streams are copied byte-for-byte; pixels are never decoded or re-encoded.

## Honest limits

- **Password-protected (encrypted) PDFs are rejected** with a clear error.
- **Scanned / image-only pages have no prose to clean.** They are reported
  (`container.pdf-no-extractable-text`); there is deliberately **no OCR**. Metadata and
  embedded-JPEG handling above still applies.
- **Text in fonts without a usable Unicode map** (e.g. a CID font with no `ToUnicode`)
  is reported (`container.pdf-undecodable-text`) and left untouched.
- **Some edits cannot be positioned safely.** If a replacement glyph is missing, a font
  width is unavailable, an edit splits a multi-character glyph, spacing or font resources
  are ambiguously inherited across streams/forms, or text uses vertical or non-Identity CID
  positioning, native editing returns an error rather than emitting a potentially shifted
  result. Exact built-in metrics are used only for unembedded base-14 fonts; compatible font
  aliases are insufficient to guarantee native positions.
- **Unsupported stream filters** (e.g. `LZWDecode`, `JBIG2Decode`, `JPXDecode` content)
  leave the affected stream untouched.
- **Digital signatures** are reported: any cleaned copy necessarily stops validating.
- Converting a PDF **to** Markdown/text/DOCX goes through the document IR (see
  `DOCUMENT_IR.md`): geometry, headings, lists, tables with spans, columns, fonts/colours
  and embedded images *are* reconstructed. Text is not streamed: the reader emits one
  `Glyph` per character code with its advance box and exact baseline, `Tc`/`Tw`/`Tz` and
  `TJ` displacements folded into that advance, and `doc::cluster` turns glyphs into whole
  words before anything else looks at them. That is what keeps
  `[(Kerning)-40(TJ)] TJ` as `KerningTJ` rather than the `K e r n i n g T J` that a
  per-chunk tokeniser produces. That conversion is still a re-render into a new
  container, so it is lossy by definition — choose “Same as input” / PDF output when the
  requirement is that the original layout be preserved exactly. A PDF→PDF conversion is an
  identity operation; native cleaning uses the position-compensated path described above.
  Reconstructed conversion is approximate: rotated pages, inline images, image masks, and
  image `/Decode` arrays are not fully supported. ICC image profiles are approximated by
  component count rather than colour-managed. Unsupported features generate warnings where
  detected, and the original PDF remains the best choice when fidelity is essential.

A ready-made fixture is in `samples/pdf/report.pdf` (Flate content, split `TJ` runs, an
embedded image, an AI-stamped Info dictionary and AI XMP):

```bash
aiclean diff samples/pdf/report.pdf
aiclean clean samples/pdf/report.pdf --level aggressive
aiclean clean samples/pdf/report.pdf -o report   # extension added: report.pdf
```
