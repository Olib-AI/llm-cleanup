//! Cross-format conversion for llm-cleanup.
//!
//! This is the *lossy*, "render into a new container" subsystem, deliberately walled off from
//! the lossless byte-splice clean path in `cleanup-formats`. The CLI uses **clean-then-convert**:
//! a document is first cleaned losslessly in its native format, then (if the requested output
//! format differs) handed here to be rendered into the target. Engines are pure-Rust.
//!
//! # Pipeline
//!
//! ```text
//!   bytes ──► input::build ──► Doc (IR) ──► sanitize ──► export::* ──► bytes + assets
//! ```
//!
//! Every conversion goes *through* the IR: readers reconstruct structure once
//! ([`input`]), the sanitizer is the only mutation point ([`sanitize`]), and each exporter
//! renders the same tree ([`export`]). Nothing re-parses the source to target a container, which
//! is what used to flatten tables, drop images and merge columns. See `docs/DOCUMENT_IR.md`.

pub mod export;
pub mod sanitize;
pub mod to_docx;
pub mod to_markdown;
pub mod to_pdf;
pub mod to_text;

mod font;
mod input;

use cleanup_core::{CleanupLevel, FormatId};
use sanitize::SanitizeOptions;

/// A conversion target (output format). PDF is both a readable input (`FormatId::Pdf`,
/// cleaned natively in place) and an output target rendered from other formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Markdown,
    Text,
    Docx,
    Pdf,
}

impl Target {
    /// Map an output-file extension to a target format.
    pub fn from_ext(ext: &str) -> Option<Target> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "md" | "markdown" | "mdown" | "mkd" => Target::Markdown,
            "txt" | "text" => Target::Text,
            "docx" => Target::Docx,
            "pdf" => Target::Pdf,
            _ => return None,
        })
    }

    /// True if this target is the same format as `f` (so no conversion is needed).
    pub fn is_same_as(self, f: FormatId) -> bool {
        matches!(
            (self, f),
            (Target::Markdown, FormatId::Markdown)
                | (Target::Text, FormatId::PlainText)
                | (Target::Docx, FormatId::Docx)
                | (Target::Pdf, FormatId::Pdf)
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::Markdown => "markdown",
            Target::Text => "text",
            Target::Docx => "docx",
            Target::Pdf => "pdf",
        }
    }

    /// The canonical file extension for this target.
    pub fn ext(self) -> &'static str {
        match self {
            Target::Markdown => "md",
            Target::Text => "txt",
            Target::Docx => "docx",
            Target::Pdf => "pdf",
        }
    }
}

/// What a conversion did and lost.
#[derive(Debug, Default)]
pub struct ConvertReport {
    pub engine: String,
    /// True for any cross-container conversion (the byte-identity guarantee no longer holds).
    pub lossy: bool,
    /// Human-readable notes about what was degraded or dropped.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error(transparent)]
    Core(#[from] cleanup_core::CoreError),
    #[error("converting {from} to {to} is not supported yet")]
    Unsupported {
        from: &'static str,
        to: &'static str,
    },
    #[error("conversion failed: {0}")]
    Failed(String),
}

pub type Result<T> = std::result::Result<T, ConvertError>;

/// Knobs for one conversion run.
#[derive(Debug, Clone, Copy)]
pub struct ConvertOptions {
    /// Run the IR sanitizer before rendering. On by default: a conversion that keeps the
    /// assistant's preamble is usually not what the caller wanted.
    pub sanitize: bool,
    pub level: CleanupLevel,
    /// Remove blocks that consist *only* of assistant boilerplate, and filter running
    /// header/footer zones recorded in the IR.
    pub drop_boilerplate: bool,
    /// Keep source typography intact while converting (see `SanitizeOptions`). Turning this
    /// off applies the `typo.*` rules too, which is useful when the input is known to be raw
    /// model output rather than a typeset document.
    pub preserve_typography: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        ConvertOptions {
            sanitize: true,
            level: CleanupLevel::Standard,
            drop_boilerplate: true,
            preserve_typography: true,
        }
    }
}

/// A rendered conversion: bytes for the target container, plus any side-car files it needs
/// (Markdown and HTML reference images as separate files).
#[derive(Debug, Default)]
pub struct ConvertOutput {
    pub bytes: Vec<u8>,
    pub assets: Vec<(String, Vec<u8>)>,
    pub report: ConvertReport,
}

/// Convert already-cleaned `src` bytes (in format `from`) into the `to` format.
///
/// This legacy single-file API cannot return image sidecars. Refuse those outputs
/// instead of returning a Markdown document with broken image links; use
/// [`convert_document`] and [`write_assets`] for multi-file conversions.
pub fn convert(from: FormatId, src: &[u8], to: Target) -> Result<(Vec<u8>, ConvertReport)> {
    let output = convert_document(from, src, to, &ConvertOptions::default())?;
    if !output.assets.is_empty() {
        return Err(ConvertError::Failed(
            "conversion contains image assets; use convert_document to retain them".to_string(),
        ));
    }
    Ok((output.bytes, output.report))
}

/// Write Markdown's relative `assets/` sidecars next to the selected document.
///
/// Sidecar names must be single path components; an existing file is reused only
/// when its bytes match. This avoids both traversal and clobbering another export.
/// Call this before committing the main document so a failure cannot silently
/// produce a document with missing image files.
pub fn write_assets(destination: &std::path::Path, assets: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write};
    use std::path::Component;

    if assets.is_empty() {
        return Ok(());
    }
    for (name, _) in assets {
        let mut components = std::path::Path::new(name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid image asset name"));
        }
    }
    let folder = destination.parent().unwrap_or_else(|| std::path::Path::new(".")).join("assets");
    std::fs::create_dir_all(&folder)?;
    if std::fs::symlink_metadata(&folder)?.file_type().is_symlink() {
        return Err(Error::new(ErrorKind::InvalidInput, "asset directory must not be a symlink"));
    }
    for (name, data) in assets {
        let path = folder.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => file.write_all(data)?,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&path)?;
                if !metadata.is_file() || std::fs::read(&path)? != *data {
                    return Err(Error::new(ErrorKind::AlreadyExists, format!("refusing to overwrite image asset {}", path.display())));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Convert through the IR, keeping side-car assets and honouring [`ConvertOptions`].
pub fn convert_document(
    from: FormatId,
    src: &[u8],
    to: Target,
    options: &ConvertOptions,
) -> Result<ConvertOutput> {
    // Same-format conversions must not round-trip: the bytes are already the target container.
    if to.is_same_as(from) {
        return Ok(ConvertOutput {
            bytes: src.to_vec(),
            assets: Vec::new(),
            report: ConvertReport {
                engine: "identity".to_string(),
                lossy: false,
                warnings: Vec::new(),
            },
        });
    }

    let (mut doc, mut warnings) = input::build(from, src)?;
    if options.sanitize {
        let report = sanitize::sanitize(
            &mut doc,
            &SanitizeOptions {
                level: options.level,
                drop_boilerplate: options.drop_boilerplate,
                preserve_typography: options.preserve_typography,
            },
        );
        let mut sanitize_report = ConvertReport::default();
        sanitize::merge_into(&report, &mut sanitize_report);
        warnings.extend(sanitize_report.warnings);
    }

    let rendered = match to {
        Target::Markdown => export::markdown::render(&doc),
        Target::Text => export::text::render(&doc),
        Target::Docx => export::docx::render(&doc),
        Target::Pdf => export::pdf::render(&doc),
    };
    if rendered.bytes.is_empty() {
        return Err(ConvertError::Failed(format!(
            "the {} exporter produced no output for a {} input",
            rendered.engine,
            from.as_str()
        )));
    }
    warnings.extend(rendered.warnings);
    Ok(ConvertOutput {
        bytes: rendered.bytes,
        assets: rendered.assets,
        report: ConvertReport {
            engine: rendered.engine.to_string(),
            lossy: true,
            warnings,
        },
    })
}

/// Markdown parser options for the converters — deliberately WITHOUT smart-punctuation, so
/// converting cleaned text never re-introduces the curly quotes / dashes we just removed.
pub(crate) fn md_options() -> pulldown_cmark::Options {
    use pulldown_cmark::Options;
    let mut o = Options::empty();
    o.insert(Options::ENABLE_TABLES);
    o.insert(Options::ENABLE_STRIKETHROUGH);
    o.insert(Options::ENABLE_FOOTNOTES);
    o.insert(Options::ENABLE_TASKLISTS);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_sidecars_are_written_without_overwriting_existing_files() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("cleanup-assets-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let destination = root.join("report.md");
        let assets = vec![("figure.png".to_string(), vec![1, 2, 3])];
        write_assets(&destination, &assets).unwrap();
        write_assets(&destination, &assets).unwrap();
        assert_eq!(std::fs::read(root.join("assets/figure.png")).unwrap(), [1, 2, 3]);
        assert!(write_assets(&destination, &[("figure.png".to_string(), vec![4])]).is_err());
        assert!(write_assets(&destination, &[("../escape.png".to_string(), vec![4])]).is_err());
        assert_eq!(std::fs::read(root.join("assets/figure.png")).unwrap(), [1, 2, 3]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_supported_input_converts_to_every_output_target() {
        let sources = [
            (
                FormatId::PlainText,
                "Bonjour, français ! ¡Hola, español!".as_bytes().to_vec(),
            ),
            (
                FormatId::Markdown,
                "# Bonjour\n\n¡Hola, mundo!".as_bytes().to_vec(),
            ),
        ];
        let docx = to_docx::convert(FormatId::PlainText, b"DOCX source fixture")
            .expect("DOCX fixture")
            .0;
        let mut sources = sources.to_vec();
        sources.push((FormatId::Docx, docx));
        let pdf = to_pdf::convert(FormatId::PlainText, b"PDF source fixture")
            .expect("PDF fixture")
            .0;
        sources.push((FormatId::Pdf, pdf));
        sources.push((
            FormatId::Html,
            b"<html><body><h1>Bonjour</h1><p>\xc2\xa1Hola, mundo!</p></body></html>".to_vec(),
        ));

        for (from, bytes) in sources {
            for to in [Target::Markdown, Target::Text, Target::Docx, Target::Pdf] {
                let (converted, report) = convert(from, &bytes, to)
                    .unwrap_or_else(|error| panic!("{from:?} to {to:?}: {error}"));
                assert!(
                    !converted.is_empty(),
                    "{from:?} to {to:?} returned empty output"
                );
                match to {
                    Target::Docx => assert!(converted.starts_with(b"PK")),
                    Target::Pdf => assert!(converted.starts_with(b"%PDF-1.7")),
                    Target::Text | Target::Markdown => {
                        std::str::from_utf8(&converted).expect("text target is UTF-8");
                    }
                }
                if to.is_same_as(from) {
                    assert!(!report.lossy, "identity conversion should remain lossless");
                }
            }
        }
    }
}
