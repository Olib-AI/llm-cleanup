//! LLM Cleanup by Olib AI (`aiclean-gui`) — a polished native desktop app over
//! `cleanup-core`, built with Slint.
//! Choose a file, pick a level and an output format, see how many AI fingerprints were cleaned,
//! save. Thin frontend: all work is done by the same core/rules/convert as the CLI. (Native OS
//! file-drop is still experimental in Slint 1.16, so the drop card opens a file picker on click.)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use cleanup_convert::Target;
use cleanup_core::{CleanupLevel, Ctx, clean, cleaned_sibling_path, ensure_output_extension};
use cleanup_formats::formatter_for_path;
use cleanup_rules::builtin_rules;

slint::include_modules!();

/// User-visible application name (window title, bundle display name).
pub const APP_NAME: &str = "LLM Cleanup by Olib AI";

/// Resolve the path a cleaned file is really written to. The native save dialog does
/// not guarantee an extension on every platform (and a user can delete or stale-edit
/// the suggested one), so the chosen path is always reconciled with the extension of
/// the format being written before any bytes touch disk.
fn resolve_save_path(chosen: &std::path::Path, out_ext: &str) -> PathBuf {
    ensure_output_extension(chosen, out_ext)
}

#[derive(Default)]
struct State {
    file: Option<PathBuf>,
    level: CleanupLevel,
    target_idx: i32,
    cleaned: Option<Vec<u8>>,
    out_ext: String,
}

fn level_from_i32(n: i32) -> CleanupLevel {
    match n {
        0 => CleanupLevel::Light,
        2 => CleanupLevel::Aggressive,
        _ => CleanupLevel::Standard,
    }
}

fn level_to_i32(l: CleanupLevel) -> i32 {
    match l {
        CleanupLevel::Light => 0,
        CleanupLevel::Standard => 1,
        CleanupLevel::Aggressive => 2,
    }
}

/// Map the "Convert to" combo index to a target format (0 = same as input).
fn target_from_idx(n: i32) -> Option<Target> {
    match n {
        1 => Some(Target::Markdown),
        2 => Some(Target::Text),
        3 => Some(Target::Docx),
        4 => Some(Target::Pdf),
        _ => None,
    }
}

/// Rule IDs are useful in the CLI and logs, but make a noisy consumer-facing report.
/// Keep the desktop report in plain language while retaining a readable fallback for
/// data-driven rules added to the built-in pack.
fn friendly_rule_name(rule_id: &str) -> String {
    let known = match rule_id {
        "typo.mojibake.repair" => "Encoding damage",
        "typo.html-entities.decode" => "HTML entities",
        "typo.invisibles.strip" => "Invisible characters",
        "unicode.ansi.strip" => "Terminal escape sequences",
        "unicode.controls.strip" => "Control characters",
        "unicode.smuggled.strip" => "Smuggled Unicode characters",
        "unicode.fillers.strip" => "Invisible filler glyphs",
        "unicode.spaces.normalize" => "Unusual spaces",
        "unicode.bidi.strip-overrides" => "Text-direction overrides",
        "unicode.bidi.flag" => "Text-direction controls",
        "unicode.private-use.flag" | "unicode.private-use.strip" => "Private-use characters",
        "unicode.combining.flag" => "Stacked combining marks",
        "unicode.zero-width.flag" | "unicode.zero-width.strip" => "Zero-width joiners",
        "unicode.whitespace-stego.flag" | "unicode.whitespace-stego.strip" => {
            "Steganographic trailing whitespace"
        }
        "unicode.math-styled.flag" | "unicode.math-styled.normalize" => {
            "Mathematical styled letters"
        }
        "unicode.confusables.flag" => "Mixed-script lookalikes",
        "watermark.ngram-disrupt" => "Statistical watermark windows",
        "typo.quotes.straighten" => "Curly quotes and apostrophes",
        "typo.ligatures.expand" => "Typographic ligatures",
        "typo.ellipsis.expand" => "Ellipsis characters",
        "typo.hyphens.normalize" => "Special hyphens",
        "typo.en-dash.to-hyphen" => "En dashes",
        "typo.em-dash.to-comma" => "Em dashes",
        "typo.fullwidth.normalize" => "Full-width punctuation",
        "typo.cjk-punctuation.normalize" => "CJK punctuation",
        "typo.detokenizer.normalize" => "Tokenizer spacing artifacts",
        "markers.chat-tokens" | "markers.chat-tokens.strip" => "Chat-template tokens",
        "markers.chat-tokens.pattern" => "Unrecognized chat tokens",
        "markers.citation-artifacts" => "Citation artifacts",
        "markers.citation-brackets" | "markers.citation-brackets.strip" => "Citation markers",
        "markers.private-use-citations" | "markers.private-use-citations.strip" => {
            "Private-use citation markers"
        }
        "markers.turn-citations" | "markers.turn-citations.strip" => "Citation identifiers",
        "markers.gemini-citations" | "markers.gemini-citations.strip" => {
            "Gemini grounding citations"
        }
        "markers.c2pa-text-manifest" => "Invisible C2PA text manifests",
        "markers.c2pa-structured-manifest" | "markers.c2pa-structured-manifest.strip" => {
            "C2PA structured-text manifests"
        }
        "markers.ai-tracking-urls" | "markers.ai-tracking-urls.strip" => "AI tracking parameters",
        "markers.legacy-citations.strip" => "Legacy citation markers",
        "markers.claude-prompt-fingerprint" => "Claude Code prompt fingerprints",
        "markers.agent-trailers" | "markers.agent-trailer-lines" => "AI attribution trailers",
        "markers.tool-calls" => "Tool-call scaffolding",
        "markers.reasoning-block" | "markers.reasoning-trace" => "Reasoning traces",
        "markers.model-identifiers" => "Model identifiers",
        "markers.chat-ui-disclaimer" | "markers.chat-ui-disclaimer.strip" => "Chat UI disclaimers",
        "markers.chat-ui-residue" => "Chat interface residue",
        "markers.html-tags" => "Raw HTML tags",
        "container.pdf-info-metadata" => "PDF document information",
        "container.pdf-xmp-metadata" => "PDF XMP metadata",
        "container.pdf-embedded-image" => "Embedded image provenance",
        "container.pdf-undecodable-text" => "Text without a Unicode map",
        "container.pdf-no-extractable-text" => "Scanned or image-only pages",
        "container.pdf-signature" => "Digital signature",
        _ => {
            let mut parts: Vec<&str> = rule_id.split('.').skip(1).collect();
            if parts.last().is_some_and(|part| {
                matches!(
                    *part,
                    "strip" | "flag" | "normalize" | "repair" | "decode" | "expand"
                )
            }) {
                parts.pop();
            }
            let mut label = parts.join(" ").replace(['-', '_'], " ");
            if let Some(first) = label.chars().next() {
                let uppercase = first.to_uppercase().to_string();
                label.replace_range(..first.len_utf8(), &uppercase);
            }
            return label;
        }
    };
    known.to_string()
}

fn main() -> Result<(), slint::PlatformError> {
    // Slint 1.16 asks winit for a transparent window on every platform. On macOS that
    // also makes the native title bar transparent, leaving the traffic lights over the
    // desktop. This app is fully opaque, so keep both its content and native frame opaque.
    #[cfg(target_os = "macos")]
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attributes| attributes.with_transparent(false))
        .select()?;

    let ui = AppWindow::new()?;
    // Show the workspace version (Cargo.toml) in the header; the Slint default is a
    // fallback for design-time previews only, so the two can never drift silently.
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let _ = APP_NAME;
    register_fonts();
    let state = Rc::new(RefCell::new(State::default()));

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_browse(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "documents & media",
                    &[
                        "md", "markdown", "txt", "html", "htm", "docx", "pdf", "png", "jpg",
                        "jpeg", "webp", "svg",
                    ],
                )
                .pick_file()
            {
                state.borrow_mut().file = Some(path);
                refresh(&ui_weak, &state);
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_set_level(move |n| {
            state.borrow_mut().level = level_from_i32(n);
            refresh(&ui_weak, &state);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_set_target(move |n| {
            state.borrow_mut().target_idx = n;
            refresh(&ui_weak, &state);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_clean_and_save(move || {
            let (data, suggested, out_ext) = {
                let s = state.borrow();
                let Some(data) = s.cleaned.clone() else {
                    return;
                };
                let suggested = match &s.file {
                    Some(input) => cleaned_sibling_path(input, &s.out_ext)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("cleaned.{}", s.out_ext)),
                    None => format!("cleaned.{}", s.out_ext),
                };
                (data, suggested, s.out_ext.clone())
            };
            // Offer the filter as a hint, but never trust the dialog (or the user's
            // edit of the name) to keep the extension: `resolve_save_path` appends a
            // missing extension and corrects a stale one before we write.
            let Some(chosen) = rfd::FileDialog::new()
                .add_filter("cleaned file", &[out_ext.as_str()])
                .set_file_name(suggested)
                .save_file()
            else {
                return;
            };
            let path = resolve_save_path(&chosen, &out_ext);
            if let Some(ui) = ui_weak.upgrade() {
                let msg = match std::fs::write(&path, &data) {
                    Ok(()) => format!("Saved to {}", path.display()),
                    Err(e) => format!("Save failed: {e}"),
                };
                ui.set_status(msg.into());
            }
        });
    }

    ui.run()
}

/// Embed the Inter font so the app looks identical on every OS. Slint 1.16 registers fonts
/// through the fontique collection (gated behind the `unstable-fontique-08` feature); this must
/// run after the backend is initialized by `AppWindow::new()`.
fn register_fonts() {
    use slint::fontique_08::fontique;
    let mut collection = slint::fontique_08::shared_collection();
    for bytes in [
        include_bytes!("../fonts/Inter-Regular.ttf").as_slice(),
        include_bytes!("../fonts/Inter-SemiBold.ttf").as_slice(),
        include_bytes!("../fonts/Inter-Bold.ttf").as_slice(),
        include_bytes!("../fonts/Inter-ExtraBold.ttf").as_slice(),
    ] {
        let blob = fontique::Blob::new(std::sync::Arc::new(bytes.to_vec()));
        let _ = collection.register_fonts(blob, None);
    }
}

/// Run the pipeline (clean, then optionally convert) on the current file and update the UI.
fn refresh(ui_weak: &slint::Weak<AppWindow>, state: &Rc<RefCell<State>>) {
    let Some(ui) = ui_weak.upgrade() else {
        return;
    };
    let (file, level, target_idx) = {
        let s = state.borrow();
        (s.file.clone(), s.level, s.target_idx)
    };
    let Some(path) = file else {
        return;
    };

    ui.set_file_name(
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .into(),
    );
    ui.set_has_file(true);
    ui.set_level(level_to_i32(level));
    ui.set_target(target_idx);

    let result = (|| -> anyhow::Result<(Vec<u8>, String, usize, usize, String)> {
        let fmt = formatter_for_path(&path)?;
        let bytes = std::fs::read(&path)?;
        let doc = fmt.parse(&bytes)?;
        let rules = builtin_rules();
        let cx = Ctx::new(level);
        let (out, report) = clean(fmt.as_ref(), &doc, &rules, &cx)?;

        let mut detail = String::new();
        if report.applied_by_rule.is_empty()
            && report.flags.is_empty()
            && report.container.is_empty()
        {
            detail.push_str(
                "Everything looks clean.\n\nNo supported AI fingerprints were found in this document. Your copy is ready to save.\n",
            );
        } else {
            if !report.applied_by_rule.is_empty() {
                detail.push_str("Removed & normalized\n\n");
                for (rule, n) in &report.applied_by_rule {
                    detail.push_str(&format!("  {n}  {}\n", friendly_rule_name(rule)));
                }
            }
            if !report.flagged_by_rule.is_empty() {
                detail.push_str("\nLeft for your review\n\n");
                for (rule, n) in &report.flagged_by_rule {
                    detail.push_str(&format!("  {n}  {}\n", friendly_rule_name(rule)));
                }
                detail.push_str("\nThese items were not changed.\n");
            }
            if !report.container.is_empty() {
                detail.push_str("\nDocument metadata\n\n");
                for c in &report.container {
                    detail.push_str(&format!(
                        "  {} {}\n",
                        if c.applied { "✓" } else { "•" },
                        c.message
                    ));
                }
                if report.container.iter().any(|c| c.scrubbable && !c.applied) {
                    detail.push_str("\nChoose Aggressive to scrub the marked fields.\n");
                }
            }
        }

        // Clean-then-convert (only when the chosen target differs from the source format).
        let src_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
            .to_ascii_lowercase();
        let target = target_from_idx(target_idx).filter(|t| !t.is_same_as(fmt.id()));
        let (final_bytes, out_ext) = if let Some(t) = target {
            let (b, rep) = cleanup_convert::convert(fmt.id(), &out, t)?;
            detail.push_str(&format!(
                "\nOutput\n\nConverted to {} using {}.\n",
                t.label(),
                rep.engine
            ));
            if rep.warnings.is_empty() {
                detail.push_str("Formatting preserved.\n");
            }
            for w in &rep.warnings {
                detail.push_str(&format!("Review: {w}\n"));
            }
            (b, t.ext().to_string())
        } else {
            (out, src_ext)
        };

        Ok((
            final_bytes,
            out_ext,
            report.edits_applied + report.container.iter().filter(|c| c.applied).count(),
            report.flags.len() + report.container.iter().filter(|c| !c.applied).count(),
            detail,
        ))
    })();

    match result {
        Ok((bytes, out_ext, cleaned, flagged, detail)) => {
            {
                let mut s = state.borrow_mut();
                s.cleaned = Some(bytes);
                s.out_ext = out_ext;
            }
            ui.set_did_run(true);
            ui.set_has_error(false);
            ui.set_cleaned_count(cleaned as i32);
            ui.set_flagged_count(flagged as i32);
            ui.set_preview_text(detail.into());
            ui.set_can_save(true);
            ui.set_status("Ready to save · your original file stays untouched.".into());
        }
        Err(e) => {
            state.borrow_mut().cleaned = None;
            ui.set_did_run(true);
            ui.set_has_error(true);
            ui.set_cleaned_count(0);
            ui.set_flagged_count(0);
            ui.set_preview_text(format!("{e}").into());
            ui.set_can_save(false);
            ui.set_status(
                "Could not process this file. Choose another document to try again.".into(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{APP_NAME, friendly_rule_name, resolve_save_path};
    use std::path::Path;

    #[test]
    fn report_labels_hide_internal_rule_ids() {
        assert_eq!(
            friendly_rule_name("markers.claude-prompt-fingerprint"),
            "Claude Code prompt fingerprints"
        );
        assert_eq!(
            friendly_rule_name("lexical.overused-verbs"),
            "Overused verbs"
        );
        assert_eq!(
            friendly_rule_name("unicode.spaces.normalize"),
            "Unusual spaces"
        );
    }

    #[test]
    fn app_is_named_and_save_paths_always_carry_the_output_extension() {
        assert_eq!(APP_NAME, "LLM Cleanup by Olib AI");
        assert_eq!(
            resolve_save_path(Path::new("report"), "pdf"),
            Path::new("report.pdf")
        );
        assert_eq!(
            resolve_save_path(Path::new("report.cleaned"), "docx"),
            Path::new("report.cleaned.docx")
        );
        assert_eq!(
            resolve_save_path(Path::new("report.docx"), "md"),
            Path::new("report.md")
        );
        assert_eq!(
            resolve_save_path(Path::new("Report.PDF"), "pdf"),
            Path::new("Report.PDF")
        );
    }
}
