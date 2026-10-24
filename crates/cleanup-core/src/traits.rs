use crate::error::Result;
use crate::ir::{Document, FormatId, ProseSpan};
use crate::level::CleanupLevel;
use std::ops::Range;

/// Immutable, side-effect-free context handed to every rule. No I/O, no clock, no
/// unseeded randomness — this is what keeps the pipeline reproducible and snapshot-testable.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub level: CleanupLevel,
    /// Hint that this is a preview run. Rules should behave identically; the *caller*
    /// decides whether to write. Kept on the context for future preview-only heuristics.
    pub dry_run: bool,
}

impl Ctx {
    pub fn new(level: CleanupLevel) -> Self {
        Ctx {
            level,
            dry_run: false,
        }
    }
}

/// A single detection result over one prose span.
///
/// `replacement: Some(_)` produces an edit; `None` is flag-only (reported, never changed).
/// Flag-only is the honest default for anything that can't be rewritten without risking meaning.
#[derive(Debug, Clone)]
pub struct Finding {
    pub rule_id: String,
    pub span_index: usize,
    /// Byte range *within `span.text`* (must fall on char boundaries).
    pub range: Range<usize>,
    pub message: String,
    pub replacement: Option<String>,
}

/// A resolved edit, expressed against a span's local text range.
#[derive(Debug, Clone)]
pub struct Edit {
    pub span_index: usize,
    pub range: Range<usize>,
    pub replacement: String,
    /// The rule that produced this edit (for the per-rule change summary).
    pub rule_id: String,
}

#[derive(Debug, Default)]
pub struct EditSet {
    pub edits: Vec<Edit>,
}

/// A document-level (container) finding that is not tied to any prose span — e.g. a
/// fingerprint in DOCX package metadata (`docProps/core.xml`) identifying the tool that
/// generated the file. Container findings are always reported; the `scrubbable` ones are
/// additionally *removed* at [`CleanupLevel::Aggressive`] via [`Format::scrub_container`].
#[derive(Debug, Clone)]
pub struct ContainerFinding {
    pub rule_id: String,
    pub message: String,
    /// Whether [`Format::scrub_container`] knows how to remove this fingerprint.
    pub scrubbable: bool,
    /// Whether the pipeline actually scrubbed it (Aggressive only). Set by the pipeline;
    /// format adapters always construct findings with `applied: false`.
    pub applied: bool,
}

/// A format adapter. Parses bytes into prose spans and renders edits back by **splicing
/// into the original bytes** — never by reserializing a parsed tree.
pub trait Format {
    fn id(&self) -> FormatId;

    /// Parse bytes into prose spans. MUST classify every byte as either prose (→ a span)
    /// or structure (→ excluded, hence frozen).
    fn parse(&self, bytes: &[u8]) -> Result<Document>;

    /// Apply edits and produce final bytes. An empty `EditSet` MUST yield bytes identical
    /// to `doc.source`.
    fn render(&self, doc: &Document, edits: &EditSet) -> Result<Vec<u8>>;

    /// A canonical fingerprint of *structure only* (prose text excluded), used by the
    /// pipeline's post-edit self-check to prove no formatting changed.
    fn skeleton(&self, bytes: &[u8]) -> Result<String>;

    /// Container-level fingerprints outside the prose (file/package metadata).
    /// Best-effort and purely advisory: return an empty vec when none are found or the
    /// container has no inspectable metadata. Default: none.
    fn container_findings(&self, _doc: &Document) -> Vec<ContainerFinding> {
        Vec::new()
    }

    /// Remove the scrubbable [`ContainerFinding`]s from already-rendered bytes, returning
    /// the scrubbed bytes, or `None` when nothing was changed. The pipeline calls this
    /// only at [`CleanupLevel::Aggressive`] and only when scrubbable findings exist. The
    /// scrub must not touch prose — the structural skeleton is (deliberately) blind to
    /// metadata, so honesty here is by construction of each adapter. Default: no-op.
    fn scrub_container(&self, _doc: &Document, _rendered: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

/// A detection rule. Format-agnostic: it only ever sees prose text.
pub trait Rule {
    fn id(&self) -> &str;
    fn min_level(&self) -> CleanupLevel;
    fn detect(&self, span_index: usize, span: &ProseSpan, cx: &Ctx) -> Vec<Finding>;
}
