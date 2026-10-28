//! Minimal raster support: PNG encode/decode and header probing.
//!
//! The pipeline never re-encodes lossy data. JPEG payloads are carried through byte-for-byte
//! (PDF `/DCTDecode` → file bytes → back into a PDF `/DCTDecode` stream), and PNG payloads
//! are either copied verbatim or, when they come out of a PDF as raw samples, encoded once
//! here. The decoder exists so the PDF exporter can embed a PNG that arrived from Markdown
//! or DOCX without pulling in an image-processing dependency.
//!
//! Supported on the way in: 8-bit greyscale, 8-bit RGB and 8-bit RGBA, non-interlaced —
//! which is what every PDF `/FlateDecode` image and every fixture produces. Anything else is
//! rejected with `None` and the caller degrades to a placeholder rather than guessing.

use std::io::{Read, Write};

/// Raw 8-bit RGB pixel data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` bytes, row-major, top row first.
    pub rgb: Vec<u8>,
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = Vec::with_capacity(data.len() + 4);
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc_input);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Encode 8-bit RGB samples as a PNG.
pub fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }
    let expected = width as usize * height as usize * 3;
    if rgb.len() < expected {
        return None;
    }
    let stride = width as usize * 3;
    // Filter type 0 (None) on every scanline: simple, deterministic, still valid.
    let mut raw = Vec::with_capacity(expected + height as usize);
    for row in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&rgb[row * stride..row * stride + stride]);
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&raw).ok()?;
    let compressed = encoder.finish().ok()?;

    let mut out = Vec::with_capacity(compressed.len() + 64);
    out.extend_from_slice(&PNG_SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // colour type: truecolour
    ihdr.push(0); // deflate
    ihdr.push(0); // adaptive filtering
    ihdr.push(0); // no interlace
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &compressed);
    push_chunk(&mut out, b"IEND", &[]);
    Some(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i32::from(a) + i32::from(b) - i32::from(c);
    let pa = (p - i32::from(a)).abs();
    let pb = (p - i32::from(b)).abs();
    let pc = (p - i32::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Decode an 8-bit, non-interlaced PNG into RGB samples. Returns `None` for formats we do
/// not handle (16-bit, palette, interlaced) so callers can degrade instead of corrupting.
pub fn decode_png(bytes: &[u8]) -> Option<RasterImage> {
    if !bytes.starts_with(&PNG_SIGNATURE) {
        return None;
    }
    let mut pos = PNG_SIGNATURE.len();
    let mut width = 0u32;
    let mut height = 0u32;
    let mut color_type = 0u8;
    let mut bit_depth = 0u8;
    let mut interlace = 0u8;
    let mut idat: Vec<u8> = Vec::new();
    let mut seen_ihdr = false;
    while pos + 8 <= bytes.len() {
        let length =
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start.checked_add(length)?;
        if data_end + 4 > bytes.len() {
            return None;
        }
        let data = &bytes[data_start..data_end];
        match kind {
            b"IHDR" => {
                if data.len() < 13 {
                    return None;
                }
                width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                bit_depth = data[8];
                color_type = data[9];
                interlace = data[12];
                seen_ihdr = true;
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos = data_end + 4;
    }
    if !seen_ihdr || width == 0 || height == 0 || bit_depth != 8 || interlace != 0 {
        return None;
    }
    let channels = match color_type {
        0 => 1usize,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    let mut decoder = flate2::read::ZlibDecoder::new(&idat[..]);
    let mut raw = Vec::new();
    decoder.read_to_end(&mut raw).ok()?;
    let stride = width as usize * channels;
    if stride == 0 || raw.len() < (stride + 1) * height as usize {
        return None;
    }
    let mut previous = vec![0u8; stride];
    let mut current = vec![0u8; stride];
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for row in 0..height as usize {
        let base = row * (stride + 1);
        let filter = raw[base];
        let line = &raw[base + 1..base + 1 + stride];
        for index in 0..stride {
            let left = if index >= channels {
                current[index - channels]
            } else {
                0
            };
            let up = previous[index];
            let up_left = if index >= channels {
                previous[index - channels]
            } else {
                0
            };
            let value = match filter {
                0 => line[index],
                1 => line[index].wrapping_add(left),
                2 => line[index].wrapping_add(up),
                3 => {
                    let average = (u16::from(left) + u16::from(up)) / 2;
                    line[index].wrapping_add(average as u8)
                }
                4 => line[index].wrapping_add(paeth(left, up, up_left)),
                _ => return None,
            };
            current[index] = value;
        }
        for column in 0..width as usize {
            let pixel = &current[column * channels..column * channels + channels];
            match channels {
                1 | 2 => rgb.extend_from_slice(&[pixel[0], pixel[0], pixel[0]]),
                3 => rgb.extend_from_slice(&pixel[0..3]),
                _ => {
                    // Composite RGBA over white so the page background never turns black.
                    let alpha = u16::from(pixel[3]);
                    for &sample in pixel.iter().take(3) {
                        let value = (u16::from(sample) * alpha + 255 * (255 - alpha)) / 255;
                        rgb.push(value as u8);
                    }
                }
            }
        }
        std::mem::swap(&mut previous, &mut current);
    }
    Some(RasterImage { width, height, rgb })
}

/// Width/height of a PNG from its IHDR chunk.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&PNG_SIGNATURE) || bytes.len() < 24 {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if width == 0 || height == 0 {
        None
    } else {
        Some((width, height))
    }
}

/// Width/height of a JPEG from its first `SOFn` marker.
pub fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    jpeg_frame(bytes).map(|(width, height, _)| (width, height))
}

/// Number of colour components declared by the first JPEG frame header.
pub fn jpeg_components(bytes: &[u8]) -> Option<u8> {
    jpeg_frame(bytes).map(|(_, _, components)| components)
}

fn jpeg_frame(bytes: &[u8]) -> Option<(u32, u32, u8)> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut pos = 2usize;
    while pos + 4 <= bytes.len() {
        if bytes[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = bytes[pos + 1];
        pos += 2;
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if pos + 2 > bytes.len() {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]));
        if length < 2 {
            return None;
        }
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            if pos + 8 > bytes.len() {
                return None;
            }
            let height = u32::from(u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]));
            let width = u32::from(u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]));
            let components = bytes[pos + 7];
            if width == 0 || height == 0 || components == 0 {
                return None;
            }
            return Some((width, height, components));
        }
        pos += length;
    }
    None
}

/// Pixel dimensions of any supported raster.
pub fn dimensions(kind: super::model::ImageKind, bytes: &[u8]) -> Option<(u32, u32)> {
    match kind {
        super::model::ImageKind::Png => png_dimensions(bytes),
        super::model::ImageKind::Jpeg => jpeg_dimensions(bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips() {
        let width = 4u32;
        let height = 3u32;
        let mut rgb = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(&[(x * 16) as u8, (y * 32) as u8, 128]);
            }
        }
        let png = encode_png(width, height, &rgb).expect("encode");
        assert_eq!(png_dimensions(&png), Some((width, height)));
        let decoded = decode_png(&png).expect("decode");
        assert_eq!(decoded.width, width);
        assert_eq!(decoded.height, height);
        assert_eq!(decoded.rgb, rgb);
    }

    #[test]
    fn decode_rejects_non_png_and_garbage() {
        assert!(decode_png(b"not a png").is_none());
        assert!(decode_png(&PNG_SIGNATURE).is_none());
    }

    #[test]
    fn jpeg_dimensions_reads_sof0() {
        // SOI, APP0 (len 4), SOF0 (len 17, precision 8, h=64, w=128, 3 components)
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x40, 0x00, 0x80, 0x03]);
        jpeg.extend_from_slice(&[0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        assert_eq!(jpeg_dimensions(&jpeg), Some((128, 64)));
        assert_eq!(jpeg_components(&jpeg), Some(3));
    }
}
