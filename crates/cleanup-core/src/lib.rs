//! `cleanup-core` — the format-agnostic contract for llm-cleanup.
//!
//! The load-bearing idea (see `DESIGN.md` §2): we never reserialize a parsed document.
//! A [`Format`] adapter parses bytes into prose [`ProseSpan`]s that point back into the
//! *original* source, rules edit only that prose, and [`Format::render`] splices the
//! replacements into the original bytes. Everything outside the spans is byte-identical
//! by construction, which is what makes "don't break formatting" provable rather than hoped-for.

pub mod error;
pub mod ir;
pub mod level;
pub mod pipeline;
pub mod splice;
pub mod traits;

pub use error::{CoreError, Result};
pub use ir::{DocMeta, Document, FormatId, Newline, ProseSpan, SpanAnchor};
pub use level::CleanupLevel;
pub use pipeline::{CleanReport, clean};
pub use splice::{render_byte_spliced, splice};
pub use traits::{Ctx, Edit, EditSet, Finding, Format, Rule};
