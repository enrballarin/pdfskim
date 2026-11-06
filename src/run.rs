//! Command execution: resolve outputs, process one file at a time, report.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use pdfcraft_cos::{Document, SaveOptions, pdf_date, write_full};
use serde_json::{Value, json};

use crate::compress::{self, CompressOutcome, OptimizeOptions};
use crate::discovery::{self, DiscoveredFile, Discovery};
use crate::signatures::{self, SignatureReport};
use crate::util::{
    MIB, describe_error, human_bytes, same_path, saved_percent, unique_output, write_file,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Compress,
    Strip,
    Clean,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Compress => "compress",
            Mode::Strip => "strip",
            Mode::Clean => "clean",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Mode::Compress => "compressed",
            Mode::Strip => "unsigned",
            Mode::Clean => "clean",
        }
    }

    fn strips(self) -> bool {
        matches!(self, Mode::Strip | Mode::Clean)
    }
}

pub struct JobOptions {
    pub mode: Mode,
    pub inputs: Vec<PathBuf>,
    pub out: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub recursive: bool,
    pub overwrite: bool,
    pub password: Option<String>,
    pub max_input_mib: u64,
    pub json: bool,
    pub quiet: bool,
    pub dry_run: bool,
    pub stop_on_error: bool,
    pub optimize: OptimizeOptions,
}

pub struct InfoOptions {
    pub inputs: Vec<PathBuf>,
    pub password: Option<String>,
    pub json: bool,
}

struct PerFile {
    input: PathBuf,
    output: Option<PathBuf>,
    status: &'static str,
    before: u64,
    after: u64,
    signatures: Option<SigJson>,
    optimize: Option<Value>,
    reused_input: bool,
    note: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct SigJson {
    fields: usize,
    signed_fields: usize,
    dictionaries: usize,
    perms: bool,
    dss: bool,
    xfa: bool,
    annotations_removed: usize,
    names: Vec<String>,
}

impl SigJson {
    fn from_strip(report: &SignatureReport, annotations_removed: usize) -> Self {
        Self {
            fields: report.fields,
            signed_fields: report.signed_fields,
            dictionaries: report.dictionaries,
            perms: report.perms,
            dss: report.dss,
            xfa: report.xfa,
            annotations_removed,
            names: report.names.clone(),
        }
    }

    fn value(&self) -> Value {
        json!({
            "fields": self.fields,
            "signed_fields": self.signed_fields,
            "dictionaries": self.dictionaries,
            "perms": self.perms,
            "dss": self.dss,
            "xfa": self.xfa,
            "annotations_removed": self.annotations_removed,
            "names": self.names,
        })
    }
}

pub fn execute(job: JobOptions) -> Result<u8> {
    validate(&job)?;
    let started = Instant::now();
    let discovery = discovery::discover(&job.inputs, job.out_dir.as_deref(), job.recursive)?;
    if discovery.files.is_empty() {
        report_nothing_found(&job, &discovery);
        return Ok(3);
    }

    let total = discovery.files.len();
    if !job.quiet && !job.json {
        eprintln!(
            "pdfskim {}: {} PDF(s){}{}",
            job.mode.name(),
            total,
            if job.dry_run { ", dry run" } else { "" },
            if discovery.skipped.is_empty() {
                String::new()
            } else {
                format!(", {} skipped", discovery.skipped.len())
            }
        );
    }

    let mut results = Vec::with_capacity(total);
    let mut used_outputs: Vec<PathBuf> = Vec::new();
    let mut completed = 0usize;
    let mut failed = 0usize;
    let mut skipped = discovery.skipped.len();
    let mut bytes_before = 0u64;
    let mut bytes_after = 0u64;
    let mut signatures_removed = 0usize;
    let mut stopped = false;

    for (index, file) in discovery.files.iter().enumerate() {
        if stopped {
            skipped += 1;
            results.push(PerFile {
                input: file.path.clone(),
                output: None,
                status: "skipped",
                before: 0,
                after: 0,
                signatures: None,
                optimize: None,
                reused_input: false,
                note: Some("stopped after an earlier error".into()),
                error: None,
            });
            continue;
        }

        let size = fs::metadata(&file.path).map(|m| m.len()).unwrap_or(0);
        let output = output_path(file, &job, &mut used_outputs)?;
        if !job.quiet && !job.json {
            eprint!(
                "[{}/{}] {:<9} {}",
                index + 1,
                total,
                job.mode.name(),
                file.path.display()
            );
        }

        let outcome = process_file(file, output.clone(), &job);
        bytes_before += size;
        match outcome {
            Ok(Processed::Done {
                after,
                signatures,
                optimize,
                reused_input,
                note,
            }) => {
                completed += 1;
                bytes_after += after;
                signatures_removed += signatures.as_ref().map_or(0, |s| s.signed_fields);
                if !job.quiet && !job.json {
                    eprintln!(
                        " -> {} ({}{}{})",
                        output.display(),
                        size_text(size, after),
                        if reused_input { ", kept original" } else { "" },
                        note.as_ref().map(|n| format!(", {n}")).unwrap_or_default()
                    );
                }
                results.push(PerFile {
                    input: file.path.clone(),
                    output: Some(output),
                    status: "ok",
                    before: size,
                    after,
                    signatures,
                    optimize,
                    reused_input,
                    note,
                    error: None,
                });
            }
            Ok(Processed::Skipped(reason)) => {
                skipped += 1;
                bytes_after += size;
                if !job.quiet && !job.json {
                    eprintln!(" skipped: {reason}");
                }
                results.push(PerFile {
                    input: file.path.clone(),
                    output: None,
                    status: "skipped",
                    before: size,
                    after: 0,
                    signatures: None,
                    optimize: None,
                    reused_input: false,
                    note: Some(reason),
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                bytes_after += size;
                if !job.quiet && !job.json {
                    eprintln!(" FAILED: {}", describe_error(&error));
                }
                results.push(PerFile {
                    input: file.path.clone(),
                    output: None,
                    status: "failed",
                    before: size,
                    after: size,
                    signatures: None,
                    optimize: None,
                    reused_input: false,
                    note: None,
                    error: Some(describe_error(&error)),
                });
                if job.stop_on_error {
                    stopped = true;
                }
            }
        }
    }

    let elapsed = started.elapsed();
    if job.json {
        let files: Vec<Value> = results.iter().map(file_json).collect();
        let skipped_json: Vec<Value> = discovery
            .skipped
            .iter()
            .map(|s| json!({"path": s.path.to_string_lossy(), "reason": s.reason}))
            .collect();
        let report = json!({
            "tool": "pdfskim",
            "version": env!("CARGO_PKG_VERSION"),
            "command": job.mode.name(),
            "dry_run": job.dry_run,
            "summary": {
                "selected": total,
                "completed": completed,
                "failed": failed,
                "skipped": skipped,
                "bytes_before": bytes_before,
                "bytes_after": bytes_after,
                "saved_bytes": bytes_before.saturating_sub(bytes_after),
                "saved_percent": round2(saved_percent(bytes_before, bytes_after)),
                "signatures_removed": signatures_removed,
                "elapsed_seconds": round3(elapsed.as_secs_f64()),
            },
            "files": files,
            "skipped": skipped_json,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Done: {completed} completed, {failed} failed, {skipped} skipped; {} -> {} ({} saved) in {:.1}s",
            human_bytes(bytes_before),
            human_bytes(bytes_after),
            human_bytes(bytes_before.saturating_sub(bytes_after)),
            elapsed.as_secs_f64()
        );
        if job.dry_run {
            println!("Dry run: no file was written.");
        }
    }

    if failed > 0 {
        Ok(1)
    } else if completed == 0 {
        Ok(3)
    } else {
        Ok(0)
    }
}

enum Processed {
    Done {
        after: u64,
        signatures: Option<SigJson>,
        optimize: Option<Value>,
        reused_input: bool,
        note: Option<String>,
    },
    Skipped(String),
}

fn process_file(file: &DiscoveredFile, output: PathBuf, job: &JobOptions) -> Result<Processed> {
    let bytes = read_input(&file.path, job.max_input_mib)?;
    let password = job.password.as_deref();

    if job.mode == Mode::Strip {
        let (stripped, report) = strip_document(bytes, password)?;
        let Some(stripped) = stripped else {
            let reason = if report.report.xfa {
                "no AcroForm signature fields; signatures inside an XFA form are not supported"
            } else {
                "no digital signatures found"
            };
            return Ok(Processed::Skipped(reason.into()));
        };
        let after = stripped.len() as u64;
        if !job.dry_run {
            write_file(&output, &stripped, job.overwrite)?;
        }
        let mut note = strip_note(&report.report);
        if report.annotations_removed > 0 {
            note.push_str(&format!(
                "; {} widget(s) removed",
                report.annotations_removed
            ));
        }
        return Ok(Processed::Done {
            after,
            signatures: Some(SigJson::from_strip(
                &report.report,
                report.annotations_removed,
            )),
            optimize: None,
            reused_input: false,
            note: Some(note),
        });
    }

    let mut options = job.optimize.clone();
    if job.mode == Mode::Clean {
        options
            .preset
            .get_or_insert(pdfcraft_optimize::Preset::Standard);
    }
    let settings = options.settings()?;
    let outcome = compress::run(bytes, password, &settings, job.mode.strips())?;
    let after = outcome.bytes.len() as u64;
    if !job.dry_run {
        write_file(&output, &outcome.bytes, job.overwrite)?;
    }
    let note = summarize(&outcome);
    Ok(Processed::Done {
        after,
        signatures: outcome
            .strip
            .as_ref()
            .map(|s| SigJson::from_strip(&s.report, s.annotations_removed)),
        optimize: Some(optimize_json(&outcome)),
        reused_input: outcome.reused_input,
        note,
    })
}

fn strip_document(
    bytes: Arc<Vec<u8>>,
    password: Option<&str>,
) -> Result<(Option<Vec<u8>>, signatures::StripOutcome)> {
    let mut doc = Document::open_with_password(bytes, password).map_err(compress::open_error)?;
    if let Some(permissions) = doc.permissions()
        && !permissions.owner
    {
        bail!("removing signatures from an encrypted PDF requires its owner password");
    }
    let outcome = signatures::strip(&mut doc)?;
    if !outcome.changed {
        return Ok((None, outcome));
    }
    let options = SaveOptions {
        mod_date: Some(pdf_date(compress::unix_now())),
        object_streams: at_least_1_5(doc.version()),
        ..SaveOptions::default()
    };
    let written = write_full(&doc, &options).with_context(|| "cannot write the unsigned PDF")?;
    let check = Document::open_with_password(Arc::new(written.clone()), password)
        .map_err(compress::open_error)?;
    let after = signatures::inspect(&check, true);
    if after.any() {
        bail!(
            "signature removal did not complete: {} field(s), {} signature dictionary(ies) remain",
            after.fields,
            after.dictionaries
        );
    }
    Ok((Some(written), outcome))
}

fn summarize(outcome: &CompressOutcome) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(strip) = &outcome.strip {
        parts.push(strip_note(&strip.report));
    }
    if outcome.reused_input {
        parts.push("optimizer did not make it smaller".into());
    }
    if outcome.merged_resources > 0 {
        parts.push(format!(
            "{} duplicate resource(s) merged",
            outcome.merged_resources
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn strip_note(report: &SignatureReport) -> String {
    let mut parts = Vec::new();
    if report.signed_fields > 0 {
        parts.push(format!(
            "{} signed signature(s) removed",
            report.signed_fields
        ));
    }
    let empty = report.fields.saturating_sub(report.signed_fields);
    if empty > 0 {
        parts.push(format!("{empty} empty signature field(s) removed"));
    }
    if report.perms {
        parts.push("certification (/Perms) removed".into());
    }
    if report.dss {
        parts.push("validation data (/DSS) removed".into());
    }
    if report.xfa {
        parts.push("XFA present: signatures inside XFA were not removed".into());
    }
    if parts.is_empty() {
        "nothing to remove".into()
    } else {
        parts.join(", ")
    }
}

fn optimize_json(outcome: &CompressOutcome) -> Value {
    let report = &outcome.report;
    json!({
        "images": report.images,
        "images_resampled": report.images_resampled,
        "images_recompressed": report.images_recompressed,
        "images_refused": report.images_refused,
        "image_bytes_before": report.image_bytes_before,
        "image_bytes_after": report.image_bytes_after,
        "merged_images": report.merged_images,
        "fragments_merged": report.fragments_merged,
        "merged_resources": outcome.merged_resources,
        "thumbnails": report.thumbnails,
        "alternate_images": report.alternate_images,
        "tags_removed": report.tags_removed,
        "print_settings": report.print_settings,
        "streams_compressed": report.streams_compressed,
        "lzw_streams": report.lzw_streams,
        "invalid_links": report.invalid_links,
        "invalid_bookmarks": report.invalid_bookmarks,
        "unreferenced_dests": report.unreferenced_dests,
        "unused_xobjects": report.unused_xobjects,
        "object_streams": outcome.object_streams,
    })
}

fn validate(job: &JobOptions) -> Result<()> {
    if job.out.is_some() && job.out_dir.is_some() {
        bail!("--out and --out-dir cannot be combined");
    }
    if job.out.is_some() && job.inputs.len() != 1 {
        bail!("--out needs exactly one input; use --out-dir for several");
    }
    if job.out.is_some() && job.mode == Mode::Strip && job.out_dir.is_some() {
        bail!("--out and --out-dir cannot be combined");
    }
    if job.max_input_mib == 0 || job.max_input_mib > 4096 {
        bail!("--max-input-mib must be between 1 and 4096");
    }
    if job.max_input_mib > 512 && !job.quiet {
        eprintln!(
            "warning: more than 512 MiB per input is beyond PdfCraft's batch limit; the whole file is held in memory"
        );
    }
    if job.out_dir.is_none() {
        for input in &job.inputs {
            if input.is_dir() {
                bail!(
                    "folder input {} needs --out-dir (the output folder)",
                    input.display()
                );
            }
        }
    }
    Ok(())
}

fn output_path(
    file: &DiscoveredFile,
    job: &JobOptions,
    used: &mut Vec<PathBuf>,
) -> Result<PathBuf> {
    if let Some(out) = &job.out {
        if same_path(&file.path, out) {
            bail!(
                "refusing to write over the input {}; choose another output with -o",
                file.path.display()
            );
        }
        if out.exists() && !job.overwrite {
            bail!(
                "output {} already exists (pass --overwrite to replace it)",
                out.display()
            );
        }
        return Ok(out.clone());
    }
    let name = file
        .path
        .file_stem()
        .map(|stem| format!("{}.{}.pdf", stem.to_string_lossy(), job.mode.suffix()))
        .unwrap_or_else(|| format!("output.{}.pdf", job.mode.suffix()));
    let base = if let Some(dir) = &job.out_dir {
        let parent = file
            .relative
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(""));
        dir.join(parent).join(name)
    } else {
        file.path.parent().unwrap_or(Path::new(".")).join(name)
    };
    let output = unique_output(&base, used, job.overwrite);
    if same_path(&file.path, &output) {
        bail!(
            "refusing to write over the input {}; choose another output with -o or --out-dir",
            file.path.display()
        );
    }
    used.push(output.clone());
    Ok(output)
}

fn read_input(path: &Path, max_mib: u64) -> Result<Arc<Vec<u8>>> {
    let metadata = fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    let limit = max_mib.saturating_mul(MIB);
    if metadata.len() > limit {
        bail!(
            "{} is {} ({} bytes) and exceeds the {} MiB limit (--max-input-mib)",
            path.display(),
            human_bytes(metadata.len()),
            metadata.len(),
            max_mib
        );
    }
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(Arc::new(bytes))
}

fn size_text(before: u64, after: u64) -> String {
    if after >= before {
        format!("{} (no reduction)", human_bytes(after))
    } else {
        format!(
            "{} (-{:.1}%)",
            human_bytes(after),
            saved_percent(before, after)
        )
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Object streams are a PDF 1.5 feature; keep an older file's header when stripping alone.
fn at_least_1_5(version: &str) -> bool {
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(1);
    let minor = parts
        .next()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    (major, minor) >= (1, 5)
}

fn file_json(file: &PerFile) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("input".into(), json!(file.input.to_string_lossy()));
    object.insert(
        "output".into(),
        file.output
            .as_ref()
            .map_or(Value::Null, |p| json!(p.to_string_lossy())),
    );
    object.insert("status".into(), json!(file.status));
    object.insert("bytes_before".into(), json!(file.before));
    object.insert("bytes_after".into(), json!(file.after));
    object.insert(
        "saved_bytes".into(),
        json!(file.before.saturating_sub(file.after)),
    );
    object.insert(
        "saved_percent".into(),
        json!(round2(saved_percent(file.before, file.after))),
    );
    object.insert("reused_input".into(), json!(file.reused_input));
    if let Some(signatures) = &file.signatures {
        object.insert("signatures".into(), signatures.value());
    }
    if let Some(optimize) = &file.optimize {
        object.insert("optimize".into(), optimize.clone());
    }
    if let Some(note) = &file.note {
        object.insert("note".into(), json!(note));
    }
    if let Some(error) = &file.error {
        object.insert("error".into(), json!(error));
    }
    Value::Object(object)
}

fn report_nothing_found(job: &JobOptions, discovery: &Discovery) {
    if job.json {
        let skipped: Vec<Value> = discovery
            .skipped
            .iter()
            .map(|s| json!({"path": s.path.to_string_lossy(), "reason": s.reason}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "tool": "pdfskim",
                "version": env!("CARGO_PKG_VERSION"),
                "command": job.mode.name(),
                "summary": {"selected": 0, "completed": 0, "failed": 0, "skipped": 0},
                "files": [],
                "skipped": skipped,
            }))
            .unwrap_or_default()
        );
    } else {
        eprintln!("pdfskim: no PDF files found");
        for skip in &discovery.skipped {
            eprintln!("  skipped {}: {}", skip.path.display(), skip.reason);
        }
    }
}

/// `pdfskim info`: what the file holds, without changing it.
pub fn info(options: InfoOptions) -> Result<u8> {
    let inputs = crate::glob::expand_all(&options.inputs)?;
    let mut failed = 0usize;
    let mut entries = Vec::new();
    for input in &inputs {
        let entry = (|| -> Result<Value> {
            let bytes = read_input(input, 512)?;
            let doc = Document::open_with_password(bytes.clone(), options.password.as_deref())
                .map_err(compress::open_error)?;
            let pages = pdfcraft_annot::page_refs(&doc)
                .map(|p| p.len())
                .unwrap_or(0);
            let signatures = signatures::inspect(&doc, true);
            let permissions = doc.permissions();
            let size = bytes.len() as u64;
            if !options.json {
                println!("{}", input.display());
                println!("  size:        {}", human_bytes(size));
                println!("  PDF version: {}", doc.version());
                println!("  pages:       {pages}");
                println!(
                    "  encrypted:   {}{}",
                    if permissions.is_some() { "yes" } else { "no" },
                    permissions
                        .as_ref()
                        .map(|p| if p.owner {
                            " (opened as owner)"
                        } else {
                            " (opened as user)"
                        })
                        .unwrap_or("")
                );
                if let Some(p) = &permissions {
                    println!(
                        "  permissions: modify {}, copy {}, annotate {}, print {}",
                        p.modify(),
                        p.copy(),
                        p.annotate(),
                        p.print()
                    );
                }
                println!(
                    "  signatures:  {} field(s), {} signed, {} signature dictionary(ies), /Perms {}, /DSS {}",
                    signatures.fields,
                    signatures.signed_fields,
                    signatures.dictionaries,
                    signatures.perms,
                    signatures.dss
                );
                if !signatures.names.is_empty() {
                    println!("  fields:      {}", signatures.names.join(", "));
                }
                if signatures.xfa {
                    println!(
                        "  XFA:         present (signatures inside XFA are not inspected or removed)"
                    );
                }
            }
            Ok(json!({
                "path": input.to_string_lossy(),
                "bytes": size,
                "pdf_version": doc.version(),
                "pages": pages,
                "encrypted": permissions.is_some(),
                "owner": permissions.as_ref().is_some_and(|p| p.owner),
                "signatures": signatures_value(&signatures),
            }))
        })();
        match entry {
            Ok(value) => entries.push(value),
            Err(error) => {
                failed += 1;
                eprintln!("pdfskim: {}: {}", input.display(), describe_error(&error));
            }
        }
    }
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"tool": "pdfskim", "command": "info", "files": entries})
            )?
        );
    }
    Ok(if failed > 0 { 1 } else { 0 })
}

fn signatures_value(report: &SignatureReport) -> Value {
    json!({
        "fields": report.fields,
        "signed_fields": report.signed_fields,
        "dictionaries": report.dictionaries,
        "perms": report.perms,
        "dss": report.dss,
        "xfa": report.xfa,
        "names": report.names,
        "signed": report.signed(),
    })
}
