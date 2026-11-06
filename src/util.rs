//! Small helpers shared by the commands: sizes, paths and atomic writes.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

/// One mebibyte, the unit every size limit in pdfskim uses.
pub const MIB: u64 = 1024 * 1024;

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Percentage of the input the output saved, 0 when nothing was saved.
pub fn saved_percent(before: u64, after: u64) -> f64 {
    if before == 0 || after >= before {
        return 0.0;
    }
    ((before - after) as f64) * 100.0 / (before as f64)
}

/// Absolute, lexically normalised path. The file need not exist.
pub fn normalize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    #[cfg(windows)]
    {
        // `canonicalize` returns `\\?\D:\...`; compare and print ordinary drive paths.
        let text = out.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    out
}

/// Whether two paths name the same file (case-insensitively on Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

/// Symlinks and Windows reparse points (junctions included) are never followed.
pub fn is_link_or_reparse(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return true;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// Write `bytes` to `path` through a temporary file in the same folder, then rename it over
/// the destination. Existing files are only replaced when `overwrite` is set.
pub fn write_file(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    if path.exists() && !overwrite {
        bail!(
            "output {} already exists (pass --overwrite to replace it)",
            path.display()
        );
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".pdfskim-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("cannot create a temporary file in {}", parent.display()))?;
    temp.write_all(bytes)
        .with_context(|| format!("cannot write the temporary copy for {}", path.display()))?;
    temp.as_file().sync_all().ok();
    // `persist` replaces an existing target atomically (rename / MoveFileEx with replace).
    temp.persist(path)
        .map_err(|e| anyhow::anyhow!("cannot write {}: {}", path.display(), e.error))?;
    Ok(())
}

/// `base` if free, else `name (2).ext`, `name (3).ext`, ... A path is considered taken when it
/// exists on disk or when this run already produced it.
pub fn unique_output(base: &Path, used: &[PathBuf], overwrite: bool) -> PathBuf {
    let taken = |p: &Path| {
        if used.iter().any(|u| same_path(u, p)) {
            return true;
        }
        !overwrite && p.exists()
    };
    if !taken(base) {
        return base.to_path_buf();
    }
    let parent = base.parent().unwrap_or(Path::new(""));
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let ext = base
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 2u32.. {
        let candidate = parent.join(format!("{stem} ({n}){ext}"));
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!("the numbered candidates are unbounded")
}

/// English description of an error chain.
///
/// Windows reports its own messages in the system language (`El nombre de archivo ...`), so
/// the OS text is replaced with a stable English description plus its error code.
pub fn describe_error(error: &anyhow::Error) -> String {
    let mut out = String::new();
    for (index, cause) in error.chain().enumerate() {
        if index > 0 {
            out.push_str(": ");
        }
        match cause.downcast_ref::<std::io::Error>() {
            Some(io) => out.push_str(&describe_io_error(io)),
            None => out.push_str(&cause.to_string()),
        }
    }
    out
}

fn describe_io_error(error: &std::io::Error) -> String {
    use std::io::ErrorKind;
    // Windows writes its own messages in the installed language, so the codes pdfskim meets
    // most often get a fixed English name here.
    #[cfg(windows)]
    let windows = match error.raw_os_error() {
        Some(5) => Some("permission denied"),
        Some(32 | 33) => Some("file is in use"),
        Some(112) => Some("no space left on device"),
        Some(123) => Some("invalid file name"),
        _ => None,
    };
    #[cfg(not(windows))]
    let windows: Option<&str> = None;
    let kind = windows.unwrap_or_else(|| match error.kind() {
        ErrorKind::NotFound => "not found",
        ErrorKind::PermissionDenied => "permission denied",
        ErrorKind::AlreadyExists => "already exists",
        ErrorKind::InvalidInput => "invalid name or argument",
        ErrorKind::IsADirectory => "is a directory",
        ErrorKind::NotADirectory => "not a directory",
        ErrorKind::DirectoryNotEmpty => "directory not empty",
        ErrorKind::ReadOnlyFilesystem => "read-only file system",
        ErrorKind::StorageFull => "no space left on device",
        ErrorKind::FileTooLarge => "file too large",
        ErrorKind::BrokenPipe => "broken pipe",
        _ => "I/O error",
    });
    match error.raw_os_error() {
        Some(code) => format!("{kind} (os error {code})"),
        None => kind.to_string(),
    }
}
