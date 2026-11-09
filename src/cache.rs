use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{cache_root, Config};
use crate::util::lock::Lock;

pub(crate) struct CacheInfo {
    pub(crate) index: u64,
    pub(crate) crates: u64,
    pub(crate) sources: u64,
    pub(crate) artifacts: u64,
    pub(crate) total: u64,
}

pub(crate) struct GcRemoved {
    pub(crate) key: String,
    pub(crate) bytes: u64,
}

pub(crate) struct GcReport {
    pub(crate) removed: Vec<GcRemoved>,
    pub(crate) remaining: u64,
}

pub(crate) struct CleanScopes {
    pub(crate) store: bool,
    pub(crate) index: bool,
    pub(crate) source: bool,
    pub(crate) all: bool,
}

pub(crate) fn info(config: &Config) -> Result<CacheInfo, String> {
    let root = cache_root(config)?.join("v1");
    Ok(CacheInfo {
        index: directory_size(&root.join("index"))?,
        crates: directory_size(&root.join("crates"))?,
        sources: directory_size(&root.join("src"))?,
        artifacts: directory_size(&root.join("artifacts"))?,
        total: directory_size(&root)?,
    })
}

pub(crate) fn gc(config: &Config, max_size: u64, dry_run: bool) -> Result<GcReport, String> {
    let root = cache_root(config)?.join("v1");
    gc_root(&root, max_size, dry_run)
}

fn gc_root(root: &Path, max_size: u64, dry_run: bool) -> Result<GcReport, String> {
    let artifacts = root.join("artifacts");
    let mut entries: Vec<(String, u64, SystemTime, PathBuf)> = Vec::new();
    if artifacts.is_dir() {
        for entry in
            fs::read_dir(&artifacts).map_err(|error| format!("{}: {error}", artifacts.display()))?
        {
            let entry = entry.map_err(|error| format!("{}: {error}", artifacts.display()))?;
            if !entry
                .file_type()
                .map_err(|error| format!("{}: {error}", entry.path().display()))?
                .is_dir()
            {
                continue;
            }
            let key = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let bytes = directory_size(&path)?;
            let used = fs::metadata(path.join("used"))
                .and_then(|metadata| metadata.modified())
                .or_else(|_| fs::metadata(&path).and_then(|metadata| metadata.modified()))
                .unwrap_or(UNIX_EPOCH);
            entries.push((key, bytes, used, path));
        }
    }
    entries.sort_by(|left, right| (left.2, &left.0).cmp(&(right.2, &right.0)));

    let mut total = directory_size(&root)?;
    let mut removed = Vec::new();
    for (key, bytes, _, path) in entries {
        if total <= max_size {
            break;
        }
        let lock_path = root.join("locks").join(format!("{key}.lock"));
        let guard = match Lock::try_acquire(&lock_path)
            .map_err(|error| format!("{}: {error}", lock_path.display()))?
        {
            Some(guard) => guard,
            None => continue,
        };
        if !dry_run {
            fs::remove_dir_all(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        }
        drop(guard);
        total = total.saturating_sub(bytes);
        removed.push(GcRemoved { key, bytes });
    }

    let remaining = if dry_run {
        total
    } else {
        directory_size(&root)?
    };
    Ok(GcReport { removed, remaining })
}

pub(crate) fn clean(config: &Config, scopes: &CleanScopes) -> Result<(), String> {
    let root = cache_root(config)?.join("v1");
    if scopes.all {
        return remove(&root);
    }

    let mut paths = Vec::new();
    if scopes.store {
        paths.push(root.join("artifacts"));
        paths.push(root.join("build-scripts"));
        paths.push(root.join("metadata"));
        paths.push(root.join("tmp"));
    }
    if scopes.index {
        paths.push(root.join("index"));
    }
    if scopes.source {
        paths.push(root.join("src"));
        paths.push(root.join("crates"));
    }
    for path in paths {
        remove(&path)?;
    }
    Ok(())
}

pub(crate) fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    let value: f64 = number
        .parse()
        .map_err(|_| format!("invalid size `{text}`"))?;
    let multiplier = match suffix.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024u64 * 1024 * 1024 * 1024,
        other => return Err(format!("unknown size suffix `{other}`")),
    };
    let bytes = value * multiplier as f64;
    if !bytes.is_finite() || bytes < 0.0 {
        return Err(format!("invalid size `{text}`"));
    }
    Ok(bytes as u64)
}

pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
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

fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn directory_size(path: &Path) -> Result<u64, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in fs::read_dir(path).map_err(|error| format!("{}: {error}", path.display()))? {
        let entry = entry.map_err(|error| format!("{}: {error}", path.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", entry.path().display()))?;
        if file_type.is_dir() {
            total += directory_size(&entry.path())?;
        } else if file_type.is_file() {
            total += entry
                .metadata()
                .map_err(|error| format!("{}: {error}", entry.path().display()))?
                .len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::{directory_size, gc_root, human_size, parse_size};
    use crate::util::lock::Lock;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1").unwrap(), 1);
        assert_eq!(parse_size("5K").unwrap(), 5 * 1024);
        assert_eq!(parse_size("5G").unwrap(), 5 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("1.5M").unwrap(), 1024 * 1024 + 512 * 1024);
        assert!(parse_size("5X").is_err());
        assert!(parse_size("nope").is_err());
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(2048), "2.0 KiB");
    }

    #[test]
    fn in_use_artifacts_are_locked() {
        let directory = std::env::temp_dir().join(format!(
            "rusted-cache-lock-{}-{}",
            std::process::id(),
            "in-use"
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let lock_path = directory.join("locks").join("key.lock");
        let lock = Lock::acquire(&lock_path).unwrap();
        assert!(Lock::try_acquire(&lock_path).unwrap().is_none());
        drop(lock);
        assert!(Lock::try_acquire(&lock_path).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn gc_skips_locked_artifacts() {
        let root = std::env::temp_dir().join(format!(
            "rusted-cache-gc-{}-{}",
            std::process::id(),
            "locked"
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("artifacts/a")).unwrap();
        std::fs::create_dir_all(root.join("artifacts/b")).unwrap();
        std::fs::write(root.join("artifacts/a/lib.rlib"), b"aaaa").unwrap();
        std::fs::write(root.join("artifacts/b/lib.rlib"), b"bbbb").unwrap();

        let lock = Lock::acquire(&root.join("locks/a.lock")).unwrap();
        let report = gc_root(&root, 0, false).unwrap();
        assert!(root.join("artifacts/a").is_dir());
        assert!(!root.join("artifacts/b").exists());
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].key, "b");
        drop(lock);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sums_directory_files() {
        let directory = std::env::temp_dir().join(format!(
            "rusted-cache-size-{}-{}",
            std::process::id(),
            "sum"
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("nested")).unwrap();
        std::fs::write(directory.join("one"), b"1234").unwrap();
        std::fs::write(directory.join("nested/two"), b"12").unwrap();
        assert_eq!(directory_size(&directory).unwrap(), 6);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
