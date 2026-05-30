//! `aiclean-gui` — a polished native desktop app over `cleanup-core`, built with Slint.
//! Choose a file, pick a level and an output format, see how many AI fingerprints were cleaned,
//! save. Thin frontend: all work is done by the same core/rules/convert as the CLI. (Native OS
//! file-drop is still experimental in Slint 1.16, so the drop card opens a file picker on click.)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use cleanup_convert::Target;
use cleanup_core::{CleanupLevel, Ctx, clean};
use cleanup_formats::formatter_for_path;
use cleanup_rules::builtin_rules;

slint::include_modules!();

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

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    register_fonts();
    let state = Rc::new(RefCell::new(State::default()));

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_browse(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("documents", &["md", "markdown", "txt", "docx"])
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
            let (data, suggested) = {
                let s = state.borrow();
                let Some(data) = s.cleaned.clone() else {
                    return;
                };
                let stem = s
                    .file
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .and_then(|x| x.to_str())
                    .map(|x| format!("{x}.cleaned"))
                    .unwrap_or_else(|| "cleaned".to_string());
                (data, format!("{stem}.{}", s.out_ext))
            };
            if let Some(path) = rfd::FileDialog::new().set_file_name(suggested).save_file()
                && let Some(ui) = ui_weak.upgrade()
            {
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
        if report.applied_by_rule.is_empty() && report.flags.is_empty() {
            detail.push_str("No AI fingerprints found. Your file is already clean.\n");
        } else {
            if !report.applied_by_rule.is_empty() {
                detail.push_str("Removed / normalized:\n");
                for (rule, n) in &report.applied_by_rule {
                    detail.push_str(&format!("    {n} ×  {rule}\n"));
                }
            }
            if !report.flagged_by_rule.is_empty() {
                detail.push_str("\nFlagged for your review (left unchanged):\n");
                for (rule, n) in &report.flagged_by_rule {
                    detail.push_str(&format!("    {n} ×  {rule}\n"));
                }
            }
        }

        // Clean-then-convert (only when the chosen target differs from the source format).
        let src_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
            .to_string();
        let target = target_from_idx(target_idx).filter(|t| !t.is_same_as(fmt.id()));
        let (final_bytes, out_ext) = if let Some(t) = target {
            let (b, rep) = cleanup_convert::convert(fmt.id(), &out, t)?;
            detail.push_str(&format!(
                "\nConvert to {} (via {}):\n",
                t.label(),
                rep.engine
            ));
            if rep.warnings.is_empty() {
                detail.push_str("    formatting preserved\n");
            }
            for w in &rep.warnings {
                detail.push_str(&format!("    ⚠ {w}\n"));
            }
            (b, t.ext().to_string())
        } else {
            (out, src_ext)
        };

        Ok((
            final_bytes,
            out_ext,
            report.edits_applied,
            report.flags.len(),
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
            ui.set_cleaned_count(cleaned as i32);
            ui.set_flagged_count(flagged as i32);
            ui.set_preview_text(detail.into());
            ui.set_can_save(true);
            ui.set_status("Ready to save.".into());
        }
        Err(e) => {
            state.borrow_mut().cleaned = None;
            ui.set_did_run(true);
            ui.set_cleaned_count(0);
            ui.set_flagged_count(0);
            ui.set_preview_text(format!("Could not process this file:\n{e}").into());
            ui.set_can_save(false);
            ui.set_status("Error".into());
        }
    }
}
