use crate::error::{CoreError, Result};
use crate::ir::Document;
use crate::traits::{Ctx, Edit, EditSet, Finding, Format, Rule};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct CleanReport {
    /// Every finding produced (both edits and flags).
    pub findings: Vec<Finding>,
    /// Flag-only findings (no replacement) — surfaced for the user to review.
    pub flags: Vec<Finding>,
    /// Number of edits actually applied after conflict resolution.
    pub edits_applied: usize,
    /// Applied-edit counts grouped by rule id (sorted), for the change summary.
    pub applied_by_rule: Vec<(String, usize)>,
    /// Flag-only counts grouped by rule id (sorted).
    pub flagged_by_rule: Vec<(String, usize)>,
    /// Whether the output differs from the input.
    pub changed: bool,
}

/// Aggregate rule ids into sorted `(rule_id, count)` pairs.
fn group_by_rule<'a>(ids: impl Iterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut map: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *map.entry(id).or_default() += 1;
    }
    map.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// Run detection + transformation over a parsed document and return cleaned bytes + a report.
///
/// Guarantees:
/// - empty edit set ⇒ output is byte-identical to input;
/// - edits never overlap (deterministic leftmost/first-rule resolution);
/// - the output's structural skeleton equals the input's, or we abort with
///   [`CoreError::SelfCheck`] (we never emit a structurally-changed file).
pub fn clean(
    fmt: &dyn Format,
    doc: &Document,
    rules: &[Box<dyn Rule>],
    cx: &Ctx,
) -> Result<(Vec<u8>, CleanReport)> {
    let mut report = CleanReport::default();

    // Phase 1 — detect (read-only).
    let mut findings: Vec<Finding> = Vec::new();
    for (i, span) in doc.spans.iter().enumerate() {
        for rule in rules {
            if !cx.level.includes(rule.min_level()) {
                continue;
            }
            findings.extend(rule.detect(i, span, cx));
        }
    }

    // Partition into edits vs flags.
    let mut edits: Vec<Edit> = Vec::new();
    for f in &findings {
        match &f.replacement {
            // A replacement containing a newline would change line structure; never apply it.
            Some(rep) if !rep.contains(['\n', '\r']) => edits.push(Edit {
                span_index: f.span_index,
                range: f.range.clone(),
                replacement: rep.clone(),
                rule_id: f.rule_id.clone(),
            }),
            _ => report.flags.push(f.clone()),
        }
    }
    report.findings = findings;
    report.flagged_by_rule = group_by_rule(report.flags.iter().map(|f| f.rule_id.as_str()));

    // Phase 2 — resolve conflicts: sort by (span, start), drop overlaps (first wins).
    edits.sort_by(|a, b| {
        a.span_index
            .cmp(&b.span_index)
            .then(a.range.start.cmp(&b.range.start))
    });
    let mut resolved: Vec<Edit> = Vec::new();
    for e in edits {
        if let Some(last) = resolved.last()
            && last.span_index == e.span_index
            && e.range.start < last.range.end
        {
            continue; // overlapping edit on the same span: skip
        }
        resolved.push(e);
    }
    report.edits_applied = resolved.len();
    report.applied_by_rule = group_by_rule(resolved.iter().map(|e| e.rule_id.as_str()));
    // Format-independent: edits exist only when a rule produced a real change. (Comparing
    // output == source is wrong for DOCX, whose output is a freshly re-zipped container.)
    report.changed = !resolved.is_empty();

    if resolved.is_empty() {
        let out = fmt.render(doc, &EditSet::default())?;
        return Ok((out, report));
    }

    let output = fmt.render(doc, &EditSet { edits: resolved })?;

    // Phase 3 — post-edit structural self-check (the keystone safety net).
    let before = fmt.skeleton(&doc.source)?;
    let after = fmt.skeleton(&output)?;
    if before != after {
        return Err(CoreError::SelfCheck(
            "document structure changed during cleaning; aborting and keeping the original"
                .to_string(),
        ));
    }

    Ok((output, report))
}
