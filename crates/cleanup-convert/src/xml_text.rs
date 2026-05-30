//! Helpers for pulling decoded prose out of a DOCX on the conversion side. (The lossless
//! parser keeps `<w:t>` text raw/escaped; for conversion we want the decoded text.)

use crate::Result;
use cleanup_core::Format;

/// Decode the five standard XML entities. `&amp;` is decoded last so escaped entities like
/// `&amp;lt;` round-trip to the literal `&lt;`.
pub fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Extract a DOCX's prose as one decoded string per paragraph, in document order.
pub fn docx_paragraph_texts(src: &[u8]) -> Result<Vec<String>> {
    let doc = cleanup_formats::Docx.parse(src)?;
    Ok(doc.spans.iter().map(|s| xml_unescape(&s.text)).collect())
}
