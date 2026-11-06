//! Finding the PDFs of a batch, with PdfCraft's batch-command limits: at most 1,000 selected
//! files, 10,000 visited directory entries and 64 folder levels.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::glob;
use crate::util::{is_link_or_reparse, is_pdf, normalize, same_path};

pub const MAX_SELECTED: usize = 1_000;
pub const MAX_VISITED: usize = 10_000;
pub const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    /// Path inside the output folder: the file name for an explicit file, the path relative to
    /// its selected folder for a folder input.
    pub relative: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Default)]
pub struct Discovery {
    pub files: Vec<DiscoveredFile>,
    pub skipped: Vec<Skipped>,
}

pub fn discover(inputs: &[PathBuf], out_dir: Option<&Path>, recursive: bool) -> Result<Discovery> {
    if inputs.is_empty() {
        bail!("no input files or folders given");
    }
    let out_canon = out_dir.map(normalize);
    let mut discovery = Discovery::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut visited = 0usize;

    for input in &glob::expand_all(inputs)? {
        let metadata =
            fs::metadata(input).with_context(|| format!("cannot read {}", input.display()))?;
        if is_link_or_reparse(input) {
            bail!(
                "{} is a symbolic link or reparse point; select the real file instead",
                input.display()
            );
        }
        let root = normalize(
            &fs::canonicalize(input)
                .with_context(|| format!("cannot resolve {}", input.display()))?,
        );
        if metadata.is_file() {
            if !is_pdf(input) {
                discovery.skipped.push(Skipped {
                    path: input.clone(),
                    reason: "not a PDF file".into(),
                });
                continue;
            }
            if seen.insert(root.clone()) {
                push_file(
                    &mut discovery,
                    root,
                    input.file_name().map(PathBuf::from).unwrap_or_default(),
                )?;
            }
            continue;
        }
        if !metadata.is_dir() {
            discovery.skipped.push(Skipped {
                path: input.clone(),
                reason: "not a regular file or folder".into(),
            });
            continue;
        }
        if is_link_or_reparse(input) {
            bail!(
                "{} is a symbolic link or reparse point; select its real folder instead",
                input.display()
            );
        }
        let root_canon = normalize(
            &fs::canonicalize(input)
                .with_context(|| format!("cannot resolve {}", input.display()))?,
        );
        if let Some(out) = &out_canon {
            if same_path(out, &root_canon) {
                bail!(
                    "the output folder cannot be the input folder ({})",
                    input.display()
                );
            }
            if root_canon.starts_with(out) {
                bail!(
                    "the output folder {} contains the input folder {}",
                    out.display(),
                    input.display()
                );
            }
        }
        let mut stack = vec![(root_canon.clone(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                bail!(
                    "more than {MAX_DEPTH} folder levels under {}",
                    input.display()
                );
            }
            let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
                .with_context(|| format!("cannot read {}", dir.display()))?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .collect();
            entries.sort();
            for path in entries {
                visited += 1;
                if visited > MAX_VISITED {
                    bail!(
                        "more than {MAX_VISITED} folder entries visited; select a smaller folder"
                    );
                }
                if is_link_or_reparse(&path) {
                    continue;
                }
                let Ok(file_type) = fs::metadata(&path) else {
                    continue;
                };
                if file_type.is_dir() {
                    if let Some(out) = &out_canon {
                        let path_canon = normalize(&path);
                        if path_canon.starts_with(out) {
                            continue;
                        }
                    }
                    if recursive {
                        stack.push((path, depth + 1));
                    }
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                if !is_pdf(&path) {
                    discovery.skipped.push(Skipped {
                        path,
                        reason: "not a PDF file".into(),
                    });
                    continue;
                }
                let canonical =
                    normalize(&fs::canonicalize(&path).unwrap_or_else(|_| path.clone()));
                if !seen.insert(canonical.clone()) {
                    continue;
                }
                let relative = path
                    .strip_prefix(&root_canon)
                    .unwrap_or(&path)
                    .to_path_buf();
                push_file(&mut discovery, canonical, relative)?;
            }
        }
    }

    discovery.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(discovery)
}

fn push_file(discovery: &mut Discovery, path: PathBuf, relative: PathBuf) -> Result<()> {
    if discovery.files.len() >= MAX_SELECTED {
        bail!("more than {MAX_SELECTED} PDFs selected; split the batch");
    }
    discovery.files.push(DiscoveredFile { path, relative });
    Ok(())
}
