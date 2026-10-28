//! Spatial reconstruction: PDF page content → glyphs, words, rules, fills and images.
//!
//! Native cleaning uses the font metrics here to compensate for changed text advances without
//! re-typesetting. Conversion additionally needs to reconstruct where everything is drawn to
//! rebuild tables, columns and figures. This module interprets page content streams with the full
//! graphics state:
//!
//! * `q`/`Q`/`cm` maintain the CTM; `BT`/`ET`/`Tm`/`Td`/`TD`/`T*`/`TL` maintain the text
//!   matrices, and every `Tj`/`TJ`/`'`/`"` produces one [`Glyph`] per character code whose box
//!   is the transformed advance, using the font's `/Widths` (or CID `/W`) where available.
//!   Word boundaries are *not* decided here: `cluster` turns the page's glyphs into
//!   [`cleanup_core::doc::TextRun`]s once the whole page has been read;
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
    Axis, FillRect, Glyph, ImageAsset, ImageKind, ImagePlacement, LayoutDoc, PageLayout, Rect, Rgb,
    RuleSegment, cluster, fontmap,
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
                page_left: entry.left,
                page_top: entry.top,
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

/// PDF dictionary numbers may be real, not just integers. Truncating a 0.5 Form
/// matrix to zero, or a fractional glyph width, destroys the page geometry.
fn as_f32(value: &Value) -> Option<f32> {
    let number = match value {
        Value::Int(number) => *number as f32,
        Value::Real(number) => number.parse::<f32>().ok()?,
        _ => return None,
    };
    number.is_finite().then_some(number)
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
        matrix[index] = as_f32(item)?;
    }
    Some(matrix)
}

struct PageEntry {
    width: f32,
    height: f32,
    /// MediaBox lower-left x and upper-right y in PDF user space.
    left: f32,
    top: f32,
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
    inherited_box: Option<(f32, f32, f32, f32)>,
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
    let (left, bottom, right, top) = media_box.unwrap_or((0.0, 0.0, 612.0, 792.0));
    out.push(PageEntry {
        width: right - left,
        height: top - bottom,
        left,
        top,
        rotate,
        resources,
        contents,
    });
}

fn media_box_of(value: &Value) -> Option<(f32, f32, f32, f32)> {
    let Value::Array(items) = value else {
        return None;
    };
    if items.len() != 4 {
        return None;
    }
    let x0 = as_f32(&items[0])?;
    let y0 = as_f32(&items[1])?;
    let x1 = as_f32(&items[2])?;
    let y1 = as_f32(&items[3])?;
    if (x1 - x0).abs() < 1.0 || (y1 - y0).abs() < 1.0 {
        return None;
    }
    Some((x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)))
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
    /// Every glyph the page drew, in paint order. Words are reconstructed from this buffer
    /// once, by [`cluster::words_from_glyphs`], instead of being guessed per showing operator.
    glyphs: Vec<Glyph>,
    rules: Vec<RuleSegment>,
    fills: Vec<FillRect>,
    images: Vec<ImagePlacement>,
}

impl PageGeometry {
    fn new(width: f32, height: f32) -> Self {
        PageGeometry {
            width,
            height,
            glyphs: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            images: Vec::new(),
        }
    }

    fn into_page(self) -> PageLayout {
        // Word reconstruction happens exactly once per page, after every content stream, form
        // XObject and text object has contributed its glyphs.
        let runs = cluster::words_from_glyphs(&self.glyphs);
        PageLayout {
            width: self.width,
            height: self.height,
            runs,
            rules: self.rules,
            fills: self.fills,
            images: self.images,
        }
    }
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// Everything the reader knows about one `/Font` resource.
///
/// Read once from the font dictionary, `/FontDescriptor` and the name, so that every glyph the
/// font shows carries the same family, weight, italic flag and metrics. Nothing downstream
/// re-guesses a style from a name.
#[derive(Debug, Clone)]
pub(super) struct FontInfo {
    pub(super) profile: FontProfile,
    pub(super) vertical: bool,
    pub(super) safe_cid_mapping: bool,
    native_base14: bool,
    metrics: FontMetrics,
    /// PostScript name with the subset tag stripped, e.g. `DejaVuSans-Bold`.
    canonical: String,
    /// Alias-folded family, e.g. `Helvetica`.
    family: String,
    /// Weight in the `100..=900` scale.
    weight: u16,
    bold: bool,
    italic: bool,
    mono: bool,
    /// Ascent above the baseline, in em.
    ascent: f32,
    /// Descent below the baseline, in em (positive).
    descent: f32,
    /// Advance of the space character, in em.
    space_em: f32,
}

impl FontInfo {
    /// Use declared widths, then the font's missing/default width. Base-14 AFM
    /// widths apply only when the PDF did not supply its own metrics. Translate
    /// the font's encoding to WinAnsi before indexing those tables.
    pub(super) fn known_width_em(&self, code: u32) -> Option<f32> {
        let base_width = || {
            let text = self.profile.decode.get(&code)?;
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            let winansi = if ch.is_ascii() {
                u32::from(ch)
            } else {
                u32::from((128u16..=255).find(|byte| winansi_char(*byte as u8) == Some(ch))?)
            };
            fontmap::base14_width_for_name(&self.canonical, winansi)
        };
        self.metrics
            .declared(code)
            .or(self.metrics.default_width)
            .or_else(base_width)
            .map(|width| width / 1000.0)
    }

    /// Native editing may not use a merely metric-compatible substitute: a
    /// small width error would move all subsequent text. Only explicit PDF
    /// widths/defaults or the exact, unembedded standard 14 face are safe.
    pub(super) fn positioning_width_em(&self, code: u32) -> Option<f32> {
        self.metrics.declared(code)
            .or(self.metrics.default_width)
            .map(|width| width / 1000.0)
            .or_else(|| if self.native_base14 { self.known_width_em(code) } else { None })
    }

    fn width_em(&self, code: u32) -> f32 {
        self.known_width_em(code).unwrap_or(0.5)
    }

    /// Advance of a space at `size` points, in text-space units.
    fn space_advance(&self, size: f32) -> f32 {
        (self.space_em * size).max(size * 0.1)
    }
}

#[derive(Debug, Clone, Default)]
struct FontMetrics {
    first_char: i32,
    widths: Vec<Option<f32>>,
    cid_widths: BTreeMap<u32, f32>,
    default_width: Option<f32>,
}

impl FontMetrics {
    /// A zero advance is meaningful (e.g. combining marks), not a missing width.
    fn declared(&self, code: u32) -> Option<f32> {
        let index = i64::from(code) - i64::from(self.first_char);
        if index >= 0
            && let Some(width) = self.widths.get(index as usize).copied().flatten()
        {
            return Some(width);
        }
        self.cid_widths.get(&code).copied()
    }
}

pub(super) fn fonts_for(pdf: &ParsedPdf, resources: Option<&Value>) -> BTreeMap<String, FontInfo> {
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

/// `/FontDescriptor /Flags` bits (PDF 32000-1 table 123).
const FLAG_FIXED_PITCH: i64 = 1 << 0;
const FLAG_ITALIC: i64 = 1 << 6;
const FLAG_FORCE_BOLD: i64 = 1 << 18;

fn descendant_font<'a>(
    pdf: &'a ParsedPdf,
    dict: &'a [(String, Value)],
) -> Option<&'a [(String, Value)]> {
    let Value::Array(fonts) = pdf.resolve(dict_get(dict, "DescendantFonts")?) else {
        return None;
    };
    pdf.resolve_dict(fonts.first()?).map(Vec::as_slice)
}

fn font_descriptor<'a>(
    pdf: &'a ParsedPdf,
    dict: &'a [(String, Value)],
) -> Option<&'a [(String, Value)]> {
    let owner = descendant_font(pdf, dict).unwrap_or(dict);
    dict_get(owner, "FontDescriptor")
        .map(|value| pdf.resolve(value))
        .and_then(as_dict_ref)
}

fn font_info(pdf: &ParsedPdf, dict: &[(String, Value)]) -> FontInfo {
    let profile = font_profile(pdf, dict);
    let metrics = font_metrics(pdf, dict);
    let descriptor = font_descriptor(pdf, dict);
    let base_raw = dict_get(dict, "BaseFont")
        .and_then(as_name)
        .or_else(|| {
            descriptor
                .and_then(|entries| dict_get(entries, "FontName"))
                .and_then(as_name)
        })
        .unwrap_or("");
    let canonical = fontmap::canonical_name(base_raw);
    let family = fontmap::family(&canonical);
    let flags = descriptor
        .and_then(|entries| dict_get(entries, "Flags"))
        .and_then(as_int)
        .unwrap_or(0);
    let italic_angle = descriptor
        .and_then(|entries| dict_get(entries, "ItalicAngle"))
        .and_then(as_f32)
        .unwrap_or(0.0);
    let stem_v = descriptor
        .and_then(|entries| dict_get(entries, "StemV"))
        .and_then(as_int);
    let descriptor_weight = descriptor
        .and_then(|entries| dict_get(entries, "FontWeight"))
        .and_then(as_int)
        .and_then(fontmap::weight_from_font_weight);

    // An explicit descriptor weight is authoritative; the name and StemV are fallbacks.
    let weight = descriptor_weight
        .or_else(|| fontmap::weight_from_name(&canonical))
        .or_else(|| (flags & FLAG_FORCE_BOLD != 0).then_some(700))
        .or_else(|| stem_v.and_then(fontmap::weight_from_stem_v))
        .unwrap_or(400);
    let italic =
        fontmap::name_is_italic(&canonical) || flags & FLAG_ITALIC != 0 || italic_angle != 0.0;
    let mono =
        fontmap::name_is_mono(&canonical) || flags & FLAG_FIXED_PITCH != 0 || family == "Courier";

    // Vertical metrics: the descriptor when it has them, the family's AFM otherwise. These
    // are what give every run an honest box instead of a `0.8 em` guess, so boxes on one
    // baseline stay aligned and boxes on different baselines never merge.
    let (base_ascent, base_descent, base_space) = fontmap::base_metrics(&family, mono);
    let ascent = descriptor
        .and_then(|entries| dict_get(entries, "Ascent"))
        .and_then(as_f32)
        .map(|value| value / 1000.0)
        .filter(|value| *value > 0.2 && *value < 1.6)
        .unwrap_or(base_ascent);
    let descent = descriptor
        .and_then(|entries| dict_get(entries, "Descent"))
        .and_then(as_f32)
        .map(|value| (value / 1000.0).abs())
        .filter(|value| *value > 0.02 && *value < 0.8)
        .unwrap_or(base_descent);
    // A CID font can map Unicode space to a code other than 32.
    let space_code = profile.encode.get(&' ').copied().unwrap_or(32);
    let space_em = metrics
        .declared(space_code)
        .map(|width| width / 1000.0)
        .or_else(|| fontmap::base14_width(&family, 32).map(|width| width / 1000.0))
        .filter(|value| (0.1..=0.9).contains(value))
        .unwrap_or(base_space);

    let encoding = dict_get(dict, "Encoding").map(|value| pdf.resolve(value)).and_then(as_name);
    let is_cid = dict_get(dict, "Subtype").and_then(as_name) == Some("Type0");
    let embedded = descriptor.is_some_and(|entries| ["FontFile", "FontFile2", "FontFile3"]
        .iter().any(|name| dict_get(entries, name).is_some()));
    let native_base14 = !embedded && matches!(base_raw,
        "Helvetica" | "Helvetica-Bold" | "Helvetica-Oblique" | "Helvetica-BoldOblique" |
        "Times-Roman" | "Times-Bold" | "Times-Italic" | "Times-BoldItalic" |
        "Courier" | "Courier-Bold" | "Courier-Oblique" | "Courier-BoldOblique"
    );
    FontInfo {
        profile,
        vertical: encoding.is_some_and(|name| name.ends_with("-V")),
        safe_cid_mapping: !is_cid || encoding == Some("Identity-H"),
        native_base14,
        metrics,
        canonical,
        family,
        weight,
        bold: weight >= 700,
        italic,
        mono,
        ascent,
        descent,
        space_em,
    }
}

/// Resolve a value to a dictionary without cloning it.
fn as_dict_ref(value: &Value) -> Option<&[(String, Value)]> {
    match value {
        Value::Dict(entries) => Some(entries.as_slice()),
        _ => None,
    }
}

fn font_metrics(pdf: &ParsedPdf, dict: &[(String, Value)]) -> FontMetrics {
    let mut metrics = FontMetrics::default();
    let descendant = descendant_font(pdf, dict);

    if let Some(descendant) = descendant {
        // PDF's default CID width is 1000, even when /DW is absent.
        metrics.default_width = Some(
            dict_get(descendant, "DW")
                .and_then(as_f32)
                .unwrap_or(1000.0),
        );
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
            metrics.widths.push(as_f32(item));
        }
    }
    if metrics.default_width.is_none() {
        metrics.default_width = font_descriptor(pdf, dict)
            .and_then(|descriptor| dict_get(descriptor, "MissingWidth"))
            .and_then(as_f32);
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
                    if let (Ok(code), Some(value)) = (u32::try_from(first), as_f32(width))
                        && let Some(code) = code.checked_add(offset as u32)
                        && code <= 65_535
                    {
                        out.insert(code, value);
                    }
                }
                index += 2;
            }
            Some(last) => {
                let Some(last) = as_int(last) else {
                    index += 1;
                    continue;
                };
                if let (Ok(first), Ok(last), Some(width)) = (
                    u32::try_from(first),
                    u32::try_from(last),
                    items.get(index + 2).and_then(as_f32),
                ) && first <= last
                    && last <= 65_535
                {
                    for code in first..=last {
                        out.insert(code, width);
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
    page_left: f32,
    page_top: f32,
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
                "BI" => {
                    lexer.skip_inline_image();
                    self.warnings.push("inline PDF images are not yet extracted during conversion".to_string());
                }
                "gs" => {
                    // Extended graphics state (alpha, blend mode) carries no geometry we use.
                }
                _ => {}
            }
            operands.clear();
        }
        // A path left un-painted at the end of a stream is not rendered by a viewer either.
    }

    /// Emit one glyph cell per character code, exactly where the text matrices put it.
    ///
    /// Every showing operator (`Tj`, each `TJ` element, `'`, `"`) funnels through here, so a
    /// word that arrives as two `Tj` calls, as one `TJ` element per letter, or with an explicit
    /// `Tm` per glyph still produces adjacent cells at the right coordinates. No spaces and no
    /// word boundaries are invented at this layer: `Tc` tracking, `Tw` word spacing, `Tz`
    /// scaling and `TJ` displacements are folded into the advance, and
    /// [`cluster::words_from_glyphs`] decides word breaks once, from that geometry.
    fn show(
        &mut self,
        bytes: &[u8],
        gs: &GraphicsState,
        tm: &mut Matrix,
        fonts: &BTreeMap<String, FontInfo>,
        geom: &mut PageGeometry,
    ) {
        let font_name_raw = gs.font.clone().unwrap_or_default();
        let trm = mul(*tm, gs.ctm);
        let x_scale = (trm[0] * trm[0] + trm[1] * trm[1]).sqrt().max(0.01);
        let y_scale = (trm[2] * trm[2] + trm[3] * trm[3]).sqrt().max(0.01);
        let size = (gs.font_size * y_scale).max(0.5);

        let Some(font) = fonts.get(&font_name_raw) else {
            // A font resource we could not resolve (or a symbolic face with no Unicode map).
            // Advance by a nominal half-em so later runs keep roughly their place, and record
            // that something was skipped instead of emitting garbage glyphs.
            if !bytes.is_empty() {
                *self.unsupported += 1;
            }
            let advance = bytes.len() as f32 * gs.font_size.max(1.0) * 0.5 * gs.hscale;
            *tm = mul(translation(advance, 0.0), *tm);
            return;
        };
        if !font.profile.supported {
            if !bytes.is_empty() {
                *self.unsupported += 1;
            }
            let advance = bytes.len() as f32 * gs.font_size.max(1.0) * 0.5 * gs.hscale;
            *tm = mul(translation(advance, 0.0), *tm);
            return;
        }

        let code_len = font.profile.code_len.max(1);
        let space_width = font.space_advance(gs.font_size) * x_scale * gs.hscale.abs();
        let mut cursor = 0.0f32;
        let mut index = 0usize;
        while index < bytes.len() {
            let end = (index + code_len).min(bytes.len());
            let code = hex_bytes_to_u32(&bytes[index..end]);
            let width = font.width_em(code);
            // PDF word spacing applies to the single-byte character code 32, not to
            // every Unicode whitespace character (nor to a two-byte CID 0x0020).
            let word_space = code_len == 1 && code == 32;
            let advance = (width * gs.font_size
                + gs.char_spacing
                + if word_space { gs.word_spacing } else { 0.0 })
                * gs.hscale;
            if let Some(text) = font.profile.decode.get(&code) {
                // Origin and end of the advance box, in page space. `rise` (`Ts`) is applied
                // along the text-space y axis, so a superscript is displaced the way a viewer
                // draws it rather than by a re-derived offset.
                let (x0, y0) = apply(trm, cursor, gs.rise);
                let (x1, _) = apply(trm, cursor + advance, gs.rise);
                let baseline = self.page_top - y0;
                geom.glyphs.push(Glyph {
                    text: text.clone(),
                    rect: Rect::new(
                        x0.min(x1) - self.page_left,
                        baseline - font.ascent * size,
                        (x1 - x0).abs().max(0.01),
                        (font.ascent + font.descent) * size,
                    ),
                    baseline,
                    space_width,
                    font_size: size,
                    font_name: font.canonical.clone(),
                    font_family: font.family.clone(),
                    font_weight: font.weight,
                    bold: font.bold,
                    italic: font.italic,
                    mono: font.mono,
                    color: gs.fill,
                });
            } else {
                *self.unsupported += 1;
            }
            cursor += advance;
            index = end;
        }
        *tm = mul(translation(cursor, 0.0), *tm);
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
            let left = x0 - self.page_left;
            let right = x1 - self.page_left;
            let top = self.page_top - y1;
            let bottom = self.page_top - y0;
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
        Rect::new(x0 - self.page_left, self.page_top - y1, width, height)
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
        let jpeg_filter = filters
            .iter()
            .position(|filter| filter == "DCTDecode" || filter == "DCT");
        if filters.iter().any(|filter| filter == "JPXDecode") {
            self.warnings.push(
                "skipped a JPEG 2000 image (JPXDecode is not supported); its position is lost"
                    .to_string(),
            );
            return;
        }
        if dict_get(dict, "SMask").is_some() || dict_get(dict, "Mask").is_some() {
            self.warnings.push("image transparency masks are not preserved during conversion".to_string());
        }
        if dict_get(dict, "Decode").is_some() {
            self.warnings.push("image Decode arrays are not applied during conversion".to_string());
        }
        if matches!(dict_get(dict, "ColorSpace").map(|v| self.pdf.resolve(v)), Some(Value::Array(items)) if items.first().and_then(as_name) == Some("ICCBased")) {
            self.warnings.push("ICC image profiles are approximated by their component count; colour management is not applied".to_string());
        }
        let (kind, data) = if let Some(index) = jpeg_filter {
            if index + 1 != filters.len() {
                self.warnings.push("skipped an image with filters after DCTDecode".to_string());
                return;
            }
            // DCT data may itself be wrapped in ASCII85, ASCIIHex, or Flate.
            // Only the bytes *after* these wrappers are a usable JPEG asset.
            let mut prefix = dict.to_vec();
            set_dict_entry(&mut prefix, "Filter", Value::Array(
                filters[..index].iter().cloned().map(Value::Name).collect()
            ));
            let Ok(jpeg) = decode_stream_data(&prefix, raw) else {
                self.warnings.push("skipped a JPEG with an unsupported outer filter".to_string());
                return;
            };
            if !jpeg.starts_with(&[0xFF, 0xD8]) {
                self.warnings.push("skipped an image with an invalid JPEG payload".to_string());
                return;
            }
            (ImageKind::Jpeg, jpeg)
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
                    let channel = ((255 - u32::from(value)) * (255 - k)) / 255;
                    rgb.push(channel as u8);
                }
            }
            Some(rgb)
        }
    }
}
