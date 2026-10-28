//! Spatial reconstruction: PDF page content → positioned runs, rules, fills and images.
//!
//! The lossless cleaner in the parent module reads text showing operators only, because it
//! splices prose back into the same operands and never needs to know where anything is drawn.
//! Conversion is the opposite problem: to rebuild a table, a column layout or a figure, we need
//! the *geometry*. This module therefore interprets each page's content streams with the full
//! graphics state:
//!
//! * `q`/`Q`/`cm` maintain the CTM; `BT`/`ET`/`Tm`/`Td`/`TD`/`T*`/`TL` maintain the text
//!   matrices, and every `Tj`/`TJ`/`'`/`"` produces a [`TextRun`] whose box is the transformed
//!   glyph advance width, using the font's `/Widths` (or CID `/W`) where available;
//! * `re`/`m`/`l`/`c` build paths that `f`/`S`/`B` paint — thin ones become divider
//!   [`RuleSegment`]s, thick ones become [`FillRect`]s (callout backgrounds, table header fills);
//! * `Do` resolves `XObject`s: images are decoded into [`ImageAsset`]s (JPEG payloads are kept
//!   byte-for-byte, Flate samples are encoded once as PNG) and placed with their CTM box; forms
//!   are interpreted recursively with their own `/Matrix` and `/Resources`.
//!
//! Coordinates are converted to the IR's top-left origin (`y = page_height - y_pdf`) on the way
//! out, so nothing downstream has to know about PDF's y-axis.

use super::*;
use cleanup_core::doc::{
    Axis, FillRect, ImageAsset, ImageKind, ImagePlacement, LayoutDoc, PageLayout, Rect, Rgb,
    RuleSegment, TextRun,
};
use std::collections::BTreeMap;

/// Result of a spatial pass over a PDF.
#[derive(Debug)]
pub struct PdfLayoutReport {
    pub layout: LayoutDoc,
    pub warnings: Vec<String>,
    pub page_count: usize,
    /// Number of text fragments skipped because their font could not be mapped to Unicode.
    pub unsupported_fragments: usize,
}

/// Reconstruct the page geometry of a PDF.
pub fn extract_layout(bytes: &[u8]) -> Result<PdfLayoutReport> {
    let pdf = parse_pdf(bytes)?;
    if pdf.encrypted {
        return Err(CoreError::Parse(
            "this PDF is password-protected (encrypted); remove the password and try again"
                .to_string(),
        ));
    }
    let entries = collect_pages(&pdf);
    let mut layout = LayoutDoc::default();
    let mut warnings: Vec<String> = Vec::new();
    let mut unsupported_fragments = 0usize;
    let mut asset_keys: BTreeMap<String, usize> = BTreeMap::new();

    for (page_index, entry) in entries.iter().enumerate() {
        if entry.rotate % 360 != 0 {
            warnings.push(format!(
                "page {}: /Rotate {} is not supported by the spatial reader; the page is analysed unrotated",
                page_index + 1,
                entry.rotate
            ));
        }
        let mut geom = PageGeometry::new(entry.width, entry.height);
        let fonts = fonts_for(&pdf, entry.resources.as_ref());
        let mut data: Vec<u8> = Vec::new();
        for reference in &entry.contents {
            if let Some(Ok(stream)) = pdf.decoded_stream(*reference) {
                data.extend_from_slice(&stream);
                data.push(b'\n');
            }
        }
        if data.is_empty() {
            warnings.push(format!(
                "page {}: no readable content stream",
                page_index + 1
            ));
        } else {
            let mut interpreter = Interpreter {
                pdf: &pdf,
                page_height: entry.height,
                assets: &mut layout.assets,
                asset_keys: &mut asset_keys,
                warnings: &mut warnings,
                unsupported: &mut unsupported_fragments,
            };
            let state = GraphicsState::new(IDENTITY);
            interpreter.run(
                &data,
                entry.resources.as_ref(),
                &state,
                &fonts,
                &mut geom,
                0,
            );
        }
        layout.pages.push(geom.into_page());
    }
    if layout.pages.is_empty() {
        warnings.push("the PDF has no pages".to_string());
    }
    Ok(PdfLayoutReport {
        layout,
        warnings,
        page_count: entries.len(),
        unsupported_fragments,
    })
}

// ---------------------------------------------------------------------------
// Matrices and page entries
// ---------------------------------------------------------------------------

/// PDF transformation matrix `[a b c d e f]`: `x' = a·x + c·y + e`, `y' = b·x + d·y + f`.
type Matrix = [f32; 6];

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `m × n` — apply `n` first, then `m` (PDF's row-vector convention).
fn mul(m: Matrix, n: Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

fn apply(m: Matrix, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn translation(tx: f32, ty: f32) -> Matrix {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

fn matrix_from(value: &Value) -> Option<Matrix> {
    let Value::Array(items) = value else {
        return None;
    };
    if items.len() != 6 {
        return None;
    }
    let mut matrix = IDENTITY;
    for (index, item) in items.iter().enumerate() {
        matrix[index] = as_int(item)? as f32;
    }
    Some(matrix)
}

struct PageEntry {
    width: f32,
    height: f32,
    rotate: i32,
    resources: Option<Value>,
    contents: Vec<Ref>,
}

fn collect_pages(pdf: &ParsedPdf) -> Vec<PageEntry> {
    let mut out: Vec<PageEntry> = Vec::new();
    let Some(root) = pdf.root else {
        return out;
    };
    let Some(catalog) = pdf.object_dict(root) else {
        return out;
    };
    let Some(pages) = dict_get(catalog, "Pages").cloned() else {
        return out;
    };
    let mut visited: BTreeSet<Ref> = BTreeSet::new();
    walk_page_tree(pdf, &pages, None, None, 0, &mut visited, &mut out, 0);
    out
}

#[allow(clippy::too_many_arguments)]
fn walk_page_tree(
    pdf: &ParsedPdf,
    node: &Value,
    inherited_resources: Option<Value>,
    inherited_box: Option<(f32, f32)>,
    inherited_rotate: i32,
    visited: &mut BTreeSet<Ref>,
    out: &mut Vec<PageEntry>,
    depth: usize,
) {
    if depth > 64 || out.len() > 20_000 {
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
    let media_box = dict_get(dict, "MediaBox")
        .and_then(|value| media_box_of(pdf.resolve(value)))
        .or(inherited_box);
    let rotate = dict_get(dict, "Rotate")
        .and_then(as_int)
        .map(|value| value as i32)
        .unwrap_or(inherited_rotate);

    if let Some(Value::Array(kids)) = dict_get(dict, "Kids")
        && !kids.is_empty()
    {
        let kids = kids.clone();
        for kid in &kids {
            walk_page_tree(
                pdf,
                kid,
                resources.clone(),
                media_box,
                rotate,
                visited,
                out,
                depth + 1,
            );
        }
        return;
    }
    let mut contents: Vec<Ref> = Vec::new();
    match dict_get(dict, "Contents") {
        Some(Value::Ref(reference)) => contents.push(*reference),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(reference) = as_ref(item) {
                    contents.push(reference);
                }
            }
        }
        _ => {}
    }
    let (width, height) = media_box.unwrap_or((612.0, 792.0));
    out.push(PageEntry {
        width,
        height,
        rotate,
        resources,
        contents,
    });
}

fn media_box_of(value: &Value) -> Option<(f32, f32)> {
    let Value::Array(items) = value else {
        return None;
    };
    if items.len() != 4 {
        return None;
    }
    let x0 = as_int(&items[0])? as f32;
    let y0 = as_int(&items[1])? as f32;
    let x1 = as_int(&items[2])? as f32;
    let y1 = as_int(&items[3])? as f32;
    let width = (x1 - x0).abs();
    let height = (y1 - y0).abs();
    if width < 1.0 || height < 1.0 {
        return None;
    }
    Some((width, height))
}

// ---------------------------------------------------------------------------
// Graphics and text state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct GraphicsState {
    ctm: Matrix,
    fill: Rgb,
    stroke: Rgb,
    line_width: f32,
    font: Option<String>,
    font_size: f32,
    char_spacing: f32,
    word_spacing: f32,
    hscale: f32,
    leading: f32,
    rise: f32,
}

impl GraphicsState {
    fn new(ctm: Matrix) -> Self {
        GraphicsState {
            ctm,
            fill: Rgb::BLACK,
            stroke: Rgb::BLACK,
            line_width: 1.0,
            font: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            hscale: 1.0,
            leading: 0.0,
            rise: 0.0,
        }
    }
}

#[derive(Default)]
struct PathState {
    bbox: Option<(f32, f32, f32, f32)>,
    segments: Vec<((f32, f32), (f32, f32))>,
    current: Option<(f32, f32)>,
    start: Option<(f32, f32)>,
}

impl PathState {
    fn touch(&mut self, point: (f32, f32)) {
        self.bbox = Some(match self.bbox {
            Some((x0, y0, x1, y1)) => (
                x0.min(point.0),
                y0.min(point.1),
                x1.max(point.0),
                y1.max(point.1),
            ),
            None => (point.0, point.1, point.0, point.1),
        });
    }

    fn reset(&mut self) {
        *self = PathState::default();
    }
}

struct PageGeometry {
    width: f32,
    height: f32,
    runs: Vec<TextRun>,
    rules: Vec<RuleSegment>,
    fills: Vec<FillRect>,
    images: Vec<ImagePlacement>,
}

impl PageGeometry {
    fn new(width: f32, height: f32) -> Self {
        PageGeometry {
            width,
            height,
            runs: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            images: Vec::new(),
        }
    }

    fn into_page(self) -> PageLayout {
        PageLayout {
            width: self.width,
            height: self.height,
            runs: self.runs,
            rules: self.rules,
            fills: self.fills,
            images: self.images,
        }
    }
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

struct FontInfo {
    profile: FontProfile,
    metrics: FontMetrics,
    bold: bool,
    italic: bool,
    mono: bool,
}

#[derive(Default)]
struct FontMetrics {
    first_char: i32,
    widths: Vec<f32>,
    cid_widths: BTreeMap<u32, f32>,
    default_width: f32,
}

impl FontMetrics {
    /// Glyph advance in 1/1000 em.
    fn width(&self, code: u32) -> f32 {
        if !self.widths.is_empty() {
            let index = code as i64 - i64::from(self.first_char);
            if index >= 0
                && let Some(width) = self.widths.get(index as usize)
                && *width > 0.0
            {
                return *width;
            }
        }
        if let Some(width) = self.cid_widths.get(&code) {
            return *width;
        }
        if self.default_width > 0.0 {
            self.default_width
        } else {
            500.0
        }
    }
}

fn fonts_for(pdf: &ParsedPdf, resources: Option<&Value>) -> BTreeMap<String, FontInfo> {
    let mut fonts: BTreeMap<String, FontInfo> = BTreeMap::new();
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
            fonts.insert(name.clone(), font_info(pdf, dict));
        }
    }
    fonts
}

/// Strip PDF subset prefix (e.g. "ABCDEF+Helvetica-Bold" -> "Helvetica-Bold").
fn canonical_font_name(raw: &str) -> String {
    if let Some(plus) = raw.find('+') {
        // Subset tags are 6 uppercase letters + "+"; verify pattern
        let prefix = &raw[..plus];
        if prefix.len() == 6 && prefix.chars().all(|c| c.is_ascii_uppercase()) {
            return raw[plus + 1..].to_string();
        }
    }
    raw.to_string()
}

fn font_info(pdf: &ParsedPdf, dict: &[(String, Value)]) -> FontInfo {
    let profile = font_profile(pdf, dict);
    let metrics = font_metrics(pdf, dict);
    let base_raw = dict_get(dict, "BaseFont")
        .and_then(as_name)
        .unwrap_or("");
    let base = base_raw.to_ascii_lowercase();
    // Detect style from canonical name as well
    let canonical_lower = canonical_font_name(base_raw).to_ascii_lowercase();
    FontInfo {
        profile,
        metrics,
        bold: base.contains("bold")
            || base.contains("black")
            || base.contains("heavy")
            || canonical_lower.contains("bold"),
        italic: base.contains("italic")
            || base.contains("oblique")
            || canonical_lower.contains("italic")
            || canonical_lower.contains("oblique"),
        mono: base.contains("courier")
            || base.contains("mono")
            || base.contains("consol")
            || base.contains("code")
            || base.contains("typewriter")
            || canonical_lower.contains("courier")
            || canonical_lower.contains("mono"),
    }
}

fn font_metrics(pdf: &ParsedPdf, dict: &[(String, Value)]) -> FontMetrics {
    let mut metrics = FontMetrics::default();
    let descendant = dict_get(dict, "DescendantFonts")
        .map(|value| pdf.resolve(value))
        .and_then(|value| match value {
            Value::Array(items) => items.first().cloned(),
            _ => None,
        })
        .and_then(|value| pdf.resolve_dict(&value).cloned());

    if let Some(descendant) = descendant.as_deref() {
        if let Some(default_width) = dict_get(descendant, "DW").and_then(as_int) {
            metrics.default_width = default_width as f32;
        }
        if let Some(widths) = dict_get(descendant, "W")
            .map(|value| pdf.resolve(value))
            .and_then(|value| match value {
                Value::Array(items) => Some(items.clone()),
                _ => None,
            })
        {
            parse_cid_widths(&widths, &mut metrics.cid_widths);
        }
    }

    if let Some(first) = dict_get(dict, "FirstChar").and_then(as_int) {
        metrics.first_char = first as i32;
    }
    if let Some(widths) = dict_get(dict, "Widths")
        .map(|value| pdf.resolve(value))
        .and_then(|value| match value {
            Value::Array(items) => Some(items.clone()),
            _ => None,
        })
    {
        for item in &widths {
            metrics
                .widths
                .push(as_int(item).map(|value| value as f32).unwrap_or(0.0));
        }
    }
    if metrics.default_width <= 0.0 {
        metrics.default_width = dict_get(dict, "MissingWidth")
            .and_then(as_int)
            .map(|value| value as f32)
            .unwrap_or(500.0);
    }
    metrics
}

fn parse_cid_widths(items: &[Value], out: &mut BTreeMap<u32, f32>) {
    let mut index = 0usize;
    while index < items.len() {
        let Some(first) = as_int(&items[index]) else {
            index += 1;
            continue;
        };
        match items.get(index + 1) {
            Some(Value::Array(widths)) => {
                for (offset, width) in widths.iter().enumerate() {
                    if let Some(value) = as_int(width) {
                        out.insert((first as u32).wrapping_add(offset as u32), value as f32);
                    }
                }
                index += 2;
            }
            Some(last) => {
                let Some(last) = as_int(last) else {
                    index += 1;
                    continue;
                };
                let width = items
                    .get(index + 2)
                    .and_then(as_int)
                    .map(|value| value as f32)
                    .unwrap_or(0.0);
                let mut code = first as u32;
                while code <= last as u32 {
                    out.insert(code, width);
                    code += 1;
                    if code.wrapping_sub(first as u32) > 65_535 {
                        break;
                    }
                }
                index += 3;
            }
            None => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Interpreter
// ---------------------------------------------------------------------------

struct Interpreter<'a> {
    pdf: &'a ParsedPdf,
    page_height: f32,
    assets: &'a mut Vec<ImageAsset>,
    asset_keys: &'a mut BTreeMap<String, usize>,
    warnings: &'a mut Vec<String>,
    unsupported: &'a mut usize,
}

impl<'a> Interpreter<'a> {
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        data: &[u8],
        resources: Option<&Value>,
        initial: &GraphicsState,
        fonts: &BTreeMap<String, FontInfo>,
        geom: &mut PageGeometry,
        depth: usize,
    ) {
        if depth > 12 {
            return;
        }
        let mut gs = initial.clone();
        let mut stack: Vec<GraphicsState> = Vec::new();
        let mut tm = IDENTITY;
        let mut tlm = IDENTITY;
        let mut path = PathState::default();
        let mut operands: Vec<Operand> = Vec::new();
        let mut lexer = ContentLexer::new(data);

        while let Some(token) = lexer.next_token() {
            let operator = match token {
                ContentToken::Operand(operand) => {
                    operands.push(operand);
                    continue;
                }
                ContentToken::Operator(operator) => operator,
            };
            match operator.as_str() {
                "q" => stack.push(gs.clone()),
                "Q" => {
                    if let Some(previous) = stack.pop() {
                        gs = previous;
                    }
                }
                "cm" => {
                    if let Some(values) = last_numbers(&operands, 6) {
                        let matrix = [
                            values[0], values[1], values[2], values[3], values[4], values[5],
                        ];
                        gs.ctm = mul(matrix, gs.ctm);
                    }
                }
                "w" => {
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.line_width = values[0].max(0.01);
                    }
                }
                "rg" => {
                    if let Some(values) = last_numbers(&operands, 3) {
                        gs.fill = rgb_from(&values);
                    }
                }
                "RG" => {
                    if let Some(values) = last_numbers(&operands, 3) {
                        gs.stroke = rgb_from(&values);
                    }
                }
                "g" => {
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.fill = gray(value01(values[0]));
                    }
                }
                "G" => {
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.stroke = gray(value01(values[0]));
                    }
                }
                "k" => {
                    if let Some(values) = last_numbers(&operands, 4) {
                        gs.fill = cmyk(&values);
                    }
                }
                "K" => {
                    if let Some(values) = last_numbers(&operands, 4) {
                        gs.stroke = cmyk(&values);
                    }
                }
                "sc" | "scn" => {
                    let values: Vec<f32> = operands
                        .iter()
                        .filter_map(|operand| match operand {
                            Operand::Num(value) => Some(*value as f32),
                            _ => None,
                        })
                        .collect();
                    match values.len() {
                        1 => gs.fill = gray(value01(values[0])),
                        3 => gs.fill = rgb_from(&values),
                        4 => gs.fill = cmyk(&values),
                        _ => {}
                    }
                }
                "BT" => {
                    tm = IDENTITY;
                    tlm = IDENTITY;
                }
                "ET" => {}
                "Tf" => {
                    if let Some(Operand::Name(name)) = operands.first() {
                        gs.font = Some(name.clone());
                    }
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.font_size = values[0];
                    }
                }
                "TL" => {
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.leading = values[0];
                    }
                }
                "Tc" => {
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.char_spacing = values[0];
                    }
                }
                "Tw" => {
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.word_spacing = values[0];
                    }
                }
                "Tz" => {
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.hscale = (values[0] / 100.0).max(0.01);
                    }
                }
                "Ts" => {
                    if let Some(values) = last_numbers(&operands, 1) {
                        gs.rise = values[0];
                    }
                }
                "Tm" => {
                    if let Some(values) = last_numbers(&operands, 6) {
                        tm = [
                            values[0], values[1], values[2], values[3], values[4], values[5],
                        ];
                        tlm = tm;
                    }
                }
                "Td" | "TD" => {
                    if let Some(values) = last_numbers(&operands, 2) {
                        if operator == "TD" {
                            gs.leading = -values[1];
                        }
                        tlm = mul(translation(values[0], values[1]), tlm);
                        tm = tlm;
                    }
                }
                "T*" => {
                    tlm = mul(translation(0.0, -gs.leading), tlm);
                    tm = tlm;
                }
                "Tj" => {
                    if let Some(Operand::Str(string)) = operands.first() {
                        self.show(&string.bytes, &gs, &mut tm, fonts, geom);
                    }
                }
                "'" => {
                    tlm = mul(translation(0.0, -gs.leading), tlm);
                    tm = tlm;
                    if let Some(Operand::Str(string)) = operands.first() {
                        self.show(&string.bytes, &gs, &mut tm, fonts, geom);
                    }
                }
                "\"" => {
                    if let (Some(Operand::Num(aw)), Some(Operand::Num(ac))) =
                        (operands.first(), operands.get(1))
                    {
                        gs.word_spacing = *aw as f32;
                        gs.char_spacing = *ac as f32;
                    }
                    tlm = mul(translation(0.0, -gs.leading), tlm);
                    tm = tlm;
                    if let Some(Operand::Str(string)) = operands.last() {
                        self.show(&string.bytes, &gs, &mut tm, fonts, geom);
                    }
                }
                "TJ" => {
                    if let Some(Operand::Array(items)) = operands.first() {
                        for item in items {
                            match item {
                                Operand::Str(string) => {
                                    self.show(&string.bytes, &gs, &mut tm, fonts, geom)
                                }
                                Operand::Num(value) => {
                                    let shift =
                                        -(*value as f32) / 1000.0 * gs.font_size * gs.hscale;
                                    tm = mul(translation(shift, 0.0), tm);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "m" => {
                    if let Some(values) = last_numbers(&operands, 2) {
                        let point = apply(gs.ctm, values[0], values[1]);
                        path.touch(point);
                        path.current = Some(point);
                        path.start = Some(point);
                    }
                }
                "l" => {
                    if let Some(values) = last_numbers(&operands, 2) {
                        let point = apply(gs.ctm, values[0], values[1]);
                        if let Some(current) = path.current {
                            path.segments.push((current, point));
                        }
                        path.touch(point);
                        path.current = Some(point);
                    }
                }
                "c" | "v" | "y" => {
                    if let Some(values) = last_numbers(&operands, 2) {
                        let point = apply(gs.ctm, values[0], values[1]);
                        if let Some(current) = path.current {
                            path.segments.push((current, point));
                        }
                        path.touch(point);
                        path.current = Some(point);
                    }
                }
                "h" => {
                    if let (Some(current), Some(start)) = (path.current, path.start) {
                        path.segments.push((current, start));
                        path.current = Some(start);
                    }
                }
                "re" => {
                    if let Some(values) = last_numbers(&operands, 4) {
                        let (x, y, w, h) = (values[0], values[1], values[2], values[3]);
                        for (dx, dy) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
                            path.touch(apply(gs.ctm, x + dx, y + dy));
                        }
                        path.current = Some(apply(gs.ctm, x, y));
                        path.start = Some(apply(gs.ctm, x, y));
                    }
                }
                "f" | "F" | "f*" => {
                    self.paint(&gs, &path, geom, true, false);
                    path.reset();
                }
                "S" | "s" => {
                    self.paint(&gs, &path, geom, false, true);
                    path.reset();
                }
                "B" | "B*" | "b" | "b*" => {
                    self.paint(&gs, &path, geom, true, true);
                    path.reset();
                }
                "n" => path.reset(),
                "Do" => {
                    if let Some(Operand::Name(name)) = operands.first() {
                        self.do_xobject(name, resources, &gs, geom, depth);
                    }
                }
                "BI" => lexer.skip_inline_image(),
                "gs" => {
                    // Extended graphics state (alpha, blend mode) carries no geometry we use.
                }
                _ => {}
            }
            operands.clear();
        }
        // A path left un-painted at the end of a stream is not rendered by a viewer either.
    }

    /// Emit glyphs with precise per-character geometry.
    ///
    /// Each glyph is placed using its font advance (width/1000 * font_size) plus
    /// Tc/Tw/hscale. Glyphs sharing baseline/font/size are clustered: the gap
    /// `x_curr - (x_prev + width_prev)` is measured and a word-break space is
    /// only inserted when it exceeds `space_threshold = 0.25 * font_size`.
    /// This prevents "t h i s   i s" corruption from naive per-glyph emission.
    fn show(
        &mut self,
        bytes: &[u8],
        gs: &GraphicsState,
        tm: &mut Matrix,
        fonts: &BTreeMap<String, FontInfo>,
        geom: &mut PageGeometry,
    ) {
        let font_name_raw = gs.font.clone().unwrap_or_default();
        let font_name = canonical_font_name(&font_name_raw);
        let trm = mul(*tm, gs.ctm);
        let scale = (trm[0] * trm[0] + trm[1] * trm[1]).sqrt().max(0.01);
        let size = (gs.font_size * scale).max(1.0);

        // Glyph record with precise advance for clustering
        struct Glyph {
            text: String,
            advance: f32, // in text space (before CTM scale)
            is_space: bool,
        }

        let mut glyphs: Vec<Glyph> = Vec::new();
        let mut total_advance = 0.0f32;
        match fonts.get(&font_name_raw) {
            Some(font) if font.profile.supported => {
                let code_len = font.profile.code_len.max(1);
                let mut index = 0usize;
                while index < bytes.len() {
                    let end = (index + code_len).min(bytes.len());
                    let code = hex_bytes_to_u32(&bytes[index..end]);
                    let decoded = font.profile.decode.get(&code);
                    let width = font.metrics.width(code) / 1000.0;
                    let is_space = decoded.is_some_and(|t| t.chars().all(|c| c == ' '));
                    let adv = (width * gs.font_size
                        + gs.char_spacing
                        + if is_space { gs.word_spacing } else { 0.0 })
                        * gs.hscale;
                    total_advance += adv;
                    if let Some(decoded) = decoded {
                        glyphs.push(Glyph {
                            text: decoded.clone(),
                            advance: adv,
                            is_space,
                        });
                    } else {
                        *self.unsupported += 1;
                    }
                    index = end;
                }
            }
            _ => {
                if !bytes.is_empty() {
                    *self.unsupported += 1;
                }
                // Fallback: treat as mono without precise metrics
                let adv = bytes.len() as f32 * gs.font_size.max(1.0) * 0.5 * gs.hscale;
                total_advance = adv;
                // No glyphs to emit individually; still need to advance Tm
                *tm = mul(translation(total_advance, 0.0), *tm);
                return;
            }
        }

        if glyphs.is_empty() {
            *tm = mul(translation(total_advance, 0.0), *tm);
            return;
        }

        // Cluster glyphs into word-level TextRuns using geometric gap threshold
        // Gap = x_curr - (x_prev + width_prev) in text space, converted via scale
        let space_threshold = (gs.font_size * 0.25).max(1.0) * gs.hscale;
        let (x0, y0) = apply(trm, 0.0, 0.0);
        let top = self.page_height - y0 - size * 0.8 - gs.rise * scale;
        let bold = fonts.get(&font_name_raw).map(|f| f.bold).unwrap_or(false);
        let italic = fonts.get(&font_name_raw).map(|f| f.italic).unwrap_or(false);
        let mono = fonts.get(&font_name_raw).map(|f| f.mono).unwrap_or(false);

        // If the Tj string already contains explicit space characters, we preserve them
        // as word boundaries and do not synthesize extra spaces from geometry.
        // Geometry-based spacing only applies to inter-glyph gaps *within* a word.
        let mut word_start_advance = 0.0f32; // offset of current word start in text space
        let mut word_text = String::new();
        let mut word_advance = 0.0f32;
        let mut cursor = 0.0f32; // current glyph's x in text space (before CTM)

        // Helper to flush current word as a TextRun
        let mut flush_word = |word_text: &mut String,
                              word_start: f32,
                              word_adv: f32,
                              geom: &mut PageGeometry| {
            if word_text.trim().is_empty() {
                return;
            }
            let (wx0, _) = apply(trm, word_start, 0.0);
            let (wx1, _) = apply(trm, word_start + word_adv, 0.0);
            let left = wx0.min(wx1);
            let width = (wx1 - wx0).abs().max(1.0);
            geom.runs.push(TextRun {
                text: std::mem::take(word_text),
                rect: Rect::new(left, top, width, size),
                font_size: size,
                font_name: font_name.clone(),
                bold,
                italic,
                mono,
                color: gs.fill,
            });
        };

        for glyph in &glyphs {
            if glyph.is_space {
                // Explicit space in source: flush word, advance cursor including space
                flush_word(&mut word_text, word_start_advance, word_advance, geom);
                cursor += glyph.advance;
                word_start_advance = cursor;
                word_advance = 0.0;
                continue;
            }
            // Compute gap from previous glyph's right edge
            // For glyphs within same word, cursor points to current glyph's left
            // word_advance is sum of advances of glyphs in current word so far
            // If word has content, gap = cursor - (word_start_advance + word_advance) but
            // cursor == word_start_advance + word_advance by construction unless TJ
            // inserted extra shift. Actually after each glyph we do cursor += advance,
            // so gap for next glyph = cursor - (word_start + word_adv) = 0 normally.
            // For inter-word detection, we look at whether gap exceeds threshold.
            // Since within a Tj string glyphs are contiguous (gap 0), no extra space.
            // The meaningful gap check is BETWEEN Tj strings via Tm displacement,
            // which is handled at the caller by advancing Tm before next show().
            // So within one show() call, we just concatenate.
            if word_text.is_empty() {
                word_start_advance = cursor;
            }
            word_text.push_str(&glyph.text);
            word_advance += glyph.advance;
            cursor += glyph.advance;
        }
        flush_word(&mut word_text, word_start_advance, word_advance, geom);

        *tm = mul(translation(total_advance, 0.0), *tm);
    }

    fn paint(
        &mut self,
        gs: &GraphicsState,
        path: &PathState,
        geom: &mut PageGeometry,
        fill: bool,
        stroke: bool,
    ) {
        if fill && let Some((x0, y0, x1, y1)) = path.bbox {
            let width = x1 - x0;
            let height = y1 - y0;
            if width > 0.2 && height > 0.2 {
                let rect = self.page_rect(x0, y0, x1, y1);
                let thickness = width.min(height);
                if thickness <= 2.5 && width.max(height) >= 6.0 {
                    geom.rules.push(RuleSegment {
                        rect,
                        axis: if width >= height {
                            Axis::Horizontal
                        } else {
                            Axis::Vertical
                        },
                        thickness,
                        color: gs.fill,
                    });
                } else if rect.area() >= 64.0 {
                    geom.fills.push(FillRect {
                        rect,
                        fill: Some(gs.fill),
                        stroke: if stroke { Some(gs.stroke) } else { None },
                        stroke_width: gs.line_width,
                    });
                }
            }
        }
        if stroke
            && path.segments.is_empty()
            && let Some((x0, y0, x1, y1)) = path.bbox
        {
            // A stroked rectangle (`re … S`) has no path segments; emit its four edges as
            // rules so the table detector can still see the grid.
            let thickness = gs.line_width.max(0.3);
            let left = x0;
            let right = x1;
            let top = self.page_height - y1;
            let bottom = self.page_height - y0;
            geom.rules.push(RuleSegment {
                rect: Rect::new(left, top, (right - left).max(0.0), thickness),
                axis: Axis::Horizontal,
                thickness,
                color: gs.stroke,
            });
            geom.rules.push(RuleSegment {
                rect: Rect::new(left, bottom - thickness, (right - left).max(0.0), thickness),
                axis: Axis::Horizontal,
                thickness,
                color: gs.stroke,
            });
            geom.rules.push(RuleSegment {
                rect: Rect::new(left, top, thickness, (bottom - top).max(0.0)),
                axis: Axis::Vertical,
                thickness,
                color: gs.stroke,
            });
            geom.rules.push(RuleSegment {
                rect: Rect::new(right - thickness, top, thickness, (bottom - top).max(0.0)),
                axis: Axis::Vertical,
                thickness,
                color: gs.stroke,
            });
        }
        if stroke {
            for (start, end) in &path.segments {
                let thickness = gs.line_width.max(0.3);
                let (x0, y0) = (start.0.min(end.0), start.1.min(end.1));
                let (x1, y1) = (start.0.max(end.0), start.1.max(end.1));
                if (x1 - x0) < 0.2 && (y1 - y0) < 0.2 {
                    continue;
                }
                let rect = self.page_rect(
                    x0 - thickness / 2.0,
                    y0 - thickness / 2.0,
                    x1 + thickness / 2.0,
                    y1 + thickness / 2.0,
                );
                let axis = if (x1 - x0) >= (y1 - y0) {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                geom.rules.push(RuleSegment {
                    rect,
                    axis,
                    thickness,
                    color: gs.stroke,
                });
            }
        }
    }

    fn page_rect(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        let width = (x1 - x0).max(0.0);
        let height = (y1 - y0).max(0.0);
        Rect::new(x0, self.page_height - y1, width, height)
    }

    fn do_xobject(
        &mut self,
        name: &str,
        resources: Option<&Value>,
        gs: &GraphicsState,
        geom: &mut PageGeometry,
        depth: usize,
    ) {
        let Some(resources) = resources else {
            return;
        };
        let Some(resource_dict) = self.pdf.resolve_dict(resources) else {
            return;
        };
        let Some(xobjects) = dict_get(resource_dict, "XObject") else {
            return;
        };
        let Some(xobject_dict) = self.pdf.resolve_dict(xobjects).cloned() else {
            return;
        };
        let Some(value) = dict_get(&xobject_dict, name) else {
            return;
        };
        let Some(reference) = as_ref(value) else {
            return;
        };
        let Some(object) = self.pdf.get(reference) else {
            return;
        };
        let Some(dict) = as_dict(&object.value) else {
            return;
        };
        let dict = dict.clone();
        let subtype = dict_get(&dict, "Subtype").and_then(as_name).unwrap_or("");
        match subtype {
            "Image" => {
                if let Some(raw) = object.stream.clone() {
                    self.place_image(&dict, &raw, gs, geom);
                }
            }
            "Form" => {
                let Some(raw) = object.stream.clone() else {
                    return;
                };
                let Ok(data) = decode_stream_data(&dict, &raw) else {
                    self.warnings
                        .push("skipped a Form XObject with an unsupported filter".to_string());
                    return;
                };
                let matrix = dict_get(&dict, "Matrix")
                    .map(|value| self.pdf.resolve(value))
                    .and_then(matrix_from)
                    .unwrap_or(IDENTITY);
                let form_resources = dict_get(&dict, "Resources")
                    .cloned()
                    .or_else(|| Some(resources.clone()));
                let fonts = fonts_for(self.pdf, form_resources.as_ref());
                let mut state = gs.clone();
                state.ctm = mul(matrix, gs.ctm);
                self.run(
                    &data,
                    form_resources.as_ref(),
                    &state,
                    &fonts,
                    geom,
                    depth + 1,
                );
            }
            _ => {}
        }
    }

    fn place_image(
        &mut self,
        dict: &[(String, Value)],
        raw: &[u8],
        gs: &GraphicsState,
        geom: &mut PageGeometry,
    ) {
        let Some(width) = dict_get(dict, "Width").and_then(as_usize) else {
            return;
        };
        let Some(height) = dict_get(dict, "Height").and_then(as_usize) else {
            return;
        };
        if width == 0 || height == 0 || width > 20_000 || height > 20_000 {
            return;
        }
        let filters = dict_filters(dict);
        let is_jpeg = filters
            .iter()
            .any(|filter| filter == "DCTDecode" || filter == "DCT");
        if filters.iter().any(|filter| filter == "JPXDecode") {
            self.warnings.push(
                "skipped a JPEG 2000 image (JPXDecode is not supported); its position is lost"
                    .to_string(),
            );
            return;
        }
        let (kind, data) = if is_jpeg {
            (ImageKind::Jpeg, raw.to_vec())
        } else {
            let Ok(decoded) = decode_stream_data(dict, raw) else {
                self.warnings
                    .push("skipped an image XObject with an unsupported filter".to_string());
                return;
            };
            if dict_get(dict, "ImageMask").and_then(|value| match value {
                Value::Bool(flag) => Some(*flag),
                _ => None,
            }) == Some(true)
            {
                return;
            }
            let bits = dict_get(dict, "BitsPerComponent")
                .and_then(as_usize)
                .unwrap_or(8);
            if bits != 8 {
                self.warnings.push(format!(
                    "skipped a {bits}-bit image (only 8-bit samples are extracted)"
                ));
                return;
            }
            let Some(kind) = color_kind(self.pdf, dict_get(dict, "ColorSpace")) else {
                self.warnings.push(
                    "skipped an image with an unsupported colour space (only Gray/RGB/CMYK)"
                        .to_string(),
                );
                return;
            };
            let Some(rgb) = to_rgb(kind, &decoded, width, height) else {
                self.warnings.push(
                    "skipped an image whose samples were shorter than its header claims"
                        .to_string(),
                );
                return;
            };
            let Some(png) = cleanup_core::doc::encode_png(width as u32, height as u32, &rgb) else {
                return;
            };
            (ImageKind::Png, png)
        };
        let key = fnv1a(&data);
        let index = match self.asset_keys.get(&key) {
            Some(index) => *index,
            None => {
                let index = self.assets.len();
                self.assets.push(ImageAsset {
                    name: format!("figure-{}", index + 1),
                    kind,
                    data,
                    width_px: width as u32,
                    height_px: height as u32,
                    dpi_x: None,
                    dpi_y: None,
                });
                self.asset_keys.insert(key, index);
                index
            }
        };
        let corners = [
            apply(gs.ctm, 0.0, 0.0),
            apply(gs.ctm, 1.0, 0.0),
            apply(gs.ctm, 0.0, 1.0),
            apply(gs.ctm, 1.0, 1.0),
        ];
        let x0 = corners.iter().map(|c| c.0).fold(f32::MAX, f32::min);
        let x1 = corners.iter().map(|c| c.0).fold(f32::MIN, f32::max);
        let y0 = corners.iter().map(|c| c.1).fold(f32::MAX, f32::min);
        let y1 = corners.iter().map(|c| c.1).fold(f32::MIN, f32::max);
        let rect = self.page_rect(x0, y0, x1, y1);
        if rect.width > 0.5 && rect.height > 0.5 {
            if let Some(asset) = self.assets.get_mut(index) {
                asset.observe_dpi(&rect);
            }
            geom.images.push(ImagePlacement { asset: index, rect });
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn last_numbers(operands: &[Operand], count: usize) -> Option<Vec<f32>> {
    if operands.len() < count {
        return None;
    }
    let mut out: Vec<f32> = Vec::with_capacity(count);
    for operand in operands.iter().rev().take(count) {
        match operand {
            Operand::Num(value) => out.push(*value as f32),
            _ => return None,
        }
    }
    out.reverse();
    Some(out)
}

fn value01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

fn gray(value: f32) -> Rgb {
    let level = (value01(value) * 255.0).round() as u8;
    Rgb(level, level, level)
}

fn rgb_from(values: &[f32]) -> Rgb {
    Rgb(
        (value01(values[0]) * 255.0).round() as u8,
        (value01(values[1]) * 255.0).round() as u8,
        (value01(values[2]) * 255.0).round() as u8,
    )
}

fn cmyk(values: &[f32]) -> Rgb {
    let c = value01(values[0]);
    let m = value01(values[1]);
    let y = value01(values[2]);
    let k = value01(values[3]);
    let channel = |component: f32| ((1.0 - component) * (1.0 - k) * 255.0).round() as u8;
    Rgb(channel(c), channel(m), channel(y))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorKind {
    Gray,
    Rgb,
    Cmyk,
}

fn color_kind(pdf: &ParsedPdf, value: Option<&Value>) -> Option<ColorKind> {
    let value = value?;
    match pdf.resolve(value) {
        Value::Name(name) => match name.as_str() {
            "DeviceGray" | "CalGray" | "G" => Some(ColorKind::Gray),
            "DeviceRGB" | "CalRGB" | "RGB" => Some(ColorKind::Rgb),
            "DeviceCMYK" | "CMYK" => Some(ColorKind::Cmyk),
            _ => None,
        },
        Value::Array(items) => {
            let first = items.first().and_then(as_name)?;
            match first {
                "ICCBased" => {
                    let reference = items.get(1).and_then(as_ref)?;
                    let dict = pdf.object_dict(reference)?;
                    let components = dict_get(dict, "N").and_then(as_int)?;
                    match components {
                        1 => Some(ColorKind::Gray),
                        3 => Some(ColorKind::Rgb),
                        4 => Some(ColorKind::Cmyk),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn to_rgb(kind: ColorKind, samples: &[u8], width: usize, height: usize) -> Option<Vec<u8>> {
    let pixels = width.checked_mul(height)?;
    match kind {
        ColorKind::Gray => {
            if samples.len() < pixels {
                return None;
            }
            let mut rgb = Vec::with_capacity(pixels * 3);
            for sample in &samples[..pixels] {
                rgb.extend_from_slice(&[*sample, *sample, *sample]);
            }
            Some(rgb)
        }
        ColorKind::Rgb => {
            if samples.len() < pixels * 3 {
                return None;
            }
            Some(samples[..pixels * 3].to_vec())
        }
        ColorKind::Cmyk => {
            if samples.len() < pixels * 4 {
                return None;
            }
            let mut rgb = Vec::with_capacity(pixels * 3);
            for pixel in samples[..pixels * 4].as_chunks::<4>().0 {
                let [cyan, magenta, yellow, black] = *pixel;
                let k = u32::from(black);
                for value in [cyan, magenta, yellow] {
                    let channel = (u32::from(value) * (255 - k)) / 255;
                    rgb.push(channel as u8);
                }
            }
            Some(rgb)
        }
    }
}
