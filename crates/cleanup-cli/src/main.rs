//! `aiclean` — strip AI-generated fingerprints from documents without breaking formatting,
//! and (optionally) convert the cleaned result to another format.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use cleanup_convert::Target;
use cleanup_core::{
    CleanupLevel, Ctx, Document, FormatId, clean, cleaned_sibling_path, ensure_output_extension,
};
use cleanup_formats::{format_for_path, formatter_for_path};
use cleanup_rules::{Pack, load_pack_str, measure_disruption, rules_with_packs};
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "aiclean",
    version,
    author = "Olib AI <dev@olib.ai>",
    about = "Strip AI fingerprints and C2PA provenance from Markdown, text, HTML, DOCX, PDF, and images.",
    after_help = "llm-cleanup by Olib AI (https://www.olib.ai)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Clean one or more files (and optionally convert the result to another format).
    Clean {
        /// Input files (.md, .txt, .html, .docx, .pdf, .png, .jpg, .webp, .svg).
        inputs: Vec<PathBuf>,
        #[arg(short, long, value_enum, default_value = "standard")]
        level: LevelArg,
        /// Write to this path (single input only). Its extension can also set the output format.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Overwrite the input file in place (atomic).
        #[arg(long)]
        in_place: bool,
        /// With --in-place, write a .bak copy first.
        #[arg(long)]
        backup: bool,
        /// Compute changes but write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print a unified diff of the changes.
        #[arg(long)]
        diff: bool,
        /// Opt-in deterministic n-gram window disruption for statistical token watermarks (SynthID-Text, textGrain).
        #[arg(long)]
        dewatermark: bool,
        /// Additional YAML/JSON rule pack(s) to load alongside built-in rules.
        #[arg(long = "pack")]
        packs: Vec<PathBuf>,
        /// Also convert the cleaned file to this format (md|txt|docx|pdf). Inferred from -o if omitted.
        #[arg(long)]
        to: Option<String>,
    },
    /// Convert a file to another format without cleaning (md|txt|docx|pdf output).
    Convert {
        /// Input file(s).
        inputs: Vec<PathBuf>,
        /// Output path (single input). Its extension sets the target format.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Target format (md|txt|docx|pdf), if not given via -o.
        #[arg(long)]
        to: Option<String>,
    },
    /// Show what would change (always dry-run, prints a diff).
    Diff {
        inputs: Vec<PathBuf>,
        #[arg(short, long, value_enum, default_value = "standard")]
        level: LevelArg,
        /// Include opt-in statistical watermark window disruption in the preview diff.
        #[arg(long)]
        dewatermark: bool,
        /// Additional YAML/JSON rule pack(s) to load alongside built-in rules.
        #[arg(long = "pack")]
        packs: Vec<PathBuf>,
    },
    /// List the active rules at a given level.
    Rules {
        #[arg(short, long, value_enum, default_value = "standard")]
        level: LevelArg,
        /// Additional YAML/JSON rule pack(s) to include.
        #[arg(long = "pack")]
        packs: Vec<PathBuf>,
    },
    /// Show which AI models and fingerprints this build covers.
    Coverage {
        /// Only show families whose id or provider name contains this text.
        #[arg(short, long)]
        provider: Option<String>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum LevelArg {
    Light,
    Standard,
    Aggressive,
}

impl From<LevelArg> for CleanupLevel {
    fn from(l: LevelArg) -> Self {
        match l {
            LevelArg::Light => CleanupLevel::Light,
            LevelArg::Standard => CleanupLevel::Standard,
            LevelArg::Aggressive => CleanupLevel::Aggressive,
        }
    }
}

/// One clean (+ optional convert) request.
struct Job<'a> {
    inputs: &'a [PathBuf],
    level: CleanupLevel,
    output: Option<PathBuf>,
    in_place: bool,
    backup: bool,
    dry_run: bool,
    diff: bool,
    dewatermark: bool,
    packs: &'a [PathBuf],
    to: Option<String>,
}

fn load_extra_packs(paths: &[PathBuf]) -> Result<Vec<Pack>> {
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading rule pack {}", path.display()))?;
        let pack = load_pack_str(&raw)
            .map_err(|e| anyhow::anyhow!("invalid rule pack {}: {e}", path.display()))?;
        out.push(pack);
    }
    Ok(out)
}

fn joined_prose(doc: &Document) -> String {
    doc.spans
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Clean {
            inputs,
            level,
            output,
            in_place,
            backup,
            dry_run,
            diff,
            dewatermark,
            packs,
            to,
        } => {
            if inputs.is_empty() {
                bail!("no input files given");
            }
            if output.is_some() && inputs.len() > 1 {
                bail!("--output can only be used with a single input file");
            }
            run_clean(Job {
                inputs: &inputs,
                level: level.into(),
                output,
                in_place,
                backup,
                dry_run,
                diff,
                dewatermark,
                packs: &packs,
                to,
            })
        }
        Command::Convert { inputs, output, to } => {
            if inputs.is_empty() {
                bail!("no input files given");
            }
            if output.is_some() && inputs.len() > 1 {
                bail!("--output can only be used with a single input file");
            }
            run_convert(&inputs, output, to)
        }
        Command::Diff {
            inputs,
            level,
            dewatermark,
            packs,
        } => run_clean(Job {
            inputs: &inputs,
            level: level.into(),
            output: None,
            in_place: false,
            backup: false,
            dry_run: true,
            diff: true,
            dewatermark,
            packs: &packs,
            to: None,
        }),
        Command::Rules { level, packs } => run_rules(level.into(), &packs),
        Command::Coverage { provider } => {
            run_coverage(provider.as_deref());
            Ok(())
        }
    }
}

fn run_clean(job: Job) -> Result<()> {
    let Job {
        inputs,
        level,
        output,
        in_place,
        backup,
        dry_run,
        diff,
        dewatermark,
        packs,
        to,
    } = job;
    let extra_packs = load_extra_packs(packs)?;
    let rules = rules_with_packs(&extra_packs);

    for input in inputs {
        let fmt = formatter_for_path(input)
            .with_context(|| format!("unsupported file: {}", input.display()))?;
        let bytes = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
        let doc = fmt
            .parse(&bytes)
            .with_context(|| format!("parsing {}", input.display()))?;

        let cx = Ctx::new(level).with_dewatermark(dewatermark);
        let (output_bytes, report) = clean(fmt.as_ref(), &doc, &rules, &cx)
            .with_context(|| format!("cleaning {}", input.display()))?;

        // Resolve an optional conversion target (from --to, else from the -o extension).
        let target = resolve_target(to.as_deref(), output.as_deref())?;
        let converting = target.is_some_and(|t| !t.is_same_as(fmt.id()));

        // Change summary.
        println!("{}  (level: {level})", input.display());
        if report.edits_applied == 0 && !report.changed {
            println!("  ✓ no AI fingerprints removed");
        } else if report.edits_applied == 0 {
            println!("  ✓ no prose fingerprints removed (metadata only — see below)");
        } else {
            println!(
                "  ✓ cleaned {} AI fingerprint{}:",
                report.edits_applied,
                plural(report.edits_applied)
            );
            for (rule, n) in &report.applied_by_rule {
                println!("      {n:>3} ×  {rule}");
            }
        }
        if !report.flags.is_empty() {
            println!(
                "  ⚑ flagged {} item{} for review (left unchanged):",
                report.flags.len(),
                plural(report.flags.len())
            );
            for (rule, n) in &report.flagged_by_rule {
                println!("      {n:>3} ×  {rule}");
            }
        }
        if !report.container.is_empty() {
            println!("  ⚑ document metadata fingerprints:");
            for c in &report.container {
                if c.applied {
                    println!("      ✓ scrubbed: {}", c.message);
                } else {
                    println!("      ⚠ {}", c.message);
                }
            }
            if report.container.iter().any(|c| c.scrubbable && !c.applied) {
                println!("      (rerun with --level aggressive to scrub the fields above)");
            }
        }
        if dewatermark && !doc.spans.is_empty() {
            let before_prose = joined_prose(&doc);
            if let Ok(after_doc) = fmt.parse(&output_bytes) {
                let after_prose = joined_prose(&after_doc);
                let m = measure_disruption(&before_prose, &after_prose);
                if m.windows_total > 0 {
                    println!(
                        "  ✦ watermark disruption: {}/{} 5-gram windows invalidated ({:.1}%), bigram divergence {:.1}%",
                        m.windows_broken,
                        m.windows_total,
                        m.window_disruption_rate * 100.0,
                        m.bigram_divergence * 100.0
                    );
                    println!(
                        "      (degrades SynthID-Text / textGrain window scoring; offline tools cannot certify keyed removal)"
                    );
                }
            }
        }

        if diff {
            print_diff(&bytes, &output_bytes);
        }
        if dry_run {
            continue;
        }

        // Same format and nothing changed (no prose edits, no metadata scrub):
        // don't re-save an identical file.
        if !converting && !report.changed {
            println!("  → file is already clean, nothing written");
            continue;
        }

        // Clean-then-convert: apply the conversion to the cleaned native bytes.
        let (final_bytes, assets, conv) =
            if let Some(t) = target.filter(|t| !t.is_same_as(fmt.id())) {
                let converted = cleanup_convert::convert_document(
                    fmt.id(),
                    &output_bytes,
                    t,
                    &cleanup_convert::ConvertOptions {
                        level,
                        ..cleanup_convert::ConvertOptions::default()
                    },
                )
                .with_context(|| format!("converting {} to {}", input.display(), t.label()))?;
                (
                    converted.bytes,
                    converted.assets,
                    Some((t, converted.report)),
                )
            } else {
                (output_bytes, Vec::new(), None)
            };

        if in_place && converting {
            bail!("--in-place cannot change the file format; use -o <path> instead");
        }

        let dest = if let Some(o) = &output {
            // `-o out` (or a stale `-o out.docx` next to `--to pdf`) must not write
            // extensionless / mislabeled bytes: reconcile the requested path with
            // the extension of the format we are actually saving.
            let expected_ext = match &conv {
                Some((t, _)) => t.ext().to_string(),
                None => source_ext(input, fmt.id()),
            };
            ensure_output_extension(o, &expected_ext)
        } else if in_place {
            input.clone()
        } else if let Some((t, _)) = &conv {
            converted_path(input, t.ext())
        } else {
            cleaned_path(input)
        };

        if in_place && backup {
            let bak = with_extra_extension(input, "bak");
            fs::write(&bak, &bytes).with_context(|| format!("writing backup {}", bak.display()))?;
        }
        cleanup_convert::write_assets(&dest, &assets)
            .with_context(|| format!("writing image assets for {}", dest.display()))?;
        atomic_write(&dest, &final_bytes).with_context(|| format!("writing {}", dest.display()))?;

        if let Some((t, rep)) = &conv {
            println!(
                "  → converted to {} via {} ({})",
                t.label(),
                rep.engine,
                if rep.lossy { "lossy" } else { "lossless" }
            );
            for w in &rep.warnings {
                println!("      ⚠ {w}");
            }
            println!("  → saved {}", dest.display());
        } else {
            println!(
                "  → saved {} ({} change{} applied)",
                dest.display(),
                report.edits_applied,
                plural(report.edits_applied)
            );
        }
    }
    Ok(())
}

fn run_convert(inputs: &[PathBuf], output: Option<PathBuf>, to: Option<String>) -> Result<()> {
    for input in inputs {
        let from = format_for_path(input)
            .ok_or_else(|| anyhow::anyhow!("unsupported input file: {}", input.display()))?;
        let target = resolve_target(to.as_deref(), output.as_deref())?.ok_or_else(|| {
            anyhow::anyhow!("specify a target with -o <file> or --to <md|txt|docx|pdf>")
        })?;
        if target.is_same_as(from) {
            bail!(
                "{} is already {} — nothing to convert",
                input.display(),
                target.label()
            );
        }
        let bytes = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
        let converted = cleanup_convert::convert_document(
            from,
            &bytes,
            target,
            &cleanup_convert::ConvertOptions {
                sanitize: false,
                ..cleanup_convert::ConvertOptions::default()
            },
        )
        .with_context(|| format!("converting {} to {}", input.display(), target.label()))?;
        let rep = converted.report;
        let dest = match &output {
            Some(o) => ensure_output_extension(o, target.ext()),
            None => converted_path(input, target.ext()),
        };
        // Save sidecars first: a failed image write must not leave a seemingly successful
        // Markdown file with broken links.
        cleanup_convert::write_assets(&dest, &converted.assets)
            .with_context(|| format!("writing image assets for {}", dest.display()))?;
        atomic_write(&dest, &converted.bytes)
            .with_context(|| format!("writing {}", dest.display()))?;
        if !converted.assets.is_empty() {
            println!(
                "  {} image asset(s) written to assets/",
                converted.assets.len()
            );
        }
        println!(
            "{} → {}  ({} via {})",
            input.display(),
            dest.display(),
            if rep.lossy { "lossy" } else { "lossless" },
            rep.engine
        );
        for w in &rep.warnings {
            println!("  ⚠ {w}");
        }
    }
    Ok(())
}

fn resolve_target(to: Option<&str>, output: Option<&Path>) -> Result<Option<Target>> {
    if let Some(t) = to {
        return Target::from_ext(t)
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("unknown --to format: {t} (use md|txt|docx|pdf)"));
    }
    if let Some(ext) = output.and_then(|o| o.extension()).and_then(|e| e.to_str()) {
        return Ok(Target::from_ext(ext));
    }
    Ok(None)
}

fn run_rules(level: CleanupLevel, packs: &[PathBuf]) -> Result<()> {
    let extra = load_extra_packs(packs)?;
    let rules = rules_with_packs(&extra);
    println!("Rules (active at level {level} marked [x]):");
    for r in &rules {
        let active = if r.id() == "watermark.ngram-disrupt" {
            '*'
        } else if level.includes(r.min_level()) {
            'x'
        } else {
            ' '
        };
        let note = if r.id() == "watermark.ngram-disrupt" {
            " (opt-in: --dewatermark)"
        } else {
            ""
        };
        println!(
            "  [{active}] {:38} min-level={}{note}",
            r.id(),
            r.min_level()
        );
    }
    Ok(())
}

/// Print the signature catalogue: what we know about, when we last checked, and which rules
/// implement it. This is the user-facing answer to "is my model covered?".
fn run_coverage(filter: Option<&str>) {
    let needle = filter.map(|f| f.to_ascii_lowercase());
    println!(
        "AI fingerprint coverage (catalogue reviewed {})",
        cleanup_rules::CATALOG_REVIEWED
    );
    println!();

    let mut shown = 0usize;
    for family in cleanup_rules::coverage() {
        if let Some(needle) = &needle {
            let haystack = format!("{} {}", family.id, family.provider).to_ascii_lowercase();
            if !haystack.contains(needle.as_str()) {
                continue;
            }
        }
        shown += 1;
        println!("{}  —  {}", family.id, family.provider);
        println!("    models:     {}", family.models);
        println!("    signatures: {}", family.signatures);
        println!("    rules:      {}", family.rules);
        println!();
    }

    if shown == 0 {
        println!("no model family matched {:?}", filter.unwrap_or(""));
        return;
    }

    println!("Out of scope — no deterministic character cleanup can remove these:");
    for item in cleanup_rules::OUT_OF_SCOPE {
        println!("  - {} [{}]", item.technique, item.owner);
        println!("    {}", item.why);
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn print_diff(before: &[u8], after: &[u8]) {
    let (Ok(b), Ok(a)) = (std::str::from_utf8(before), std::str::from_utf8(after)) else {
        println!("  (binary content — diff not shown)");
        return;
    };
    if b == a {
        return;
    }
    let diff = TextDiff::from_lines(b, a);
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => continue,
        };
        print!("  {sign} {change}");
    }
}

/// Atomic write: temp file in the same directory, then rename over the target.
fn atomic_write(dest: &Path, data: &[u8]) -> Result<()> {
    let dir = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let tmp = dir.join(format!(".{name}.tmp"));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, dest)?;
    Ok(())
}

/// The extension a same-format (unconverted) output must carry: the input's own
/// extension when it has one (so `.markdown` / `.jpeg` survive), else the canonical
/// extension for the parsed format.
fn source_ext(input: &Path, format: FormatId) -> String {
    if let Some(ext) = input.extension().and_then(|e| e.to_str())
        && !ext.is_empty()
    {
        return ext.to_ascii_lowercase();
    }
    match format {
        FormatId::Markdown => "md",
        FormatId::PlainText => "txt",
        FormatId::Docx => "docx",
        FormatId::Html => "html",
        FormatId::Pdf => "pdf",
        FormatId::Image => "png",
    }
    .to_string()
}

/// `foo.md` -> `foo.cleaned.md` (extension guaranteed by `cleaned_sibling_path`).
fn cleaned_path(input: &Path) -> PathBuf {
    let ext = input.extension().and_then(|s| s.to_str()).unwrap_or("txt");
    cleaned_sibling_path(input, ext)
}

/// `foo.docx` + `md` -> `foo.cleaned.md`
fn converted_path(input: &Path, ext: &str) -> PathBuf {
    cleaned_sibling_path(input, ext)
}

/// Append an extra extension, e.g. `foo.md` -> `foo.md.bak`.
fn with_extra_extension(input: &Path, extra: &str) -> PathBuf {
    let mut name = input
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("output")
        .to_string();
    name.push('.');
    name.push_str(extra);
    let mut p = input.to_path_buf();
    p.set_file_name(name);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_output_paths_gain_the_saved_format_extension() {
        assert_eq!(
            ensure_output_extension(Path::new("out"), "pdf"),
            PathBuf::from("out.pdf")
        );
        assert_eq!(
            ensure_output_extension(Path::new("out.docx"), "pdf"),
            PathBuf::from("out.pdf")
        );
        assert_eq!(
            ensure_output_extension(Path::new("out.markdown"), "md"),
            PathBuf::from("out.markdown")
        );
    }

    #[test]
    fn standalone_convert_does_not_clean_content() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("aiclean-convert-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        let destination = dir.join("result.txt");
        std::fs::write(
            &source,
            b"# Title\n\nCertainly! This is the original content.\n",
        )
        .unwrap();
        run_convert(&[source], Some(destination.clone()), None).unwrap();
        let result = std::fs::read_to_string(destination).unwrap();
        assert!(result.contains("Certainly!"), "{result}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_sibling_paths_keep_or_change_extensions_truthfully() {
        assert_eq!(
            cleaned_path(Path::new("docs/report.pdf")),
            PathBuf::from("docs/report.cleaned.pdf")
        );
        assert_eq!(
            converted_path(Path::new("docs/report.pdf"), "docx"),
            PathBuf::from("docs/report.cleaned.docx")
        );
        assert_eq!(source_ext(Path::new("a/b"), FormatId::Pdf), "pdf");
    }
}
