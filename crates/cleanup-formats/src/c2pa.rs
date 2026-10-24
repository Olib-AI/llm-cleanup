//! Lossless C2PA (Coalition for Content Provenance and Authenticity) JUMBF inspector and
//! media/text container scrubber.
//!
//! Covers ISO/IEC 19566-5 JUMBF superboxes (`jumb` / `jumd`) carrying the C2PA 2.x UUID
//! family (`0011-0010-8000-00AA00389B71`), container embeddings in PNG (`caBX`, `iTXt`,
//! `tEXt`, `eXIf`), JPEG (`APP11` multi-segment XTBox `JP` and `APP1` XMP / Extended XMP),
//! WebP (`C2PA`, `XMP `, `EXIF` RIFF chunks with `VP8X` flag and `RIFF` size fixup), SVG
//! (`<metadata>` and `<?xpacket ...?>`), and C2PA 2.4 Structured Text comment / front-matter
//! blocks (`-----BEGIN C2PA MANIFEST-----` ... `-----END C2PA MANIFEST-----`).
//!
//! Every operation is pure safe Rust and strictly lossless: image pixel streams (`IDAT`,
//! JPEG entropy data after `SOS`, WebP `VP8 `/`VP8L`) are copied verbatim and never decoded
//! or re-encoded.

use cleanup_core::ContainerFinding;
use std::ops::Range;

/// Shared 12-byte tail of every UUID registered by the C2PA specification:
/// `XXXXXXXX-0011-0010-8000-00AA00389B71`.
const C2PA_UUID_TAIL: [u8; 12] = [
    0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

/// Four-byte ASCII box prefixes used in the high 32 bits of C2PA JUMBF UUIDs.
const C2PA_UUID_PREFIXES: &[[u8; 4]] = &[
    *b"c2pa", // Manifest Store
    *b"c2ma", // Manifest
    *b"c2md", // Update Manifest
    *b"c2as", // Assertion Store
    *b"c2cl", // Claim
    *b"c2cs", // Claim Signature
    *b"c2vc", // Verifiable Credentials
    *b"c2db", // Data Box
];

const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const XMP_STD_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXT_HEADER: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";

/// Known AI claim generators / software agents as they appear inside JUMBF claims, XMP
/// packets, PNG text chunks, and EXIF `Software` tags.
const AI_MEDIA_GENERATORS: &[(&str, &str)] = &[
    ("openai", "OpenAI"),
    ("chatgpt", "ChatGPT"),
    ("dall-e", "DALL-E"),
    ("dall\u{00b7}e", "DALL-E"),
    ("gpt-4o", "GPT-4o"),
    ("gpt-image", "GPT-Image"),
    ("anthropic", "Anthropic"),
    ("claude", "Claude"),
    ("adobe firefly", "Adobe Firefly"),
    ("firefly", "Adobe Firefly"),
    ("google imagen", "Google Imagen"),
    ("imagen", "Google Imagen"),
    ("synthid", "Google SynthID"),
    ("gemini", "Google Gemini"),
    ("microsoft designer", "Microsoft Designer"),
    ("bing image creator", "Bing Image Creator"),
    ("copilot", "Microsoft Copilot"),
    ("midjourney", "Midjourney"),
    ("stable diffusion", "Stable Diffusion"),
    ("comfyui", "ComfyUI"),
    ("automatic1111", "Stable Diffusion WebUI"),
    ("novelai", "NovelAI"),
    ("black forest labs", "Black Forest Labs Flux"),
    ("grok", "xAI Grok"),
    ("aurora", "xAI Aurora"),
    ("ideogram", "Ideogram"),
    ("recraft", "Recraft"),
    ("qwen-vl", "Qwen"),
    ("c2pa-rs", "c2pa-rs"),
    ("c2patool", "c2patool"),
];

pub const C2PA_BEGIN_BANNER: &str = "-----BEGIN C2PA MANIFEST-----";
pub const C2PA_END_BANNER: &str = "-----END C2PA MANIFEST-----";

/// Summary extracted from a validated C2PA JUMBF superbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct C2paSummary {
    pub box_kind: &'static str,
    pub claim_generator: Option<&'static str>,
}

impl C2paSummary {
    pub fn describe(&self, location: &str) -> String {
        match self.claim_generator {
            Some(gen_name) => format!(
                "{location} carries a C2PA Content Credentials manifest ({}, generator: {gen_name})",
                self.box_kind
            ),
            None => format!(
                "{location} carries a C2PA Content Credentials manifest ({})",
                self.box_kind
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Png,
    Jpeg,
    Webp,
    Svg,
}

/// Detect whether `bytes` begins with PNG, JPEG, WebP, or SVG content.
pub fn sniff_media(bytes: &[u8]) -> Option<MediaKind> {
    if bytes.starts_with(PNG_SIG) {
        return Some(MediaKind::Png);
    }
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some(MediaKind::Jpeg);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(MediaKind::Webp);
    }
    if looks_like_svg(bytes) {
        return Some(MediaKind::Svg);
    }
    None
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head_len = bytes.len().min(2048);
    let Ok(head) = std::str::from_utf8(&bytes[..head_len]) else {
        return false;
    };
    let trimmed = head.trim_start_matches('\u{FEFF}').trim_start();
    if trimmed.starts_with("<svg")
        && trimmed[4..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '>')
    {
        return true;
    }
    if (trimmed.starts_with("<?xml")
        || trimmed.starts_with("<!DOCTYPE svg")
        || trimmed.starts_with("<!--"))
        && head.to_ascii_lowercase().contains("<svg")
    {
        return true;
    }
    false
}

/// Parse one ISO BMFF box header at `offset`, returning `(box_type, payload_start, box_end)`.
fn parse_bmff_box(bytes: &[u8], offset: usize) -> Option<([u8; 4], usize, usize)> {
    let hdr = bytes.get(offset..offset + 8)?;
    let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
    let box_type = [hdr[4], hdr[5], hdr[6], hdr[7]];
    match size32 {
        0 => Some((box_type, offset + 8, bytes.len())),
        1 => {
            let ext = bytes.get(offset + 8..offset + 16)?;
            let size64 = u64::from_be_bytes([
                ext[0], ext[1], ext[2], ext[3], ext[4], ext[5], ext[6], ext[7],
            ]);
            let size = usize::try_from(size64).ok()?;
            if size < 16 {
                return None;
            }
            let end = offset.checked_add(size)?;
            if end > bytes.len() {
                return None;
            }
            Some((box_type, offset + 16, end))
        }
        2..=7 => None,
        n => {
            let end = offset.checked_add(n)?;
            if end > bytes.len() {
                return None;
            }
            Some((box_type, offset + 8, end))
        }
    }
}

fn c2pa_uuid_kind(uuid: &[u8; 16]) -> Option<&'static str> {
    if uuid[4..16] != C2PA_UUID_TAIL {
        return None;
    }
    let prefix = [uuid[0], uuid[1], uuid[2], uuid[3]];
    match &prefix {
        b"c2pa" => Some("JUMBF c2pa store"),
        b"c2ma" => Some("JUMBF c2ma manifest"),
        b"c2md" => Some("JUMBF c2md update manifest"),
        b"c2as" => Some("JUMBF c2as assertions"),
        b"c2cl" => Some("JUMBF c2cl claim"),
        b"c2cs" => Some("JUMBF c2cs signature"),
        _ if C2PA_UUID_PREFIXES.contains(&prefix) => Some("JUMBF C2PA box"),
        _ => None,
    }
}

/// Inspect raw bytes (e.g. a `.c2pa` sidecar/archive entry, a PNG `caBX` payload, or a WebP
/// `C2PA` chunk) for a valid ISO/IEC 19566-5 JUMBF superbox with a C2PA UUID.
pub fn inspect_jumbf_c2pa(bytes: &[u8]) -> Option<C2paSummary> {
    let mut cursor = 0usize;
    while cursor + 8 <= bytes.len() {
        let Some((box_type, payload_start, box_end)) = parse_bmff_box(bytes, cursor) else {
            break;
        };
        if &box_type == b"jumb"
            && let Some((child_type, child_payload, child_end)) =
                parse_bmff_box(bytes, payload_start)
            && &child_type == b"jumd"
            && child_end <= box_end
            && let Some(uuid_slice) = bytes.get(child_payload..child_payload + 16)
        {
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(uuid_slice);
            if let Some(box_kind) = c2pa_uuid_kind(&uuid) {
                return Some(C2paSummary {
                    box_kind,
                    claim_generator: sniff_generator_ascii(bytes),
                });
            }
        }
        if box_end <= cursor {
            break;
        }
        cursor = box_end;
    }

    // Fragmented JPEG APP11 first packet can pass a truncated outer `jumb` length larger than
    // the single APP11 segment while still containing a complete `jumd` description box at the
    // start of the JUMBF payload. Check that exact prefix layout as well.
    inspect_jumbf_prefix(bytes)
}

/// Check the beginning of a potentially multi-segment JUMBF stream whose outer `jumb` length
/// exceeds the first packet.
pub fn inspect_jumbf_prefix(bytes: &[u8]) -> Option<C2paSummary> {
    if bytes.len() < 32 {
        return None;
    }
    let size32 = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if &bytes[4..8] != b"jumb" {
        return None;
    }
    let jumd_offset = match size32 {
        1 => {
            if bytes.len() < 40 {
                return None;
            }
            16
        }
        0 | 8.. => 8,
        _ => return None,
    };
    let (child_type, child_payload, _) = parse_bmff_box(bytes, jumd_offset)?;
    if &child_type != b"jumd" {
        return None;
    }
    let uuid_slice = bytes.get(child_payload..child_payload + 16)?;
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(uuid_slice);
    let box_kind = c2pa_uuid_kind(&uuid)?;
    Some(C2paSummary {
        box_kind,
        claim_generator: sniff_generator_ascii(bytes),
    })
}

fn sniff_generator_ascii(bytes: &[u8]) -> Option<&'static str> {
    let cap = bytes.len().min(65_536);
    let mut lower_ascii = String::with_capacity(cap);
    for &b in &bytes[..cap] {
        if b.is_ascii_graphic() || b == b' ' {
            lower_ascii.push(char::from(b.to_ascii_lowercase()));
        } else {
            lower_ascii.push(' ');
        }
    }
    for (needle, label) in AI_MEDIA_GENERATORS {
        if needle.is_ascii() && contains_token_bounded(&lower_ascii, needle) {
            return Some(label);
        }
    }
    None
}

fn contains_token_bounded(haystack_lower: &str, needle: &str) -> bool {
    let bytes = haystack_lower.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = haystack_lower[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let right_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if left_ok && right_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XmpVerdict {
    C2pa { generator: Option<&'static str> },
    AiProvenance { detail: &'static str },
}

fn classify_xmp(xmp_bytes: &[u8]) -> Option<XmpVerdict> {
    let text = String::from_utf8_lossy(xmp_bytes);
    let lower = text.to_ascii_lowercase();
    let generator = find_ai_generator_in_text(&lower);

    if lower.contains("c2pa:")
        || lower.contains("http://c2pa.org")
        || lower.contains("https://c2pa.org")
        || lower.contains("dcterms:provenance")
        || lower.contains("contentcredentials")
    {
        return Some(XmpVerdict::C2pa { generator });
    }
    if lower.contains("trainedalgorithmicmedia")
        || lower.contains("compositewithtrainedalgorithmicmedia")
        || lower.contains("algorithmicmedia")
        || lower.contains("digitalsourcetype") && generator.is_some()
    {
        return Some(XmpVerdict::AiProvenance {
            detail: generator.unwrap_or("IPTC AI digitalSourceType"),
        });
    }
    if let Some(gen_name) = generator
        && (lower.contains("creatortool")
            || lower.contains("softwareagent")
            || lower.contains("dc:creator")
            || lower.contains("ai-generated")
            || lower.contains("xmp:"))
    {
        return Some(XmpVerdict::AiProvenance { detail: gen_name });
    }
    None
}

fn find_ai_generator_in_text(lower: &str) -> Option<&'static str> {
    for (needle, label) in AI_MEDIA_GENERATORS {
        if *needle == "c2pa-rs" || *needle == "c2patool" {
            continue;
        }
        if contains_token_bounded(lower, needle) {
            return Some(label);
        }
    }
    None
}

fn extract_extended_xmp_guid(xmp_bytes: &[u8]) -> Option<[u8; 32]> {
    let text = String::from_utf8_lossy(xmp_bytes);
    let needle = "HasExtendedXMP";
    let idx = text.find(needle)?;
    let rest = &text[idx + needle.len()..];
    // Attribute syntax: HasExtendedXMP="0123456789ABCDEF..." or element content.
    for quote in ['"', '\''] {
        if let Some(q1) = rest.find(quote) {
            let after_q1 = &rest[q1 + 1..];
            if after_q1.len() >= 32
                && after_q1.as_bytes()[..32]
                    .iter()
                    .all(|b| b.is_ascii_hexdigit())
                && after_q1.as_bytes().get(32) == Some(&(quote as u8))
            {
                let mut guid = [0u8; 32];
                guid.copy_from_slice(&after_q1.as_bytes()[..32]);
                return Some(guid);
            }
        }
    }
    None
}

/// Inspect a standalone or embedded image/SVG payload for C2PA manifests and AI provenance
/// metadata. `label` names the target in human messages (e.g. `"PNG image"` or
/// `"word/media/image1.png"`).
pub fn inspect_media(bytes: &[u8], label: &str) -> Vec<ContainerFinding> {
    match sniff_media(bytes) {
        Some(MediaKind::Png) => inspect_or_scrub_png(bytes, label, false).0,
        Some(MediaKind::Jpeg) => inspect_or_scrub_jpeg(bytes, label, false).0,
        Some(MediaKind::Webp) => inspect_or_scrub_webp(bytes, label, false).0,
        Some(MediaKind::Svg) => inspect_or_scrub_svg(bytes, label, false).0,
        None => Vec::new(),
    }
}

/// Losslessly scrub C2PA manifests and AI provenance metadata from a PNG, JPEG, WebP, or SVG
/// payload, returning `Some(cleaned_bytes)` only when at least one chunk/segment was removed.
pub fn scrub_media(bytes: &[u8]) -> Option<Vec<u8>> {
    match sniff_media(bytes)? {
        MediaKind::Png => inspect_or_scrub_png(bytes, "PNG image", true).1,
        MediaKind::Jpeg => inspect_or_scrub_jpeg(bytes, "JPEG image", true).1,
        MediaKind::Webp => inspect_or_scrub_webp(bytes, "WebP image", true).1,
        MediaKind::Svg => inspect_or_scrub_svg(bytes, "SVG graphic", true).1,
    }
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------

fn inspect_or_scrub_png(
    bytes: &[u8],
    label: &str,
    build_scrubbed: bool,
) -> (Vec<ContainerFinding>, Option<Vec<u8>>) {
    if !bytes.starts_with(PNG_SIG) {
        return (Vec::new(), None);
    }
    let mut findings = Vec::new();
    let mut drop_ranges: Vec<Range<usize>> = Vec::new();
    let mut cursor = PNG_SIG.len();
    let mut saw_iend = false;

    while cursor < bytes.len() {
        let Some(hdr) = bytes.get(cursor..cursor + 8) else {
            return (Vec::new(), None);
        };
        let data_len = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        let chunk_type = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let Some(chunk_end) = cursor.checked_add(12).and_then(|n| n.checked_add(data_len)) else {
            return (Vec::new(), None);
        };
        if chunk_end > bytes.len() {
            return (Vec::new(), None);
        }
        let data = &bytes[cursor + 8..cursor + 8 + data_len];

        match &chunk_type {
            b"caBX" => {
                let msg = if let Some(summary) = inspect_jumbf_c2pa(data) {
                    summary.describe(&format!("{label} (PNG caBX chunk)"))
                } else {
                    format!("{label} (PNG caBX chunk) carries a C2PA Content Credentials payload")
                };
                findings.push(ContainerFinding {
                    rule_id: "container.c2pa-manifest".to_string(),
                    message: msg,
                    scrubbable: true,
                    applied: false,
                });
                drop_ranges.push(cursor..chunk_end);
            }
            b"iTXt" | b"tEXt" => {
                if let Some((rule_id, msg)) = classify_png_text_chunk(&chunk_type, data, label) {
                    findings.push(ContainerFinding {
                        rule_id: rule_id.to_string(),
                        message: msg,
                        scrubbable: true,
                        applied: false,
                    });
                    drop_ranges.push(cursor..chunk_end);
                }
            }
            b"eXIf" => {
                if let Some(gen_name) = sniff_exif_ai_generator(data) {
                    findings.push(ContainerFinding {
                        rule_id: "container.image-ai-metadata".to_string(),
                        message: format!(
                            "{label} (PNG eXIf chunk) names AI generator {gen_name:?}"
                        ),
                        scrubbable: true,
                        applied: false,
                    });
                    drop_ranges.push(cursor..chunk_end);
                }
            }
            b"IEND" => {
                saw_iend = true;
                break;
            }
            _ => {}
        }
        cursor = chunk_end;
    }

    if !saw_iend || drop_ranges.is_empty() {
        return (findings, None);
    }
    let scrubbed = build_scrubbed.then(|| omit_byte_ranges(bytes, &drop_ranges));
    (findings, scrubbed)
}

fn classify_png_text_chunk(
    chunk_type: &[u8; 4],
    data: &[u8],
    label: &str,
) -> Option<(&'static str, String)> {
    let nul = data.iter().position(|&b| b == 0)?;
    let keyword = std::str::from_utf8(&data[..nul]).ok()?;
    let value_bytes = if chunk_type == b"tEXt" {
        &data[nul + 1..]
    } else {
        // iTXt: keyword\0 + compression_flag(1) + compression_method(1) + lang\0 + trans_kw\0 + text
        let rest = data.get(nul + 3..)?;
        let compression_flag = data[nul + 1];
        if compression_flag != 0 {
            return None;
        }
        let lang_nul = rest.iter().position(|&b| b == 0)?;
        let after_lang = rest.get(lang_nul + 1..)?;
        let trans_nul = after_lang.iter().position(|&b| b == 0)?;
        after_lang.get(trans_nul + 1..)?
    };

    if keyword.eq_ignore_ascii_case("XML:com.adobe.xmp") {
        return match classify_xmp(value_bytes)? {
            XmpVerdict::C2pa { generator } => {
                let gen_note = generator
                    .map(|g| format!(" (generator: {g})"))
                    .unwrap_or_default();
                Some((
                    "container.c2pa-manifest",
                    format!("{label} (PNG XMP packet) carries C2PA provenance{gen_note}"),
                ))
            }
            XmpVerdict::AiProvenance { detail } => Some((
                "container.image-ai-metadata",
                format!("{label} (PNG XMP packet) carries AI generation metadata ({detail})"),
            )),
        };
    }

    let text = String::from_utf8_lossy(value_bytes);
    let lower = text.to_ascii_lowercase();
    let kw_lower = keyword.to_ascii_lowercase();

    // Stable Diffusion / Forge `parameters` and ComfyUI `prompt` + `workflow` chunks.
    if kw_lower == "parameters"
        && (lower.contains("steps:")
            || lower.contains("negative prompt:")
            || lower.contains("sampler:"))
    {
        return Some((
            "container.image-ai-metadata",
            format!(
                "{label} (PNG {keyword:?} chunk) carries Stable Diffusion generation parameters"
            ),
        ));
    }
    if matches!(kw_lower.as_str(), "prompt" | "workflow")
        && (lower.contains("\"class_type\"")
            || lower.contains("\"ksampler\"")
            || lower.contains("\"inputs\""))
    {
        return Some((
            "container.image-ai-metadata",
            format!("{label} (PNG {keyword:?} chunk) carries a ComfyUI generation graph"),
        ));
    }
    if matches!(
        kw_lower.as_str(),
        "software" | "comment" | "description" | "source" | "author" | "ai-generated"
    ) && let Some(gen_name) = find_ai_generator_in_text(&lower)
    {
        return Some((
            "container.image-ai-metadata",
            format!("{label} (PNG {keyword:?} chunk) names AI generator {gen_name:?}"),
        ));
    }
    None
}

fn sniff_exif_ai_generator(exif_bytes: &[u8]) -> Option<&'static str> {
    let cap = exif_bytes.len().min(32_768);
    let mut lower = String::with_capacity(cap);
    for &b in &exif_bytes[..cap] {
        if b.is_ascii_graphic() || b == b' ' {
            lower.push(char::from(b.to_ascii_lowercase()));
        } else {
            lower.push(' ');
        }
    }
    if lower.contains("trainedalgorithmicmedia") {
        return Some("IPTC trainedAlgorithmicMedia");
    }
    find_ai_generator_in_text(&lower)
}

// ---------------------------------------------------------------------------
// JPEG
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct JpegSegment {
    range: Range<usize>,
    marker: u8,
    payload: Range<usize>,
}

fn parse_jpeg_header_segments(bytes: &[u8]) -> Option<Vec<JpegSegment>> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut segments = Vec::new();
    let mut cursor = 2usize;

    while cursor < bytes.len() {
        if bytes[cursor] != 0xFF {
            return None;
        }
        let seg_start = cursor;
        while cursor < bytes.len() && bytes[cursor] == 0xFF {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return None;
        }
        let marker = bytes[cursor];
        cursor += 1;

        match marker {
            0x00 => return None,
            0xD9 => break,                  // EOI without SOS
            0xDA => break, // SOS: compressed entropy stream starts after SOS header; stop here
            0x01 | 0xD0..=0xD7 => continue, // standalone markers
            _ => {
                let len_bytes = bytes.get(cursor..cursor + 2)?;
                let seg_len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
                if seg_len < 2 {
                    return None;
                }
                let seg_end = cursor.checked_add(seg_len)?;
                if seg_end > bytes.len() {
                    return None;
                }
                segments.push(JpegSegment {
                    range: seg_start..seg_end,
                    marker,
                    payload: (cursor + 2)..seg_end,
                });
                cursor = seg_end;
            }
        }
    }
    Some(segments)
}

struct JpegXtBox<'a> {
    en: u16,
    tbox: [u8; 4],
    first_bmff: &'a [u8],
    continuation: &'a [u8],
}

/// Parse an ISO/IEC 19566-5 Annex B `JP` XTBox header inside a JPEG `APP11` (`0xFFEB`)
/// payload.
fn parse_jpeg_xt_box(payload: &[u8]) -> Option<JpegXtBox<'_>> {
    if payload.len() < 16 || &payload[0..2] != b"JP" {
        return None;
    }
    let en = u16::from_be_bytes([payload[2], payload[3]]);
    let lbox = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let tbox = [payload[12], payload[13], payload[14], payload[15]];
    let body_offset = if lbox == 1 {
        if payload.len() < 24 {
            return None;
        }
        24
    } else {
        16
    };
    Some(JpegXtBox {
        en,
        tbox,
        first_bmff: &payload[8..],
        continuation: &payload[body_offset..],
    })
}

fn inspect_or_scrub_jpeg(
    bytes: &[u8],
    label: &str,
    build_scrubbed: bool,
) -> (Vec<ContainerFinding>, Option<Vec<u8>>) {
    let Some(segments) = parse_jpeg_header_segments(bytes) else {
        return (Vec::new(), None);
    };

    // Pass 1: reassemble APP11 `JP` `jumb` instances by `En` (capped at 128 KiB per instance
    // for inspection) and inspect APP1 XMP / EXIF headers.
    let mut app11_groups: Vec<(u16, Vec<usize>, Vec<u8>)> = Vec::new();
    let mut ext_xmp_guids: Vec<[u8; 32]> = Vec::new();
    let mut findings = Vec::new();
    let mut drop_flags = vec![false; segments.len()];

    for (idx, seg) in segments.iter().enumerate() {
        let payload = &bytes[seg.payload.clone()];
        if seg.marker == 0xEB
            && let Some(xt) = parse_jpeg_xt_box(payload)
            && &xt.tbox == b"jumb"
        {
            if let Some((_, indices, assembled)) = app11_groups
                .iter_mut()
                .find(|(known, _, _)| *known == xt.en)
            {
                indices.push(idx);
                if assembled.len() < 131_072 {
                    let take = xt.continuation.len().min(131_072 - assembled.len());
                    assembled.extend_from_slice(&xt.continuation[..take]);
                }
            } else {
                let mut assembled = Vec::new();
                let take = xt.first_bmff.len().min(131_072);
                assembled.extend_from_slice(&xt.first_bmff[..take]);
                app11_groups.push((xt.en, vec![idx], assembled));
            }
        } else if seg.marker == 0xE1 && payload.starts_with(XMP_STD_HEADER) {
            let xmp_body = &payload[XMP_STD_HEADER.len()..];
            if let Some(verdict) = classify_xmp(xmp_body) {
                if let Some(guid) = extract_extended_xmp_guid(xmp_body) {
                    ext_xmp_guids.push(guid);
                }
                drop_flags[idx] = true;
                match verdict {
                    XmpVerdict::C2pa { generator } => {
                        let gen_note = generator
                            .map(|g| format!(" (generator: {g})"))
                            .unwrap_or_default();
                        findings.push(ContainerFinding {
                            rule_id: "container.c2pa-manifest".to_string(),
                            message: format!(
                                "{label} (JPEG APP1 XMP) carries C2PA provenance{gen_note}"
                            ),
                            scrubbable: true,
                            applied: false,
                        });
                    }
                    XmpVerdict::AiProvenance { detail } => {
                        findings.push(ContainerFinding {
                            rule_id: "container.image-ai-metadata".to_string(),
                            message: format!(
                                "{label} (JPEG APP1 XMP) carries AI generation metadata ({detail})"
                            ),
                            scrubbable: true,
                            applied: false,
                        });
                    }
                }
            }
        } else if seg.marker == 0xE1
            && payload.starts_with(b"Exif\0\0")
            && let Some(gen_name) = sniff_exif_ai_generator(&payload[6..])
        {
            drop_flags[idx] = true;
            findings.push(ContainerFinding {
                rule_id: "container.image-ai-metadata".to_string(),
                message: format!("{label} (JPEG EXIF) names AI generator {gen_name:?}"),
                scrubbable: true,
                applied: false,
            });
        }
    }

    for (_en, indices, assembled) in &app11_groups {
        let Some(summary) = inspect_jumbf_c2pa(assembled) else {
            continue;
        };
        for &idx in indices {
            drop_flags[idx] = true;
        }
        let frag_count = indices.len();
        let loc = if frag_count > 1 {
            format!("{label} (JPEG APP11, {frag_count} segments)")
        } else {
            format!("{label} (JPEG APP11)")
        };
        findings.push(ContainerFinding {
            rule_id: "container.c2pa-manifest".to_string(),
            message: summary.describe(&loc),
            scrubbable: true,
            applied: false,
        });
    }

    if !ext_xmp_guids.is_empty() {
        for (idx, seg) in segments.iter().enumerate() {
            let payload = &bytes[seg.payload.clone()];
            if seg.marker == 0xE1
                && payload.starts_with(XMP_EXT_HEADER)
                && let Some(guid_slice) =
                    payload.get(XMP_EXT_HEADER.len()..XMP_EXT_HEADER.len() + 32)
                && ext_xmp_guids
                    .iter()
                    .any(|g| g.eq_ignore_ascii_case(guid_slice))
            {
                drop_flags[idx] = true;
            }
        }
    }

    let drop_ranges: Vec<Range<usize>> = segments
        .iter()
        .zip(drop_flags)
        .filter(|&(_, drop_it)| drop_it)
        .map(|(seg, _)| seg.range.clone())
        .collect();

    if drop_ranges.is_empty() {
        return (findings, None);
    }
    let scrubbed = build_scrubbed.then(|| omit_byte_ranges(bytes, &drop_ranges));
    (findings, scrubbed)
}

// ---------------------------------------------------------------------------
// WebP
// ---------------------------------------------------------------------------

fn inspect_or_scrub_webp(
    bytes: &[u8],
    label: &str,
    build_scrubbed: bool,
) -> (Vec<ContainerFinding>, Option<Vec<u8>>) {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return (Vec::new(), None);
    }
    let mut findings = Vec::new();
    let mut drop_ranges: Vec<Range<usize>> = Vec::new();
    let mut cursor = 12usize;
    let mut kept_xmp = false;
    let mut kept_exif = false;

    while cursor < bytes.len() {
        let Some(hdr) = bytes.get(cursor..cursor + 8) else {
            return (Vec::new(), None);
        };
        let fourcc = [hdr[0], hdr[1], hdr[2], hdr[3]];
        let chunk_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        let Some(data_end) = cursor.checked_add(8).and_then(|n| n.checked_add(chunk_len)) else {
            return (Vec::new(), None);
        };
        if data_end > bytes.len() {
            return (Vec::new(), None);
        }
        let padded_end = if chunk_len % 2 == 1 && data_end < bytes.len() {
            data_end + 1
        } else {
            data_end
        };
        let data = &bytes[cursor + 8..data_end];

        match &fourcc {
            b"C2PA" | b"c2pa" => {
                let msg = if let Some(summary) = inspect_jumbf_c2pa(data) {
                    summary.describe(&format!("{label} (WebP C2PA chunk)"))
                } else {
                    format!("{label} (WebP C2PA chunk) carries a C2PA Content Credentials payload")
                };
                findings.push(ContainerFinding {
                    rule_id: "container.c2pa-manifest".to_string(),
                    message: msg,
                    scrubbable: true,
                    applied: false,
                });
                drop_ranges.push(cursor..padded_end);
            }
            b"XMP " => {
                if let Some(verdict) = classify_xmp(data) {
                    let (rule_id, msg) = match verdict {
                        XmpVerdict::C2pa { generator } => {
                            let gen_note = generator
                                .map(|g| format!(" (generator: {g})"))
                                .unwrap_or_default();
                            (
                                "container.c2pa-manifest",
                                format!(
                                    "{label} (WebP XMP chunk) carries C2PA provenance{gen_note}"
                                ),
                            )
                        }
                        XmpVerdict::AiProvenance { detail } => (
                            "container.image-ai-metadata",
                            format!(
                                "{label} (WebP XMP chunk) carries AI generation metadata ({detail})"
                            ),
                        ),
                    };
                    findings.push(ContainerFinding {
                        rule_id: rule_id.to_string(),
                        message: msg,
                        scrubbable: true,
                        applied: false,
                    });
                    drop_ranges.push(cursor..padded_end);
                } else {
                    kept_xmp = true;
                }
            }
            b"EXIF" => {
                if let Some(gen_name) = sniff_exif_ai_generator(data) {
                    findings.push(ContainerFinding {
                        rule_id: "container.image-ai-metadata".to_string(),
                        message: format!(
                            "{label} (WebP EXIF chunk) names AI generator {gen_name:?}"
                        ),
                        scrubbable: true,
                        applied: false,
                    });
                    drop_ranges.push(cursor..padded_end);
                } else {
                    kept_exif = true;
                }
            }
            _ => {}
        }
        cursor = padded_end;
    }

    if drop_ranges.is_empty() {
        return (findings, None);
    }
    if !build_scrubbed {
        return (findings, None);
    }

    let mut out = omit_byte_ranges(bytes, &drop_ranges);
    if out.len() >= 8 {
        let riff_len = u32::try_from(out.len() - 8).unwrap_or(u32::MAX);
        out[4..8].copy_from_slice(&riff_len.to_le_bytes());
    }
    // Update VP8X presence flags (bit 0x04 = XMP, bit 0x08 = EXIF) when present.
    if out.len() >= 24 && &out[12..16] == b"VP8X" {
        let vp8x_len = u32::from_le_bytes([out[16], out[17], out[18], out[19]]) as usize;
        if vp8x_len >= 4 {
            if !kept_xmp {
                out[20] &= !0x04;
            }
            if !kept_exif {
                out[20] &= !0x08;
            }
        }
    }
    (findings, Some(out))
}

// ---------------------------------------------------------------------------
// SVG
// ---------------------------------------------------------------------------

fn inspect_or_scrub_svg(
    bytes: &[u8],
    label: &str,
    build_scrubbed: bool,
) -> (Vec<ContainerFinding>, Option<Vec<u8>>) {
    let Ok(src) = std::str::from_utf8(bytes) else {
        return (Vec::new(), None);
    };
    let lower = src.to_ascii_lowercase();
    let mut findings = Vec::new();
    let mut drop_ranges: Vec<Range<usize>> = Vec::new();

    // 1. `<metadata ...>...</metadata>` elements carrying C2PA or AI provenance.
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("<metadata") {
        let start = from + rel;
        let after_tag = start + "<metadata".len();
        if !lower[after_tag..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '>' || c == ':' || c == '/')
        {
            from = after_tag;
            continue;
        }
        let Some(end_rel) = lower[after_tag..].find("</metadata>") else {
            break;
        };
        let mut end = after_tag + end_rel + "</metadata>".len();
        end = extend_through_line_break(src, start, end);
        let block = &src[start..end];
        if let Some(verdict) = classify_xmp(block.as_bytes()) {
            push_svg_verdict(&mut findings, label, verdict);
            drop_ranges.push(start..end);
        }
        from = end;
    }

    // 2. Top-level `<?xpacket ...?>...<?xpacket end=...?>` packets outside `<metadata>`.
    let mut xfrom = 0usize;
    while let Some(rel) = lower[xfrom..].find("<?xpacket") {
        let start = xfrom + rel;
        let after_open = start + "<?xpacket".len();
        let Some(next_pi) = lower[after_open..].find("<?xpacket") else {
            break;
        };
        let close_start = after_open + next_pi;
        let Some(close_end_rel) = lower[close_start..].find("?>") else {
            break;
        };
        let mut end = close_start + close_end_rel + 2;
        end = extend_through_line_break(src, start, end);
        let overlaps_metadata = drop_ranges.iter().any(|r| r.start < end && start < r.end);
        if !overlaps_metadata && let Some(verdict) = classify_xmp(&src.as_bytes()[start..end]) {
            push_svg_verdict(&mut findings, label, verdict);
            drop_ranges.push(start..end);
        }
        xfrom = end;
    }

    // 3. C2PA structured-text XML comments `<!-- -----BEGIN C2PA MANIFEST----- ... -->`.
    for range in find_structured_text_manifests(src) {
        if !drop_ranges
            .iter()
            .any(|r| r.start < range.end && range.start < r.end)
        {
            findings.push(ContainerFinding {
                rule_id: "container.c2pa-manifest".to_string(),
                message: format!("{label} carries a C2PA structured-text manifest comment"),
                scrubbable: true,
                applied: false,
            });
            drop_ranges.push(range);
        }
    }

    drop_ranges.sort_by_key(|r| (r.start, r.end));
    if drop_ranges.is_empty() {
        return (findings, None);
    }
    let scrubbed = build_scrubbed.then(|| omit_byte_ranges(bytes, &drop_ranges));
    (findings, scrubbed)
}

fn push_svg_verdict(findings: &mut Vec<ContainerFinding>, label: &str, verdict: XmpVerdict) {
    match verdict {
        XmpVerdict::C2pa { generator } => {
            let gen_note = generator
                .map(|g| format!(" (generator: {g})"))
                .unwrap_or_default();
            findings.push(ContainerFinding {
                rule_id: "container.c2pa-manifest".to_string(),
                message: format!("{label} (<metadata>) carries C2PA provenance{gen_note}"),
                scrubbable: true,
                applied: false,
            });
        }
        XmpVerdict::AiProvenance { detail } => {
            findings.push(ContainerFinding {
                rule_id: "container.image-ai-metadata".to_string(),
                message: format!("{label} (<metadata>) carries AI generation metadata ({detail})"),
                scrubbable: true,
                applied: false,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// C2PA 2.4 Structured-Text Manifest Blocks
// ---------------------------------------------------------------------------

/// Locate complete C2PA 2.4 Structured-Text manifest blocks
/// (`-----BEGIN C2PA MANIFEST-----` ... `-----END C2PA MANIFEST-----`), expanding to the
/// enclosing HTML/XML comment (`<!-- ... -->`), block comment (`/* ... */`), or standalone
/// line(s) when the block occupies the entire comment/line.
pub fn find_structured_text_manifests(src: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = src[from..].find(C2PA_BEGIN_BANNER) {
        let begin_pos = from + rel;
        let after_begin = begin_pos + C2PA_BEGIN_BANNER.len();
        let Some(end_rel) = src[after_begin..].find(C2PA_END_BANNER) else {
            break;
        };
        let end_pos = after_begin + end_rel + C2PA_END_BANNER.len();
        let inner = src[after_begin..after_begin + end_rel].trim();
        // Guard against prose/docs merely talking about the banner strings with paragraphs in
        // between: real C2PA structured-text payloads are a URI or `data:application/c2pa;base64,...`
        // (with optional line-comment prefixes `#` / `//` / `--`).
        if !looks_like_structured_manifest_payload(inner) {
            from = end_pos;
            continue;
        }

        let (expanded_start, expanded_end) = expand_structured_wrapper(src, begin_pos, end_pos);
        ranges.push(expanded_start..expanded_end);
        from = expanded_end;
    }
    ranges
}

fn looks_like_structured_manifest_payload(inner: &str) -> bool {
    if inner.is_empty() || inner.len() > 262_144 {
        return false;
    }
    let mut cleaned = String::new();
    for line in inner.lines() {
        let t = line
            .trim()
            .trim_start_matches('#')
            .trim_start_matches("//")
            .trim_start_matches("--")
            .trim_start_matches('*')
            .trim();
        cleaned.push_str(t);
    }
    cleaned.starts_with("data:application/c2pa")
        || cleaned.starts_with("https://")
        || cleaned.starts_with("http://")
        || cleaned.starts_with("urn:c2pa:")
}

fn expand_structured_wrapper(src: &str, begin_pos: usize, end_pos: usize) -> (usize, usize) {
    // Case 1: enclosed in `<!-- ... -->` with only whitespace around the manifest block.
    if let Some(comment_open) = src[..begin_pos].rfind("<!--")
        && src[comment_open + 4..begin_pos].trim().is_empty()
        && let Some(close_rel) = src[end_pos..].find("-->")
        && src[end_pos..end_pos + close_rel].trim().is_empty()
    {
        let close_end = end_pos + close_rel + 3;
        return (
            line_start_if_blank_prefix(src, comment_open),
            extend_through_line_break(src, comment_open, close_end),
        );
    }

    // Case 2: enclosed in `/* ... */` with only whitespace/asterisks around the manifest block.
    if let Some(block_open) = src[..begin_pos].rfind("/*")
        && src[block_open + 2..begin_pos]
            .chars()
            .all(|c| c.is_whitespace() || c == '*')
        && let Some(close_rel) = src[end_pos..].find("*/")
        && src[end_pos..end_pos + close_rel]
            .chars()
            .all(|c| c.is_whitespace() || c == '*')
    {
        let close_end = end_pos + close_rel + 2;
        return (
            line_start_if_blank_prefix(src, block_open),
            extend_through_line_break(src, block_open, close_end),
        );
    }

    // Case 3: standalone line(s) (possibly prefixed by `#`, `//`, `--`, or YAML key/indent).
    let line_start = src[..begin_pos].rfind('\n').map_or(0, |n| n + 1);
    let prefix = src[line_start..begin_pos].trim();
    if prefix.is_empty() || matches!(prefix, "#" | "//" | "--") {
        let line_end = extend_through_line_break(src, line_start, end_pos);
        let between = src[end_pos..line_end].trim();
        if between.is_empty() {
            return (line_start, line_end);
        }
    }

    (begin_pos, end_pos)
}

fn line_start_if_blank_prefix(src: &str, pos: usize) -> usize {
    let line_start = src[..pos].rfind('\n').map_or(0, |n| n + 1);
    if src[line_start..pos]
        .bytes()
        .all(|b| matches!(b, b' ' | b'\t'))
    {
        line_start
    } else {
        pos
    }
}

fn extend_through_line_break(src: &str, start: usize, end: usize) -> usize {
    let starts_at_bol = start == 0 || src.as_bytes().get(start - 1) == Some(&b'\n');
    if !starts_at_bol {
        return end;
    }
    let rest = &src.as_bytes()[end..];
    let mut i = 0usize;
    while i < rest.len() && matches!(rest[i], b' ' | b'\t') {
        i += 1;
    }
    if rest.get(i..i + 2) == Some(b"\r\n") {
        end + i + 2
    } else if rest.get(i) == Some(&b'\n') {
        end + i + 1
    } else {
        end
    }
}

pub(crate) fn omit_byte_ranges(bytes: &[u8], drop_ranges: &[Range<usize>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut cursor = 0usize;
    for r in drop_ranges {
        if r.start > cursor {
            out.extend_from_slice(&bytes[cursor..r.start]);
        }
        cursor = cursor.max(r.end);
    }
    if cursor < bytes.len() {
        out.extend_from_slice(&bytes[cursor..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid ISO BMFF `jumb` + `jumd` C2PA Manifest Store payload.
    pub(crate) fn synthetic_c2pa_jumbf(extra_claim_text: &[u8]) -> Vec<u8> {
        let mut jumd_payload = Vec::new();
        jumd_payload.extend_from_slice(b"c2pa");
        jumd_payload.extend_from_slice(&C2PA_UUID_TAIL);
        jumd_payload.push(0x03); // toggles: requestable + label present
        jumd_payload.extend_from_slice(b"c2pa\0");

        let jumd_len = (8 + jumd_payload.len()) as u32;
        let mut cbor_box = Vec::new();
        let claim_len = (8 + extra_claim_text.len()) as u32;
        cbor_box.extend_from_slice(&claim_len.to_be_bytes());
        cbor_box.extend_from_slice(b"cbor");
        cbor_box.extend_from_slice(extra_claim_text);

        let jumb_len = 8 + jumd_len + cbor_box.len() as u32;
        let mut out = Vec::with_capacity(jumb_len as usize);
        out.extend_from_slice(&jumb_len.to_be_bytes());
        out.extend_from_slice(b"jumb");
        out.extend_from_slice(&jumd_len.to_be_bytes());
        out.extend_from_slice(b"jumd");
        out.extend_from_slice(&jumd_payload);
        out.extend_from_slice(&cbor_box);
        out
    }

    fn png_chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + data.len());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(chunk_type);
        out.extend_from_slice(data);
        out.extend_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        out
    }

    #[test]
    fn png_cabx_and_ai_xmp_removed_while_pixels_stay_intact() {
        let ihdr = png_chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        let cabx = png_chunk(
            b"caBX",
            &synthetic_c2pa_jumbf(b"claim_generator: OpenAI ChatGPT"),
        );
        let xmp = b"XML:com.adobe.xmp\0\0\0\0\0<x:xmpmeta><rdf:Description iptcExt:DigitalSourceType=\"http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia\"/></x:xmpmeta>";
        let itxt = png_chunk(b"iTXt", xmp);
        let idat = png_chunk(b"IDAT", b"\x78\x9c\x63\x60\x00\x00\x00\x02\x00\x01");
        let iend = png_chunk(b"IEND", &[]);

        let mut clean_png = Vec::new();
        clean_png.extend_from_slice(PNG_SIG);
        clean_png.extend_from_slice(&ihdr);
        clean_png.extend_from_slice(&idat);
        clean_png.extend_from_slice(&iend);

        let mut signed_png = Vec::new();
        signed_png.extend_from_slice(PNG_SIG);
        signed_png.extend_from_slice(&ihdr);
        signed_png.extend_from_slice(&cabx);
        signed_png.extend_from_slice(&itxt);
        signed_png.extend_from_slice(&idat);
        signed_png.extend_from_slice(&iend);

        let findings = inspect_media(&signed_png, "test.png");
        assert_eq!(findings.len(), 2);
        assert!(
            findings
                .iter()
                .any(|f| f.rule_id == "container.c2pa-manifest" && f.message.contains("OpenAI"))
        );
        assert!(
            findings
                .iter()
                .any(|f| f.rule_id == "container.image-ai-metadata")
        );

        let scrubbed = scrub_media(&signed_png).expect("should scrub PNG");
        assert_eq!(
            scrubbed, clean_png,
            "pixel & header chunks must match byte-for-byte"
        );
        assert!(inspect_media(&scrubbed, "test.png").is_empty());
    }

    #[test]
    fn jpeg_multisegment_app11_c2pa_removed_preserving_camera_exif_and_sos() {
        let jumbf = synthetic_c2pa_jumbf(b"Anthropic Claude C2PA Claim");
        let split = 20;
        let make_app11 = |z: u32, slice: &[u8]| -> Vec<u8> {
            let mut payload = Vec::new();
            payload.extend_from_slice(b"JP");
            payload.extend_from_slice(&1u16.to_be_bytes()); // En = 1
            payload.extend_from_slice(&z.to_be_bytes());
            if z == 1 {
                payload.extend_from_slice(slice); // starts with LBox + b"jumb"
            } else {
                payload.extend_from_slice(&(8 + slice.len() as u32).to_be_bytes());
                payload.extend_from_slice(b"jumb");
                payload.extend_from_slice(slice);
            }
            let seg_len = (2 + payload.len()) as u16;
            let mut seg = vec![0xFF, 0xEB];
            seg.extend_from_slice(&seg_len.to_be_bytes());
            seg.extend_from_slice(&payload);
            seg
        };

        let camera_exif = {
            let payload = b"Exif\0\0Canon EOS R5\0";
            let mut seg = vec![0xFF, 0xE1];
            seg.extend_from_slice(&((2 + payload.len()) as u16).to_be_bytes());
            seg.extend_from_slice(payload);
            seg
        };
        let sos_and_scan = b"\xFF\xDA\x00\x08\x01\x01\x00\x00\x3F\x00\xAB\xCD\xFF\x00\xEF\xFF\xD9";

        let mut clean_jpg = vec![0xFF, 0xD8];
        clean_jpg.extend_from_slice(&camera_exif);
        clean_jpg.extend_from_slice(sos_and_scan);

        let mut signed_jpg = vec![0xFF, 0xD8];
        signed_jpg.extend_from_slice(&camera_exif);
        signed_jpg.extend_from_slice(&make_app11(1, &jumbf[..split]));
        signed_jpg.extend_from_slice(&make_app11(2, &jumbf[split..]));
        signed_jpg.extend_from_slice(sos_and_scan);

        let findings = inspect_media(&signed_jpg, "photo.jpg");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("2 segments"));

        let scrubbed = scrub_media(&signed_jpg).expect("should scrub JPEG");
        assert_eq!(scrubbed, clean_jpg);
    }

    #[test]
    fn webp_c2pa_and_xmp_scrub_updates_riff_length_and_vp8x_flags() {
        let vp8x_payload = [0x04u8, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // XMP flag set
        let vp8_payload = [0x10u8, 0x20, 0x30, 0x40];
        let c2pa_payload = synthetic_c2pa_jumbf(b"Adobe Firefly");
        let xmp_payload =
            b"<x:xmpmeta xmlns:c2pa=\"http://c2pa.org/manifest\">c2pa:manifest</x:xmpmeta>";

        let push_riff_chunk = |buf: &mut Vec<u8>, fourcc: &[u8; 4], data: &[u8]| {
            buf.extend_from_slice(fourcc);
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(data);
            if data.len() % 2 == 1 {
                buf.push(0);
            }
        };

        let mut webp = Vec::new();
        webp.extend_from_slice(b"RIFF\0\0\0\0WEBP");
        push_riff_chunk(&mut webp, b"VP8X", &vp8x_payload);
        push_riff_chunk(&mut webp, b"VP8 ", &vp8_payload);
        push_riff_chunk(&mut webp, b"C2PA", &c2pa_payload);
        push_riff_chunk(&mut webp, b"XMP ", xmp_payload);
        let riff_len = (webp.len() - 8) as u32;
        webp[4..8].copy_from_slice(&riff_len.to_le_bytes());

        let scrubbed = scrub_media(&webp).expect("should scrub WebP");
        let expected_riff_len = (scrubbed.len() - 8) as u32;
        assert_eq!(
            u32::from_le_bytes([scrubbed[4], scrubbed[5], scrubbed[6], scrubbed[7]]),
            expected_riff_len
        );
        assert_eq!(scrubbed[20] & 0x04, 0, "VP8X XMP flag must be cleared");
        assert!(inspect_media(&scrubbed, "img.webp").is_empty());
    }
}
