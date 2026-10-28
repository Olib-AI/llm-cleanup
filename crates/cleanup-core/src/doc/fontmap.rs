//! Font-name normalisation and the standardised fallback map.
//!
//! A PDF names its fonts three different ways at once: `/BaseFont` is a PostScript name
//! (`ABCDEF+Helvetica-Bold`), `/FontDescriptor /FontName` repeats it, and the descriptor's
//! `/Flags`, `/ItalicAngle`, `/StemV` and `/FontWeight` carry the style the name pretends to.
//! Producers are careless about all of it: subset tags are `ABCDEF+`, `AAAAAA+`, `BAAAAA+`,
//! or absent; the style is spelled `-Bold`, `,Bold`, `Bold`, `-Bd`, `-BoldMT`; family names
//! are aliased eleven ways (`ArialMT`, `Helvetica`, `NimbusSans`, `LiberationSans`, all
//! metric-compatible with Helvetica).
//!
//! Everything in this module is pure string/constant work with no PDF knowledge, so readers,
//! the analyser and the exporters share one interpretation of a font name. That is what stops
//! a font from being re-guessed (and downgraded) at every stage of the pipeline.

/// Suffixes that mark a font *format* rather than a family or a style.
const FORMAT_SUFFIXES: [&str; 6] = ["psmt", "ps", "mt", "mx", "lt", "ce"];

/// Style words a PostScript name may append to its family, longest first so that
/// `ExtraBold` is consumed before `Bold`.
const STYLE_WORDS: [&str; 28] = [
    "extrabold",
    "ultrabold",
    "semibold",
    "demibold",
    "bolditalic",
    "boldoblique",
    "bold",
    "oblique",
    "italic",
    "regular",
    "normal",
    "roman",
    "medium",
    "light",
    "black",
    "heavy",
    "thin",
    "book",
    "condensed",
    "narrow",
    "extended",
    "display",
    "text",
    "std",
    "bd",
    "ital",
    "obl",
    "it",
];

/// Strip a six-capital-letter subset tag: `ABCDEF+Helvetica-Bold` → `Helvetica-Bold`.
///
/// Only the `<6 uppercase letters>+` form is a subset tag. A family that legitimately contains
/// a plus (`C++Mono`) is left alone because its prefix is not six capitals.
pub fn strip_subset_prefix(name: &str) -> &str {
    if let Some((prefix, rest)) = name.split_once('+')
        && prefix.len() == 6
        && prefix.chars().all(|c| c.is_ascii_uppercase())
    {
        return rest;
    }
    name
}

/// Canonical PostScript-ish name: subset tag and leading `/` removed, `,` separators folded
/// into `-`, stray separator runs collapsed.
pub fn canonical_name(raw: &str) -> String {
    // Strip the resource name's leading `/` first: `/BAAAAA+Inter` is a subset-tagged name.
    let trimmed = raw.trim().trim_start_matches('/');
    let stripped = strip_subset_prefix(trimmed);
    let folded = stripped.replace(',', "-");
    let mut out = String::with_capacity(folded.len());
    for ch in folded.chars() {
        if ch == '-' || ch == '_' || ch == ' ' {
            if out.ends_with('-') || out.is_empty() {
                continue;
            }
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out.trim_matches('-').to_string()
}

/// Split `DejaVuSans-Bold` into `["DejaVuSans", "Bold"]` for style inspection.
fn parts(name: &str) -> Vec<&str> {
    name.split(['-', '_', ' ', ','])
        .filter(|part| !part.is_empty())
        .collect()
}

/// True when a name component is nothing but style words (`BoldOblique`, `MT`, `Roman`).
fn is_style_component(component: &str) -> bool {
    let mut rest = component.to_ascii_lowercase();
    if rest.is_empty() {
        return true;
    }
    loop {
        let mut consumed = false;
        for word in STYLE_WORDS.iter().chain(FORMAT_SUFFIXES.iter()).copied() {
            let tail = match rest.strip_prefix(word) {
                Some(tail) => tail.to_string(),
                None => continue,
            };
            rest = tail;
            consumed = true;
            break;
        }
        if !consumed {
            break;
        }
    }
    rest.is_empty()
}

/// PostScript name with every style/format component removed: `DejaVuSans-BoldOblique` →
/// `DejaVuSans`, `Times-Roman` → `Times`.
fn family_stem(name: &str) -> String {
    let canonical = canonical_name(name);
    let mut kept: Vec<&str> = Vec::new();
    for part in parts(&canonical) {
        if is_style_component(part) {
            continue;
        }
        kept.push(part);
    }
    if kept.is_empty() {
        // Everything was a style word (`Bold`): fall back to the whole name so the run still
        // carries *some* family instead of an empty string.
        return canonical.clone();
    }
    let mut out = kept.join("-");
    // `DejaVuSansMT` / `ArialPSMT`: drop a trailing format suffix glued to the family.
    let lowered = out.to_ascii_lowercase();
    for suffix in FORMAT_SUFFIXES {
        if lowered.len() > suffix.len() && lowered.ends_with(suffix) {
            // Re-check the *stem* is not itself a real word ending in those letters: only strip
            // when the remaining stem is at least three characters.
            let stem_len = out.len() - suffix.len();
            if stem_len >= 3 {
                out.truncate(stem_len);
            }
            break;
        }
    }
    out
}

/// Normalise a family name for alias matching: lowercase, letters and digits only.
fn alias_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The standardised fallback map: metric-compatible aliases collapse onto the family the rest
/// of the world (and every office suite) knows.
///
/// Keeping this table in one place is what makes "ArialMT" and "Helvetica" produce the same
/// DOCX `w:rFonts` value instead of two arbitrary substitutions.
fn alias_target(key: &str) -> Option<&'static str> {
    const TABLE: [(&str, &str); 22] = [
        ("helvetica", "Helvetica"),
        ("helveticaneue", "Helvetica"),
        ("arial", "Helvetica"),
        ("arialmt", "Helvetica"),
        ("arialnarrow", "Helvetica"),
        ("nimbussans", "Helvetica"),
        ("liberationsans", "Helvetica"),
        ("arimo", "Helvetica"),
        ("freesans", "Helvetica"),
        ("times", "Times"),
        ("timesnewroman", "Times"),
        ("timesroman", "Times"),
        ("timesnewromanpsmt", "Times"),
        ("tinos", "Times"),
        ("nimbusroman", "Times"),
        ("liberationserif", "Times"),
        ("courier", "Courier"),
        ("couriernew", "Courier"),
        ("couriernewpsmt", "Courier"),
        ("liberationmono", "Courier"),
        ("nimbusmono", "Courier"),
        ("freesansmono", "Courier"),
    ];
    for (alias, mapped) in TABLE {
        if alias == key {
            return Some(mapped);
        }
    }
    // Producers glue their own suffixes onto a metric-compatible name
    // (`CourierNewPSMT` loses `MT` and leaves `CourierNewPS`), so fall back to the longest
    // alias that the key starts with rather than inventing a family nobody can resolve.
    let mut best: Option<(&'static str, usize)> = None;
    for (alias, mapped) in TABLE {
        if key.starts_with(alias) && best.is_none_or(|(_, length)| alias.len() > length) {
            best = Some((mapped, alias.len()));
        }
    }
    best.map(|(mapped, _)| mapped)
}

/// Human-readable family for a font name: subset tag stripped, style words removed, aliases
/// folded onto their canonical family.
pub fn family(name: &str) -> String {
    let stem = family_stem(name);
    let key = alias_key(&stem);
    if let Some(target) = alias_target(&key) {
        return target.to_string();
    }
    if stem.is_empty() {
        "Unknown".to_string()
    } else {
        stem
    }
}

/// Weight in the CSS/OpenType `100..=900` scale implied by the name alone.
///
/// Returns `None` for a name with no weight word, which lets the caller prefer the
/// `/FontDescriptor` (`/FontWeight`, `/StemV`, `ForceBold`) instead of assuming regular.
pub fn weight_from_name(name: &str) -> Option<u16> {
    let lowered = name.to_ascii_lowercase();
    let has = |needle: &str| lowered.contains(needle);
    if has("extrabold") || has("ultrabold") {
        return Some(800);
    }
    if has("semibold") || has("demibold") {
        return Some(600);
    }
    let abbreviated_bold = parts(name)
        .iter()
        .any(|part| part.eq_ignore_ascii_case("bd"));
    if has("bold") || abbreviated_bold {
        return Some(700);
    }
    if has("black") || has("heavy") || has("fat") {
        return Some(900);
    }
    if has("hairline") {
        return Some(100);
    }
    if has("thin") {
        return Some(200);
    }
    if has("extralight") || has("ultralight") {
        return Some(200);
    }
    if has("light") {
        return Some(300);
    }
    if has("medium") {
        return Some(500);
    }
    if has("book") {
        return Some(450);
    }
    None
}

/// Weight implied by a `/FontWeight` entry (some producers write 0, others 1000).
pub fn weight_from_font_weight(value: i64) -> Option<u16> {
    if !(1..=1000).contains(&value) {
        return None;
    }
    Some(value.clamp(100, 900) as u16)
}

/// Weight implied by `/StemV`, the vertical stem thickness in 1/1000 em.
///
/// The bands follow the classic PostScript usage: regular text sits at 80–110, bold at 140+.
pub fn weight_from_stem_v(stem_v: i64) -> Option<u16> {
    if stem_v <= 0 {
        return None;
    }
    Some(match stem_v {
        0..=49 => 200,
        50..=79 => 300,
        80..=114 => 400,
        115..=139 => 500,
        140..=179 => 700,
        _ => 800,
    })
}

/// True when the name says italic or oblique.
pub fn name_is_italic(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered.contains("italic") || lowered.contains("oblique") || {
        // PostScript abbreviations that are unambiguous as a whole component.
        parts(name).iter().any(|part| {
            let part = part.to_ascii_lowercase();
            part == "it" || part == "ital" || part == "obl"
        })
    }
}

/// True when the name is bold (weight 700+), including the `Bd` abbreviation.
pub fn name_is_bold(name: &str) -> bool {
    weight_from_name(name).is_some_and(|weight| weight >= 700)
}

/// True when the name describes a monospaced face.
pub fn name_is_mono(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    const MARKERS: [&str; 14] = [
        "courier",
        "mono",
        "consol",
        "menlo",
        "monaco",
        "sourcecode",
        "source code",
        "code",
        "typewriter",
        "fixed",
        "inconsolata",
        "jetbrains",
        "hack",
        "terminal",
    ];
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// True when the family is one of the serif faces that should keep a serif fallback.
pub fn is_serif_family(family: &str) -> bool {
    let lowered = family.to_ascii_lowercase();
    const MARKERS: [&str; 9] = [
        "times",
        "serif",
        "georgia",
        "garamond",
        "cambria",
        "book",
        "roman",
        "baskerville",
        "palatino",
    ];
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Base-14 (and common look-alike) text metrics in em: `(ascent, descent, space advance)`.
///
/// Used when a PDF font has no readable `/FontDescriptor` — a base-14 font referenced without
/// an embedded descriptor, or a symbolic subset whose descriptor was stripped. Without this,
/// every such run would fall back to one guessed box height and the baselines would drift.
pub fn base_metrics(family: &str, mono: bool) -> (f32, f32, f32) {
    let lowered = family.to_ascii_lowercase();
    if mono || lowered.contains("courier") {
        return (0.629, 0.157, 0.600);
    }
    if lowered.contains("times") || lowered.contains("serif") {
        return (0.683, 0.217, 0.250);
    }
    if lowered.contains("helvetica") || lowered.contains("arial") {
        return (0.718, 0.207, 0.278);
    }
    // A generic text face. DejaVu Sans, Inter, Source Sans and friends all sit close to this.
    (0.76, 0.24, 0.30)
}

/// Base-14 AFM widths in 1/1000 em, indexed by WinAnsi character code.
/// Zero means no corresponding glyph. Oblique Helvetica shares its upright metrics.
const HELVETICA_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
    350, 556, 350, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 350, 611, 350, 350,
    222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 350, 500, 667, 278, 333, 556,
    556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333, 400, 584, 333, 333, 333, 556,
    537, 278, 333, 333, 365, 556, 834, 834, 834, 611, 667, 667, 667, 667, 667, 667, 1000, 722, 667,
    667, 667, 667, 278, 278, 278, 278, 722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722,
    722, 667, 667, 611, 556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278,
    278, 556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500,
];

const HELVETICA_BOLD_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
    350, 556, 350, 278, 556, 500, 1000, 556, 556, 333, 1000, 667, 333, 1000, 350, 611, 350, 350,
    278, 278, 500, 500, 350, 556, 1000, 333, 1000, 556, 333, 944, 350, 500, 667, 278, 333, 556,
    556, 556, 556, 280, 556, 333, 737, 370, 556, 584, 333, 737, 333, 400, 584, 333, 333, 333, 611,
    556, 278, 333, 333, 365, 556, 834, 834, 834, 611, 722, 722, 722, 722, 722, 722, 1000, 722, 667,
    667, 667, 667, 278, 278, 278, 278, 722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722,
    722, 667, 667, 611, 556, 556, 556, 556, 556, 556, 889, 556, 556, 556, 556, 556, 278, 278, 278,
    278, 611, 611, 611, 611, 611, 611, 611, 584, 611, 611, 611, 611, 611, 556, 611, 556,
];

const TIMES_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
    350, 500, 350, 333, 500, 444, 1000, 500, 500, 333, 1000, 556, 333, 889, 350, 611, 350, 350,
    333, 333, 444, 444, 350, 500, 1000, 333, 980, 389, 333, 722, 350, 444, 722, 250, 333, 500, 500,
    500, 500, 200, 500, 333, 760, 276, 500, 564, 333, 760, 333, 400, 564, 300, 300, 333, 500, 453,
    250, 333, 300, 310, 500, 750, 750, 750, 444, 722, 722, 722, 722, 722, 722, 889, 667, 611, 611,
    611, 611, 333, 333, 333, 333, 722, 722, 722, 722, 722, 722, 722, 564, 722, 722, 722, 722, 722,
    722, 556, 500, 444, 444, 444, 444, 444, 444, 667, 444, 444, 444, 444, 444, 278, 278, 278, 278,
    500, 500, 500, 500, 500, 500, 500, 564, 500, 500, 500, 500, 500, 500, 500, 500,
];

const TIMES_BOLD_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 930, 722, 667, 722, 722, 667,
    611, 778, 778, 389, 500, 778, 667, 944, 722, 778, 611, 778, 722, 556, 667, 722, 722, 1000, 722,
    722, 667, 333, 278, 333, 581, 500, 333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556,
    278, 833, 556, 500, 556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520,
    350, 500, 350, 333, 500, 500, 1000, 500, 500, 333, 1000, 556, 333, 1000, 350, 667, 350, 350,
    333, 333, 500, 500, 350, 500, 1000, 333, 1000, 389, 333, 722, 350, 444, 722, 250, 333, 500,
    500, 500, 500, 220, 500, 333, 747, 300, 500, 570, 333, 747, 333, 400, 570, 300, 300, 333, 556,
    540, 250, 333, 300, 330, 500, 750, 750, 750, 500, 722, 722, 722, 722, 722, 722, 1000, 722, 667,
    667, 667, 667, 389, 389, 389, 389, 722, 722, 778, 778, 778, 778, 778, 570, 778, 722, 722, 722,
    722, 722, 611, 556, 500, 500, 500, 500, 500, 500, 722, 444, 444, 444, 444, 444, 278, 278, 278,
    278, 500, 556, 500, 500, 500, 500, 500, 570, 500, 556, 556, 556, 556, 500, 556, 500,
];

const TIMES_ITALIC_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 420, 500, 500, 833, 778, 214, 333, 333, 500, 675, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 675, 675, 675, 500, 920, 611, 611, 667, 722, 611,
    611, 722, 722, 333, 444, 667, 556, 833, 667, 722, 611, 722, 611, 500, 556, 722, 611, 833, 611,
    556, 556, 389, 278, 389, 422, 500, 333, 500, 500, 444, 500, 444, 278, 500, 500, 278, 278, 444,
    278, 722, 500, 500, 500, 500, 389, 389, 278, 500, 444, 667, 444, 444, 389, 400, 275, 400, 541,
    350, 500, 350, 333, 500, 556, 889, 500, 500, 333, 1000, 500, 333, 944, 350, 556, 350, 350, 333,
    333, 556, 556, 350, 500, 889, 333, 980, 389, 333, 667, 350, 389, 556, 250, 389, 500, 500, 500,
    500, 275, 500, 333, 760, 276, 500, 675, 333, 760, 333, 400, 675, 300, 300, 333, 500, 523, 250,
    333, 300, 310, 500, 750, 750, 750, 500, 611, 611, 611, 611, 611, 611, 889, 667, 611, 611, 611,
    611, 333, 333, 333, 333, 722, 667, 722, 722, 722, 722, 722, 675, 722, 722, 722, 722, 722, 556,
    611, 500, 500, 500, 500, 500, 500, 500, 667, 444, 444, 444, 444, 444, 278, 278, 278, 278, 500,
    500, 500, 500, 500, 500, 500, 675, 500, 500, 500, 500, 500, 444, 500, 444,
];

const TIMES_BOLD_ITALIC_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 389, 555, 500, 500, 833, 778, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 832, 667, 667, 667, 722, 667,
    667, 722, 778, 389, 500, 667, 611, 889, 722, 722, 611, 722, 667, 556, 611, 722, 667, 889, 667,
    611, 611, 333, 278, 333, 570, 500, 333, 500, 500, 444, 500, 444, 333, 500, 556, 278, 278, 500,
    278, 778, 556, 500, 500, 500, 389, 389, 278, 556, 444, 667, 500, 444, 389, 348, 220, 348, 570,
    350, 500, 350, 333, 500, 500, 1000, 500, 500, 333, 1000, 556, 333, 944, 350, 611, 350, 350,
    333, 333, 500, 500, 350, 500, 1000, 333, 1000, 389, 333, 722, 350, 389, 611, 250, 389, 500,
    500, 500, 500, 220, 500, 333, 747, 266, 500, 606, 333, 747, 333, 400, 570, 300, 300, 333, 576,
    500, 250, 333, 300, 300, 500, 750, 750, 750, 500, 667, 667, 667, 667, 667, 667, 944, 667, 667,
    667, 667, 667, 389, 389, 389, 389, 722, 722, 722, 722, 722, 722, 722, 570, 722, 722, 722, 722,
    722, 611, 611, 500, 500, 500, 500, 500, 500, 500, 722, 444, 444, 444, 444, 444, 278, 278, 278,
    278, 500, 556, 500, 500, 500, 500, 500, 570, 500, 556, 556, 556, 556, 444, 500, 444,
];

/// Advance width for a named base-14 face, in 1/1000 em. `code` is WinAnsi.
/// Other simple-font encodings must first be mapped to the same Unicode character.
pub fn base14_width_for_name(name: &str, code: u32) -> Option<f32> {
    let index = usize::try_from(code).ok()?;
    if !(0x20..=0xff).contains(&code) {
        return None;
    }
    let family = family(name);
    let lowered = family.to_ascii_lowercase();
    if lowered == "courier" {
        return Some(600.0);
    }
    let bold = name_is_bold(name);
    let italic = name_is_italic(name);
    let table = match (lowered.as_str(), bold, italic) {
        ("helvetica", true, _) => &HELVETICA_BOLD_WIDTHS,
        ("helvetica", false, _) => &HELVETICA_WIDTHS,
        ("times", true, true) => &TIMES_BOLD_ITALIC_WIDTHS,
        ("times", true, false) => &TIMES_BOLD_WIDTHS,
        ("times", false, true) => &TIMES_ITALIC_WIDTHS,
        ("times", false, false) => &TIMES_WIDTHS,
        _ => return None,
    };
    table
        .get(index)
        .copied()
        .filter(|width| *width != 0)
        .map(f32::from)
}

/// Advance width for a regular base-14 family, retained for callers without a face name.
pub fn base14_width(family: &str, code: u32) -> Option<f32> {
    base14_width_for_name(family, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_tags_are_stripped_but_real_plus_signs_survive() {
        assert_eq!(canonical_name("ABCDEF+Helvetica-Bold"), "Helvetica-Bold");
        assert_eq!(canonical_name("AAAAAA+DejaVuSans"), "DejaVuSans");
        assert_eq!(canonical_name("/BAAAAA+Inter"), "Inter");
        assert_eq!(canonical_name("C++Mono"), "C++Mono");
        assert_eq!(canonical_name("ABC+NotASubset"), "ABC+NotASubset");
    }

    #[test]
    fn style_words_are_removed_from_the_family() {
        assert_eq!(family("ABCDEF+Helvetica-Bold"), "Helvetica");
        assert_eq!(family("Times-Roman"), "Times");
        assert_eq!(family("DejaVuSans-BoldOblique"), "DejaVuSans");
        assert_eq!(family("Arial,Italic"), "Helvetica");
        assert_eq!(family("CourierNewPSMT"), "Courier");
        assert_eq!(family("LiberationSerif-Bold"), "Times");
    }

    #[test]
    fn weights_follow_the_open_type_scale() {
        assert_eq!(weight_from_name("Inter-Thin"), Some(200));
        assert_eq!(weight_from_name("Inter Light"), Some(300));
        assert_eq!(weight_from_name("Inter-Regular"), None);
        assert_eq!(weight_from_name("Inter-Medium"), Some(500));
        assert_eq!(weight_from_name("Inter-SemiBold"), Some(600));
        assert_eq!(weight_from_name("Helvetica-Bold"), Some(700));
        assert_eq!(weight_from_name("Inter-ExtraBold"), Some(800));
        assert_eq!(weight_from_name("Inter-Black"), Some(900));
    }

    #[test]
    fn italic_and_mono_detection_is_name_based() {
        assert!(name_is_italic("DejaVuSans-Oblique"));
        assert!(name_is_italic("Times-Italic"));
        assert!(!name_is_italic("DejaVuSans"));
        assert!(name_is_mono("DejaVuSansMono"));
        assert!(name_is_mono("Courier"));
        assert!(!name_is_mono("Helvetica"));
        assert!(name_is_bold("Inter-Bold"));
        assert!(!name_is_bold("Inter-Light"));
    }

    #[test]
    fn descriptor_weight_bands() {
        assert_eq!(weight_from_stem_v(87), Some(400));
        assert_eq!(weight_from_stem_v(165), Some(700));
        assert_eq!(weight_from_stem_v(0), None);
        assert_eq!(weight_from_font_weight(700), Some(700));
        assert_eq!(weight_from_font_weight(0), None);
        assert_eq!(weight_from_font_weight(1234), None);
    }

    #[test]
    fn base14_advances_match_the_afm_tables() {
        // Helvetica: space 278, `M` 833, `i` 222.
        assert_eq!(base14_width("Helvetica", 0x20), Some(278.0));
        assert_eq!(base14_width("Helvetica", u32::from(b'M')), Some(833.0));
        assert_eq!(base14_width("Helvetica", u32::from(b'i')), Some(222.0));
        // Times: narrower space and lowercase.
        assert_eq!(base14_width("Times", 0x20), Some(250.0));
        assert_eq!(base14_width("Times", u32::from(b'a')), Some(444.0));
        // Courier is flat.
        assert_eq!(base14_width("Courier", u32::from(b'W')), Some(600.0));
        // Control codes and non-Latin faces have no table.
        assert_eq!(base14_width("Helvetica", 0x09), None);
        assert_eq!(base14_width("Symbol", u32::from(b'a')), None);
    }

    #[test]
    fn base_metrics_stay_in_a_sane_range() {
        for (ascent, descent, space) in [
            base_metrics("Helvetica", false),
            base_metrics("Times", false),
            base_metrics("Courier", true),
            base_metrics("DejaVuSans", false),
        ] {
            assert!((0.5..1.2).contains(&ascent), "ascent {ascent}");
            assert!((0.0..0.4).contains(&descent), "descent {descent}");
            assert!((0.1..0.8).contains(&space), "space {space}");
        }
    }
}
