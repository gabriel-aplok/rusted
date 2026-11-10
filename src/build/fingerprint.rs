use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::util::hash::{to_hex, Sha256};

use super::plan::{Unit, UnitKind};
use super::rustc::Flags;
use super::Mode;

#[derive(Clone)]
pub(crate) struct RustcIdentity {
    commit: String,
    host: String,
    release: String,
}

impl RustcIdentity {
    pub(crate) fn release(&self) -> &str {
        &self.release
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn commit(&self) -> &str {
        &self.commit
    }
}

static IDENTITY: OnceLock<RustcIdentity> = OnceLock::new();

pub(crate) fn rustc_identity() -> Result<RustcIdentity, String> {
    if let Some(identity) = IDENTITY.get() {
        return Ok(identity.clone());
    }
    let identity = rustc_identity_fresh()?;
    let _ = IDENTITY.set(identity.clone());
    Ok(identity)
}

fn rustc_identity_fresh() -> Result<RustcIdentity, String> {
    let output = Command::new(super::rustc::program())
        .arg("-vV")
        .output()
        .map_err(|error| format!("failed to run rustc -vV: {error}"))?;
    if !output.status.success() {
        return Err(format!("rustc -vV failed with {}", output.status));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("rustc -vV output is not UTF-8: {error}"))?;

    let mut commit = None;
    let mut host = None;
    let mut release = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("commit-hash: ") {
            commit = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("host: ") {
            host = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("release: ") {
            release = Some(value.trim().to_string());
        }
    }

    Ok(RustcIdentity {
        commit: commit.ok_or("rustc -vV did not report a commit hash")?,
        host: host.ok_or("rustc -vV did not report a host triple")?,
        release: release.ok_or("rustc -vV did not report a release")?,
    })
}

#[derive(Clone)]
struct Entry {
    size: u64,
    mtime_ns: u128,
    hash: [u8; 32],
}

pub(crate) struct SourceHashCache {
    entries: BTreeMap<String, Entry>,
}

impl SourceHashCache {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub(crate) fn load(path: &Path) -> Self {
        let mut entries = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let Some((values, path)) = line.split_once('\t') else {
                    continue;
                };
                let path = path.replace("%09", "\t");
                let mut values = values.split_whitespace();
                let Some(size) = values.next().and_then(|size| size.parse().ok()) else {
                    continue;
                };
                let Some(mtime_ns) = values.next().and_then(|value| value.parse().ok()) else {
                    continue;
                };
                let Some(hash) = values.next().and_then(|hash| decode_hex(hash)) else {
                    continue;
                };
                entries.insert(
                    path,
                    Entry {
                        size,
                        mtime_ns,
                        hash,
                    },
                );
            }
        }
        Self { entries }
    }

    pub(crate) fn save(&self, path: &Path) -> Result<(), String> {
        let mut text = String::new();
        for (path, entry) in &self.entries {
            let encoded = path.replace('\t', "%09");
            text.push_str(&format!(
                "{} {}\t{}\t{}\n",
                entry.size,
                entry.mtime_ns,
                to_hex(&entry.hash),
                encoded
            ));
        }
        let temp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&temp, text)
            .and_then(|_| std::fs::rename(&temp, path))
            .map_err(|error| format!("{}: {error}", path.display()))
    }

    fn file_hash(&mut self, path: &Path, metadata: &std::fs::Metadata) -> Result<[u8; 32], String> {
        let mtime_ns = match metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        {
            Some(duration) => duration.as_nanos(),
            None => return fresh_file_hash(path),
        };
        let key = path.to_string_lossy().into_owned();
        if let Some(entry) = self.entries.get(&key) {
            if entry.size == metadata.len() && entry.mtime_ns == mtime_ns {
                return Ok(entry.hash);
            }
        }
        let hash = fresh_file_hash(path)?;
        self.entries.insert(
            key,
            Entry {
                size: metadata.len(),
                mtime_ns,
                hash,
            },
        );
        Ok(hash)
    }
}

fn fresh_file_hash(path: &Path) -> Result<[u8; 32], String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; crate::util::READ_BUFFER];
    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finish())
}

fn decode_hex(text: &str) -> Option<[u8; 32]> {
    let bytes: Vec<u8> = text.as_bytes().to_vec();
    let mut out = [0u8; 32];
    let mut index = 0;
    for chunk in bytes.chunks(2) {
        if chunk.len() != 2 {
            return None;
        }
        let high = (chunk[0] as char).to_digit(16)?;
        let low = (chunk[1] as char).to_digit(16)?;
        out[index] = (high * 16 + low) as u8;
        index += 1;
    }
    if index != 32 {
        return None;
    }
    Some(out)
}

pub(crate) fn source_hash(
    package_dir: &Path,
    excluded: &[PathBuf],
    cache: &mut SourceHashCache,
) -> Result<[u8; 32], String> {
    let mut files = Vec::new();
    collect_files(package_dir, package_dir, excluded, &mut files)?;
    files.sort();

    let mut hasher = Sha256::new();

    for path in files {
        let relative = path
            .strip_prefix(package_dir)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        hasher.update(relative.as_os_str().as_encoded_bytes());
        hasher.update(&[0]);

        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            hasher.update(b"link");
            let target = std::fs::read_link(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            hasher.update(target.as_os_str().as_encoded_bytes());
            hasher.update(&[0]);
        } else if metadata.is_file() {
            hasher.update(b"file");
            let hash = cache.file_hash(&path, &metadata)?;
            hasher.update(&metadata.len().to_le_bytes());
            hasher.update(&hash);
        } else if metadata.is_dir() {
            hasher.update(b"dir");
        } else {
            hasher.update(b"other");
        }
        hasher.update(&[0]);
    }

    Ok(hasher.finish())
}

fn collect_files(
    package_dir: &Path,
    directory: &Path,
    excluded: &[PathBuf],
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    entries.sort();

    for path in entries {
        if excluded.iter().any(|excluded| path.starts_with(excluded)) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if metadata.is_dir() {
            let name = path.file_name().and_then(|name| name.to_str());
            if name == Some(".git") {
                continue;
            }
            if path.parent() == Some(package_dir) && name == Some("target") {
                continue;
            }
            collect_files(package_dir, &path, excluded, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

pub(crate) fn artifact_key(
    unit: &Unit,
    flags: &Flags,
    rustc: &RustcIdentity,
    mode: Mode,
    source: &[u8; 32],
    build_script: Option<&str>,
    dependencies: &[(String, String)],
    salt: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();

    field(&mut hasher, "rusted-artifact-v1");
    field(&mut hasher, &rustc.commit);
    field(&mut hasher, &rustc.host);
    field(&mut hasher, &rustc.release);
    field(
        &mut hasher,
        match unit.kind {
            UnitKind::BuildScript => "build-script",
            UnitKind::ProcMacro => "proc-macro",
            UnitKind::Lib | UnitKind::Bin | UnitKind::Test | UnitKind::Example
            | UnitKind::Bench => match mode {
                Mode::Build => "build",
                Mode::Check => "check",
                Mode::Test => "test",
            },
        },
    );
    field(&mut hasher, &unit.package_name);
    field(&mut hasher, &unit.package_version);
    field(
        &mut hasher,
        match unit.kind {
            UnitKind::Lib => "lib",
            UnitKind::Bin => "bin",
            UnitKind::Test => "test",
            UnitKind::Example => "example",
            UnitKind::Bench => "bench",
            UnitKind::BuildScript => "build-script",
            UnitKind::ProcMacro => "proc-macro",
        },
    );
    field(&mut hasher, &unit.crate_name);
    field(&mut hasher, &unit.target_name);
    field(&mut hasher, unit.edition.as_str());
    field(&mut hasher, &flags.cache_key_material());
    field(
        &mut hasher,
        if unit.cap_lints {
            "cap-lints"
        } else {
            "no-cap-lints"
        },
    );
    field(&mut hasher, &to_hex(source));
    field(&mut hasher, build_script.unwrap_or(""));

    let mut features = unit.features.clone();
    features.sort();
    for feature in &features {
        field(&mut hasher, feature);
    }

    let mut dependencies = dependencies.to_vec();
    dependencies.sort();
    for (name, key) in &dependencies {
        field(&mut hasher, name);
        field(&mut hasher, key);
    }

    field(&mut hasher, &salt_key_component(salt));

    to_hex(&hasher.finish())
}

fn salt_key_component(salt: Option<&str>) -> String {
    match salt {
        Some(name) => format!("salt={name}"),
        None => "salt=none".to_string(),
    }
}

fn field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.as_bytes());
    hasher.update(&[0]);
}

#[cfg(test)]
mod key_tests {
    use std::path::PathBuf;

    use crate::build::plan::{Unit, UnitKind};
    use crate::build::rustc::Flags;
    use crate::build::Mode;
    use crate::manifest::Edition;
    use crate::resolver::resolve::{BuildContext, PackageId, PackageSource};
    use crate::resolver::semver::Version;

    use super::{artifact_key, RustcIdentity};

    fn unit() -> Unit {
        Unit {
            package: PackageId {
                name: "demo".to_string(),
                version: Version::parse("0.1.0").unwrap(),
                source: PackageSource::Path(PathBuf::from("/tmp/demo")),
                context: BuildContext::Target,
            },
            package_name: "demo".to_string(),
            package_version: "0.1.0".to_string(),
            package_dir: PathBuf::from("/tmp/demo"),
            crate_name: "demo".to_string(),
            target_name: "demo".to_string(),
            edition: Edition::E2021,
            kind: UnitKind::Lib,
            source: PathBuf::from("/tmp/demo/src/lib.rs"),
            output_name: "libdemo.rlib".to_string(),
            metadata_name: None,
            externs: Vec::new(),
            features: Vec::new(),
            cap_lints: false,
        }
    }

    fn identity(commit: &str) -> RustcIdentity {
        RustcIdentity {
            commit: commit.to_string(),
            host: "x86_64-unknown-linux-gnu".to_string(),
            release: "1.85.1".to_string(),
        }
    }

    #[test]
    fn artifact_key_changes_with_inputs() {
        let unit = unit();
        let flags = Flags::for_test();
        let source = [7u8; 32];
        let base = artifact_key(
            &unit,
            &flags,
            &identity("a"),
            Mode::Build,
            &source,
            None,
            &[],
            None,
        );

        assert_eq!(
            base,
            artifact_key(
                &unit,
                &flags,
                &identity("a"),
                Mode::Build,
                &source,
                None,
                &[],
                None
            )
        );
        assert_ne!(
            base,
            artifact_key(
                &unit,
                &flags,
                &identity("b"),
                Mode::Build,
                &source,
                None,
                &[],
                None
            )
        );
        assert_ne!(
            base,
            artifact_key(
                &unit,
                &flags,
                &identity("a"),
                Mode::Check,
                &source,
                None,
                &[],
                None
            )
        );
        assert_ne!(
            base,
            artifact_key(
                &unit,
                &flags,
                &identity("a"),
                Mode::Build,
                &[8u8; 32],
                None,
                &[],
                None
            )
        );
        assert_ne!(
            base,
            artifact_key(
                &unit,
                &flags,
                &identity("a"),
                Mode::Build,
                &source,
                None,
                &[("dep".to_string(), "key".to_string())],
                None
            )
        );

        let mut changed = unit;
        changed.crate_name = "other".to_string();
        assert_ne!(
            base,
            artifact_key(
                &changed,
                &flags,
                &identity("a"),
                Mode::Build,
                &source,
                None,
                &[],
                None
            )
        );
    }

    #[test]
    fn artifact_key_salt_separates_clippy_from_check() {
        let unit = unit();
        let flags = Flags::for_test();
        let source = [7u8; 32];
        let base = artifact_key(
            &unit,
            &flags,
            &identity("a"),
            Mode::Check,
            &source,
            None,
            &[],
            None,
        );
        let salted = artifact_key(
            &unit,
            &flags,
            &identity("a"),
            Mode::Check,
            &source,
            None,
            &[],
            Some(crate::build::CLIPPY_ARTIFACT_SALT),
        );
        assert_ne!(base, salted);
        assert_ne!(
            base,
            artifact_key(
                &unit,
                &flags,
                &identity("a"),
                Mode::Check,
                &source,
                None,
                &[],
                Some("other")
            )
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{source_hash, SourceHashCache};

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-hash-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("src/lib.rs"), "pub fn value() -> u32 { 1 }\n").unwrap();
            std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
            Self { dir }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn source_hash_is_stable_and_content_sensitive() {
        let fixture = Fixture::new("stable");
        let first = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();
        let second = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();
        assert_eq!(first, second);

        std::fs::write(
            fixture.dir.join("src/lib.rs"),
            "pub fn value() -> u32 { 2 }\n",
        )
        .unwrap();
        let changed = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();
        assert_ne!(first, changed);
    }

    #[test]
    fn source_hash_cache_round_trips_and_tolerates_garbage() {
        let directory = std::env::temp_dir().join(format!(
            "rusted-hash-cache-{}-{}",
            std::process::id(),
            "roundtrip"
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("hash-cache");

        let garbage_body = "not a real cache line\n";
        std::fs::write(&path, garbage_body).unwrap();
        let cache = SourceHashCache::load(&path);
        assert_eq!(cache.entries.len(), 0);

        let source = directory.join("lib.rs");
        std::fs::write(&source, "pub fn value() -> u32 { 7 }\n").unwrap();
        let mut cache = SourceHashCache::new();
        let metadata = std::fs::metadata(&source).unwrap();
        let hash = cache.file_hash(&source, &metadata).unwrap();

        cache.save(&path).unwrap();
        let mut loaded = SourceHashCache::load(&path);
        let metadata = std::fs::metadata(&source).unwrap();
        assert_eq!(loaded.file_hash(&source, &metadata).unwrap(), hash);
        assert!(std::fs::write(&source, "pub fn changed() {}\n").is_ok());
        let metadata = std::fs::metadata(&source).unwrap();
        let current = loaded.file_hash(&source, &metadata).unwrap();
        assert_ne!(current, hash);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn source_hash_ignores_target_and_excluded_directories() {
        let fixture = Fixture::new("excluded");
        let before = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();

        std::fs::create_dir_all(fixture.dir.join("target/debug")).unwrap();
        std::fs::write(fixture.dir.join("target/debug/junk"), "junk").unwrap();
        assert_eq!(
            before,
            source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap()
        );

        let cache = fixture.dir.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("artifact"), "data").unwrap();
        assert_eq!(
            before,
            source_hash(&fixture.dir, &[cache.clone()], &mut SourceHashCache::new()).unwrap()
        );
        assert_ne!(
            before,
            source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_hash_tracks_names_and_symlinks() {
        let fixture = Fixture::new("names");
        let before = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();

        std::fs::rename(
            fixture.dir.join("src/lib.rs"),
            fixture.dir.join("src/other.rs"),
        )
        .unwrap();
        let renamed = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();
        assert_ne!(before, renamed);

        std::os::unix::fs::symlink("other.rs", fixture.dir.join("src/link.rs")).unwrap();
        let linked = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();
        assert_ne!(renamed, linked);
    }

    #[test]
    fn source_hash_tracks_renamed_files() {
        let fixture = Fixture::new("renames");
        let before = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();

        std::fs::rename(
            fixture.dir.join("src/lib.rs"),
            fixture.dir.join("src/other.rs"),
        )
        .unwrap();
        let renamed = source_hash(&fixture.dir, &[], &mut SourceHashCache::new()).unwrap();
        assert_ne!(before, renamed);
    }
}
