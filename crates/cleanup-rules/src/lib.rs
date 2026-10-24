//! Detection rules and the declarative template loader.
//!
//! The rule set is layered by *confidence*, and the layering is the honesty mechanism of the
//! project:
//!
//! * [`typographic`] — deterministic, meaning-neutral character normalization. Safe to apply.
//! * [`unicode_hidden`] — invisible characters, smuggling vectors, whitespace steganography,
//!   and homoglyphs. Safe to apply, except where a script genuinely uses the character.
//! * [`markers`] — exact machine artifacts (chat-template tokens, citation plumbing, C2PA text
//!   wrappers, agent trailers, reasoning traces). Reported, and unambiguous serialization
//!   artifacts are removed.
//! * [`style`] — formatting tells that are suggestive at best. Always flag-only.
//! * [`watermark`] — opt-in (`--dewatermark`) deterministic n-gram tournament window disruption
//!   for statistical token watermarks (SynthID-Text, textGrain), dormant on standard runs.
//! * Data-driven rules from `templates/core.yaml` (flag-only by default) and user `--pack` files.
//!
//! [`catalog`] records which model families each layer covers and when it was reviewed.

pub mod catalog;
pub mod lexicon;
pub mod markers;
pub mod registry;
pub mod repair;
pub mod schema;
pub mod style;
pub mod typographic;
pub mod unicode_hidden;
pub mod watermark;

pub use catalog::{CATALOG_REVIEWED, OUT_OF_SCOPE, SignatureFamily, coverage};
pub use registry::{
    builtin_rules, load_builtin_pack, load_pack_str, rules_from_pack, rules_with_packs,
};
pub use schema::{Action, Category, Pack, RuleSpec};
pub use watermark::{DisruptionMetrics, WatermarkDisruptRule, measure_disruption};
