//! Wildcard expansion for command-line inputs.
//!
//! Windows shells do not expand `*.pdf`, and a quoted pattern is passed through on every
//! platform, so patterns are expanded here instead: `*`, `?`, `[abc]`, `[a-z]` and `[!a-z]`
//! match inside one path component, and a `**` component matches any number of folders.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::discovery::{MAX_DEPTH, MAX_SELECTED};
use crate::util::is_link_or_reparse;

/// Whether any component of `path` is a wildcard pattern.
pub fn is_pattern(path: &Path) -> bool {
    path.components()
        .any(|component| has_wildcards(&component.as_os_str().to_string_lossy()))
}

/// Expand every pattern in `inputs`, in order. Paths without wildcards are returned as is.
pub fn expand_all(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for input in inputs {
        out.extend(expand(input)?);
    }
    Ok(out)
}

/// Expand one input. A path without wildcards is returned unchanged; a pattern that matches
/// nothing is an error, so a typo never becomes a silently empty batch.
pub fn expand(pattern: &Path) -> Result<Vec<PathBuf>> {
    if !is_pattern(pattern) {
        return Ok(vec![pattern.to_path_buf()]);
    }

    // Split the folders that are literal (`docs/`) from the components that may match many
    // entries (`*.pdf`).
    let mut base = PathBuf::new();
    let mut parts: Vec<String> = Vec::new();
    let mut in_pattern = false;
    for component in pattern.components() {
        let text = component.as_os_str().to_string_lossy().into_owned();
        if !in_pattern && !has_wildcards(&text) {
            base.push(component.as_os_str());
            continue;
        }
        match component {
            Component::Normal(_) | Component::CurDir | Component::ParentDir => {
                in_pattern = true;
                parts.push(text);
            }
            _ => bail!("unsupported wildcard pattern {}", pattern.display()),
        }
    }
    if base.as_os_str().is_empty() {
        base.push(".");
    }

    let mut found = Vec::new();
    walk(&base, &parts, 0, &mut found)?;
    found.sort();
    found.dedup();
    if found.is_empty() {
        bail!("no files match {}", pattern.display());
    }
    Ok(found)
}

fn walk(dir: &Path, parts: &[String], depth: usize, found: &mut Vec<PathBuf>) -> Result<()> {
    if found.len() > MAX_SELECTED {
        bail!("more than {MAX_SELECTED} paths match the pattern; narrow it");
    }
    let Some((part, rest)) = parts.split_first() else {
        if dir.exists() {
            found.push(dir.to_path_buf());
        }
        return Ok(());
    };

    if part == "**" {
        if depth > MAX_DEPTH {
            bail!(
                "more than {MAX_DEPTH} folder levels under {}",
                dir.display()
            );
        }
        walk(dir, rest, depth, found)?;
        for entry in entries(dir)? {
            if entry.is_dir() && !is_link_or_reparse(&entry) {
                walk(&entry, parts, depth + 1, found)?;
            }
        }
        return Ok(());
    }

    if !has_wildcards(part) {
        return walk(&dir.join(part), rest, depth, found);
    }

    for entry in entries(dir)? {
        let name = entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if matches(part, &name) {
            walk(&entry, rest, depth, found)?;
        }
    }
    Ok(())
}

fn entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", dir.display())),
    };
    // Keep the names relative when the pattern starts in the current folder, so messages and
    // output names read `report.pdf` rather than `.\report.pdf`.
    let bare = dir == Path::new(".");
    let mut out: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            if bare {
                PathBuf::from(entry.file_name())
            } else {
                entry.path()
            }
        })
        .collect();
    out.sort();
    Ok(out)
}

fn has_wildcards(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// Wildcard match for one path component. `*` matches any run, `?` one character, and
/// `[abc]`, `[a-z]`, `[!a-z]` one character from (or not from) the set. Windows compares
/// names case-insensitively, like its file system.
pub fn matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    match_at(&pattern, &name)
}

fn match_at(pattern: &[char], name: &[char]) -> bool {
    let Some((&first, rest)) = pattern.split_first() else {
        return name.is_empty();
    };
    match first {
        '*' => (0..=name.len()).any(|skip| match_at(rest, &name[skip..])),
        '?' => !name.is_empty() && match_at(rest, &name[1..]),
        '[' => match class(pattern, name) {
            Some((true, after)) => match_at(after, &name[1..]),
            _ => false,
        },
        literal => !name.is_empty() && same_char(literal, name[0]) && match_at(rest, &name[1..]),
    }
}

/// Reads the character class at the start of `pattern` and tests the first character of
/// `name`: whether it matched, and the pattern after the closing `]`.
fn class<'a>(pattern: &'a [char], name: &[char]) -> Option<(bool, &'a [char])> {
    let mut index = 1;
    let negated = matches!(pattern.get(index), Some('!') | Some('^'));
    if negated {
        index += 1;
    }
    let mut hit = false;
    let mut seen_any = false;
    while let Some(&item) = pattern.get(index) {
        if item == ']' && seen_any {
            return Some((hit != negated, &pattern[index + 1..]));
        }
        seen_any = true;
        match (
            pattern.get(index),
            pattern.get(index + 1),
            pattern.get(index + 2),
        ) {
            (Some(&low), Some('-'), Some(&high)) if high != ']' => {
                if let Some(&ch) = name.first() {
                    let ch = lower(ch);
                    if lower(low) <= ch && ch <= lower(high) {
                        hit = true;
                    }
                }
                index += 3;
            }
            _ => {
                if let Some(&ch) = name.first()
                    && same_char(item, ch)
                {
                    hit = true;
                }
                index += 1;
            }
        }
    }
    None
}

fn lower(ch: char) -> char {
    ch.to_ascii_lowercase()
}

fn same_char(a: char, b: char) -> bool {
    if cfg!(windows) {
        lower(a) == lower(b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn matches_plain_wildcards() {
        assert!(matches("*.pdf", "report.pdf"));
        assert!(matches("*.pdf", ".pdf"));
        assert!(!matches("*.pdf", "report.pdfx"));
        assert!(matches("chapter?.pdf", "chapter1.pdf"));
        assert!(!matches("chapter?.pdf", "chapter10.pdf"));
        assert!(matches("a*b*c", "axxbyyc"));
        assert!(matches("*", "anything"));
    }

    #[test]
    fn matches_character_classes() {
        assert!(matches("chapter[0-9].pdf", "chapter7.pdf"));
        assert!(!matches("chapter[0-9].pdf", "chapterx.pdf"));
        assert!(matches("[ab]*.pdf", "beta.pdf"));
        assert!(!matches("[ab]*.pdf", "gamma.pdf"));
        assert!(matches("[!ab]*.pdf", "gamma.pdf"));
        assert!(!matches("[!ab]*.pdf", "alpha.pdf"));
    }
}
