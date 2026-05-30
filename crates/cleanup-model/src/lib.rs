//! Optional statistical / ML detection — a clean seam for post-v1 work (DESIGN.md §11).
//!
//! v1 ships with NO bundled model: deterministic rules + heuristics carry the cleaning task,
//! and spaCy (Python) is a non-starter for a single static binary. When enabled, this crate
//! is intended to provide a *report-only* document-level "AI-likelihood" score (e.g. an
//! n-gram perplexity proxy plus sentence-length burstiness), never to drive edits.

/// Document-level AI-likelihood score in `0.0..=1.0`. Returns `None` until the `ml`
/// feature/roadmap item lands.
pub fn ai_likelihood_score(_text: &str) -> Option<f32> {
    None
}
