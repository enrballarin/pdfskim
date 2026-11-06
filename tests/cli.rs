//! End-to-end tests of the `pdfskim` binary.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pdfcraft_cos::Document;
use serde_json::Value;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pdfskim"))
}

fn run(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("run pdfskim")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {text}\nstderr: {}", stderr(output)))
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn compress_one_file_writes_a_compressed_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let input = write(dir.path(), "report.pdf", &common::image_pdf());
    let output = run(&["compress", input.to_str().unwrap(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["summary"]["completed"], 1);
    assert_eq!(report["files"][0]["status"], "ok");
    let written = PathBuf::from(report["files"][0]["output"].as_str().unwrap());
    assert_eq!(written, dir.path().join("report.compressed.pdf"));
    assert!(written.exists());
    let bytes = fs::read(&written).unwrap();
    assert!(bytes.len() < fs::metadata(&input).unwrap().len() as usize);
    let doc = Document::open(std::sync::Arc::new(bytes)).unwrap();
    assert_eq!(pdfcraft_annot::page_refs(&doc).unwrap().len(), 1);
}

#[test]
fn compress_refuses_a_signed_pdf_until_it_is_stripped() {
    let dir = tempfile::tempdir().unwrap();
    let input = write(dir.path(), "signed.pdf", &common::signed_pdf());
    let output = run(&["compress", input.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("digitally signed"),
        "stderr: {}",
        stderr(&output)
    );

    let output = run(&["clean", input.to_str().unwrap(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["files"][0]["signatures"]["fields"], 1);
    let written = PathBuf::from(report["files"][0]["output"].as_str().unwrap());
    assert_eq!(written, dir.path().join("signed.clean.pdf"));
    assert!(written.exists());
}

#[test]
fn strip_writes_an_unsigned_copy() {
    let dir = tempfile::tempdir().unwrap();
    let input = write(dir.path(), "signed.pdf", &common::signed_pdf());
    let output = run(&["strip", input.to_str().unwrap(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["files"][0]["status"], "ok");
    assert_eq!(report["files"][0]["signatures"]["signed_fields"], 1);
    let written = PathBuf::from(report["files"][0]["output"].as_str().unwrap());
    assert_eq!(written, dir.path().join("signed.unsigned.pdf"));
    let doc = Document::open(std::sync::Arc::new(fs::read(&written).unwrap())).unwrap();
    assert!(!pdfskim::signatures::inspect(&doc, true).any());
}

#[test]
fn strip_says_when_there_is_nothing_to_remove() {
    let dir = tempfile::tempdir().unwrap();
    let input = write(dir.path(), "plain.pdf", &common::text_pdf());
    let output = run(&["strip", input.to_str().unwrap(), "--json"]);
    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["summary"]["skipped"], 1);
    assert!(!dir.path().join("plain.unsigned.pdf").exists());
}

#[test]
fn batch_preserves_the_folder_structure_and_skips_non_pdfs() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("scans");
    write(&input, "a.pdf", &common::text_pdf());
    write(&input, "sub/b.pdf", &common::image_pdf());
    write(&input, "notes.txt", b"not a pdf");
    let out = dir.path().join("compressed");
    let output = run(&[
        "compress",
        input.to_str().unwrap(),
        "--recursive",
        "--out-dir",
        out.to_str().unwrap(),
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["summary"]["completed"], 2);
    assert!(out.join("a.compressed.pdf").exists());
    assert!(out.join("sub/b.compressed.pdf").exists());
    assert!(!out.join("notes.txt").exists());
}

#[test]
fn info_reports_the_signature_state_as_json() {
    let dir = tempfile::tempdir().unwrap();
    let input = write(dir.path(), "signed.pdf", &common::signed_pdf());
    let output = run(&["info", input.to_str().unwrap(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["files"][0]["signatures"]["fields"], 1);
    assert_eq!(report["files"][0]["signatures"]["signed"], true);
    assert_eq!(report["files"][0]["pages"], 1);
}

#[test]
fn enforces_the_input_size_limit() {
    let dir = tempfile::tempdir().unwrap();
    let input = write(dir.path(), "big.pdf", &common::image_pdf());
    let output = run(&["compress", input.to_str().unwrap(), "--max-input-mib", "1"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("exceeds the 1 MiB limit"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(!dir.path().join("big.compressed.pdf").exists());
}

#[test]
fn dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let input = write(dir.path(), "report.pdf", &common::text_pdf());
    let output = run(&["compress", input.to_str().unwrap(), "--dry-run", "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(!dir.path().join("report.compressed.pdf").exists());
    let report = json(&output);
    assert_eq!(report["dry_run"], true);
}

#[test]
fn refuses_to_write_over_the_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = write(dir.path(), "report.pdf", &common::text_pdf());
    let output = run(&[
        "compress",
        input.to_str().unwrap(),
        "-o",
        input.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("refusing to write over the input"),
        "stderr: {}",
        stderr(&output)
    );
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run pdfskim")
}

#[test]
fn expands_a_wildcard_pattern() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "one.pdf", &common::text_pdf());
    write(dir.path(), "two.pdf", &common::image_pdf());
    write(dir.path(), "notes.txt", b"not a pdf");
    let output = run_in(
        dir.path(),
        &["compress", "*.pdf", "--out-dir", "out", "--json"],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["summary"]["completed"], 2);
    assert!(dir.path().join("out/one.compressed.pdf").exists());
    assert!(dir.path().join("out/two.compressed.pdf").exists());
}

#[test]
fn a_pattern_that_matches_nothing_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_in(dir.path(), &["compress", "*.pdf"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no files match *.pdf"),
        "stderr: {}",
        stderr(&output)
    );
}
