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
4. **Splice, never re-render.** A rule's replacement is re-encoded in the same font and
   written into the same string operand. `Tm`, `Td`, `Tf`, colour, path and image
   operators are not touched, so text keeps its position, font, size and styling.
5. **Repack byte-for-byte.** Unmodified objects — including every image XObject and
   embedded font stream — are copied verbatim; only edited content-stream objects are
   rewritten and the xref table is regenerated. With no edits, the output is the input,
   byte for byte. The pipeline's structural self-check compares a canonical skeleton
   (every object, image/font streams hashed, content-stream operators with prose
   operands blanked) before and after, and aborts rather than emit a changed layout.

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
- **Subset-font glyphs can be missing.** If a replacement character has no glyph in the
  text's own font encoding, that single edit is skipped instead of rendering the wrong
  glyph.
- **Unsupported stream filters** (e.g. `LZWDecode`, `JBIG2Decode`, `JPXDecode` content)
  leave the affected stream untouched.
- **Digital signatures** are reported: any cleaned copy necessarily stops validating.
- Converting a PDF **to** Markdown/text/DOCX is extractive (reading-order prose only;
  layout and images are not reconstructed). Keep “Same as input” / PDF output to
  preserve the document exactly.

A ready-made fixture is in `samples/pdf/report.pdf` (Flate content, split `TJ` runs, an
embedded image, an AI-stamped Info dictionary and AI XMP):

```bash
aiclean diff samples/pdf/report.pdf
aiclean clean samples/pdf/report.pdf --level aggressive
aiclean clean samples/pdf/report.pdf -o report   # extension added: report.pdf
```
