use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::util::lock::Lock;

use super::download::Request;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn ensure_extracted(
    root: &Path,
    request: &Request,
    tarball: &Path,
) -> Result<PathBuf, String> {
    let final_dir = root
        .join("src")
        .join(format!("{}-{}", request.name, request.version));
    if is_complete(&final_dir) {
        return Ok(final_dir);
    }

    let lock_path = root.join("locks").join(format!(
        "registry-src-{}-{}.lock",
        request.name, request.version
    ));
    let _lock =
        Lock::acquire(&lock_path).map_err(|error| format!("{}: {error}", lock_path.display()))?;

    if is_complete(&final_dir) {
        return Ok(final_dir);
    }
    if final_dir.exists() {
        fs::remove_dir_all(&final_dir)
            .map_err(|error| format!("{}: {error}", final_dir.display()))?;
    }

    let temp = root.join("tmp").join(format!(
        "src-{}-{}-{}-{}",
        request.name,
        request.version,
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;

    let result = extract_into(tarball, &temp, request);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temp);
        return Err(error);
    }

    let extracted = temp.join(format!("{}-{}", request.name, request.version));
    if !extracted.is_dir() {
        let _ = fs::remove_dir_all(&temp);
        return Err(format!(
            "archive did not contain `{}-{}`",
            request.name, request.version
        ));
    }

    let source_dir = root.join("src");
    fs::create_dir_all(&source_dir)
        .map_err(|error| format!("{}: {error}", source_dir.display()))?;
    fs::rename(&extracted, &final_dir).map_err(|error| {
        format!(
            "{} -> {}: {error}",
            extracted.display(),
            final_dir.display()
        )
    })?;
    fs::remove_dir_all(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;

    Ok(final_dir)
}

fn is_complete(directory: &Path) -> bool {
    directory.is_dir() && directory.join("Cargo.toml").is_file()
}

fn extract_into(tarball: &Path, destination: &Path, request: &Request) -> Result<(), String> {
    validate_listing(tarball, request)?;

    let status = Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(destination)
        .arg("--no-same-owner")
        .arg("--no-same-permissions")
        .status()
        .map_err(|error| format!("failed to run tar: {error}"))?;
    if !status.success() {
        return Err(format!(
            "tar failed to extract `{}` with {status}",
            tarball.display()
        ));
    }

    validate_symlinks(destination)
}

fn validate_listing(tarball: &Path, request: &Request) -> Result<(), String> {
    let output = Command::new("tar")
        .arg("-tzf")
        .arg(tarball)
        .output()
        .map_err(|error| format!("failed to run tar -tzf: {error}"))?;
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("archive listing is not UTF-8: {error}"))?;
    let prefix = format!("{}-{}/", request.name, request.version);
    let root_entry = prefix.trim_end_matches('/');

    for line in text.lines() {
        let entry = line.trim_end_matches('/');
        if entry.is_empty() {
            continue;
        }
        if entry.starts_with('/') {
            return Err(format!("archive contains absolute path `{entry}`"));
        }
        if entry.split('/').any(|component| component == "..") {
            return Err(format!("archive contains parent path `{entry}`"));
        }
        if entry != root_entry && !entry.starts_with(&prefix) {
            return Err(format!("archive entry `{entry}` is outside `{prefix}`"));
        }
    }

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "tar -tzf failed for `{}`: {}",
            tarball.display(),
            error.trim()
        ));
    }
    Ok(())
}

fn validate_symlinks(root: &Path) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() {
                let target =
                    fs::read_link(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                if !symlink_is_safe(root, &path, &target) {
                    return Err(format!(
                        "archive contains unsafe symlink `{}` -> `{}`",
                        path.display(),
                        target.display()
                    ));
                }
            } else if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

fn symlink_is_safe(root: &Path, link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let Some(parent) = link.parent() else {
        return false;
    };
    let Ok(relative_parent) = parent.strip_prefix(root) else {
        return false;
    };
    let mut depth: i64 = relative_parent.components().count() as i64;

    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::symlink_is_safe;
    use std::path::Path;

    #[test]
    fn relative_symlinks_stay_inside_the_root() {
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/package/link");
        assert!(symlink_is_safe(root, link, Path::new("other")));
        assert!(symlink_is_safe(root, link, Path::new("../shared")));
        assert!(!symlink_is_safe(root, link, Path::new("../../escape")));
        assert!(!symlink_is_safe(root, link, Path::new("/etc/passwd")));
    }
}
