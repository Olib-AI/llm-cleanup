//! Detection rules and the (declarative) template loader.
//!
//! The rule set is layered by *confidence*, and the layering is the honesty mechanism of the
//! project:
//!
//! * [`typographic`] — deterministic, meaning-neutral character normalization. Safe to apply.
//! * [`unicode_hidden`] — invisible characters, smuggling vectors and homoglyphs. Safe to
//!   apply, except where a script genuinely uses the character (documented per rule).
//! * [`markers`] — exact machine artifacts (chat-template tokens, citation plumbing, agent
//!   trailers, reasoning traces). Reported, and only the unambiguous serialization tokens are
//!   removed.
//! * [`style`] — formatting tells that are suggestive at best. Always flag-only.
//! * Data-driven lexical/phrasal rules from `templates/core.yaml` — always flag-only: single
//!   "AI words" are ordinary English and rewriting them would change what the author said.
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

pub use catalog::{CATALOG_REVIEWED, OUT_OF_SCOPE, SignatureFamily, coverage};
pub use registry::{builtin_rules, load_builtin_pack};
pub use schema::{Action, Category, Pack, RuleSpec};
