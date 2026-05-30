use crate::error::{CoreError, Result};
use crate::ir::{Document, SpanAnchor};
use crate::traits::EditSet;
use std::ops::Range;

/// Apply non-overlapping byte-range replacements to `source`, producing a new buffer.
///
/// This is the primitive that guarantees zero style breakage: structure bytes are copied
/// verbatim, only the matched ranges are substituted. Edits may be unsorted; overlapping
/// or out-of-bounds edits are a programming error and return [`CoreError::SelfCheck`].
pub fn splice(source: &[u8], edits: &[(Range<usize>, String)]) -> Result<Vec<u8>> {
    let mut sorted: Vec<&(Range<usize>, String)> = edits.iter().collect();
    sorted.sort_by_key(|(r, _)| r.start);

    let mut prev_end = 0usize;
    for (r, _) in &sorted {
        if r.start < prev_end {
            return Err(CoreError::SelfCheck(format!(
                "overlapping edit at byte {}",
                r.start
            )));
        }
        if r.start > r.end || r.end > source.len() {
            return Err(CoreError::SelfCheck(format!(
                "edit range {}..{} out of bounds (source len {})",
                r.start,
                r.end,
                source.len()
            )));
        }
        prev_end = r.end;
    }

    let mut out = Vec::with_capacity(source.len());
    let mut cursor = 0usize;
    for (r, replacement) in &sorted {
        out.extend_from_slice(&source[cursor..r.start]);
        out.extend_from_slice(replacement.as_bytes());
        cursor = r.end;
    }
    out.extend_from_slice(&source[cursor..]);
    Ok(out)
}

/// Translate span-relative edits into absolute byte edits, for byte-anchored documents
/// (Markdown / plain text). DOCX edits are projected back onto runs by its own adapter.
pub fn resolve_byte_edits(doc: &Document, edits: &EditSet) -> Result<Vec<(Range<usize>, String)>> {
    let mut out = Vec::with_capacity(edits.edits.len());
    for e in &edits.edits {
        let span = doc.spans.get(e.span_index).ok_or_else(|| {
            CoreError::SelfCheck(format!("edit references missing span {}", e.span_index))
        })?;
        match &span.anchor {
            SpanAnchor::Bytes(base) => {
                out.push((
                    base.start + e.range.start..base.start + e.range.end,
                    e.replacement.clone(),
                ));
            }
            // The span text is the concatenation of `segs`. Project the edit (a range within
            // that logical text) onto the byte ranges of the runs it overlaps: the first
            // overlapped run receives the whole replacement, later runs have their overlap
            // trimmed away. Run formatting is untouched because only `<w:t>` text bytes change.
            SpanAnchor::Segments(segs) => {
                let mut logical = 0usize;
                let mut first = true;
                for seg in segs {
                    let (seg_lo, seg_hi) = (logical, logical + (seg.end - seg.start));
                    logical = seg_hi;
                    let ov_lo = e.range.start.max(seg_lo);
                    let ov_hi = e.range.end.min(seg_hi);
                    if ov_lo >= ov_hi {
                        continue;
                    }
                    let byte_lo = seg.start + (ov_lo - seg_lo);
                    let byte_hi = seg.start + (ov_hi - seg_lo);
                    let replacement = if first {
                        e.replacement.clone()
                    } else {
                        String::new()
                    };
                    first = false;
                    out.push((byte_lo..byte_hi, replacement));
                }
            }
        }
    }
    Ok(out)
}

/// Convenience for byte-anchored formats: map edits to absolute ranges and splice.
pub fn render_byte_spliced(doc: &Document, edits: &EditSet) -> Result<Vec<u8>> {
    splice(&doc.source, &resolve_byte_edits(doc, edits)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_edits_are_byte_identical() {
        let src = b"hello world";
        let out = splice(src, &[]).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn splices_in_order() {
        let src = "a X b Y c".as_bytes();
        let edits = vec![(2..3, "1".to_string()), (6..7, "2".to_string())];
        let out = splice(src, &edits).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a 1 b 2 c");
    }

    #[test]
    fn rejects_overlap() {
        let src = b"abcdef";
        let edits = vec![(1..3, "x".to_string()), (2..4, "y".to_string())];
        assert!(splice(src, &edits).is_err());
    }
}
