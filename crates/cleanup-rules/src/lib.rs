//! Detection rules and the (declarative) template loader.
//!
//! Typographic normalization is implemented as built-in **code** rules (deterministic,
//! Unicode-exact). Lexical/phrasal rules are **data-driven**: the engine constructs them
//! from a YAML/JSON template pack at load time. See `DESIGN.md` §6–§7.

pub mod lexicon;
pub mod registry;
pub mod schema;
pub mod typographic;
pub mod unicode_hidden;

pub use registry::{builtin_rules, load_builtin_pack};
pub use schema::{Action, Category, Pack, RuleSpec};
