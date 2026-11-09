use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{cache_root, Config};
use crate::util::lock::Lock;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(crate) struct Store {
    root: PathBuf,
}

pub(crate) struct Artifact {
    pub(crate) key: String,
    pub(crate) dir: PathBuf,
}

impl Store {
    pub(crate) fn open(config: &Config) -> Result<Self, String> {
        Self::from_root(cache_root(config)?.join("v1"))
    }

    fn from_root(root: PathBuf) -> Result<Self, String> {
        for directory in ["artifacts", "locks", "tmp"] {
            let path = root.join(directory);
            fs::create_dir_all(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        }
        Ok(Self { root })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn lookup(&self, key: &str, outputs: &[(String, bool)]) -> Option<Artifact> {
        let directory = self.root.join("artifacts").join(key);
        if !directory.is_dir() {
            return None;
        }
        for (name, require_nonzero) in outputs {
            let path = directory.join(name);
            let metadata = fs::metadata(&path).ok()?;
            if !metadata.is_file() {
                return None;
            }
            if *require_nonzero && metadata.len() == 0 {
                return None;
            }
        }
        Some(Artifact {
            key: key.to_string(),
            dir: directory,
        })
    }

    pub(crate) fn lock(&self, key: &str) -> Result<Lock, String> {
        let path = self.root.join("locks").join(format!("{key}.lock"));
        Lock::acquire(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub(crate) fn temp_dir(&self, key: &str) -> Result<PathBuf, String> {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = self
            .root
            .join("tmp")
            .join(format!("{key}-{}-{counter}", std::process::id()));
        if directory.exists() {
            fs::remove_dir_all(&directory)
                .map_err(|error| format!("{}: {error}", directory.display()))?;
        }
        fs::create_dir_all(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        Ok(directory)
    }

    pub(crate) fn commit(&self, key: &str, temp: &Path, meta: &str) -> Result<Artifact, String> {
        let meta_path = temp.join("meta");
        fs::write(&meta_path, meta).map_err(|error| format!("{}: {error}", meta_path.display()))?;

        let directory = self.root.join("artifacts").join(key);
        if directory.exists() {
            fs::remove_dir_all(&directory)
                .map_err(|error| format!("{}: {error}", directory.display()))?;
        }
        fs::rename(temp, &directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;

        let artifact = Artifact {
            key: key.to_string(),
            dir: directory,
        };
        self.touch(&artifact)?;
        Ok(artifact)
    }

    pub(crate) fn touch(&self, artifact: &Artifact) -> Result<(), String> {
        let path = artifact.dir.join("used");
        fs::write(&path, b"").map_err(|error| format!("{}: {error}", path.display()))
    }

    pub(crate) fn link_into(&self, source: &Path, destination: &Path) -> Result<(), String> {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        match fs::remove_file(destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("{}: {error}", destination.display()));
            }
        }
        match fs::hard_link(source, destination) {
            Ok(()) => Ok(()),
            Err(_) => fs::copy(source, destination).map(|_| ()).map_err(|error| {
                format!("{} -> {}: {error}", source.display(), destination.display())
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    use super::Store;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-store-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn outputs() -> Vec<(String, bool)> {
        vec![("libx.rlib".to_string(), true)]
    }

    #[test]
    fn commit_and_lookup_round_trip() {
        let fixture = Fixture::new("round-trip");
        let store = Store::from_root(fixture.dir.join("v1")).unwrap();

        assert!(store.lookup("abc", &outputs()).is_none());

        let temp = store.temp_dir("abc").unwrap();
        std::fs::write(temp.join("libx.rlib"), b"rlib").unwrap();
        let artifact = store.commit("abc", &temp, "key=abc\n").unwrap();

        assert_eq!(artifact.key, "abc");
        let found = store.lookup("abc", &outputs()).unwrap();
        assert_eq!(found.dir, artifact.dir);
        assert!(artifact.dir.join("meta").is_file());
        assert!(artifact.dir.join("used").is_file());
    }

    #[test]
    fn lookup_rejects_missing_or_empty_outputs() {
        let fixture = Fixture::new("corrupt");
        let store = Store::from_root(fixture.dir.join("v1")).unwrap();

        let temp = store.temp_dir("abc").unwrap();
        std::fs::write(temp.join("libx.rlib"), b"rlib").unwrap();
        let artifact = store.commit("abc", &temp, "").unwrap();

        std::fs::write(artifact.dir.join("libx.rlib"), b"").unwrap();
        assert!(store.lookup("abc", &outputs()).is_none());

        std::fs::remove_file(artifact.dir.join("libx.rlib")).unwrap();
        assert!(store.lookup("abc", &outputs()).is_none());
    }

    #[test]
    fn link_into_uses_hardlinks_on_the_same_filesystem() {
        let fixture = Fixture::new("link");
        let store = Store::from_root(fixture.dir.join("v1")).unwrap();
        let source = fixture.dir.join("source");
        std::fs::write(&source, b"binary").unwrap();
        let destination = fixture.dir.join("target/app");

        store.link_into(&source, &destination).unwrap();

        let source_inode = std::fs::metadata(&source).unwrap().ino();
        let destination_inode = std::fs::metadata(&destination).unwrap().ino();
        assert_eq!(source_inode, destination_inode);

        std::fs::remove_file(&destination).unwrap();
        std::fs::write(&destination, b"new").unwrap();
        store.link_into(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"binary");
        assert_eq!(std::fs::metadata(&destination).unwrap().ino(), source_inode);
    }
}
