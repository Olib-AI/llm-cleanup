//! Minimal selectable-text PDF output with an embedded Inter font.
//!
//! The input is first rendered to plain text. Page layout is intentionally simple and lossy,
//! but UTF-8 text remains searchable/copyable through the embedded font's ToUnicode map.

use crate::{ConvertError, ConvertReport, Result};
use cleanup_core::FormatId;
use std::collections::BTreeMap;

const FONT: &[u8] = include_bytes!("../../cleanup-gui/fonts/Inter-Regular.ttf");
const PAGE_LINES: usize = 48;
const LINE_WIDTH: usize = 92;

pub fn convert(from: FormatId, src: &[u8]) -> Result<(Vec<u8>, ConvertReport)> {
    let (text, _) = crate::to_text::convert(from, src)?;
    let text = std::str::from_utf8(&text).map_err(|e| {
        ConvertError::Failed(format!("text conversion produced invalid UTF-8: {e}"))
    })?;
    let font = TrueType::new(FONT)?;
    let lines = wrap_lines(text);
    let pages: Vec<&[String]> = if lines.is_empty() {
        vec![&[]]
    } else {
        lines.chunks(PAGE_LINES).collect()
    };

    let mut unicode_by_gid = BTreeMap::<u16, u32>::new();
    let mut missing = false;
    let mut encoded_pages = Vec::with_capacity(pages.len());
    for page in pages {
        let mut encoded_lines = Vec::with_capacity(page.len());
        for line in page {
            let mut encoded = String::with_capacity(line.len() * 4);
            for ch in line.chars() {
                let glyph = font.glyph_index(ch as u32).filter(|glyph| *glyph != 0);
                let gid = if let Some(glyph) = glyph {
                    unicode_by_gid.entry(glyph).or_insert(ch as u32);
                    glyph
                } else {
                    missing = true;
                    unicode_by_gid.entry(0).or_insert(0xFFFD);
                    0
                };
                encoded.push_str(&format!("{gid:04X}"));
            }
            encoded_lines.push(encoded);
        }
        encoded_pages.push(encoded_lines);
    }

    let pdf = write_pdf(&font, &encoded_pages, &unicode_by_gid)?;
    let mut warnings = vec![
        "PDF output is text-only: document formatting, images, tables and links are not preserved"
            .to_string(),
    ];
    if missing {
        warnings.push("the embedded font does not contain every input glyph; unsupported characters may render as replacement glyphs".to_string());
    }
    Ok((
        pdf,
        ConvertReport {
            engine: "embedded-inter-pdf".into(),
            lossy: true,
            warnings,
        },
    ))
}

fn wrap_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.lines() {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > LINE_WIDTH {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            let mut chars = word.chars().peekable();
            while chars.peek().is_some() {
                if line.chars().count() >= LINE_WIDTH {
                    out.push(std::mem::take(&mut line));
                }
                line.push(chars.next().expect("peeked character"));
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

struct TrueType<'a> {
    data: &'a [u8],
    cmap: Cmap,
    units_per_em: u16,
    glyph_count: u16,
    horizontal_metrics: u16,
    hmtx_offset: usize,
    bbox: [i16; 4],
    ascent: i16,
    descent: i16,
    cap_height: i16,
}

impl<'a> TrueType<'a> {
    fn new(data: &'a [u8]) -> Result<Self> {
        let table = |tag: &[u8; 4]| -> Result<usize> {
            let count = read_u16(data, 4)? as usize;
            for i in 0..count {
                let entry = 12 + i * 16;
                if data.get(entry..entry + 4) == Some(tag.as_slice()) {
                    return read_u32(data, entry + 8).map(|v| v as usize);
                }
            }
            Err(ConvertError::Failed(format!(
                "embedded font is missing the {:?} table",
                tag
            )))
        };
        let head = table(b"head")?;
        let hhea = table(b"hhea")?;
        let maxp = table(b"maxp")?;
        let hmtx_offset = table(b"hmtx")?;
        let cmap_offset = table(b"cmap")?;
        let os2 = table(b"OS/2").ok();
        let units_per_em = read_u16(data, head + 18)?;
        let glyph_count = read_u16(data, maxp + 4)?;
        let horizontal_metrics = read_u16(data, hhea + 34)?;
        if units_per_em == 0 || glyph_count == 0 || horizontal_metrics == 0 {
            return Err(ConvertError::Failed(
                "invalid embedded TrueType metrics".into(),
            ));
        }
        let bbox = [
            read_i16(data, head + 36)?,
            read_i16(data, head + 38)?,
            read_i16(data, head + 40)?,
            read_i16(data, head + 42)?,
        ];
        let ascent = read_i16(data, hhea + 4)?;
        let descent = read_i16(data, hhea + 6)?;
        let cap_height = os2
            .filter(|offset| data.get(*offset + 88..*offset + 90).is_some())
            .and_then(|offset| read_i16(data, offset + 88).ok())
            .unwrap_or(ascent);
        let cmap = Cmap::new(data, cmap_offset)?;
        Ok(Self {
            data,
            cmap,
            units_per_em,
            glyph_count,
            horizontal_metrics,
            hmtx_offset,
            bbox,
            ascent,
            descent,
            cap_height,
        })
    }

    fn glyph_index(&self, codepoint: u32) -> Option<u16> {
        self.cmap.glyph_index(self.data, codepoint)
    }

    fn width(&self, glyph: u16) -> Result<u16> {
        let index = usize::from(glyph.min(self.horizontal_metrics - 1));
        let advance = read_u16(self.data, self.hmtx_offset + index * 4)?;
        Ok(
            ((u32::from(advance) * 1000 + u32::from(self.units_per_em) / 2)
                / u32::from(self.units_per_em)) as u16,
        )
    }

    fn scaled(&self, units: i16) -> i32 {
        (i32::from(units) * 1000) / i32::from(self.units_per_em)
    }
}

enum Cmap {
    Format4(usize),
    Format12(usize),
}

impl Cmap {
    fn new(data: &[u8], offset: usize) -> Result<Self> {
        let count = read_u16(data, offset + 2)? as usize;
        let mut format4 = None;
        let mut format12 = None;
        for index in 0..count {
            let record = offset + 4 + index * 8;
            let platform = read_u16(data, record)?;
            let encoding = read_u16(data, record + 2)?;
            let subtable = offset + read_u32(data, record + 4)? as usize;
            match read_u16(data, subtable)? {
                12 if platform == 3 && encoding == 10 => format12 = Some(subtable),
                12 => {
                    format12.get_or_insert(subtable);
                }
                4 if format4.is_none() => format4 = Some(subtable),
                _ => {}
            }
        }
        format12
            .map(Cmap::Format12)
            .or_else(|| format4.map(Cmap::Format4))
            .ok_or_else(|| {
                ConvertError::Failed("embedded font has no supported Unicode cmap".into())
            })
    }

    fn glyph_index(&self, data: &[u8], codepoint: u32) -> Option<u16> {
        match self {
            Cmap::Format12(offset) => {
                let groups = read_u32(data, offset + 12).ok()? as usize;
                let (mut low, mut high) = (0usize, groups);
                while low < high {
                    let mid = (low + high) / 2;
                    let group = offset + 16 + mid * 12;
                    let start = read_u32(data, group).ok()?;
                    let end = read_u32(data, group + 4).ok()?;
                    if codepoint < start {
                        high = mid;
                    } else if codepoint > end {
                        low = mid + 1;
                    } else {
                        let glyph = read_u32(data, group + 8).ok()? + codepoint - start;
                        return u16::try_from(glyph).ok();
                    }
                }
                None
            }
            Cmap::Format4(offset) => {
                let code = u16::try_from(codepoint).ok()?;
                let segments = usize::from(read_u16(data, offset + 6).ok()? / 2);
                let end_codes = offset + 14;
                let start_codes = end_codes + segments * 2 + 2;
                let deltas = start_codes + segments * 2;
                let range_offsets = deltas + segments * 2;
                for i in 0..segments {
                    let end = read_u16(data, end_codes + i * 2).ok()?;
                    let start = read_u16(data, start_codes + i * 2).ok()?;
                    if code < start || code > end {
                        continue;
                    }
                    let delta = read_i16(data, deltas + i * 2).ok()?;
                    let range = read_u16(data, range_offsets + i * 2).ok()? as usize;
                    if range == 0 {
                        return Some(code.wrapping_add(delta as u16));
                    }
                    let glyph_offset =
                        range_offsets + i * 2 + range + usize::from(code - start) * 2;
                    let glyph = read_u16(data, glyph_offset).ok()?;
                    return Some(if glyph == 0 {
                        0
                    } else {
                        glyph.wrapping_add(delta as u16)
                    });
                }
                None
            }
        }
    }
}

fn write_pdf(
    font: &TrueType<'_>,
    pages: &[Vec<String>],
    unicode_by_gid: &BTreeMap<u16, u32>,
) -> Result<Vec<u8>> {
    let mut objects = vec![Vec::<u8>::new(); 8 + pages.len() * 2];
    let page_ids: Vec<usize> = (0..pages.len()).map(|index| 8 + index * 2).collect();
    let kids = page_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
    objects[2] = format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).into_bytes();
    objects[3] = b"<< /Type /Font /Subtype /Type0 /BaseFont /Inter-Regular /Encoding /Identity-H /DescendantFonts [4 0 R] /ToUnicode 7 0 R >>".to_vec();

    let widths = (0..font.glyph_count)
        .map(|glyph| font.width(glyph).map(|w| w.to_string()))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    objects[4] = format!(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Inter-Regular /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 5 0 R /DW 600 /W [0 [{}]] /CIDToGIDMap /Identity >>",
        widths
    ).into_bytes();

    let bbox = font.bbox.map(|value| font.scaled(value));
    objects[5] = format!(
        "<< /Type /FontDescriptor /FontName /Inter-Regular /Flags 32 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} /CapHeight {} /StemV 80 /FontFile2 6 0 R >>",
        bbox[0], bbox[1], bbox[2], bbox[3], font.scaled(font.ascent), font.scaled(font.descent), font.scaled(font.cap_height)
    ).into_bytes();
    objects[6] = stream_object(
        &format!("/Length {} /Length1 {}", font.data.len(), font.data.len()),
        font.data,
    );
    let cmap = to_unicode_cmap(unicode_by_gid);
    objects[7] = stream_object(&format!("/Length {}", cmap.len()), &cmap);

    for (index, page_lines) in pages.iter().enumerate() {
        let page_id = page_ids[index];
        let content_id = page_id + 1;
        objects[page_id] = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
        ).into_bytes();
        let mut content = String::from("BT\n/F1 10 Tf\n54 744 Td\n14 TL\n");
        for line in page_lines {
            content.push('<');
            content.push_str(line);
            content.push_str("> Tj\nT*\n");
        }
        content.push_str("ET\n");
        objects[content_id] =
            stream_object(&format!("/Length {}", content.len()), content.as_bytes());
    }

    let mut output = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = vec![0usize; objects.len()];
    for (id, object) in objects.iter().enumerate().skip(1) {
        offsets[id] = output.len();
        output.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        output.extend_from_slice(object);
        output.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = output.len();
    output.extend_from_slice(format!("xref\n0 {}\n", objects.len()).as_bytes());
    output.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    output.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len()
        )
        .as_bytes(),
    );
    Ok(output)
}

fn stream_object(dictionary: &str, stream: &[u8]) -> Vec<u8> {
    let mut object = format!("<< {dictionary} >>\nstream\n").into_bytes();
    object.extend_from_slice(stream);
    object.extend_from_slice(b"\nendstream");
    object
}

fn to_unicode_cmap(mappings: &BTreeMap<u16, u32>) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /InterToUnicode def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in mappings.iter().collect::<Vec<_>>().chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (glyph, codepoint) in chunk {
            let utf16: Vec<u16> = char::from_u32(**codepoint)
                .unwrap_or('\u{FFFD}')
                .encode_utf16(&mut [0; 2])
                .to_vec();
            cmap.push_str(&format!("<{:04X}> <", glyph));
            for unit in utf16 {
                cmap.push_str(&format!("{unit:04X}"));
            }
            cmap.push_str(">\n");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| ConvertError::Failed("truncated embedded font".into()))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_i16(data: &[u8], offset: usize) -> Result<i16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| ConvertError::Failed("truncated embedded font".into()))?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| ConvertError::Failed("truncated embedded font".into()))?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_pdf_embeds_font_and_text_mapping() {
        let (pdf, report) = convert(
            FormatId::PlainText,
            "Bonjour, français !\n¡Hola, español!\n日本語".as_bytes(),
        )
        .expect("PDF should be generated");
        let pdf = String::from_utf8_lossy(&pdf);
        assert!(pdf.starts_with("%PDF-1.7"));
        assert!(pdf.contains("/FontFile2"));
        assert!(pdf.contains("/ToUnicode"));
        assert!(pdf.contains("beginbfchar"));
        assert!(report.lossy);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("text-only"))
        );
    }

    #[test]
    fn wraps_long_lines_and_creates_multiple_pages() {
        let words_per_line = LINE_WIDTH / 5 + 1;
        let text = format!("{}\n", "word ".repeat(words_per_line * (PAGE_LINES + 1)));
        let (pdf, _) = convert(FormatId::PlainText, text.as_bytes()).expect("PDF generation");
        let pdf = String::from_utf8_lossy(&pdf);
        assert!(pdf.contains("/Count 2"));
        assert!(pdf.contains("/MediaBox [0 0 612 792]"));
    }
}
