use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::manifest::Manifest;
use crate::registry::index::{IndexDependency, IndexEntry};
use crate::resolver::resolve::Registry;
use crate::resolver::semver::Version;
use crate::toml::Value;
use crate::util::hash::{to_hex, Sha256};

const CONFIG_FILE: &str = ".cargo/config.toml";
const CHECKSUM_FILE: &str = ".cargo-checksum.json";
const OK_FILE: &str = ".cargo-ok";
const MANIFEST: &str = "Cargo.toml";

pub(crate) struct Replacement {
    pub(crate) directory: PathBuf,
}

// Reads the Cargo-style source replacement that `rusted vendor` prints:
// `[source.crates-io]` with `replace-with`, plus `[source.<name>]` with
// `directory`, from `.cargo/config.toml` in the workspace root.
pub(crate) fn replacement(workspace_root: &Path) -> Result<Option<Replacement>, String> {
    let path = workspace_root.join(CONFIG_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let table =
        crate::toml::parse(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let Some(Value::Table(sources)) = table.get("source") else {
        return Ok(None);
    };
    let Some(Value::Table(crates_io)) = sources.get("crates-io") else {
        return Ok(None);
    };
    let Some(Value::String(replace_with)) = crates_io.get("replace-with") else {
        return Ok(None);
    };
    let Some(Value::Table(source)) = sources.get(replace_with) else {
        return Err(format!(
            "{}: replaces crates-io with `{replace_with}`, which has no `[source.{replace_with}]` table",
            path.display()
        ));
    };
    let Some(Value::String(directory)) = source.get("directory") else {
        return Err(format!(
            "{}: `[source.{replace_with}]` must set `directory`",
            path.display()
        ));
    };
    let directory = PathBuf::from(directory);
    let directory = if directory.is_absolute() {
        directory
    } else {
        workspace_root.join(directory)
    };
    match fs::canonicalize(&directory) {
        Ok(directory) => Ok(Some(Replacement { directory })),
        Err(error) => Err(format!(
            "vendored source directory `{}` is missing, run `rusted vendor`: {error}",
            directory.display()
        )),
    }
}

pub(crate) struct VendorRegistry {
    root: PathBuf,
}

impl VendorRegistry {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Registry for VendorRegistry {
    fn entries(&mut self, name: &str) -> Result<Vec<IndexEntry>, String> {
        let mut out = Vec::new();
        let prefix = format!("{name}-");
        let read = match fs::read_dir(&self.root) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(error) => return Err(format!("{}: {error}", self.root.display())),
        };
        for entry in read {
            let entry = entry.map_err(|error| format!("{}: {error}", self.root.display()))?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = file_name.strip_prefix(&prefix) else {
                continue;
            };
            let directory = entry.path();
            if !directory.join("Cargo.toml").is_file() {
                continue;
            }
            let Ok(version) = Version::parse(rest) else {
                continue;
            };
            out.push(entry_for(&directory, name, &version)?);
        }
        out.sort_by(|left, right| right.version.cmp(&left.version));
        Ok(out)
    }
}

fn entry_for(directory: &Path, name: &str, version: &Version) -> Result<IndexEntry, String> {
    let manifest_path = directory.join("Cargo.toml");
    let text = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text, &manifest_path).map_err(|error| format!("{error}"))?;
    if manifest.name != name {
        return Err(format!(
            "{} declares package `{}`, expected `{name}`",
            manifest_path.display(),
            manifest.name
        ));
    }
    let declared = Version::parse(&manifest.version)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    if &declared != version {
        return Err(format!(
            "{} declares version `{}`, directory is `{}`",
            manifest_path.display(),
            manifest.version,
            version
        ));
    }

    let mut dependencies = Vec::new();
    for (kind, deps) in [
        ("normal", &manifest.dependencies),
        ("build", &manifest.build_dependencies),
    ] {
        for (key, dependency) in deps {
            if dependency.git.is_some() || dependency.path.is_some() {
                return Err(format!(
                    "vendored crate `{name}` has a {kind} dependency `{key}` with its own source, which vendored sources cannot resolve"
                ));
            }
            if dependency
                .registry
                .as_deref()
                .is_some_and(|registry| registry != "crates-io")
            {
                return Err(format!(
                    "vendored crate `{name}` depends on `{key}` from another registry"
                ));
            }
            dependencies.push(IndexDependency {
                name: dependency.package.clone().unwrap_or_else(|| key.clone()),
                requirement: dependency
                    .requirement
                    .clone()
                    .unwrap_or_else(|| "*".to_string()),
                features: dependency.features.clone(),
                optional: dependency.optional,
                default_features: dependency.default_features.unwrap_or(true),
                target: None,
                kind: kind.to_string(),
                registry: dependency.registry.clone(),
                package: dependency.package.clone(),
            });
        }
    }
    for (expression, deps) in &manifest.targets {
        for (key, dependency) in deps
            .dependencies
            .iter()
            .chain(deps.build_dependencies.iter())
        {
            if dependency.git.is_some() || dependency.path.is_some() {
                return Err(format!(
                    "vendored crate `{name}` has a target dependency `{key}` with its own source, which vendored sources cannot resolve"
                ));
            }
            dependencies.push(IndexDependency {
                name: dependency.package.clone().unwrap_or_else(|| key.clone()),
                requirement: dependency
                    .requirement
                    .clone()
                    .unwrap_or_else(|| "*".to_string()),
                features: dependency.features.clone(),
                optional: dependency.optional,
                default_features: dependency.default_features.unwrap_or(true),
                target: Some(expression.clone()),
                kind: "normal".to_string(),
                registry: dependency.registry.clone(),
                package: dependency.package.clone(),
            });
        }
    }

    Ok(IndexEntry {
        name: name.to_string(),
        version: version.clone(),
        checksum: checksum_of(directory)?,
        dependencies,
        features: manifest.features.clone(),
        yanked: false,
        rust_version: manifest.rust_version.clone(),
    })
}

fn checksum_of(directory: &Path) -> Result<String, String> {
    let status = read_checksum_file(directory)?;
    Ok(status.package)
}

struct ChecksumStatus {
    files: BTreeMap<String, String>,
    package: String,
}

fn read_checksum_file(directory: &Path) -> Result<ChecksumStatus, String> {
    let path = directory.join(CHECKSUM_FILE);
    let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let value =
        crate::util::json::parse(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let package = value
        .get("package")
        .and_then(|package| package.as_str())
        .ok_or_else(|| format!("{} has no `package` checksum", path.display()))?
        .to_string();
    let mut files = BTreeMap::new();
    let Some(entries) = value.get("files").and_then(|files| files.as_object()) else {
        return Ok(ChecksumStatus { files, package });
    };
    for (relative, hash) in entries {
        let Some(hash) = hash.as_str() else {
            return Err(format!("{}: `{relative}` is not a string", path.display()));
        };
        files.insert(relative.clone(), hash.to_string());
    }
    Ok(ChecksumStatus { files, package })
}

pub(crate) fn source_dir(root: &Path, name: &str, version: &Version) -> Result<PathBuf, String> {
    let directory = root.join(format!("{name}-{version}"));
    if !directory.join("Cargo.toml").is_file() {
        return Err(format!(
            "vendored crate `{name} {version}` is missing from `{}`, run `rusted vendor`",
            root.display()
        ));
    }
    let sentinel = directory.join(OK_FILE);
    if sentinel.is_file() {
        return Ok(directory);
    }
    let status = read_checksum_file(&directory)?;
    for (relative, expected) in &status.files {
        verify_file(&directory, relative, expected)?;
    }
    fs::write(&sentinel, "ok").map_err(|error| format!("{}: {error}", sentinel.display()))?;
    Ok(directory)
}

fn verify_file(directory: &Path, relative: &str, expected: &str) -> Result<(), String> {
    let path = safe_join(directory, relative)
        .ok_or_else(|| format!("vendored checksum entry `{relative}` escapes the crate"))?;
    let mut hasher = Sha256::new();
    let mut file = File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut buffer = [0u8; crate::util::READ_BUFFER];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if to_hex(&hasher.finish()).eq_ignore_ascii_case(expected) {
        return Ok(());
    }
    Err(format!(
        "checksum mismatch for vendored file `{}`",
        path.display()
    ))
}

fn safe_join(directory: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let mut path = directory.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            _ => return None,
        }
    }
    Some(path)
}

pub(crate) fn write_crate(
    output: &Path,
    name: &str,
    version: &Version,
    checksum: &str,
    source: &Path,
) -> Result<(), String> {
    let directory = output.join(format!("{name}-{version}"));
    if directory.join(MANIFEST).is_file() {
        return Ok(());
    }
    fs::create_dir_all(&directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    copy_tree(source, &directory)?;
    let mut files = BTreeMap::new();
    let mut pending = vec![directory.clone()];
    while let Some(current) = pending.pop() {
        let read =
            fs::read_dir(&current).map_err(|error| format!("{}: {error}", current.display()))?;
        for entry in read {
            let entry = entry.map_err(|error| format!("{}: {error}", current.display()))?;
            let path = entry.path();
            let relative = path
                .strip_prefix(&directory)
                .map_err(|error| format!("{}: {error}", path.display()))?
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let mut hasher = Sha256::new();
            let mut file =
                File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let mut buffer = [0u8; crate::util::READ_BUFFER];
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            files.insert(relative, to_hex(&hasher.finish()));
        }
    }
    let mut json = String::from("{\"files\":{");
    for (index, (relative, hash)) in files.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\"{}\":\"{hash}\"",
            crate::util::json::escape(relative)
        ));
    }
    json.push_str(&format!(
        "}},\"package\":\"{}\"}}",
        crate::util::json::escape(checksum)
    ));
    fs::write(directory.join(CHECKSUM_FILE), json)
        .map_err(|error| format!("{}: {error}", directory.join(CHECKSUM_FILE).display()))?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    let read = fs::read_dir(source).map_err(|error| format!("{}: {error}", source.display()))?;
    for entry in read {
        let entry = entry.map_err(|error| format!("{}: {error}", source.display()))?;
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            fs::copy(&path, &target)
                .map_err(|error| format!("{} -> {}: {error}", path.display(), target.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{replacement, safe_join, source_dir, write_crate, VendorRegistry};
    use crate::resolver::resolve::Registry as _;
    use crate::resolver::semver::Version;

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rusted-vendor-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn hex(value: &[u8]) -> String {
        crate::util::hash::to_hex(&crate::util::hash::sha256(value))
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn reads_a_replacement_from_cargo_config() {
        let temp = TempDir::new("replacement");
        write(
            &temp.path.join(".cargo/config.toml"),
            "[source.crates-io]\nreplace-with = \"vendored\"\n\n[source.vendored]\ndirectory = \"vendor\"\n",
        );
        std::fs::create_dir_all(temp.path.join("vendor")).unwrap();

        let declared = replacement(&temp.path).unwrap().unwrap();
        assert_eq!(
            declared.directory,
            temp.path.join("vendor").canonicalize().unwrap()
        );
    }

    #[test]
    fn accepts_an_absolute_replacement_directory() {
        let temp = TempDir::new("replacement-absolute");
        let vendor = temp.path.join("elsewhere");
        std::fs::create_dir_all(&vendor).unwrap();
        write(
            &temp.path.join(".cargo/config.toml"),
            &format!(
                "[source.crates-io]\nreplace-with = \"vendored\"\n\n[source.vendored]\ndirectory = \"{}\"\n",
                vendor.display()
            ),
        );

        let declared = replacement(&temp.path).unwrap().unwrap();
        assert_eq!(declared.directory, vendor.canonicalize().unwrap());
    }

    #[test]
    fn ignores_config_without_replacement() {
        let temp = TempDir::new("replacement-none");
        write(&temp.path.join(".cargo/config.toml"), "[build]\njobs = 2\n");

        assert!(replacement(&temp.path).unwrap().is_none());
        assert!(replacement(&temp.path.join("missing")).unwrap().is_none());
    }

    #[test]
    fn rejects_a_replacement_without_a_directory() {
        let temp = TempDir::new("replacement-dangling");
        write(
            &temp.path.join(".cargo/config.toml"),
            "[source.crates-io]\nreplace-with = \"vendored\"\n",
        );

        let error = replacement(&temp.path).err().unwrap();
        assert!(error.contains("vendored"), "{error}");
    }

    #[test]
    fn vendored_crates_round_trip_through_the_checksum_file() {
        let temp = TempDir::new("round-trip");
        let output = temp.path.join("vendor");
        let source = temp.path.join("extracted");
        write(
            &source.join("Cargo.toml"),
            "[package]\nname = \"hello\"\nversion = \"1.0.0\"\n",
        );
        write(
            &source.join("src/lib.rs"),
            "pub fn hello() -> &'static str { \"hello\" }\n",
        );

        write_crate(
            &output,
            "hello",
            &Version::parse("1.0.0").unwrap(),
            &hex(b"tar"),
            &source,
        )
        .unwrap();

        let directory = source_dir(&output, "hello", &Version::parse("1.0.0").unwrap()).unwrap();
        assert_eq!(directory, output.join("hello-1.0.0"));
    }

    #[test]
    fn tampered_vendored_files_are_rejected() {
        let temp = TempDir::new("tamper");
        let output = temp.path.join("vendor");
        let source = temp.path.join("extracted");
        write(
            &source.join("Cargo.toml"),
            "[package]\nname = \"hello\"\nversion = \"1.0.0\"\n",
        );
        write(
            &source.join("src/lib.rs"),
            "pub fn hello() -> &'static str { \"hello\" }\n",
        );
        write_crate(
            &output,
            "hello",
            &Version::parse("1.0.0").unwrap(),
            &hex(b"tar"),
            &source,
        )
        .unwrap();
        source_dir(&output, "hello", &Version::parse("1.0.0").unwrap()).unwrap();
        std::fs::remove_file(output.join("hello-1.0.0/.cargo-ok")).unwrap();
        std::fs::write(output.join("hello-1.0.0/src/lib.rs"), "tampered").unwrap();

        let error = source_dir(&output, "hello", &Version::parse("1.0.0").unwrap())
            .err()
            .unwrap();
        assert!(error.contains("checksum mismatch"), "{error}");
    }

    #[test]
    fn checksum_entries_cannot_escape_the_crate() {
        assert!(safe_join(Path::new("/tmp"), "../evil").is_none());
        assert!(safe_join(Path::new("/tmp"), "/absolute").is_none());
        assert!(safe_join(Path::new("/tmp"), "src/lib.rs").is_some());
    }

    #[test]
    fn vendor_registry_serves_entries_from_directories() {
        let temp = TempDir::new("registry");
        let output = temp.path.join("vendor");
        for (name, version) in [("hello", "1.0.0"), ("hello", "1.2.0"), ("other", "0.1.0")] {
            let directory = output.join(format!("{name}-{version}"));
            write(
                &directory.join("Cargo.toml"),
                &format!(
                    "[package]\nname = \"{name}\"\nversion = \"{version}\"\n\n[features]\nfancy = []\n\n[dependencies]\nplain = \"2\"\n"
                ),
            );
            write(&directory.join("src/lib.rs"), "");
            write(
                &directory.join(".cargo-checksum.json"),
                &format!(
                    "{{\"files\":{{}},\"package\":\"{}\"}}",
                    hex(name.as_bytes())
                ),
            );
        }

        let mut registry = VendorRegistry::new(output);
        let entries = registry.entries("hello").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].version, Version::parse("1.2.0").unwrap());
        assert_eq!(entries[1].version, Version::parse("1.0.0").unwrap());
        assert_eq!(entries[0].checksum, hex(b"hello"));
        assert_eq!(entries[0].features.get("fancy"), Some(&vec![]));
        assert_eq!(entries[0].dependencies.len(), 1);
        assert_eq!(entries[0].dependencies[0].name, "plain");
        assert_eq!(entries[0].dependencies[0].requirement, "2");

        assert!(registry.entries("missing").unwrap().is_empty());
    }
}
