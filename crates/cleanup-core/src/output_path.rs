//! Robust output-file naming.
//!
//! Native save dialogs do not reliably append an extension: a user can delete the
//! suggested suffix, type a bare stem, or leave a stale extension from a different
//! output format selected a moment earlier. The same is true of a CLI `-o` path.
//! [`ensure_output_extension`] is the single place that turns "what the user typed"
//! plus "the format we are actually about to write" into a path that tells the truth
//! about its contents.

use std::path::{Path, PathBuf};

/// Extensions this application can read or write. A trailing component in this set is
/// understood to be a *format claim* (e.g. the `.docx` in `report.docx`); anything else
/// (`report.v1`, `notes.backup`, `draft.cleaned`) is understood to be part of the stem.
const KNOWN_EXTENSIONS: &[&str] = &[
    "md", "markdown", "mdown", "mkd", "txt", "text", "html", "htm", "xhtml", "docx", "pdf", "png",
    "jpg", "jpeg", "webp", "svg",
];

/// Interchangeable spellings of one format. Keeping `report.markdown` when the writer
/// expects `md` is correct — both name the same bytes — so an equivalent extension is
/// never rewritten.
fn extension_family(ext: &str) -> &str {
    match ext {
        "md" | "markdown" | "mdown" | "mkd" => "md",
        "txt" | "text" => "txt",
        "html" | "htm" | "xhtml" => "html",
        "jpg" | "jpeg" => "jpg",
        other => other,
    }
}

fn normalize_ext(ext: &str) -> String {
    ext.trim().trim_start_matches('.').to_ascii_lowercase()
}

/// Return `path` guaranteed to carry `expected_ext` (without a leading dot):
///
/// - no extension (`report`, `report.cleaned`, `.hidden`) → the extension is appended;
/// - an equivalent extension (`markdown` for `md`, `jpeg` for `jpg`) → unchanged;
/// - a *different known* format extension (`report.docx` while writing Markdown) →
///   replaced, so the saved name never mislabels the content;
/// - an unknown dot-suffix (`report.v1`) is treated as part of the stem → appended,
///   giving `report.v1.pdf`.
///
/// Extension comparison is case-insensitive (`Report.PDF` already satisfies `pdf`).
/// An empty `expected_ext` leaves the path unchanged.
pub fn ensure_output_extension(path: &Path, expected_ext: &str) -> PathBuf {
    let expected = normalize_ext(expected_ext);
    if expected.is_empty() {
        return path.to_path_buf();
    }

    // A path with no file name at all (e.g. a bare directory typed into `-o`) still has
    // to land somewhere truthful rather than failing downstream with a bare I/O error.
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return path.join(format!("cleaned.{expected}"));
    };
    if file_name.is_empty() || file_name == "." || file_name == ".." {
        let mut out = path.to_path_buf();
        out.set_file_name(format!("cleaned.{expected}"));
        return out;
    }

    let current = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(normalize_ext)
        .unwrap_or_default();

    if current.is_empty() {
        // No extension, an extension-like stem suffix (`report.cleaned`), or a hidden
        // name (`.report`): `Path::set_extension` appends in exactly these cases,
        // except that a trailing dot (`report.`) needs the append done by hand.
        if file_name.ends_with('.') {
            let mut out = path.to_path_buf();
            out.set_file_name(format!("{file_name}{expected}"));
            return out;
        }
        let mut out = path.to_path_buf();
        out.set_extension(&expected);
        return out;
    }

    if extension_family(&current) == extension_family(&expected) {
        return path.to_path_buf();
    }

    let mut out = path.to_path_buf();
    if KNOWN_EXTENSIONS.contains(&current.as_str()) {
        // A known but wrong format claim: replace it instead of stacking
        // (`report.docx.md` helps nobody and still misleads most openers).
        out.set_extension(&expected);
    } else {
        // Unknown suffixes are part of the user's stem (`report.v1`): append.
        out.set_file_name(format!("{file_name}.{expected}"));
    }
    out
}

/// Append `.cleaned` to a path's stem and guarantee `expected_ext` — the default
/// sibling name for a cleaned copy (`report.docx` → `report.cleaned.docx`, or
/// `report.cleaned.pdf` when converting to PDF).
pub fn cleaned_sibling_path(input: &Path, expected_ext: &str) -> PathBuf {
    let expected = normalize_ext(expected_ext);
    let stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("cleaned");
    let fallback = if stem.ends_with(".cleaned") {
        stem.to_string()
    } else {
        format!("{stem}.cleaned")
    };
    let mut out = input.to_path_buf();
    out.set_file_name(fallback);
    ensure_output_extension(&out, &expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensured(path: &str, ext: &str) -> String {
        ensure_output_extension(Path::new(path), ext)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn appends_a_missing_extension() {
        assert_eq!(ensured("report", "pdf"), "report.pdf");
        assert_eq!(ensured("out/report", "docx"), "out/report.docx");
        assert_eq!(ensured("report.cleaned", "md"), "report.cleaned.md");
        assert_eq!(ensured(".hidden", "txt"), ".hidden.txt");
        assert_eq!(ensured("report.", "pdf"), "report.pdf");
    }

    #[test]
    fn keeps_correct_and_equivalent_extensions() {
        assert_eq!(ensured("report.pdf", "pdf"), "report.pdf");
        assert_eq!(ensured("Report.PDF", "pdf"), "Report.PDF");
        assert_eq!(ensured("notes.markdown", "md"), "notes.markdown");
        assert_eq!(ensured("photo.jpeg", "jpg"), "photo.jpeg");
        assert_eq!(ensured("page.htm", "html"), "page.htm");
    }

    #[test]
    fn replaces_a_known_but_wrong_extension() {
        assert_eq!(ensured("report.docx", "md"), "report.md");
        assert_eq!(ensured("report.md", "pdf"), "report.pdf");
        assert_eq!(ensured("report.pdf", "txt"), "report.txt");
        assert_eq!(ensured("report.png", "pdf"), "report.pdf");
    }

    #[test]
    fn appends_after_unknown_stem_suffixes() {
        assert_eq!(ensured("report.v1", "pdf"), "report.v1.pdf");
        assert_eq!(ensured("draft.bak", "docx"), "draft.bak.docx");
        assert_eq!(ensured("archive.2026", "md"), "archive.2026.md");
    }

    #[test]
    fn tolerates_degenerate_inputs() {
        assert_eq!(ensured("report", ""), "report");
        assert_eq!(ensured("report", ".PDF"), "report.pdf");
        assert_eq!(
            ensure_output_extension(Path::new(".."), "pdf").to_string_lossy(),
            "../cleaned.pdf"
        );
    }

    #[test]
    fn cleaned_sibling_names_the_format_it_writes() {
        assert_eq!(
            cleaned_sibling_path(Path::new("docs/report.docx"), "docx").to_string_lossy(),
            "docs/report.cleaned.docx"
        );
        assert_eq!(
            cleaned_sibling_path(Path::new("docs/report.docx"), "pdf").to_string_lossy(),
            "docs/report.cleaned.pdf"
        );
        assert_eq!(
            cleaned_sibling_path(Path::new("docs/report.pdf"), "md").to_string_lossy(),
            "docs/report.cleaned.md"
        );
    }
}
