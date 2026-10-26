//! PDF adapter — native, layout-preserving cleanup of `.pdf` documents.
//!
//! PDF joins the byte-splice family with one extra layer of indirection. We never
//! re-render a page (no PDF → text → PDF round trip), because that would destroy the
//! very thing the user asked us to keep: page geometry, fonts, vector art, form
//! fields, links, and embedded images. Instead, mirroring the DOCX adapter:
//!
//!   1. the file is read through its cross-reference data (classic xref tables,
//!      xref streams, hybrid files, incremental updates, and objects compressed in
//!      object streams all resolve to the *latest* revision of every object),
//!   2. page (and Form XObject) content streams are decoded (`FlateDecode`,
//!      ASCIIHex/85, RunLength), and every text-showing string (`Tj`, `TJ`, `'`, `"`)
//!      becomes a decoded segment of a prose span — the PDF analogue of a `<w:t>`
//!      run. Font encodings (`WinAnsi`, `MacRoman`, `Differences`, and `ToUnicode`
//!      CMaps for composite/Identity-H fonts) translate glyph codes to Unicode and
//!      back, exactly as XML entity decoding does for DOCX,
//!   3. rules edit only that decoded prose; rendering splices the re-encoded
//!      replacement into the *same* string operand of the *same* content stream, so
//!      every `Tm`/`Td`/`Tf`/graphics operator — the layout — is untouched,
//!   4. the file is repacked object-by-object: every unmodified object (pages,
//!      fonts, image XObjects, annotations, outlines, …) is copied byte-for-byte,
//!      only edited content-stream objects change, and the cross-reference table is
//!      regenerated. An empty edit set returns the original bytes unchanged.
//!
//! Container findings cover the places provenance hides in a PDF outside the prose:
//! the document information dictionary, XMP metadata streams (including C2PA /
//! `trainedAlgorithmicMedia` claims), and JPEG (`DCTDecode`) image XObjects, which
//! are inspected and scrubbed with the same lossless media logic as standalone
//! images. Encrypted files are rejected (we cannot know the password), and
//! scanned/image-only or unmapped-font text is reported and left untouched rather
//! than guessed at — there is deliberately no OCR in this tool.

use cleanup_core::ir::{
    DecodedSegment, DocMeta, Document, FormatId, Newline, ProseSpan, SpanAnchor,
};
use cleanup_core::{ContainerFinding, CoreError, EditSet, Format, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::ops::Range;

pub struct Pdf;

/// Generous-but-finite guard rails for untrusted files: a hostile PDF must not be
/// able to make the cleaner allocate without bound while parsing indexes.
const MAX_OBJECTS: usize = 1_000_000;
const MAX_XREF_OFFSET: usize = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Ref {
    num: u32,
    generation: u16,
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Real(String),
    Name(String),
    Str(Vec<u8>),
    Array(Vec<Value>),
    Dict(Vec<(String, Value)>),
    Ref(Ref),
}

fn dict_get<'a>(dict: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    dict.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn as_dict(value: &Value) -> Option<&Vec<(String, Value)>> {
    match value {
        Value::Dict(dict) => Some(dict),
        _ => None,
    }
}

fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Int(n) => Some(*n),
        Value::Real(text) => text.parse::<f64>().ok().map(|n| n as i64),
        _ => None,
    }
}

fn as_usize(value: &Value) -> Option<usize> {
    usize::try_from(as_int(value)?).ok()
}

fn as_name(value: &Value) -> Option<&str> {
    match value {
        Value::Name(name) => Some(name),
        _ => None,
    }
}

fn as_ref(value: &Value) -> Option<Ref> {
    match value {
        Value::Ref(reference) => Some(*reference),
        _ => None,
    }
}

fn is_pdf_ws(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_pdf_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn find_subslice(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from > haystack.len() - needle.len() {
        if needle.is_empty() {
            return Some(from.min(haystack.len()));
        }
        if from >= haystack.len() {
            return None;
        }
    }
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn parse_literal_string_bytes(bytes: &[u8], start: usize) -> Result<(Vec<u8>, usize)> {
    if bytes.get(start) != Some(&b'(') {
        return Err(CoreError::Parse("expected PDF literal string".to_string()));
    }
    let mut out = Vec::new();
    let mut depth = 1usize;
    let mut pos = start + 1;
    while pos < bytes.len() {
        let byte = bytes[pos];
        pos += 1;
        match byte {
            b'\\' => {
                let Some(escaped) = bytes.get(pos).copied() else {
                    break;
                };
                pos += 1;
                match escaped {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'(' | b')' | b'\\' => out.push(escaped),
                    b'\r' => {
                        if bytes.get(pos) == Some(&b'\n') {
                            pos += 1;
                        }
                    }
                    b'\n' => {}
                    b'0'..=b'7' => {
                        let mut value = u32::from(escaped - b'0');
                        for _ in 0..2 {
                            match bytes.get(pos).copied() {
                                Some(digit @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(digit - b'0');
                                    pos += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push((value & 0xFF) as u8);
                    }
                    other => out.push(other),
                }
            }
            b'(' => {
                depth += 1;
                out.push(byte);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((out, pos));
                }
                out.push(byte);
            }
            other => out.push(other),
        }
    }
    Err(CoreError::Parse(
        "unterminated PDF literal string".to_string(),
    ))
}

struct Lexer<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Lexer { bytes, pos: 0 }
    }

    fn at(bytes: &'a [u8], pos: usize) -> Self {
        Lexer { bytes, pos }
    }

    fn skip_ws(&mut self) {
        while let Some(&byte) = self.bytes.get(self.pos) {
            if is_pdf_ws(byte) {
                self.pos += 1;
            } else if byte == b'%' {
                while let Some(&inner) = self.bytes.get(self.pos) {
                    self.pos += 1;
                    if inner == b'\n' || inner == b'\r' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn starts_with(&mut self, token: &str) -> bool {
        self.skip_ws();
        self.bytes
            .get(self.pos..self.pos + token.len())
            .is_some_and(|slice| slice == token.as_bytes())
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        if !self.starts_with(keyword) {
            return false;
        }
        let after = self.pos + keyword.len();
        let boundary = self
            .bytes
            .get(after)
            .is_none_or(|&byte| is_pdf_ws(byte) || is_pdf_delimiter(byte));
        if boundary {
            self.pos = after;
            true
        } else {
            false
        }
    }

    fn parse_keyword(&mut self) -> Option<String> {
        self.skip_ws();
        let start = self.pos;
        while let Some(&byte) = self.bytes.get(self.pos) {
            if is_pdf_ws(byte) || is_pdf_delimiter(byte) {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        Some(String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned())
    }

    fn parse_name(&mut self) -> Result<String> {
        self.skip_ws();
        if self.bytes.get(self.pos) != Some(&b'/') {
            return Err(CoreError::Parse("expected PDF name".to_string()));
        }
        self.pos += 1;
        let mut decoded = Vec::new();
        while let Some(&byte) = self.bytes.get(self.pos) {
            if is_pdf_ws(byte) || is_pdf_delimiter(byte) {
                break;
            }
            self.pos += 1;
            if byte == b'#'
                && let (Some(&hi), Some(&lo)) =
                    (self.bytes.get(self.pos), self.bytes.get(self.pos + 1))
                && let (Some(hi), Some(lo)) = (hex_value(hi), hex_value(lo))
            {
                decoded.push(hi * 16 + lo);
                self.pos += 2;
            } else {
                decoded.push(byte);
            }
        }
        Ok(String::from_utf8_lossy(&decoded).into_owned())
    }

    fn parse_number_token(&mut self) -> Option<String> {
        self.skip_ws();
        let start = self.pos;
        while let Some(&byte) = self.bytes.get(self.pos) {
            if byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return None;
        }
        Some(String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned())
    }

    fn parse_value(&mut self) -> Result<Value> {
        let Some(byte) = self.peek() else {
            return Err(CoreError::Parse("unexpected end of PDF data".to_string()));
        };
        match byte {
            b'<' => {
                if self.bytes.get(self.pos + 1) == Some(&b'<') {
                    self.parse_dict()
                } else {
                    self.pos += 1;
                    let mut digits = Vec::new();
                    while let Some(&inner) = self.bytes.get(self.pos) {
                        self.pos += 1;
                        if inner == b'>' {
                            break;
                        }
                        if let Some(value) = hex_value(inner) {
                            digits.push(value);
                        }
                    }
                    if digits.len() % 2 == 1 {
                        digits.push(0);
                    }
                    let mut out = Vec::with_capacity(digits.len() / 2);
                    for pair in digits.chunks_exact(2) {
                        out.push(pair[0] * 16 + pair[1]);
                    }
                    Ok(Value::Str(out))
                }
            }
            b'(' => {
                let (text, end) = parse_literal_string_bytes(self.bytes, self.pos)?;
                self.pos = end;
                Ok(Value::Str(text))
            }
            b'[' => {
                self.pos += 1;
                let mut out = Vec::new();
                loop {
                    match self.peek() {
                        None => {
                            return Err(CoreError::Parse("unterminated PDF array".to_string()));
                        }
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(_) => out.push(self.parse_value()?),
                    }
                }
                Ok(Value::Array(out))
            }
            b'/' => Ok(Value::Name(self.parse_name()?)),
            b't' | b'f' | b'n' => {
                let keyword = self
                    .parse_keyword()
                    .ok_or_else(|| CoreError::Parse("expected PDF keyword".to_string()))?;
                match keyword.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "null" => Ok(Value::Null),
                    other => Ok(Value::Name(other.to_string())),
                }
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let first = self
                    .parse_number_token()
                    .ok_or_else(|| CoreError::Parse("expected PDF number".to_string()))?;
                let after_first = self.pos;
                if let Ok(num) = first.parse::<u32>()
                    && let Some(second) = self.parse_number_token()
                    && let Ok(generation) = second.parse::<u16>()
                {
                    self.skip_ws();
                    if self.bytes.get(self.pos) == Some(&b'R')
                        && self
                            .bytes
                            .get(self.pos + 1)
                            .is_none_or(|&next| is_pdf_ws(next) || is_pdf_delimiter(next))
                    {
                        self.pos += 1;
                        return Ok(Value::Ref(Ref { num, generation }));
                    }
                }
                self.pos = after_first;
                if !first.contains('.')
                    && let Ok(value) = first.parse::<i64>()
                {
                    Ok(Value::Int(value))
                } else {
                    Ok(Value::Real(first))
                }
            }
            _ => {
                let keyword = self
                    .parse_keyword()
                    .ok_or_else(|| CoreError::Parse("unexpected PDF token".to_string()))?;
                Ok(Value::Name(keyword))
            }
        }
    }

    fn parse_dict(&mut self) -> Result<Value> {
        self.skip_ws();
        if !self
            .bytes
            .get(self.pos..self.pos + 2)
            .is_some_and(|s| s == b"<<")
        {
            return Err(CoreError::Parse("expected PDF dictionary".to_string()));
        }
        self.pos += 2;
        let mut entries = Vec::new();
        loop {
            self.skip_ws();
            if self
                .bytes
                .get(self.pos..self.pos + 2)
                .is_some_and(|s| s == b">>")
            {
                self.pos += 2;
                break;
            }
            if self.pos >= self.bytes.len() {
                return Err(CoreError::Parse("unterminated PDF dictionary".to_string()));
            }
            let key = self.parse_name()?;
            let value = self.parse_value()?;
            entries.push((key, value));
        }
        Ok(Value::Dict(entries))
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct Obj {
    reference: Ref,
    value: Value,
    stream: Option<Vec<u8>>,
    raw_body: Vec<u8>,
}

/// Parse one indirect object at `offset`, returning it and the offset just past
/// its `endobj`. Stream extents prefer a direct `/Length`, falling back to the
/// `endstream` marker (an indirect `/Length` cannot be resolved yet at this stage).
fn parse_indirect_at(bytes: &[u8], offset: usize) -> Result<(Obj, usize)> {
    let mut lexer = Lexer::at(bytes, offset);
    let num_token = lexer
        .parse_number_token()
        .ok_or_else(|| CoreError::Parse("expected PDF object number".to_string()))?;
    let gen_token = lexer
        .parse_number_token()
        .ok_or_else(|| CoreError::Parse("expected PDF generation number".to_string()))?;
    let num: u32 = num_token
        .parse()
        .map_err(|_| CoreError::Parse("invalid PDF object number".to_string()))?;
    let generation: u16 = gen_token
        .parse()
        .map_err(|_| CoreError::Parse("invalid PDF generation number".to_string()))?;
    if !lexer.consume_keyword("obj") {
        return Err(CoreError::Parse("expected `obj` in PDF".to_string()));
    }
    let body_start = lexer.pos;
    let value = lexer.parse_value()?;
    let after_value = lexer.pos;

    let mut stream = None;
    let mut body_end_search = after_value;
    if matches!(value, Value::Dict(_)) && lexer.consume_keyword("stream") {
        // The stream keyword is followed by CRLF or a single LF/CR.
        if bytes.get(lexer.pos..lexer.pos + 2) == Some(b"\r\n") {
            lexer.pos += 2;
        } else if matches!(bytes.get(lexer.pos), Some(b'\n' | b'\r')) {
            lexer.pos += 1;
        }
        let data_start = lexer.pos;
        let endstream = find_subslice(bytes, data_start, b"endstream")
            .ok_or_else(|| CoreError::Parse("unterminated PDF stream".to_string()))?;
        let declared_len = as_dict(&value)
            .and_then(|dict| dict_get(dict, "Length"))
            .and_then(as_usize);
        let data_end = match declared_len {
            Some(len) if data_start + len <= endstream => data_start + len,
            _ => {
                let mut end = endstream;
                if end >= data_start + 2 && &bytes[end - 2..end] == b"\r\n" {
                    end -= 2;
                } else if end > data_start && matches!(bytes[end - 1], b'\n' | b'\r') {
                    end -= 1;
                }
                end
            }
        };
        stream = Some(bytes[data_start..data_end].to_vec());
        body_end_search = endstream + b"endstream".len();
    }

    let endobj = find_subslice(bytes, body_end_search, b"endobj")
        .ok_or_else(|| CoreError::Parse("unterminated PDF object".to_string()))?;
    let raw_body = bytes[body_start..endobj].to_vec();
    Ok((
        Obj {
            reference: Ref { num, generation },
            value,
            stream,
            raw_body,
        },
        endobj + b"endobj".len(),
    ))
}

#[derive(Debug, Clone, Copy)]
enum XrefEntry {
    Normal { offset: usize, generation: u16 },
    Compressed { objstm: u32, index: u32 },
    Free,
}

fn merge_trailer(trailer: &mut Vec<(String, Value)>, entries: &[(String, Value)]) {
    for (key, value) in entries {
        if dict_get(trailer, key).is_none() {
            trailer.push((key.clone(), value.clone()));
        }
    }
}

fn parse_xref_table_at(
    bytes: &[u8],
    offset: usize,
    entries: &mut BTreeMap<u32, XrefEntry>,
    trailer: &mut Vec<(String, Value)>,
) -> Result<Vec<(String, Value)>> {
    let mut lexer = Lexer::at(bytes, offset);
    if !lexer.consume_keyword("xref") {
        return Err(CoreError::Parse("expected PDF xref table".to_string()));
    }
    loop {
        lexer.skip_ws();
        if lexer.consume_keyword("trailer") {
            let dict = lexer.parse_value()?;
            let Some(entries_here) = as_dict(&dict).cloned() else {
                return Err(CoreError::Parse(
                    "PDF trailer is not a dictionary".to_string(),
                ));
            };
            merge_trailer(trailer, &entries_here);
            return Ok(entries_here);
        }
        let first = lexer
            .parse_number_token()
            .and_then(|token| token.parse::<u32>().ok())
            .ok_or_else(|| CoreError::Parse("malformed PDF xref subsection".to_string()))?;
        let count = lexer
            .parse_number_token()
            .and_then(|token| token.parse::<u32>().ok())
            .ok_or_else(|| CoreError::Parse("malformed PDF xref count".to_string()))?;
        if count as usize > MAX_OBJECTS {
            return Err(CoreError::Parse(
                "PDF xref claims too many objects".to_string(),
            ));
        }
        for i in 0..count {
            let entry_offset = lexer
                .parse_number_token()
                .and_then(|token| token.parse::<usize>().ok())
                .ok_or_else(|| CoreError::Parse("malformed PDF xref entry".to_string()))?;
            let generation = lexer
                .parse_number_token()
                .and_then(|token| token.parse::<u16>().ok())
                .ok_or_else(|| CoreError::Parse("malformed PDF xref generation".to_string()))?;
            let kind = lexer
                .parse_keyword()
                .ok_or_else(|| CoreError::Parse("malformed PDF xref entry kind".to_string()))?;
            let num = first + i;
            entries.entry(num).or_insert(if kind == "n" {
                XrefEntry::Normal {
                    offset: entry_offset,
                    generation,
                }
            } else {
                XrefEntry::Free
            });
        }
    }
}

fn read_be_int(bytes: &[u8], cursor: &mut usize, width: usize) -> Option<u64> {
    let mut value = 0u64;
    for _ in 0..width {
        value = (value << 8) | u64::from(*bytes.get(*cursor)?);
        *cursor += 1;
    }
    Some(value)
}

fn parse_xref_stream_at(
    bytes: &[u8],
    offset: usize,
    entries: &mut BTreeMap<u32, XrefEntry>,
    trailer: &mut Vec<(String, Value)>,
) -> Result<Vec<(String, Value)>> {
    let (object, _) = parse_indirect_at(bytes, offset)?;
    let dict = as_dict(&object.value)
        .ok_or_else(|| CoreError::Parse("PDF xref stream has no dictionary".to_string()))?
        .clone();
    let raw = object
        .stream
        .as_deref()
        .ok_or_else(|| CoreError::Parse("PDF xref object has no stream".to_string()))?;
    let data = decode_stream_data(&dict, raw)?;
    let widths: Vec<usize> = match dict_get(&dict, "W") {
        Some(Value::Array(items)) => items.iter().filter_map(as_usize).collect(),
        _ => Vec::new(),
    };
    if widths.len() != 3 {
        return Err(CoreError::Parse(
            "PDF xref stream has no /W array".to_string(),
        ));
    }
    let size = dict_get(&dict, "Size").and_then(as_usize).unwrap_or(0);
    let mut index: Vec<(u32, u32)> = Vec::new();
    match dict_get(&dict, "Index") {
        Some(Value::Array(items)) => {
            for pair in items.chunks_exact(2) {
                if let (Some(first), Some(count)) = (as_usize(&pair[0]), as_usize(&pair[1])) {
                    index.push((first as u32, count as u32));
                }
            }
        }
        _ => index.push((0, size as u32)),
    }
    let mut cursor = 0usize;
    for (first, count) in index {
        if count as usize > MAX_OBJECTS {
            return Err(CoreError::Parse(
                "PDF xref stream claims too many objects".to_string(),
            ));
        }
        for i in 0..count {
            let field1 = read_be_int(&data, &mut cursor, widths[0]).unwrap_or(0);
            let field2 = read_be_int(&data, &mut cursor, widths[1]).unwrap_or(0);
            let field3 = read_be_int(&data, &mut cursor, widths[2]).unwrap_or(0);
            let kind = if widths[0] == 0 { 1 } else { field1 };
            let entry = match kind {
                1 => XrefEntry::Normal {
                    offset: field2 as usize,
                    generation: field3 as u16,
                },
                2 => XrefEntry::Compressed {
                    objstm: field2 as u32,
                    index: field3 as u32,
                },
                _ => XrefEntry::Free,
            };
            entries.entry(first + i).or_insert(entry);
        }
    }
    merge_trailer(trailer, &dict);
    Ok(dict)
}

#[derive(Debug)]
struct ParsedPdf {
    objects: BTreeMap<Ref, Obj>,
    trailer: Vec<(String, Value)>,
    root: Option<Ref>,
    info: Option<Ref>,
    encrypted: bool,
}

impl ParsedPdf {
    fn get(&self, reference: Ref) -> Option<&Obj> {
        self.objects.get(&reference).or_else(|| {
            // References in the wild occasionally carry a stale generation;
            // resolve by object number rather than failing the whole document.
            self.objects
                .values()
                .filter(|obj| obj.reference.num == reference.num)
                .max_by_key(|obj| obj.reference.generation)
        })
    }

    fn resolve<'a>(&'a self, value: &'a Value) -> &'a Value {
        let mut current = value;
        for _ in 0..16 {
            match current {
                Value::Ref(reference) => match self.get(*reference) {
                    Some(object) => current = &object.value,
                    None => break,
                },
                _ => break,
            }
        }
        current
    }

    fn resolve_dict<'a>(&'a self, value: &'a Value) -> Option<&'a Vec<(String, Value)>> {
        as_dict(self.resolve(value))
    }

    fn object_dict(&self, reference: Ref) -> Option<&Vec<(String, Value)>> {
        as_dict(&self.get(reference)?.value)
    }

    fn decoded_stream(&self, reference: Ref) -> Option<Result<Vec<u8>>> {
        let object = self.get(reference)?;
        let raw = object.stream.as_deref()?;
        let dict = as_dict(&object.value)?;
        Some(decode_stream_data(dict, raw))
    }
}

fn object_is(obj: &Obj, key: &str, expected: &str) -> bool {
    as_dict(&obj.value)
        .and_then(|dict| dict_get(dict, key))
        .and_then(as_name)
        == Some(expected)
}

fn scan_objects_fallback(bytes: &[u8]) -> BTreeMap<Ref, Obj> {
    let mut objects = BTreeMap::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let at_line_start = pos == 0 || matches!(bytes[pos - 1], b'\n' | b'\r');
        if at_line_start && bytes[pos].is_ascii_digit() {
            if let Ok((object, end)) = parse_indirect_at(bytes, pos) {
                let reference = object.reference;
                objects.insert(reference, object);
                pos = end.max(pos + 1);
                continue;
            }
        }
        pos += 1;
    }
    objects
}

fn parse_pdf(bytes: &[u8]) -> Result<ParsedPdf> {
    let header = if bytes.starts_with(b"%PDF-") {
        Some(0usize)
    } else {
        find_subslice(bytes, 0, b"%PDF-").filter(|offset| *offset < 1024)
    };
    if header.is_none() {
        return Err(CoreError::Parse(
            "not a PDF file (missing %PDF- header)".to_string(),
        ));
    }

    let mut entries: BTreeMap<u32, XrefEntry> = BTreeMap::new();
    let mut trailer: Vec<(String, Value)> = Vec::new();

    // Locate startxref (the last one wins in incrementally updated files).
    let mut current: Option<usize> = None;
    let mut search_from = 0usize;
    while let Some(at) = find_subslice(bytes, search_from, b"startxref") {
        let mut lexer = Lexer::at(bytes, at + b"startxref".len());
        if let Some(value) = lexer
            .parse_number_token()
            .and_then(|t| t.parse::<usize>().ok())
        {
            if value <= MAX_XREF_OFFSET {
                current = Some(value);
            }
        }
        search_from = at + 1;
    }

    let mut visited: BTreeSet<usize> = BTreeSet::new();
    while let Some(offset) = current {
        if !visited.insert(offset) || offset >= bytes.len() {
            break;
        }
        let mut probe = Lexer::at(bytes, offset);
        let here = if probe.starts_with("xref") {
            parse_xref_table_at(bytes, offset, &mut entries, &mut trailer)
        } else {
            parse_xref_stream_at(bytes, offset, &mut entries, &mut trailer)
        };
        let Ok(here) = here else {
            break;
        };
        if let Some(xref_stm) = dict_get(&here, "XRefStm").and_then(as_usize)
            && visited.insert(xref_stm)
            && xref_stm < bytes.len()
        {
            let _ = parse_xref_stream_at(bytes, xref_stm, &mut entries, &mut trailer);
        }
        current = dict_get(&here, "Prev")
            .and_then(as_usize)
            .filter(|value| *value <= MAX_XREF_OFFSET);
    }

    let mut objects: BTreeMap<Ref, Obj> = BTreeMap::new();
    let mut compressed: Vec<(u32, u32, u32)> = Vec::new(); // (num, objstm, index)
    for (num, entry) in &entries {
        match entry {
            XrefEntry::Normal { offset, .. } => {
                if let Ok((object, _)) = parse_indirect_at(bytes, *offset)
                    && object.reference.num == *num
                {
                    objects.insert(object.reference, object);
                }
            }
            XrefEntry::Compressed { objstm, index } => {
                compressed.push((*num, *objstm, *index));
            }
            XrefEntry::Free => {}
        }
        if objects.len() > MAX_OBJECTS {
            return Err(CoreError::Parse(
                "PDF contains too many objects".to_string(),
            ));
        }
    }

    if objects.is_empty() {
        objects = scan_objects_fallback(bytes);
    }

    // Materialize objects stored inside object streams. Stream objects cannot
    // live there, so this only affects dictionaries/values (pages, fonts, …).
    let objstm_numbers: Vec<u32> = compressed.iter().map(|(_, objstm, _)| *objstm).collect();
    let mut objstm_cache: BTreeMap<u32, Vec<(u32, Vec<u8>)>> = BTreeMap::new();
    for objstm_num in objstm_numbers {
        if objstm_cache.contains_key(&objstm_num) {
            continue;
        }
        let Some(container) = objects
            .values()
            .find(|obj| obj.reference.num == objstm_num && object_is(obj, "Type", "ObjStm"))
            .cloned()
        else {
            continue;
        };
        let Some(dict) = as_dict(&container.value).cloned() else {
            continue;
        };
        let (Some(raw), Some(count), Some(first)) = (
            container.stream.as_deref(),
            dict_get(&dict, "N").and_then(as_usize),
            dict_get(&dict, "First").and_then(as_usize),
        ) else {
            continue;
        };
        let Ok(decoded) = decode_stream_data(&dict, raw) else {
            continue;
        };
        if first > decoded.len() {
            continue;
        }
        let mut lexer = Lexer::new(&decoded[..first]);
        let mut pairs: Vec<(u32, usize)> = Vec::new();
        for _ in 0..count {
            let Some(num) = lexer
                .parse_number_token()
                .and_then(|t| t.parse::<u32>().ok())
            else {
                break;
            };
            let Some(at) = lexer
                .parse_number_token()
                .and_then(|t| t.parse::<usize>().ok())
            else {
                break;
            };
            pairs.push((num, at));
        }
        let mut extracted: Vec<(u32, Vec<u8>)> = Vec::new();
        for (i, (num, at)) in pairs.iter().enumerate() {
            let start = first + at;
            let end = pairs
                .get(i + 1)
                .map(|(_, next)| first + next)
                .unwrap_or(decoded.len());
            if start <= end && end <= decoded.len() {
                extracted.push((*num, decoded[start..end].to_vec()));
            }
        }
        // Index the extracted bodies by their ObjStm position for the loop below.
        objstm_cache.insert(
            objstm_num,
            pairs
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    (
                        i as u32,
                        extracted.get(i).map(|(_, b)| b.clone()).unwrap_or_default(),
                    )
                })
                .collect(),
        );
    }
    for (num, objstm_num, index) in compressed {
        let Some(bodies) = objstm_cache.get(&objstm_num) else {
            continue;
        };
        let Some((_, body)) = bodies.iter().find(|(i, _)| *i == index) else {
            continue;
        };
        let mut lexer = Lexer::new(body);
        let Ok(value) = lexer.parse_value() else {
            continue;
        };
        objects.entry(Ref { num, generation: 0 }).or_insert(Obj {
            reference: Ref { num, generation: 0 },
            value,
            stream: None,
            raw_body: body.clone(),
        });
    }

    if objects.is_empty() {
        return Err(CoreError::Parse(
            "could not read any objects from this PDF (damaged cross-reference data)".to_string(),
        ));
    }

    let root = dict_get(&trailer, "Root").and_then(as_ref).or_else(|| {
        objects
            .values()
            .find(|obj| object_is(obj, "Type", "Catalog"))
            .map(|obj| obj.reference)
    });
    let info = dict_get(&trailer, "Info").and_then(as_ref);
    let encrypted = dict_get(&trailer, "Encrypt").is_some();
    Ok(ParsedPdf {
        objects,
        trailer,
        root,
        info,
        encrypted,
    })
}

// ---------------------------------------------------------------------------
// Stream decoding (content streams, object streams, xref streams, XMP, ToUnicode)
// ---------------------------------------------------------------------------

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    let mut out = Vec::new();
    if decoder.read_to_end(&mut out).is_ok() {
        return Ok(out);
    }
    // A minority of writers emit a bare Deflate stream without the zlib wrapper.
    let mut decoder = flate2::read::DeflateDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| CoreError::Parse(format!("corrupt FlateDecode stream: {e}")))?;
    Ok(out)
}

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| CoreError::Parse(format!("compressing PDF stream: {e}")))?;
    encoder
        .finish()
        .map_err(|e| CoreError::Parse(format!("compressing PDF stream: {e}")))
}

fn apply_predictor(data: Vec<u8>, parms: &[(String, Value)]) -> Result<Vec<u8>> {
    let predictor = dict_get(parms, "Predictor").and_then(as_usize).unwrap_or(1);
    if predictor <= 1 {
        return Ok(data);
    }
    let columns = dict_get(parms, "Columns")
        .and_then(as_usize)
        .unwrap_or(1)
        .max(1);
    let colors = dict_get(parms, "Colors")
        .and_then(as_usize)
        .unwrap_or(1)
        .max(1);
    let bits = dict_get(parms, "BitsPerComponent")
        .and_then(as_usize)
        .unwrap_or(8);
    if bits != 8 {
        return Err(CoreError::Parse(
            "unsupported PDF stream predictor (BitsPerComponent is not 8)".to_string(),
        ));
    }
    let row_len = columns
        .checked_mul(colors)
        .ok_or_else(|| CoreError::Parse("PDF stream predictor row is too large".to_string()))?;
    if row_len == 0 {
        return Ok(data);
    }
    if predictor == 2 {
        let mut out = data;
        for row in out.chunks_mut(row_len) {
            for i in colors..row.len() {
                row[i] = row[i].wrapping_add(row[i - colors]);
            }
        }
        return Ok(out);
    }
    if !(10..=15).contains(&predictor) {
        return Err(CoreError::Parse(format!(
            "unsupported PDF stream predictor {predictor}"
        )));
    }
    let stride = row_len + 1;
    let mut out = Vec::with_capacity(data.len());
    let mut previous = vec![0u8; row_len];
    for chunk in data.chunks(stride) {
        if chunk.is_empty() {
            continue;
        }
        let filter = chunk[0];
        let mut row = vec![0u8; row_len];
        let available = (chunk.len() - 1).min(row_len);
        row[..available].copy_from_slice(&chunk[1..1 + available]);
        for i in 0..row_len {
            let left = if i >= colors { row[i - colors] } else { 0 };
            let up = previous[i];
            let up_left = if i >= colors { previous[i - colors] } else { 0 };
            row[i] = match filter {
                0 => row[i],
                1 => row[i].wrapping_add(left),
                2 => row[i].wrapping_add(up),
                3 => row[i].wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => row[i].wrapping_add(paeth(left, up, up_left)),
                _ => {
                    return Err(CoreError::Parse(format!(
                        "unsupported PNG predictor filter {filter} in PDF stream"
                    )));
                }
            };
        }
        out.extend_from_slice(&row);
        previous = row;
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (i32::from(a), i32::from(b), i32::from(c));
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

fn decode_stream_data(dict: &[(String, Value)], raw: &[u8]) -> Result<Vec<u8>> {
    let mut filters: Vec<String> = Vec::new();
    match dict_get(dict, "Filter").or_else(|| dict_get(dict, "F")) {
        Some(Value::Name(name)) => filters.push(name.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(name) = as_name(item) {
                    filters.push(name.to_string());
                }
            }
        }
        _ => {}
    }
    if filters.is_empty() {
        return Ok(raw.to_vec());
    }
    let mut parms_list: Vec<Option<Vec<(String, Value)>>> = Vec::new();
    match dict_get(dict, "DecodeParms").or_else(|| dict_get(dict, "DP")) {
        Some(Value::Dict(parms)) => parms_list.push(Some(parms.clone())),
        Some(Value::Array(items)) => {
            for item in items {
                parms_list.push(as_dict(item).cloned());
            }
        }
        _ => {}
    }

    let mut data = raw.to_vec();
    for (index, filter) in filters.iter().enumerate() {
        let parms = parms_list.get(index).cloned().unwrap_or(None);
        data = match filter.as_str() {
            "FlateDecode" | "Fl" => {
                let decoded = inflate(&data)?;
                match &parms {
                    Some(parms) => apply_predictor(decoded, parms)?,
                    None => decoded,
                }
            }
            "ASCIIHexDecode" | "AHx" => {
                let mut digits = Vec::new();
                for &byte in &data {
                    if byte == b'>' {
                        break;
                    }
                    if let Some(value) = hex_value(byte) {
                        digits.push(value);
                    }
                }
                if digits.len() % 2 == 1 {
                    digits.push(0);
                }
                digits
                    .chunks_exact(2)
                    .map(|pair| pair[0] * 16 + pair[1])
                    .collect()
            }
            "ASCII85Decode" | "A85" => {
                let mut out = Vec::new();
                let mut group: Vec<u32> = Vec::new();
                let mut bytes = data.iter().copied().peekable();
                if bytes.peek() == Some(&b'<') {
                    bytes.next();
                    if bytes.peek() == Some(&b'~') {
                        bytes.next();
                    }
                }
                for byte in bytes {
                    if byte == b'~' {
                        break;
                    }
                    if is_pdf_ws(byte) {
                        continue;
                    }
                    if byte == b'z' && group.is_empty() {
                        out.extend_from_slice(&[0, 0, 0, 0]);
                        continue;
                    }
                    if !(33..=117).contains(&byte) {
                        continue;
                    }
                    group.push(u32::from(byte - 33));
                    if group.len() == 5 {
                        let value = group
                            .iter()
                            .fold(0u32, |acc, digit| acc.wrapping_mul(85).wrapping_add(*digit));
                        out.extend_from_slice(&value.to_be_bytes());
                        group.clear();
                    }
                }
                if !group.is_empty() {
                    let keep = group.len() - 1;
                    while group.len() < 5 {
                        group.push(84);
                    }
                    let value = group
                        .iter()
                        .fold(0u32, |acc, digit| acc.wrapping_mul(85).wrapping_add(*digit));
                    out.extend_from_slice(&value.to_be_bytes()[..keep]);
                }
                out
            }
            "RunLengthDecode" | "RL" => {
                let mut out = Vec::new();
                let mut pos = 0usize;
                while pos < data.len() {
                    let length = data[pos];
                    pos += 1;
                    match length {
                        128 => break,
                        0..=127 => {
                            let count = usize::from(length) + 1;
                            let end = (pos + count).min(data.len());
                            out.extend_from_slice(&data[pos..end]);
                            pos = end;
                        }
                        _ => {
                            let Some(&byte) = data.get(pos) else { break };
                            pos += 1;
                            out.extend(std::iter::repeat_n(byte, 257 - usize::from(length)));
                        }
                    }
                }
                out
            }
            other => {
                return Err(CoreError::Parse(format!(
                    "unsupported PDF stream filter {other}"
                )));
            }
        };
    }
    Ok(data)
}

// ---------------------------------------------------------------------------
// Font encodings: glyph code <-> Unicode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FontProfile {
    code_len: usize,
    decode: HashMap<u32, String>,
    encode: HashMap<char, u32>,
    supported: bool,
}

impl FontProfile {
    fn unsupported() -> Self {
        FontProfile {
            code_len: 1,
            decode: HashMap::new(),
            encode: HashMap::new(),
            supported: false,
        }
    }

    fn from_decode(code_len: usize, decode: HashMap<u32, String>) -> Self {
        let mut encode: HashMap<char, u32> = HashMap::new();
        let mut codes: Vec<u32> = decode.keys().copied().collect();
        codes.sort_unstable();
        for code in codes {
            let text = &decode[&code];
            let mut chars = text.chars();
            if let Some(ch) = chars.next()
                && chars.next().is_none()
            {
                encode.entry(ch).or_insert(code);
            }
        }
        FontProfile {
            code_len: code_len.max(1),
            decode,
            encode,
            supported: true,
        }
    }
}

const WINANSI_HIGH: [u32; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

const MACROMAN_HIGH: [u32; 128] = [
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1, 0x00E0, 0x00E2, 0x00E4, 0x00E3,
    0x00E5, 0x00E7, 0x00E9, 0x00E8, 0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3,
    0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC, 0x2020, 0x00B0, 0x00A2, 0x00A3,
    0x00A7, 0x2022, 0x00B6, 0x00DF, 0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211, 0x220F, 0x03C0, 0x222B, 0x00AA,
    0x00BA, 0x03A9, 0x00E6, 0x00F8, 0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB,
    0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153, 0x2013, 0x2014, 0x201C, 0x201D,
    0x2018, 0x2019, 0x00F7, 0x25CA, 0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02,
    0x2021, 0x00B7, 0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1, 0x00CB, 0x00C8, 0x00CD, 0x00CE,
    0x00CF, 0x00CC, 0x00D3, 0x00D4, 0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC,
    0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
];

fn winansi_char(code: u8) -> Option<char> {
    match code {
        0x80..=0x9F => char::from_u32(WINANSI_HIGH[usize::from(code - 0x80)]),
        _ => char::from_u32(u32::from(code)),
    }
}

fn macroman_char(code: u8) -> Option<char> {
    match code {
        0x80..=0xFF => char::from_u32(MACROMAN_HIGH[usize::from(code - 0x80)]),
        _ => char::from_u32(u32::from(code)),
    }
}

fn glyph_name_to_string(name: &str) -> Option<String> {
    if let Some(hex) = name.strip_prefix("uni").filter(|rest| rest.len() >= 4)
        && hex.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        let mut out = String::new();
        let mut rest = hex;
        while rest.len() >= 4 {
            let (head, tail) = rest.split_at(4);
            out.push(char::from_u32(u32::from_str_radix(head, 16).ok()?)?);
            rest = tail;
        }
        return Some(out);
    }
    if let Some(hex) = name
        .strip_prefix('u')
        .filter(|rest| (4..=6).contains(&rest.len()))
        && hex.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return char::from_u32(u32::from_str_radix(hex, 16).ok()?).map(|ch| ch.to_string());
    }
    if name.chars().count() == 1 {
        return Some(name.to_string());
    }
    let mapped: &str = match name {
        "space" => " ",
        "exclam" => "!",
        "quotedbl" => "\"",
        "numbersign" => "#",
        "dollar" => "$",
        "percent" => "%",
        "ampersand" => "&",
        "quotesingle" => "'",
        "parenleft" => "(",
        "parenright" => ")",
        "asterisk" => "*",
        "plus" => "+",
        "comma" => ",",
        "hyphen" | "minus" => "-",
        "period" => ".",
        "slash" => "/",
        "zero" => "0",
        "one" => "1",
        "two" => "2",
        "three" => "3",
        "four" => "4",
        "five" => "5",
        "six" => "6",
        "seven" => "7",
        "eight" => "8",
        "nine" => "9",
        "colon" => ":",
        "semicolon" => ";",
        "less" => "<",
        "equal" => "=",
        "greater" => ">",
        "question" => "?",
        "at" => "@",
        "bracketleft" => "[",
        "backslash" => "\\",
        "bracketright" => "]",
        "asciicircum" => "^",
        "underscore" => "_",
        "grave" => "`",
        "braceleft" => "{",
        "bar" => "|",
        "braceright" => "}",
        "asciitilde" => "~",
        "bullet" => "\u{2022}",
        "ellipsis" => "\u{2026}",
        "dagger" => "\u{2020}",
        "daggerdbl" => "\u{2021}",
        "endash" => "\u{2013}",
        "emdash" => "\u{2014}",
        "quoteleft" => "\u{2018}",
        "quoteright" => "\u{2019}",
        "quotedblleft" => "\u{201C}",
        "quotedblright" => "\u{201D}",
        "quotesinglbase" => "\u{201A}",
        "quotedblbase" => "\u{201E}",
        "guilsinglleft" => "\u{2039}",
        "guilsinglright" => "\u{203A}",
        "guillemotleft" => "\u{00AB}",
        "guillemotright" => "\u{00BB}",
        "perthousand" => "\u{2030}",
        "trademark" => "\u{2122}",
        "Euro" => "\u{20AC}",
        "sterling" => "\u{00A3}",
        "yen" => "\u{00A5}",
        "cent" => "\u{00A2}",
        "currency" => "\u{00A4}",
        "copyright" => "\u{00A9}",
        "registered" => "\u{00AE}",
        "degree" => "\u{00B0}",
        "plusminus" => "\u{00B1}",
        "multiply" => "\u{00D7}",
        "divide" => "\u{00F7}",
        "fraction" => "\u{2044}",
        "ff" => "\u{FB00}",
        "fi" => "\u{FB01}",
        "fl" => "\u{FB02}",
        "ffi" => "\u{FB03}",
        "ffl" => "\u{FB04}",
        "Agrave" => "\u{00C0}",
        "Aacute" => "\u{00C1}",
        "Acircumflex" => "\u{00C2}",
        "Atilde" => "\u{00C3}",
        "Adieresis" => "\u{00C4}",
        "Aring" => "\u{00C5}",
        "AE" => "\u{00C6}",
        "Ccedilla" => "\u{00C7}",
        "Egrave" => "\u{00C8}",
        "Eacute" => "\u{00C9}",
        "Ecircumflex" => "\u{00CA}",
        "Edieresis" => "\u{00CB}",
        "Igrave" => "\u{00CC}",
        "Iacute" => "\u{00CD}",
        "Icircumflex" => "\u{00CE}",
        "Idieresis" => "\u{00CF}",
        "Eth" => "\u{00D0}",
        "Ntilde" => "\u{00D1}",
        "Ograve" => "\u{00D2}",
        "Oacute" => "\u{00D3}",
        "Ocircumflex" => "\u{00D4}",
        "Otilde" => "\u{00D5}",
        "Odieresis" => "\u{00D6}",
        "Oslash" => "\u{00D8}",
        "Ugrave" => "\u{00D9}",
        "Uacute" => "\u{00DA}",
        "Ucircumflex" => "\u{00DB}",
        "Udieresis" => "\u{00DC}",
        "Yacute" => "\u{00DD}",
        "Thorn" => "\u{00DE}",
        "germandbls" => "\u{00DF}",
        "agrave" => "\u{00E0}",
        "aacute" => "\u{00E1}",
        "acircumflex" => "\u{00E2}",
        "atilde" => "\u{00E3}",
        "adieresis" => "\u{00E4}",
        "aring" => "\u{00E5}",
        "ae" => "\u{00E6}",
        "ccedilla" => "\u{00E7}",
        "egrave" => "\u{00E8}",
        "eacute" => "\u{00E9}",
        "ecircumflex" => "\u{00EA}",
        "edieresis" => "\u{00EB}",
        "igrave" => "\u{00EC}",
        "iacute" => "\u{00ED}",
        "icircumflex" => "\u{00EE}",
        "idieresis" => "\u{00EF}",
        "eth" => "\u{00F0}",
        "ntilde" => "\u{00F1}",
        "ograve" => "\u{00F2}",
        "oacute" => "\u{00F3}",
        "ocircumflex" => "\u{00F4}",
        "otilde" => "\u{00F5}",
        "odieresis" => "\u{00F6}",
        "oslash" => "\u{00F8}",
        "ugrave" => "\u{00F9}",
        "uacute" => "\u{00FA}",
        "ucircumflex" => "\u{00FB}",
        "udieresis" => "\u{00FC}",
        "yacute" => "\u{00FD}",
        "thorn" => "\u{00FE}",
        "ydieresis" => "\u{00FF}",
        "OE" => "\u{0152}",
        "oe" => "\u{0153}",
        "Scaron" => "\u{0160}",
        "scaron" => "\u{0161}",
        "Ydieresis" => "\u{0178}",
        "Zcaron" => "\u{017D}",
        "zcaron" => "\u{017E}",
        "florin" => "\u{0192}",
        _ => return None,
    };
    Some(mapped.to_string())
}

#[derive(Debug, Clone, PartialEq)]
enum CmapToken {
    Hex(Vec<u8>),
    Word(String),
    OpenBracket,
    CloseBracket,
}

fn lex_cmap(data: &[u8]) -> Vec<CmapToken> {
    let mut tokens = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let byte = data[pos];
        if is_pdf_ws(byte) {
            pos += 1;
        } else if byte == b'%' {
            while pos < data.len() && !matches!(data[pos], b'\n' | b'\r') {
                pos += 1;
            }
        } else if byte == b'<' && data.get(pos + 1) == Some(&b'<') {
            tokens.push(CmapToken::Word("<<".to_string()));
            pos += 2;
        } else if byte == b'>' && data.get(pos + 1) == Some(&b'>') {
            tokens.push(CmapToken::Word(">>".to_string()));
            pos += 2;
        } else if byte == b'<' {
            pos += 1;
            let mut digits = Vec::new();
            while pos < data.len() && data[pos] != b'>' {
                if let Some(value) = hex_value(data[pos]) {
                    digits.push(value);
                }
                pos += 1;
            }
            pos += 1;
            if digits.len() % 2 == 1 {
                digits.push(0);
            }
            tokens.push(CmapToken::Hex(
                digits
                    .chunks_exact(2)
                    .map(|pair| pair[0] * 16 + pair[1])
                    .collect(),
            ));
        } else if byte == b'[' {
            tokens.push(CmapToken::OpenBracket);
            pos += 1;
        } else if byte == b']' {
            tokens.push(CmapToken::CloseBracket);
            pos += 1;
        } else {
            let start = pos;
            while pos < data.len()
                && !is_pdf_ws(data[pos])
                && !matches!(data[pos], b'<' | b'>' | b'[' | b']')
            {
                pos += 1;
            }
            if pos > start {
                tokens.push(CmapToken::Word(
                    String::from_utf8_lossy(&data[start..pos]).into_owned(),
                ));
            } else {
                pos += 1;
            }
        }
    }
    tokens
}

fn hex_bytes_to_u32(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |acc, byte| (acc << 8) | u32::from(*byte))
}

fn utf16be_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    char::decode_utf16(units)
        .map(|result| result.unwrap_or('\u{FFFD}'))
        .collect()
}

fn increment_utf16be(bytes: &mut [u8]) {
    let mut index = bytes.len();
    while index >= 2 {
        let unit = u16::from_be_bytes([bytes[index - 2], bytes[index - 1]]);
        if unit == 0xFFFF {
            bytes[index - 2] = 0;
            bytes[index - 1] = 0;
            index -= 2;
        } else {
            let next = (unit + 1).to_be_bytes();
            bytes[index - 2] = next[0];
            bytes[index - 1] = next[1];
            return;
        }
    }
}

/// Parse a `ToUnicode` CMap into a code → Unicode map plus the code byte length
/// declared by its codespace range (when present).
fn parse_tounicode_cmap(data: &[u8]) -> (HashMap<u32, String>, Option<usize>) {
    let tokens = lex_cmap(data);
    let mut decode: HashMap<u32, String> = HashMap::new();
    let mut code_len: Option<usize> = None;
    let mut index = 0usize;
    let word_at = |tokens: &[CmapToken], at: usize, word: &str| -> bool {
        matches!(tokens.get(at), Some(CmapToken::Word(w)) if w == word)
    };
    while index < tokens.len() {
        if word_at(&tokens, index, "begincodespacerange") {
            index += 1;
            while index < tokens.len() && !word_at(&tokens, index, "endcodespacerange") {
                if let Some(CmapToken::Hex(low)) = tokens.get(index) {
                    code_len = Some(low.len().max(1));
                    index += 1;
                } else {
                    index += 1;
                }
                if matches!(tokens.get(index), Some(CmapToken::Hex(_))) {
                    index += 1;
                }
            }
        } else if word_at(&tokens, index, "beginbfchar") {
            index += 1;
            while index < tokens.len() && !word_at(&tokens, index, "endbfchar") {
                let source = match tokens.get(index) {
                    Some(CmapToken::Hex(bytes)) => Some(hex_bytes_to_u32(bytes)),
                    _ => None,
                };
                index += 1;
                let target = match tokens.get(index) {
                    Some(CmapToken::Hex(bytes)) => Some(utf16be_to_string(bytes)),
                    _ => None,
                };
                index += 1;
                if let (Some(code), Some(text)) = (source, target) {
                    decode.insert(code, text);
                }
            }
        } else if word_at(&tokens, index, "beginbfrange") {
            index += 1;
            while index < tokens.len() && !word_at(&tokens, index, "endbfrange") {
                let low = match tokens.get(index) {
                    Some(CmapToken::Hex(bytes)) => hex_bytes_to_u32(bytes),
                    _ => {
                        index += 1;
                        continue;
                    }
                };
                index += 1;
                let high = match tokens.get(index) {
                    Some(CmapToken::Hex(bytes)) => hex_bytes_to_u32(bytes),
                    _ => {
                        index += 1;
                        continue;
                    }
                };
                index += 1;
                let count = (high.saturating_sub(low) as usize)
                    .saturating_add(1)
                    .min(65_536);
                match tokens.get(index).cloned() {
                    Some(CmapToken::Hex(start)) => {
                        index += 1;
                        let mut current = start;
                        for offset in 0..count {
                            decode.insert(low + offset as u32, utf16be_to_string(&current));
                            increment_utf16be(&mut current);
                        }
                    }
                    Some(CmapToken::OpenBracket) => {
                        index += 1;
                        let mut offset = 0u32;
                        while index < tokens.len()
                            && !matches!(tokens.get(index), Some(CmapToken::CloseBracket))
                        {
                            if let Some(CmapToken::Hex(bytes)) = tokens.get(index) {
                                decode.insert(low + offset, utf16be_to_string(bytes));
                                offset += 1;
                            }
                            index += 1;
                        }
                        index += 1;
                    }
                    _ => {
                        index += 1;
                    }
                }
            }
        } else {
            index += 1;
        }
    }
    (decode, code_len)
}

fn font_profile(pdf: &ParsedPdf, font_dict: &[(String, Value)]) -> FontProfile {
    let subtype = dict_get(font_dict, "Subtype")
        .and_then(as_name)
        .unwrap_or("");

    if let Some(tounicode) = dict_get(font_dict, "ToUnicode")
        && let Some(reference) = as_ref(tounicode)
        && let Some(Ok(data)) = pdf.decoded_stream(reference)
    {
        let (decode, codespace_len) = parse_tounicode_cmap(&data);
        if !decode.is_empty() {
            let code_len = codespace_len.unwrap_or(if subtype == "Type0" { 2 } else { 1 });
            return FontProfile::from_decode(code_len, decode);
        }
    }

    // Composite (CID) fonts without a ToUnicode map name glyphs, not Unicode:
    // guessing would corrupt prose, so their text is reported and left alone.
    if subtype == "Type0" {
        return FontProfile::unsupported();
    }
    let base_font = dict_get(font_dict, "BaseFont")
        .and_then(as_name)
        .unwrap_or("")
        .to_ascii_lowercase();
    if base_font.contains("symbol") || base_font.contains("zapfdingbats") {
        return FontProfile::unsupported();
    }

    let encoding = dict_get(font_dict, "Encoding").map(|value| pdf.resolve(value).clone());
    let (mut base_name, mut differences): (String, Vec<Value>) =
        ("WinAnsiEncoding".to_string(), Vec::new());
    match &encoding {
        Some(Value::Name(name)) => base_name = name.clone(),
        Some(Value::Dict(dict)) => {
            if let Some(name) = dict_get(dict, "BaseEncoding").and_then(as_name) {
                base_name = name.to_string();
            }
            if let Some(Value::Array(items)) = dict_get(dict, "Differences") {
                differences = items.clone();
            }
        }
        _ => {}
    }
    if base_name == "MacExpertEncoding" {
        return FontProfile::unsupported();
    }

    let mut decode: HashMap<u32, String> = HashMap::new();
    for code in 0u16..=255 {
        let byte = code as u8;
        let ch = if base_name == "MacRomanEncoding" {
            macroman_char(byte)
        } else {
            winansi_char(byte)
        };
        if let Some(ch) = ch {
            decode.insert(u32::from(byte), ch.to_string());
        }
    }
    let mut current: Option<u32> = None;
    for item in &differences {
        match item {
            Value::Int(code) => current = u32::try_from(*code).ok(),
            Value::Name(name) => {
                if let Some(code) = current {
                    match glyph_name_to_string(name) {
                        Some(text) => {
                            decode.insert(code, text);
                        }
                        None => {
                            decode.remove(&code);
                        }
                    }
                    current = Some(code + 1);
                }
            }
            _ => {}
        }
    }
    FontProfile::from_decode(1, decode)
}

fn fonts_for_resources(
    pdf: &ParsedPdf,
    resources: Option<&Value>,
) -> BTreeMap<String, FontProfile> {
    let mut fonts = BTreeMap::new();
    let Some(resources) = resources else {
        return fonts;
    };
    let Some(resource_dict) = pdf.resolve_dict(resources) else {
        return fonts;
    };
    let Some(font_value) = dict_get(resource_dict, "Font") else {
        return fonts;
    };
    let Some(font_dict) = pdf.resolve_dict(font_value).cloned() else {
        return fonts;
    };
    for (name, font) in &font_dict {
        if let Some(dict) = pdf.resolve_dict(font) {
            fonts.insert(name.clone(), font_profile(pdf, dict));
        }
    }
    fonts
}

// ---------------------------------------------------------------------------
// Page tree, content streams, and prose spans
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct StreamCtx {
    reference: Ref,
    decoded: Vec<u8>,
    fonts: BTreeMap<String, FontProfile>,
    base: usize,
}

#[derive(Debug, Clone)]
struct SegmentInfo {
    stream_index: usize,
    local_range: Range<usize>,
    boundaries: Vec<usize>,
    text: String,
    font_name: String,
    is_hex: bool,
}

#[derive(Debug)]
struct DerivedPdf {
    parsed: ParsedPdf,
    streams: Vec<StreamCtx>,
    spans: Vec<ProseSpan>,
    span_segments: Vec<Vec<SegmentInfo>>,
    #[allow(dead_code)]
    virtual_source: Vec<u8>,
    unsupported_fragments: usize,
    page_count: usize,
}

struct PageInfo {
    content_refs: Vec<Ref>,
    fonts: BTreeMap<String, FontProfile>,
    resources: Option<Value>,
}

fn walk_pages(
    pdf: &ParsedPdf,
    node: &Value,
    inherited_resources: &Option<Value>,
    visited: &mut BTreeSet<Ref>,
    out: &mut Vec<PageInfo>,
    depth: usize,
) {
    if depth > 64 || out.len() > 100_000 {
        return;
    }
    if let Value::Ref(reference) = node
        && !visited.insert(*reference)
    {
        return;
    }
    let Some(dict) = pdf.resolve_dict(node) else {
        return;
    };
    let resources = dict_get(dict, "Resources")
        .cloned()
        .or_else(|| inherited_resources.clone());
    if let Some(Value::Array(kids)) = dict_get(dict, "Kids")
        && !kids.is_empty()
    {
        let kids = kids.clone();
        for kid in &kids {
            walk_pages(pdf, kid, &resources, visited, out, depth + 1);
        }
        return;
    }
    let mut content_refs = Vec::new();
    match dict_get(dict, "Contents") {
        Some(Value::Ref(reference)) => content_refs.push(*reference),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(reference) = as_ref(item) {
                    content_refs.push(reference);
                }
            }
        }
        _ => {}
    }
    let fonts = fonts_for_resources(pdf, resources.as_ref());
    out.push(PageInfo {
        content_refs,
        fonts,
        resources,
    });
}

fn push_content_stream(
    pdf: &ParsedPdf,
    streams: &mut Vec<StreamCtx>,
    seen: &mut BTreeSet<Ref>,
    reference: Ref,
    fonts: BTreeMap<String, FontProfile>,
) {
    if !seen.insert(reference) {
        return;
    }
    if let Some(Ok(decoded)) = pdf.decoded_stream(reference) {
        streams.push(StreamCtx {
            reference,
            decoded,
            fonts,
            base: 0,
        });
    }
}

fn collect_form_streams(
    pdf: &ParsedPdf,
    resources: Option<&Value>,
    inherited_fonts: &BTreeMap<String, FontProfile>,
    streams: &mut Vec<StreamCtx>,
    seen: &mut BTreeSet<Ref>,
    visited_forms: &mut BTreeSet<Ref>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    let Some(resources) = resources else {
        return;
    };
    let Some(resource_dict) = pdf.resolve_dict(resources).cloned() else {
        return;
    };
    let Some(xobjects) = dict_get(&resource_dict, "XObject") else {
        return;
    };
    let Some(xobject_dict) = pdf.resolve_dict(xobjects).cloned() else {
        return;
    };
    for (_, xobject) in &xobject_dict {
        let Some(reference) = as_ref(xobject) else {
            continue;
        };
        if !visited_forms.insert(reference) {
            continue;
        }
        let Some(dict) = pdf.object_dict(reference).cloned() else {
            continue;
        };
        if dict_get(&dict, "Subtype").and_then(as_name) != Some("Form") {
            continue;
        }
        let own_resources = dict_get(&dict, "Resources").cloned();
        let mut fonts = inherited_fonts.clone();
        for (name, profile) in fonts_for_resources(pdf, own_resources.as_ref()) {
            fonts.insert(name, profile);
        }
        push_content_stream(pdf, streams, seen, reference, fonts.clone());
        collect_form_streams(
            pdf,
            own_resources.as_ref().or(Some(resources)),
            &fonts,
            streams,
            seen,
            visited_forms,
            depth + 1,
        );
    }
}

#[derive(Debug, Clone)]
struct ContentString {
    inner: Range<usize>,
    bytes: Vec<u8>,
    boundaries: Vec<usize>,
    is_hex: bool,
}

#[derive(Debug, Clone)]
enum Operand {
    Name(String),
    Str(ContentString),
    Array(Vec<Operand>),
    Other,
}

fn parse_content_literal(bytes: &[u8], start: usize) -> Result<(ContentString, usize)> {
    let inner_start = start + 1;
    let mut out = Vec::new();
    let mut boundaries = vec![0usize];
    let mut depth = 1usize;
    let mut pos = inner_start;
    while pos < bytes.len() {
        let byte_start = pos;
        let byte = bytes[pos];
        pos += 1;
        let produced: Option<u8> = match byte {
            b'\\' => {
                let Some(escaped) = bytes.get(pos).copied() else {
                    break;
                };
                pos += 1;
                match escaped {
                    b'n' => Some(b'\n'),
                    b'r' => Some(b'\r'),
                    b't' => Some(b'\t'),
                    b'b' => Some(8),
                    b'f' => Some(12),
                    b'(' | b')' | b'\\' => Some(escaped),
                    b'\r' => {
                        if bytes.get(pos) == Some(&b'\n') {
                            pos += 1;
                        }
                        None
                    }
                    b'\n' => None,
                    b'0'..=b'7' => {
                        let mut value = u32::from(escaped - b'0');
                        for _ in 0..2 {
                            match bytes.get(pos).copied() {
                                Some(digit @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(digit - b'0');
                                    pos += 1;
                                }
                                _ => break,
                            }
                        }
                        Some((value & 0xFF) as u8)
                    }
                    other => Some(other),
                }
            }
            b'(' => {
                depth += 1;
                Some(byte)
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    let inner = inner_start..byte_start;
                    return Ok((
                        ContentString {
                            inner,
                            bytes: out,
                            boundaries,
                            is_hex: false,
                        },
                        pos,
                    ));
                }
                Some(byte)
            }
            other => Some(other),
        };
        if let Some(value) = produced {
            out.push(value);
            boundaries.push(pos - inner_start);
        }
    }
    Err(CoreError::Parse(
        "unterminated string in PDF content stream".to_string(),
    ))
}

fn parse_content_hex(bytes: &[u8], start: usize) -> Result<(ContentString, usize)> {
    let inner_start = start + 1;
    let mut digits: Vec<(u8, usize)> = Vec::new();
    let mut pos = inner_start;
    while pos < bytes.len() {
        let byte = bytes[pos];
        pos += 1;
        if byte == b'>' {
            let mut out = Vec::new();
            let mut boundaries = vec![0usize];
            let mut index = 0usize;
            while index < digits.len() {
                if index + 1 < digits.len() {
                    out.push(digits[index].0 * 16 + digits[index + 1].0);
                    boundaries.push(digits[index + 1].1 - inner_start);
                    index += 2;
                } else {
                    out.push(digits[index].0 * 16);
                    boundaries.push(digits[index].1 - inner_start);
                    index += 1;
                }
            }
            return Ok((
                ContentString {
                    inner: inner_start..pos - 1,
                    bytes: out,
                    boundaries,
                    is_hex: true,
                },
                pos,
            ));
        }
        if let Some(value) = hex_value(byte) {
            digits.push((value, pos));
        }
    }
    Err(CoreError::Parse(
        "unterminated hex string in PDF content stream".to_string(),
    ))
}

struct ContentLexer<'a> {
    bytes: &'a [u8],
    pos: usize,
}

#[derive(Debug, Clone)]
enum ContentToken {
    Operand(Operand),
    Operator(String),
}

impl<'a> ContentLexer<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        ContentLexer { bytes, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while let Some(&byte) = self.bytes.get(self.pos) {
            if is_pdf_ws(byte) {
                self.pos += 1;
            } else if byte == b'%' {
                while let Some(&inner) = self.bytes.get(self.pos) {
                    self.pos += 1;
                    if matches!(inner, b'\n' | b'\r') {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn next_token(&mut self) -> Option<ContentToken> {
        self.skip_ws();
        let byte = *self.bytes.get(self.pos)?;
        match byte {
            b'(' => {
                let (string, end) = parse_content_literal(self.bytes, self.pos).ok()?;
                self.pos = end;
                Some(ContentToken::Operand(Operand::Str(string)))
            }
            b'<' => {
                if self.bytes.get(self.pos + 1) == Some(&b'<') {
                    // Inline dictionary operand (marked content): skip it whole.
                    self.pos += 2;
                    let mut depth = 1usize;
                    while self.pos < self.bytes.len() && depth > 0 {
                        match self.next_token() {
                            Some(ContentToken::Operator(operator)) if operator == "<<" => {
                                depth += 1;
                            }
                            Some(ContentToken::Operator(operator)) if operator == ">>" => {
                                depth -= 1;
                            }
                            Some(_) => {}
                            None => break,
                        }
                    }
                    Some(ContentToken::Operand(Operand::Other))
                } else {
                    let (string, end) = parse_content_hex(self.bytes, self.pos).ok()?;
                    self.pos = end;
                    Some(ContentToken::Operand(Operand::Str(string)))
                }
            }
            b'>' => {
                if self.bytes.get(self.pos + 1) == Some(&b'>') {
                    self.pos += 2;
                    Some(ContentToken::Operator(">>".to_string()))
                } else {
                    self.pos += 1;
                    Some(ContentToken::Operand(Operand::Other))
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.bytes.get(self.pos) {
                        None => break,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(_) => match self.next_token() {
                            Some(ContentToken::Operand(operand)) => items.push(operand),
                            Some(_) => items.push(Operand::Other),
                            None => break,
                        },
                    }
                }
                Some(ContentToken::Operand(Operand::Array(items)))
            }
            b'/' => {
                self.pos += 1;
                let start = self.pos;
                while let Some(&inner) = self.bytes.get(self.pos) {
                    if is_pdf_ws(inner) || is_pdf_delimiter(inner) {
                        break;
                    }
                    self.pos += 1;
                }
                Some(ContentToken::Operand(Operand::Name(
                    String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned(),
                )))
            }
            b'\'' | b'"' => {
                self.pos += 1;
                Some(ContentToken::Operator((byte as char).to_string()))
            }
            _ => {
                let start = self.pos;
                while let Some(&inner) = self.bytes.get(self.pos) {
                    if is_pdf_ws(inner) || is_pdf_delimiter(inner) || matches!(inner, b'\'' | b'"')
                    {
                        break;
                    }
                    self.pos += 1;
                }
                if self.pos == start {
                    self.pos += 1;
                    return self.next_token();
                }
                let token = String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned();
                let numeric = token
                    .chars()
                    .all(|ch| ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.'))
                    && token.chars().any(|ch| ch.is_ascii_digit());
                if numeric || token == "true" || token == "false" || token == "null" {
                    Some(ContentToken::Operand(Operand::Other))
                } else {
                    Some(ContentToken::Operator(token))
                }
            }
        }
    }

    /// Skip an inline image (`BI … ID … EI`) so its binary samples are never
    /// mistaken for operators or prose. The image bytes stay in the stream.
    fn skip_inline_image(&mut self) {
        loop {
            match self.next_token() {
                Some(ContentToken::Operator(operator)) if operator == "ID" => {
                    if self.bytes.get(self.pos).is_some_and(|&b| is_pdf_ws(b)) {
                        self.pos += 1;
                    }
                    let data_start = self.pos;
                    let mut search = data_start;
                    while let Some(at) = find_subslice(self.bytes, search, b"EI") {
                        let before_ok = at > data_start && is_pdf_ws(self.bytes[at - 1]);
                        let after_ok = self
                            .bytes
                            .get(at + 2)
                            .is_none_or(|&b| is_pdf_ws(b) || is_pdf_delimiter(b));
                        if before_ok && after_ok {
                            self.pos = at + 2;
                            return;
                        }
                        search = at + 1;
                    }
                    self.pos = self.bytes.len();
                    return;
                }
                Some(_) => {}
                None => return,
            }
        }
    }
}

fn make_segment(
    stream_index: usize,
    fonts: &BTreeMap<String, FontProfile>,
    font_name: Option<&str>,
    string: &ContentString,
    unsupported: &mut usize,
) -> Option<SegmentInfo> {
    let Some(font_name) = font_name else {
        *unsupported += 1;
        return None;
    };
    let Some(profile) = fonts.get(font_name).filter(|profile| profile.supported) else {
        *unsupported += 1;
        return None;
    };
    let code_len = profile.code_len.max(1);
    let mut text = String::new();
    let mut boundaries = vec![0usize];
    let mut byte_index = 0usize;
    while byte_index < string.bytes.len() {
        let end = (byte_index + code_len).min(string.bytes.len());
        let code = hex_bytes_to_u32(&string.bytes[byte_index..end]);
        let unicode = profile
            .decode
            .get(&code)
            .cloned()
            .unwrap_or_else(|| "\u{FFFD}".to_string());
        let source_start = string.boundaries.get(byte_index).copied().unwrap_or(0);
        let source_end = string.boundaries.get(end).copied().unwrap_or(source_start);
        text.push_str(&unicode);
        while boundaries.len() < text.len() {
            boundaries.push(source_start);
        }
        boundaries.push(source_end);
        byte_index = end;
    }
    if text.is_empty() {
        return None;
    }
    Some(SegmentInfo {
        stream_index,
        local_range: string.inner.clone(),
        boundaries,
        text,
        font_name: font_name.to_string(),
        is_hex: string.is_hex,
    })
}

fn flush_span(
    streams: &[StreamCtx],
    spans: &mut Vec<ProseSpan>,
    span_segments: &mut Vec<Vec<SegmentInfo>>,
    segments: &mut Vec<SegmentInfo>,
) {
    if segments.is_empty() {
        return;
    }
    let taken: Vec<SegmentInfo> = std::mem::take(segments);
    let text: String = taken.iter().map(|segment| segment.text.as_str()).collect();
    let decoded_segments: Vec<DecodedSegment> = taken
        .iter()
        .map(|segment| {
            let base = streams
                .get(segment.stream_index)
                .map(|stream| stream.base)
                .unwrap_or(0);
            DecodedSegment {
                source: (base + segment.local_range.start)..(base + segment.local_range.end),
                boundaries: segment.boundaries.clone(),
            }
        })
        .collect();
    spans.push(ProseSpan::new(
        SpanAnchor::DecodedSegments(decoded_segments),
        text,
    ));
    span_segments.push(taken);
}

fn tokenize_stream(
    stream_index: usize,
    streams: &[StreamCtx],
    spans: &mut Vec<ProseSpan>,
    span_segments: &mut Vec<Vec<SegmentInfo>>,
    unsupported: &mut usize,
) {
    let Some(ctx) = streams.get(stream_index) else {
        return;
    };
    let fonts = ctx.fonts.clone();
    let mut lexer = ContentLexer::new(&ctx.decoded);
    let mut operands: Vec<Operand> = Vec::new();
    let mut current_font: Option<String> = None;
    let mut font_stack: Vec<Option<String>> = Vec::new();
    let mut segments: Vec<SegmentInfo> = Vec::new();

    while let Some(token) = lexer.next_token() {
        match token {
            ContentToken::Operand(operand) => operands.push(operand),
            ContentToken::Operator(operator) => {
                match operator.as_str() {
                    "BI" => lexer.skip_inline_image(),
                    "BT" => {
                        flush_span(streams, spans, span_segments, &mut segments);
                    }
                    "ET" => {
                        flush_span(streams, spans, span_segments, &mut segments);
                    }
                    "Tf" => {
                        if let Some(Operand::Name(name)) = operands.first() {
                            current_font = Some(name.clone());
                        }
                    }
                    "Tj" | "'" => {
                        if let Some(Operand::Str(string)) = operands.into_iter().next() {
                            if let Some(segment) = make_segment(
                                stream_index,
                                &fonts,
                                current_font.as_deref(),
                                &string,
                                unsupported,
                            ) {
                                segments.push(segment);
                            }
                        }
                        operands = Vec::new();
                        continue;
                    }
                    "\"" => {
                        if let Some(Operand::Str(string)) = operands.into_iter().last() {
                            if let Some(segment) = make_segment(
                                stream_index,
                                &fonts,
                                current_font.as_deref(),
                                &string,
                                unsupported,
                            ) {
                                segments.push(segment);
                            }
                        }
                        operands = Vec::new();
                        continue;
                    }
                    "TJ" => {
                        if let Some(Operand::Array(items)) = operands.into_iter().next() {
                            for item in items {
                                if let Operand::Str(string) = item
                                    && let Some(segment) = make_segment(
                                        stream_index,
                                        &fonts,
                                        current_font.as_deref(),
                                        &string,
                                        unsupported,
                                    )
                                {
                                    segments.push(segment);
                                }
                            }
                        }
                        operands = Vec::new();
                        continue;
                    }
                    "q" => font_stack.push(current_font.clone()),
                    "Q" => current_font = font_stack.pop().unwrap_or(current_font),
                    _ => {}
                }
                operands.clear();
            }
        }
    }
    flush_span(streams, spans, span_segments, &mut segments);
}

fn derive_pdf(bytes: &[u8]) -> Result<DerivedPdf> {
    let parsed = parse_pdf(bytes)?;
    if parsed.encrypted {
        return Err(CoreError::Parse(
            "this PDF is password-protected (encrypted); remove the password and try again"
                .to_string(),
        ));
    }

    let mut pages: Vec<PageInfo> = Vec::new();
    if let Some(root) = parsed.root
        && let Some(catalog) = parsed.object_dict(root)
        && let Some(pages_root) = dict_get(catalog, "Pages").cloned()
    {
        let mut visited = BTreeSet::new();
        walk_pages(&parsed, &pages_root, &None, &mut visited, &mut pages, 0);
    }
    let page_count = pages.len();

    let mut streams: Vec<StreamCtx> = Vec::new();
    let mut seen: BTreeSet<Ref> = BTreeSet::new();
    for page in &pages {
        for &reference in &page.content_refs {
            push_content_stream(
                &parsed,
                &mut streams,
                &mut seen,
                reference,
                page.fonts.clone(),
            );
        }
    }
    let mut visited_forms: BTreeSet<Ref> = BTreeSet::new();
    for page in &pages {
        collect_form_streams(
            &parsed,
            page.resources.as_ref(),
            &page.fonts,
            &mut streams,
            &mut seen,
            &mut visited_forms,
            0,
        );
    }

    let mut virtual_source = Vec::new();
    for stream in &mut streams {
        stream.base = virtual_source.len();
        virtual_source.extend_from_slice(&stream.decoded);
        virtual_source.push(b'\n');
    }

    let mut spans = Vec::new();
    let mut span_segments = Vec::new();
    let mut unsupported_fragments = 0usize;
    for index in 0..streams.len() {
        tokenize_stream(
            index,
            &streams,
            &mut spans,
            &mut span_segments,
            &mut unsupported_fragments,
        );
    }

    Ok(DerivedPdf {
        parsed,
        streams,
        spans,
        span_segments,
        virtual_source,
        unsupported_fragments,
        page_count,
    })
}

// ---------------------------------------------------------------------------
// Value serialization and whole-file repacking
// ---------------------------------------------------------------------------

fn serialize_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Int(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::Real(text) => out.extend_from_slice(text.as_bytes()),
        Value::Name(name) => {
            out.push(b'/');
            for &byte in name.as_bytes() {
                if is_pdf_ws(byte) || is_pdf_delimiter(byte) || byte < 33 || byte > 126 {
                    out.extend_from_slice(format!("#{byte:02X}").as_bytes());
                } else {
                    out.push(byte);
                }
            }
        }
        Value::Str(bytes) => {
            out.push(b'<');
            for byte in bytes {
                out.extend_from_slice(format!("{byte:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Value::Array(items) => {
            out.push(b'[');
            for item in items {
                out.push(b' ');
                serialize_value(item, out);
            }
            out.push(b']');
        }
        Value::Dict(entries) => {
            out.extend_from_slice(b"<<");
            for (key, item) in entries {
                out.push(b' ');
                serialize_value(&Value::Name(key.clone()), out);
                out.push(b' ');
                serialize_value(item, out);
            }
            out.extend_from_slice(b" >>");
        }
        Value::Ref(reference) => {
            out.extend_from_slice(
                format!("{} {} R", reference.num, reference.generation).as_bytes(),
            );
        }
    }
}

fn serialize_to_vec(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    serialize_value(value, &mut out);
    out
}

fn set_dict_entry(dict: &mut Vec<(String, Value)>, key: &str, value: Value) {
    if let Some(entry) = dict.iter_mut().find(|(name, _)| name == key) {
        entry.1 = value;
    } else {
        dict.push((key.to_string(), value));
    }
}

fn remove_dict_entries(dict: &mut Vec<(String, Value)>, keys: &[&str]) {
    dict.retain(|(name, _)| !keys.contains(&name.as_str()));
}

fn dict_filters(dict: &[(String, Value)]) -> Vec<String> {
    let mut filters = Vec::new();
    match dict_get(dict, "Filter").or_else(|| dict_get(dict, "F")) {
        Some(Value::Name(name)) => filters.push(name.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(name) = as_name(item) {
                    filters.push(name.to_string());
                }
            }
        }
        _ => {}
    }
    filters
}

/// Rebuild one stream object's body around new (decoded) stream data, fixing
/// `/Length` and the filter chain. When the original stream was Flate-compressed
/// the replacement is compressed again; otherwise it is stored plainly.
fn build_stream_body(
    original_dict: &[(String, Value)],
    decoded_data: &[u8],
    compress: bool,
) -> Result<Vec<u8>> {
    let data = if compress {
        deflate(decoded_data)?
    } else {
        decoded_data.to_vec()
    };
    let mut dict = original_dict.to_vec();
    if compress {
        set_dict_entry(&mut dict, "Filter", Value::Name("FlateDecode".to_string()));
    } else {
        remove_dict_entries(&mut dict, &["Filter", "F"]);
    }
    remove_dict_entries(&mut dict, &["DecodeParms", "DP", "DL"]);
    set_dict_entry(&mut dict, "Length", Value::Int(data.len() as i64));
    let mut body = serialize_to_vec(&Value::Dict(dict));
    body.extend_from_slice(b"\nstream\n");
    body.extend_from_slice(&data);
    body.extend_from_slice(b"\nendstream");
    Ok(body)
}

/// Rebuild one stream object's body around replacement *raw* stream bytes
/// (the filter chain is preserved; only `/Length` changes). Used for scrubbed
/// JPEG image XObjects, whose bytes are a complete JFIF stream.
fn build_raw_stream_body(original_dict: &[(String, Value)], raw_data: &[u8]) -> Vec<u8> {
    let mut dict = original_dict.to_vec();
    set_dict_entry(&mut dict, "Length", Value::Int(raw_data.len() as i64));
    let mut body = serialize_to_vec(&Value::Dict(dict));
    body.extend_from_slice(b"\nstream\n");
    body.extend_from_slice(raw_data);
    body.extend_from_slice(b"\nendstream");
    body
}

fn trim_body(raw: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = raw.len();
    while start < end && is_pdf_ws(raw[start]) {
        start += 1;
    }
    while end > start && is_pdf_ws(raw[end - 1]) {
        end -= 1;
    }
    &raw[start..end]
}

/// Rewrite the whole file object-by-object: unmodified objects are copied
/// byte-for-byte (images, fonts, page geometry, annotations, …), replaced
/// objects use the supplied bodies, packaging objects (old xref streams and
/// object streams) are dropped in favour of their materialized contents, and
/// a fresh classic xref table + trailer is generated.
fn repack_pdf(
    original: &[u8],
    parsed: &ParsedPdf,
    replacements: &BTreeMap<Ref, Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(original.len());
    let header_offset = if original.starts_with(b"%PDF-") {
        0
    } else {
        find_subslice(original, 0, b"%PDF-").unwrap_or(0)
    };
    let header_end = original[header_offset..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|at| header_offset + at + 1)
        .unwrap_or(original.len().min(header_offset + 9));
    out.extend_from_slice(&original[header_offset..header_end]);
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let second_line_end = original[header_end..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|at| header_end + at + 1)
        .unwrap_or(header_end);
    let second_line = &original[header_end..second_line_end];
    if second_line.starts_with(b"%")
        && !second_line.starts_with(b"%PDF-")
        && !second_line.starts_with(b"%%EOF")
    {
        out.extend_from_slice(second_line);
    } else {
        out.extend_from_slice(b"%\xe2\xe3\xcf\xd3\n");
    }

    let mut offsets: BTreeMap<u32, (usize, u16)> = BTreeMap::new();
    for object in parsed.objects.values() {
        if object_is(object, "Type", "XRef") || object_is(object, "Type", "ObjStm") {
            continue;
        }
        let offset = out.len();
        out.extend_from_slice(
            format!(
                "{} {} obj\n",
                object.reference.num, object.reference.generation
            )
            .as_bytes(),
        );
        match replacements.get(&object.reference) {
            Some(body) => out.extend_from_slice(body),
            None => out.extend_from_slice(trim_body(&object.raw_body)),
        }
        out.extend_from_slice(b"\nendobj\n");
        offsets.insert(object.reference.num, (offset, object.reference.generation));
    }

    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    let numbers: Vec<u32> = offsets.keys().copied().filter(|num| *num > 0).collect();
    let mut index = 0usize;
    while index < numbers.len() {
        let first = numbers[index];
        let mut count = 1usize;
        while index + count < numbers.len() && numbers[index + count] == first + count as u32 {
            count += 1;
        }
        out.extend_from_slice(format!("{first} {count}\n").as_bytes());
        for num in &numbers[index..index + count] {
            let (offset, generation) = offsets[num];
            out.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
        index += count;
    }

    let size = numbers.last().copied().unwrap_or(0) + 1;
    let mut trailer: Vec<(String, Value)> = vec![("Size".to_string(), Value::Int(i64::from(size)))];
    if let Some(root) = parsed.root {
        trailer.push(("Root".to_string(), Value::Ref(root)));
    } else {
        return Err(CoreError::Parse("PDF has no document catalog".to_string()));
    }
    if let Some(info) = parsed.info
        && parsed.get(info).is_some()
    {
        trailer.push(("Info".to_string(), Value::Ref(info)));
    }
    if let Some(id) = dict_get(&parsed.trailer, "ID") {
        trailer.push(("ID".to_string(), id.clone()));
    }
    out.extend_from_slice(b"trailer\n");
    out.extend_from_slice(&serialize_to_vec(&Value::Dict(trailer)));
    out.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    Ok(out)
}

// ---------------------------------------------------------------------------
// Rendering edits back into content streams
// ---------------------------------------------------------------------------

fn encode_replacement(profile: &FontProfile, replacement: &str, is_hex: bool) -> Option<Vec<u8>> {
    let mut encoded: Vec<u8> = Vec::new();
    for ch in replacement.chars() {
        let code = *profile.encode.get(&ch)?;
        if profile.code_len < 4 && (code >> (profile.code_len * 8)) != 0 {
            return None;
        }
        for shift in (0..profile.code_len).rev() {
            encoded.push(((code >> (shift * 8)) & 0xFF) as u8);
        }
    }
    if is_hex {
        let mut out = Vec::with_capacity(encoded.len() * 2);
        for byte in encoded {
            out.extend_from_slice(format!("{byte:02X}").as_bytes());
        }
        Some(out)
    } else {
        let mut out = Vec::new();
        for byte in encoded {
            match byte {
                b'(' | b')' | b'\\' => {
                    out.push(b'\\');
                    out.push(byte);
                }
                0x20..=0x7E => out.push(byte),
                _ => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
            }
        }
        Some(out)
    }
}

fn splice_bytes(source: &[u8], edits: &[(Range<usize>, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut sorted: Vec<&(Range<usize>, Vec<u8>)> = edits.iter().collect();
    sorted.sort_by_key(|(range, _)| range.start);
    let mut previous_end = 0usize;
    for (range, _) in &sorted {
        if range.start < previous_end || range.end > source.len() || range.start > range.end {
            return Err(CoreError::SelfCheck(format!(
                "invalid PDF content edit at byte {}",
                range.start
            )));
        }
        previous_end = range.end;
    }
    let mut out = Vec::with_capacity(source.len());
    let mut cursor = 0usize;
    for (range, replacement) in sorted {
        out.extend_from_slice(&source[cursor..range.start]);
        out.extend_from_slice(replacement);
        cursor = range.end;
    }
    out.extend_from_slice(&source[cursor..]);
    Ok(out)
}

fn fnv1a(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

// ---------------------------------------------------------------------------
// Container (non-prose) fingerprints: Info dict, XMP, embedded JPEG images
// ---------------------------------------------------------------------------

const AI_SIGNS: &[&str] = &[
    "chatgpt",
    "openai",
    "gpt-",
    "claude",
    "anthropic",
    "gemini",
    "copilot",
    "deepseek",
    "qwen",
    "mistral",
    "perplexity",
    "grok",
    "kimi",
    "manus",
    "genspark",
    "notebooklm",
    "ai-generated",
    "ai generated",
    "generated by ai",
    "large language model",
    "c2pa",
    "content credentials",
    "trainedalgorithmicmedia",
];

fn ai_sign_in(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    AI_SIGNS.iter().copied().find(|sign| lower.contains(sign))
}

fn decode_pdf_text_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return utf16be_to_string(&bytes[2..]);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return char::decode_utf16(units)
            .map(|result| result.unwrap_or('\u{FFFD}'))
            .collect();
    }
    bytes
        .iter()
        .map(|&byte| winansi_char(byte).unwrap_or('\u{FFFD}'))
        .collect()
}

fn inspect_xmp_bytes(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let lower = text.to_ascii_lowercase();
    if lower.contains("c2pa") || lower.contains("contentcredentials") {
        return Some("carries C2PA / Content Credentials provenance".to_string());
    }
    if lower.contains("trainedalgorithmicmedia")
        || lower.contains("compositewithtrainedalgorithmicmedia")
    {
        return Some("declares IPTC trainedAlgorithmicMedia (AI-generated) provenance".to_string());
    }
    if let Some(sign) = ai_sign_in(&text)
        && (lower.contains("creatortool")
            || lower.contains("softwareagent")
            || lower.contains("dc:creator")
            || lower.contains("digitalsourcetype")
            || lower.contains("xmp:"))
    {
        return Some(format!(
            "names an AI generator ({sign:?}) as its creating tool"
        ));
    }
    None
}

struct ContainerTargets {
    info_hits: Vec<(String, String)>,
    xmp_hits: Vec<(Ref, String)>,
    jpeg_refs: Vec<Ref>,
}

fn container_targets(parsed: &ParsedPdf) -> ContainerTargets {
    let mut info_hits = Vec::new();
    if let Some(info) = parsed.info
        && let Some(dict) = parsed.object_dict(info)
    {
        for field in [
            "Title", "Author", "Creator", "Producer", "Subject", "Keywords",
        ] {
            if let Some(Value::Str(bytes)) = dict_get(dict, field) {
                let value = decode_pdf_text_string(bytes);
                if !value.trim().is_empty() && ai_sign_in(&value).is_some() {
                    info_hits.push((field.to_string(), value));
                }
            }
        }
    }

    let mut xmp_candidates: BTreeSet<Ref> = BTreeSet::new();
    for object in parsed.objects.values() {
        if let Some(dict) = as_dict(&object.value)
            && let Some(reference) = dict_get(dict, "Metadata").and_then(as_ref)
        {
            xmp_candidates.insert(reference);
        }
    }
    let mut xmp_hits = Vec::new();
    for reference in xmp_candidates {
        if let Some(Ok(data)) = parsed.decoded_stream(reference)
            && let Some(reason) = inspect_xmp_bytes(&data)
        {
            xmp_hits.push((reference, reason));
        }
    }

    let mut jpeg_refs = Vec::new();
    for object in parsed.objects.values() {
        let Some(dict) = as_dict(&object.value) else {
            continue;
        };
        if dict_get(dict, "Subtype").and_then(as_name) != Some("Image") {
            continue;
        }
        if !dict_filters(dict)
            .iter()
            .any(|f| f == "DCTDecode" || f == "DCT")
        {
            continue;
        }
        if let Some(raw) = object.stream.as_deref()
            && crate::c2pa::scrub_media(raw).is_some()
        {
            jpeg_refs.push(object.reference);
        }
    }

    ContainerTargets {
        info_hits,
        xmp_hits,
        jpeg_refs,
    }
}

const EMPTY_XMP: &[u8] = b"<?xpacket begin=\"\xef\xbb\xbf\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n</x:xmpmeta>\n<?xpacket end=\"w\"?>";

// ---------------------------------------------------------------------------
// Format implementation
// ---------------------------------------------------------------------------

impl Format for Pdf {
    fn id(&self) -> FormatId {
        FormatId::Pdf
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let derived = derive_pdf(bytes)?;
        Ok(Document::new(
            bytes.to_vec(),
            derived.spans,
            DocMeta {
                format: FormatId::Pdf,
                newline: Newline::Lf,
                had_bom: false,
            },
        ))
    }

    fn render(&self, doc: &Document, edits: &EditSet) -> Result<Vec<u8>> {
        if edits.edits.is_empty() {
            return Ok(doc.source.clone());
        }
        let derived = derive_pdf(&doc.source)?;

        let mut per_stream: BTreeMap<usize, Vec<(Range<usize>, Vec<u8>)>> = BTreeMap::new();
        for edit in &edits.edits {
            let Some(segments) = derived.span_segments.get(edit.span_index) else {
                return Err(CoreError::SelfCheck(format!(
                    "edit references missing PDF span {}",
                    edit.span_index
                )));
            };
            // Project the span-local edit onto the encoded string operands it
            // covers (first operand receives the replacement, later ones are
            // trimmed) — the same multi-run rule as DOCX. If the replacement
            // cannot be encoded in that operand's font (a subset font that
            // lacks the glyph), the edit is skipped rather than corrupting
            // the text; everything else still applies.
            let mut projected: Vec<(usize, Range<usize>, Vec<u8>)> = Vec::new();
            let mut logical = 0usize;
            let mut first = true;
            let mut encodable = true;
            for segment in segments {
                let segment_start = logical;
                let segment_end = logical + segment.text.len();
                logical = segment_end;
                let overlap_start = edit.range.start.max(segment_start);
                let overlap_end = edit.range.end.min(segment_end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let local_start = overlap_start - segment_start;
                let local_end = overlap_end - segment_start;
                let byte_start = segment.local_range.start
                    + segment
                        .boundaries
                        .get(local_start)
                        .copied()
                        .ok_or_else(|| {
                            CoreError::SelfCheck("PDF edit start outside segment".to_string())
                        })?;
                let byte_end = segment.local_range.start
                    + segment.boundaries.get(local_end).copied().ok_or_else(|| {
                        CoreError::SelfCheck("PDF edit end outside segment".to_string())
                    })?;
                let replacement = if first {
                    let profile = derived
                        .streams
                        .get(segment.stream_index)
                        .and_then(|stream| stream.fonts.get(&segment.font_name));
                    match profile.and_then(|profile| {
                        encode_replacement(profile, &edit.replacement, segment.is_hex)
                    }) {
                        Some(bytes) => bytes,
                        None => {
                            encodable = false;
                            break;
                        }
                    }
                } else {
                    Vec::new()
                };
                first = false;
                projected.push((segment.stream_index, byte_start..byte_end, replacement));
            }
            if !encodable {
                continue;
            }
            for (stream_index, range, replacement) in projected {
                per_stream
                    .entry(stream_index)
                    .or_default()
                    .push((range, replacement));
            }
        }

        if per_stream.is_empty() {
            return Ok(doc.source.clone());
        }

        let mut replacements: BTreeMap<Ref, Vec<u8>> = BTreeMap::new();
        for (stream_index, stream_edits) in &per_stream {
            let Some(stream) = derived.streams.get(*stream_index) else {
                continue;
            };
            let new_data = splice_bytes(&stream.decoded, stream_edits)?;
            let Some(object) = derived.parsed.get(stream.reference) else {
                continue;
            };
            let Some(dict) = as_dict(&object.value) else {
                continue;
            };
            let compress = dict_filters(dict)
                .iter()
                .any(|filter| filter == "FlateDecode" || filter == "Fl");
            replacements.insert(
                stream.reference,
                build_stream_body(dict, &new_data, compress)?,
            );
        }
        if replacements.is_empty() {
            return Ok(doc.source.clone());
        }
        repack_pdf(&doc.source, &derived.parsed, &replacements)
    }

    fn skeleton(&self, bytes: &[u8]) -> Result<String> {
        let derived = derive_pdf(bytes)?;
        let parsed = &derived.parsed;

        // Canonical form of each content stream: the decoded operators with
        // every prose string operand replaced by a fixed placeholder. Equal
        // skeletons therefore prove the layout operators (and everything
        // outside content streams) survived, whatever the prose became.
        let mut canonical_content: BTreeMap<Ref, String> = BTreeMap::new();
        for (index, stream) in derived.streams.iter().enumerate() {
            let edits: Vec<(Range<usize>, Vec<u8>)> = derived
                .span_segments
                .iter()
                .flatten()
                .filter(|segment| segment.stream_index == index)
                .map(|segment| (segment.local_range.clone(), b"TEXT".to_vec()))
                .collect();
            let canonical =
                splice_bytes(&stream.decoded, &edits).unwrap_or_else(|_| stream.decoded.clone());
            canonical_content.insert(
                stream.reference,
                format!("content:{}:{}", canonical.len(), fnv1a(&canonical)),
            );
        }

        let mut length_refs: BTreeSet<Ref> = BTreeSet::new();
        for object in parsed.objects.values() {
            if object.stream.is_some()
                && let Some(dict) = as_dict(&object.value)
                && let Some(reference) = dict_get(dict, "Length").and_then(as_ref)
            {
                length_refs.insert(reference);
            }
        }

        let mut out = String::from("pdf\n");
        for object in parsed.objects.values() {
            if object_is(object, "Type", "XRef") || object_is(object, "Type", "ObjStm") {
                continue;
            }
            out.push_str(&format!(
                "{} {} ",
                object.reference.num, object.reference.generation
            ));
            if length_refs.contains(&object.reference)
                && matches!(object.value, Value::Int(_) | Value::Real(_))
            {
                out.push_str("stream-length\n");
                continue;
            }
            match &object.stream {
                Some(raw) => {
                    if let Some(dict) = as_dict(&object.value) {
                        let mut canonical_dict = dict.clone();
                        remove_dict_entries(&mut canonical_dict, &["Length"]);
                        if canonical_content.contains_key(&object.reference) {
                            remove_dict_entries(
                                &mut canonical_dict,
                                &["Filter", "F", "DecodeParms", "DP", "DL"],
                            );
                        }
                        out.push_str(&String::from_utf8_lossy(&serialize_to_vec(&Value::Dict(
                            canonical_dict,
                        ))));
                    }
                    out.push(' ');
                    match canonical_content.get(&object.reference) {
                        Some(canonical) => out.push_str(canonical),
                        None => out.push_str(&format!("stream:{}:{}", raw.len(), fnv1a(raw))),
                    }
                    out.push('\n');
                }
                None => {
                    out.push_str(&String::from_utf8_lossy(&serialize_to_vec(&object.value)));
                    out.push('\n');
                }
            }
        }
        Ok(out)
    }

    fn container_findings(&self, doc: &Document) -> Vec<ContainerFinding> {
        let mut out: Vec<ContainerFinding> = Vec::new();
        let Ok(derived) = derive_pdf(&doc.source) else {
            return out;
        };
        let parsed = &derived.parsed;
        let targets = container_targets(parsed);

        for (field, value) in &targets.info_hits {
            out.push(ContainerFinding {
                rule_id: "container.pdf-info-metadata".to_string(),
                message: format!(
                    "PDF document information <{field}> = {value:?} matches a known AI/programmatic generator fingerprint"
                ),
                scrubbable: true,
                applied: false,
            });
        }
        for (reference, reason) in &targets.xmp_hits {
            out.push(ContainerFinding {
                rule_id: "container.pdf-xmp-metadata".to_string(),
                message: format!(
                    "PDF XMP metadata (object {} {}) {reason}",
                    reference.num, reference.generation
                ),
                scrubbable: true,
                applied: false,
            });
        }
        let mut signed = false;
        for object in parsed.objects.values() {
            let Some(dict) = as_dict(&object.value) else {
                continue;
            };
            if dict_get(dict, "Subtype").and_then(as_name) == Some("Image")
                && dict_filters(dict)
                    .iter()
                    .any(|f| f == "DCTDecode" || f == "DCT")
                && let Some(raw) = object.stream.as_deref()
            {
                out.extend(crate::c2pa::inspect_media(
                    raw,
                    &format!(
                        "PDF image (object {} {})",
                        object.reference.num, object.reference.generation
                    ),
                ));
            }
            if dict_get(dict, "Type").and_then(as_name) == Some("Sig")
                || dict_get(dict, "ByteRange").is_some()
            {
                signed = true;
            }
        }
        if signed {
            out.push(ContainerFinding {
                rule_id: "container.pdf-signature".to_string(),
                message: "this PDF is digitally signed; any cleaned or scrubbed copy will no longer validate against the signature"
                    .to_string(),
                scrubbable: false,
                applied: false,
            });
        }
        if derived.unsupported_fragments > 0 {
            out.push(ContainerFinding {
                rule_id: "container.pdf-undecodable-text".to_string(),
                message: format!(
                    "{} PDF text fragment(s) use a font encoding without a Unicode map and were left untouched",
                    derived.unsupported_fragments
                ),
                scrubbable: false,
                applied: false,
            });
        }
        if derived.spans.is_empty() && derived.page_count > 0 {
            out.push(ContainerFinding {
                rule_id: "container.pdf-no-extractable-text".to_string(),
                message:
                    "no extractable prose was found in this PDF - its pages may be scanned images (cleaning text inside images would require OCR, which this tool does not do); embedded image and metadata fingerprints above are still handled"
                        .to_string(),
                scrubbable: false,
                applied: false,
            });
        }
        out
    }

    fn scrub_container(&self, _doc: &Document, rendered: &[u8]) -> Option<Vec<u8>> {
        let parsed = parse_pdf(rendered).ok()?;
        let targets = container_targets(&parsed);
        let mut replacements: BTreeMap<Ref, Vec<u8>> = BTreeMap::new();

        if !targets.info_hits.is_empty()
            && let Some(info) = parsed.info
            && let Some(dict) = parsed.object_dict(info).cloned()
        {
            let mut scrubbed = dict;
            for (field, _) in &targets.info_hits {
                set_dict_entry(&mut scrubbed, field, Value::Str(Vec::new()));
            }
            replacements.insert(info, serialize_to_vec(&Value::Dict(scrubbed)));
        }
        for (reference, _) in &targets.xmp_hits {
            let dict = parsed.object_dict(*reference)?.clone();
            let body = build_stream_body(&dict, EMPTY_XMP, false).ok()?;
            replacements.insert(*reference, body);
        }
        for reference in &targets.jpeg_refs {
            let object = parsed.get(*reference)?;
            let dict = as_dict(&object.value)?.clone();
            let raw = object.stream.as_deref()?;
            let scrubbed = crate::c2pa::scrub_media(raw)?;
            replacements.insert(*reference, build_raw_stream_body(&dict, &scrubbed));
        }

        if replacements.is_empty() {
            return None;
        }
        repack_pdf(rendered, &parsed, &replacements).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanup_core::Edit;

    fn assemble_pdf(objects: &[Vec<u8>], root: u32, info: Option<u32>) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        let info_part = info
            .map(|num| format!(" /Info {num} 0 R"))
            .unwrap_or_default();
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {root} 0 R{info_part} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn stream_body(extra: &str, data: &[u8], compress: bool) -> Vec<u8> {
        let stored = if compress {
            deflate(data).unwrap()
        } else {
            data.to_vec()
        };
        let filter = if compress {
            " /Filter /FlateDecode"
        } else {
            ""
        };
        let mut body =
            format!("<< /Length {}{filter}{extra} >>\nstream\n", stored.len()).into_bytes();
        body.extend_from_slice(&stored);
        body.extend_from_slice(b"\nendstream");
        body
    }

    fn sample_pdf(compress: bool) -> Vec<u8> {
        let content: &[u8] = b"BT /F1 12 Tf 72 720 Td (\\223Hello\\224 \\226 clean\\205) Tj ET\nBT /F1 12 Tf 72 690 Td [(Wor) 0 (ld \\227 again)] TJ ET\nq 10 0 0 10 72 600 cm /Im1 Do Q\n";
        let xmp: &[u8] = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:Description xmlns:photoshop=\"http://ns.adobe.com/photoshop/1.0/\" photoshop:Credit=\"trainedAlgorithmicMedia\"/></x:xmpmeta>";
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R /Metadata 8 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> /Contents 4 0 R >>".to_vec(),
            stream_body("", content, compress),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
            stream_body(
                " /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceRGB",
                &[255, 0, 0],
                false,
            ),
            b"<< /Title (Quarterly report) /Producer (ChatGPT) /Creator (Jane Doe) >>".to_vec(),
            stream_body(" /Type /Metadata /Subtype /XML", xmp, false),
        ];
        assemble_pdf(&objects, 1, Some(7))
    }

    fn span_texts(bytes: &[u8]) -> Vec<String> {
        Pdf.parse(bytes)
            .unwrap()
            .spans
            .iter()
            .map(|span| span.text.clone())
            .collect()
    }

    fn char_range(text: &str, ch: char) -> Range<usize> {
        let start = text.find(ch).unwrap();
        start..start + ch.len_utf8()
    }

    #[test]
    fn parses_winansi_text_from_literal_and_tj_array_strings() {
        let texts = span_texts(&sample_pdf(false));
        assert_eq!(
            texts,
            [
                "\u{201C}Hello\u{201D} \u{2013} clean\u{2026}",
                "World \u{2014} again"
            ]
        );
    }

    #[test]
    fn empty_render_is_byte_identical() {
        let bytes = sample_pdf(true);
        let doc = Pdf.parse(&bytes).unwrap();
        assert_eq!(Pdf.render(&doc, &EditSet::default()).unwrap(), bytes);
    }

    #[test]
    fn render_splices_only_text_and_preserves_layout_and_images() {
        let bytes = sample_pdf(false);
        let doc = Pdf.parse(&bytes).unwrap();
        let edit = Edit {
            span_index: 0,
            range: char_range(&doc.spans[0].text, '\u{201C}'),
            replacement: "\"".to_string(),
            rule_id: "test".to_string(),
        };
        let out = Pdf.render(&doc, &EditSet { edits: vec![edit] }).unwrap();
        assert!(span_texts(&out)[0].starts_with("\"Hello"));
        assert_eq!(Pdf.skeleton(&bytes).unwrap(), Pdf.skeleton(&out).unwrap());
        let reparsed = parse_pdf(&out).unwrap();
        let image = reparsed
            .get(Ref {
                num: 6,
                generation: 0,
            })
            .unwrap();
        assert_eq!(image.stream.as_deref(), Some(&[255u8, 0, 0][..]));
    }

    #[test]
    fn flate_content_streams_round_trip() {
        let bytes = sample_pdf(true);
        let doc = Pdf.parse(&bytes).unwrap();
        let edit = Edit {
            span_index: 0,
            range: char_range(&doc.spans[0].text, '\u{2026}'),
            replacement: "...".to_string(),
            rule_id: "test".to_string(),
        };
        let out = Pdf.render(&doc, &EditSet { edits: vec![edit] }).unwrap();
        assert!(span_texts(&out)[0].ends_with("clean..."));
        assert_eq!(Pdf.skeleton(&bytes).unwrap(), Pdf.skeleton(&out).unwrap());
    }

    #[test]
    fn info_and_xmp_provenance_are_reported_and_scrubbed() {
        let bytes = sample_pdf(false);
        let doc = Pdf.parse(&bytes).unwrap();
        let findings = Pdf.container_findings(&doc);
        assert!(
            findings
                .iter()
                .any(|f| f.rule_id == "container.pdf-info-metadata" && f.scrubbable),
            "{findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.rule_id == "container.pdf-xmp-metadata" && f.scrubbable),
            "{findings:?}"
        );
        let scrubbed = Pdf.scrub_container(&doc, &bytes).expect("scrub");
        let parsed = parse_pdf(&scrubbed).unwrap();
        let info = parsed
            .object_dict(Ref {
                num: 7,
                generation: 0,
            })
            .unwrap();
        assert_eq!(dict_get(info, "Producer"), Some(&Value::Str(Vec::new())));
        assert_eq!(
            dict_get(info, "Creator"),
            Some(&Value::Str(b"Jane Doe".to_vec()))
        );
        let xmp = parsed
            .decoded_stream(Ref {
                num: 8,
                generation: 0,
            })
            .unwrap()
            .unwrap();
        assert!(!String::from_utf8_lossy(&xmp).contains("trainedAlgorithmicMedia"));
        // Prose and the image survive a metadata scrub untouched.
        assert_eq!(span_texts(&scrubbed), span_texts(&bytes));
        assert!(Pdf.scrub_container(&doc, &scrubbed).is_none());
    }

    #[test]
    fn type0_identity_h_text_uses_tounicode_map() {
        let cmap: &[u8] = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n4 beginbfchar\n<0001> <201C>\n<0002> <0048>\n<0003> <0069>\n<0004> <0022>\nendbfchar\nendcmap\nend\nend\n";
        let content: &[u8] = b"BT /F1 12 Tf 72 720 Td <000100020003> Tj ET\n";
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
            stream_body("", content, false),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /Subset /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>".to_vec(),
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Subset /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>".to_vec(),
            stream_body("", cmap, false),
        ];
        let bytes = assemble_pdf(&objects, 1, None);
        assert_eq!(span_texts(&bytes), ["\u{201C}Hi"]);
        let doc = Pdf.parse(&bytes).unwrap();
        let edit = Edit {
            span_index: 0,
            range: char_range(&doc.spans[0].text, '\u{201C}'),
            replacement: "\"".to_string(),
            rule_id: "test".to_string(),
        };
        let out = Pdf.render(&doc, &EditSet { edits: vec![edit] }).unwrap();
        assert_eq!(span_texts(&out), ["\"Hi"]);
        assert_eq!(Pdf.skeleton(&bytes).unwrap(), Pdf.skeleton(&out).unwrap());
    }

    #[test]
    fn rejects_non_pdf_and_encrypted_files() {
        assert!(Pdf.parse(b"not a pdf").is_err());
        let bytes = sample_pdf(false);
        // Point the trailer at an encryption dictionary.
        let text = String::from_utf8_lossy(&bytes).replace(
            "/Root 1 0 R /Info 7 0 R",
            "/Root 1 0 R /Info 7 0 R /Encrypt 7 0 R",
        );
        assert!(Pdf.parse(text.as_bytes()).is_err());
    }
}
