use std::path::{Path, PathBuf};

use crate::config::ManifestChoice;

use super::Manifest;

pub(crate) const RUSTED_MANIFEST_FILE: &str = "rusted.toml";
pub(crate) const CARGO_MANIFEST_FILE: &str = "Cargo.toml";

#[derive(Debug)]
pub(crate) struct ManifestFile {
    pub(crate) path: PathBuf,
    pub(crate) manifest: Manifest,
}

impl ManifestFile {
    pub(crate) fn dir(&self) -> &Path {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    pub(crate) fn is_workspace(&self) -> bool {
        self.manifest.is_virtual || self.manifest.workspace.is_some()
    }
}

pub(crate) fn discover(dir: &Path, choice: ManifestChoice) -> Result<ManifestFile, String> {
    match choice {
        ManifestChoice::Cargo => load(dir, CARGO_MANIFEST_FILE),
        ManifestChoice::Rusted => load(dir, RUSTED_MANIFEST_FILE),
        ManifestChoice::Auto => {
            let rusted = dir.join(RUSTED_MANIFEST_FILE);
            let cargo = dir.join(CARGO_MANIFEST_FILE);
            match (rusted.is_file(), cargo.is_file()) {
                (false, false) => Err(format!(
                    "no `{}` or `{}` found in {}",
                    RUSTED_MANIFEST_FILE,
                    CARGO_MANIFEST_FILE,
                    dir.display()
                )),
                (true, false) => load_path(&rusted),
                (false, true) => load_path(&cargo),
                (true, true) => Err(format!(
                    "`{}` and `{}` both exist in {}. Delete one of them, or set `manifest = \"cargo\"` or `manifest = \"rusted\"` in the global config, or `RUSTED_MANIFEST=cargo`",
                    rusted.display(),
                    cargo.display(),
                    dir.display()
                )),
            }
        }
    }
}

pub(crate) fn discover_exact(path: &Path) -> Result<ManifestFile, String> {
    if !path.is_file() {
        return Err(format!("manifest `{}` does not exist", path.display()));
    }
    load_path(path)
}

fn load(dir: &Path, file: &str) -> Result<ManifestFile, String> {
    let path = dir.join(file);
    if !path.is_file() {
        return Err(format!(
            "`{}` not found in {}",
            path.display(),
            dir.display()
        ));
    }
    load_path(&path)
}

fn load_path(path: &Path) -> Result<ManifestFile, String> {
    let input =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let manifest = Manifest::parse(&input, path)?;
    Ok(ManifestFile {
        path: path.to_path_buf(),
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{discover, discover_exact, CARGO_MANIFEST_FILE, RUSTED_MANIFEST_FILE};
    use crate::config::ManifestChoice;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-select-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const PACKAGE: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    #[test]
    fn auto_selects_the_only_manifest() {
        let fixture = Fixture::new("auto-rusted");
        fixture.write(RUSTED_MANIFEST_FILE, PACKAGE);
        let found = discover(&fixture.dir, ManifestChoice::Auto).unwrap();
        assert_eq!(found.path, fixture.dir.join(RUSTED_MANIFEST_FILE));

        let fixture = Fixture::new("auto-cargo");
        fixture.write(CARGO_MANIFEST_FILE, PACKAGE);
        let found = discover(&fixture.dir, ManifestChoice::Auto).unwrap();
        assert_eq!(found.path, fixture.dir.join(CARGO_MANIFEST_FILE));
    }

    #[test]
    fn both_manifest_files_fail_under_auto() {
        let fixture = Fixture::new("both");
        fixture.write(RUSTED_MANIFEST_FILE, PACKAGE);
        fixture.write(CARGO_MANIFEST_FILE, PACKAGE);

        let error = discover(&fixture.dir, ManifestChoice::Auto).unwrap_err();
        assert!(error.contains("both exist"), "{error}");
        assert!(error.contains("manifest = \"cargo\""), "{error}");

        let found = discover(&fixture.dir, ManifestChoice::Cargo).unwrap();
        assert_eq!(found.path, fixture.dir.join(CARGO_MANIFEST_FILE));
    }

    #[test]
    fn pinned_choice_requires_its_file() {
        let fixture = Fixture::new("pinned");
        fixture.write(CARGO_MANIFEST_FILE, PACKAGE);

        let error = discover(&fixture.dir, ManifestChoice::Rusted).unwrap_err();
        assert!(error.contains(RUSTED_MANIFEST_FILE), "{error}");
    }

    #[test]
    fn missing_manifest_reports_the_directory() {
        let fixture = Fixture::new("missing");
        let error = discover(&fixture.dir, ManifestChoice::Auto).unwrap_err();
        assert!(error.contains("found in"), "{error}");
    }

    #[test]
    fn exact_discovery_verifies_the_file() {
        let fixture = Fixture::new("exact");
        let error = discover_exact(&fixture.dir.join("Cargo.toml")).unwrap_err();
        assert!(error.contains("does not exist"), "{error}");

        let path = fixture.write(CARGO_MANIFEST_FILE, PACKAGE);
        let found = discover_exact(&path).unwrap();
        assert_eq!(found.manifest.name, "demo");
    }
}
