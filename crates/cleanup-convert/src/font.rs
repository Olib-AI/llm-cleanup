//! Minimal TrueType parsing for PDF export.
//!
//! Everything here is read-only: the `cmap` is used to map Unicode scalars to glyph ids, the
//! `hmtx`/`hhea` tables give advance widths in 1/1000 em, and `head`/`OS/2` give the metrics a
//! `FontDescriptor` needs. Only the tables required to embed a CIDFontType2 are touched, so a
//! font with a missing `OS/2` still works.

use crate::{ConvertError, Result};

pub(crate) struct TrueType<'a> {
    pub(crate) data: &'a [u8],
    cmap: Cmap,
    pub(crate) units_per_em: u16,
    pub(crate) glyph_count: u16,
    horizontal_metrics: u16,
    hmtx_offset: usize,
    pub(crate) bbox: [i16; 4],
    pub(crate) ascent: i16,
    pub(crate) descent: i16,
    pub(crate) cap_height: i16,
}

impl<'a> TrueType<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Result<Self> {
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

    pub(crate) fn glyph_index(&self, codepoint: u32) -> Option<u16> {
        self.cmap.glyph_index(self.data, codepoint)
    }

    pub(crate) fn width(&self, glyph: u16) -> Result<u16> {
        let index = usize::from(glyph.min(self.horizontal_metrics - 1));
        let advance = read_u16(self.data, self.hmtx_offset + index * 4)?;
        Ok(
            ((u32::from(advance) * 1000 + u32::from(self.units_per_em) / 2)
                / u32::from(self.units_per_em)) as u16,
        )
    }

    pub(crate) fn scaled(&self, units: i16) -> i32 {
        (i32::from(units) * 1000) / i32::from(self.units_per_em)
    }
}

pub(crate) enum Cmap {
    Format4(usize),
    Format12(usize),
}

impl Cmap {
    pub(crate) fn new(data: &[u8], offset: usize) -> Result<Self> {
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

    pub(crate) fn glyph_index(&self, data: &[u8], codepoint: u32) -> Option<u16> {
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


pub(crate) fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| ConvertError::Failed("truncated embedded font".into()))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn read_i16(data: &[u8], offset: usize) -> Result<i16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| ConvertError::Failed("truncated embedded font".into()))?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| ConvertError::Failed("truncated embedded font".into()))?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

