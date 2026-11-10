use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::manifest::{Edition, Manifest};
use crate::workspace::{PackageKey, Workspace};

use super::plan;

const RUSTFMT_ENV_VAR: &str = "RUSTFMT";
const DEFAULT_RUSTFMT: &str = "rustfmt";
const CHECK_FLAG: &str = "--check";
const NO_FILES_MESSAGE: &str = "no files to format";
const STDERR_TAIL_LINES: usize = 5;
const FAILURE_FILE_TAIL_LINES: usize = 5;

fn target_source_files(dir: &Path, manifest: &Manifest) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(lib) = plan::lib_target(manifest, dir) {
        let path = dir.join(&lib.path);
        if path.is_file() {
            files.push(path);
        }
    }
    for bin in plan::bin_targets(dir, manifest)? {
        files.push(bin.path);
    }
    for test in plan::test_targets(dir, manifest)? {
        files.push(test.path);
    }
    for example in plan::example_targets(dir, manifest)? {
        files.push(example.path);
    }
    for bench in plan::bench_targets(dir, manifest)? {
        files.push(bench.path);
    }
    files.sort();
    files.dedup();
    Ok(files)
}

pub(crate) fn rustfmt_program() -> std::ffi::OsString {
    std::env::var_os(RUSTFMT_ENV_VAR).unwrap_or_else(|| DEFAULT_RUSTFMT.into())
}

pub(crate) fn format(
    workspace: &Workspace,
    roots: &[PackageKey],
    pass_through: &[String],
    check_only: bool,
) -> Result<(), String> {
    let groups = format_groups(workspace, roots)?;
    format_files(&groups, pass_through, check_only)
}

fn format_groups(
    workspace: &Workspace,
    roots: &[PackageKey],
) -> Result<Vec<(Edition, Vec<PathBuf>)>, String> {
    let mut groups: BTreeMap<Edition, Vec<PathBuf>> = BTreeMap::new();
    for key in roots {
        let package = workspace
            .package(key)
            .ok_or_else(|| format!("missing package `{}`", key.name))?;
        if package.manifest.is_virtual {
            continue;
        }
        let files = target_source_files(&package.key.dir, &package.manifest)?;
        groups
            .entry(package.manifest.edition)
            .or_default()
            .extend(files);
    }
    let groups: Vec<(Edition, Vec<PathBuf>)> = groups.into_iter().collect();
    if groups.iter().all(|(_, files)| files.is_empty()) {
        return Err(NO_FILES_MESSAGE.to_string());
    }
    Ok(groups)
}

fn format_files(
    groups: &[(Edition, Vec<PathBuf>)],
    pass_through: &[String],
    check_only: bool,
) -> Result<(), String> {
    let program = rustfmt_program();
    for (edition, files) in groups {
        let mut arguments = vec![
            "--edition".to_string(),
            edition.as_str().to_string(),
        ];
        if check_only {
            arguments.push(CHECK_FLAG.to_string());
        }
        arguments.extend(pass_through.iter().cloned());
        arguments.extend(files.iter().map(|file| file.display().to_string()));

        let output = Command::new(&program)
            .args(&arguments)
            .output()
            .map_err(|error| format!("failed to run rustfmt: {error}"))?;
        if !output.status.success() {
            if check_only {
                return Err(format!(
                    "rustfmt {CHECK_FLAG} found files that need formatting for edition {}: {} ({})",
                    edition.as_str(),
                    file_list_tail(files),
                    stderr_tail(&output.stderr)
                ));
            }
            return Err(format!(
                "rustfmt failed for edition {}: {} ({})",
                edition.as_str(),
                file_list_tail(files),
                stderr_tail(&output.stderr)
            ));
        }
    }
    Ok(())
}

fn file_list_tail(files: &[PathBuf]) -> String {
    let start = files.len().saturating_sub(FAILURE_FILE_TAIL_LINES);
    files[start..]
        .iter()
        .map(|file| file.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(STDERR_TAIL_LINES);
    lines[start..].join(", ")
}

#[cfg(test)]
pub(crate) fn rustfmt_available() -> bool {
    Command::new(rustfmt_program())
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod test_support {
    use std::path::PathBuf;

    pub(crate) struct Fixture {
        pub(crate) dir: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-fmt-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        pub(crate) fn write(&self, relative: &str, contents: &str) -> PathBuf {
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

    // without rustfmt the fmt tests cannot run, CI runners must provide it
    pub(crate) fn rustfmt_or_skip() -> bool {
        const CI_ENV: &str = "CI";
        if super::rustfmt_available() {
            return true;
        }
        if std::env::var_os(CI_ENV).is_some() {
            panic!("rustfmt is missing, but CI runners must provide it");
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::manifest::{Edition, Manifest};

    use super::test_support::{rustfmt_or_skip, Fixture};
    use super::{format_files, format_groups, target_source_files};

    fn manifest(dir: &Path) -> Manifest {
        let path = dir.join("Cargo.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        Manifest::parse(&text, &path).unwrap()
    }

    #[test]
    fn target_source_files_collect_targets_dedup_and_skip_build_scripts() {
        let fixture = Fixture::new("targets");
        fixture.write(
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"demo\"\npath = \"src/lib.rs\"\n\n[[example]]\nname = \"simple\"\n\n[[test]]\nname = \"integration\"\n\n[[bench]]\nname = \"thumbs\"\n",
        );
        fixture.write("src/lib.rs", "pub fn value() -> u32 { 7 }\n");
        fixture.write("examples/simple.rs", "fn main() {}\n");
        fixture.write("tests/integration.rs", "#[test]\nfn ok() {}\n");
        fixture.write("benches/thumbs.rs", "#[bench]\nfn bench() {}\n");
        fixture.write("build.rs", "fn main() {}\n");

        let files = target_source_files(&fixture.dir, &manifest(&fixture.dir)).unwrap();
        assert_eq!(
            files,
            vec![
                fixture.dir.join("benches").join("thumbs.rs"),
                fixture.dir.join("examples").join("simple.rs"),
                fixture.dir.join("src").join("lib.rs"),
                fixture.dir.join("tests").join("integration.rs"),
            ]
        );
        assert!(!files.iter().any(|file| file.ends_with("build.rs")));
    }

    #[test]
    fn target_source_files_skips_a_missing_declared_lib() {
        let fixture = Fixture::new("missing-lib");
        fixture.write(
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/gone.rs\"\n",
        );
        fixture.write("src/main.rs", "fn main() {}\n");
        let files = target_source_files(&fixture.dir, &manifest(&fixture.dir)).unwrap();
        assert_eq!(files, vec![fixture.dir.join("src").join("main.rs")]);
    }

    #[test]
    fn workspace_roots_group_files_by_edition() {
        let workspace = Fixture::new("editions");
        workspace.write("Cargo.toml", "[workspace]\nmembers = [\"old\", \"new\"]\n");
        workspace.write(
            "old/Cargo.toml",
            "[package]\nname = \"old\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        workspace.write("old/src/lib.rs", "pub fn value() -> u32 { 7 }\n");
        workspace.write(
            "new/Cargo.toml",
            "[package]\nname = \"new\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        workspace.write("new/src/lib.rs", "pub fn value() -> u32 { 7 }\n");

        let entry =
            crate::manifest::select::discover_exact(&workspace.dir.join("Cargo.toml")).unwrap();
        let loaded = crate::workspace::Workspace::load(
            &entry,
            crate::config::ManifestChoice::Auto,
        )
        .unwrap();
        let dir = workspace.dir.canonicalize().unwrap();
        let groups = format_groups(&loaded, &loaded.default_targets).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, Edition::E2021);
        assert_eq!(groups[1].0, Edition::E2024);
        assert_eq!(
            groups[0].1,
            vec![dir.join("old").join("src").join("lib.rs")]
        );
        assert_eq!(
            groups[1].1,
            vec![dir.join("new").join("src").join("lib.rs")]
        );
    }

    #[test]
    fn formats_unformatted_files_and_reports_check_failures() {
        if !rustfmt_or_skip() {
            return;
        }
        let fixture = Fixture::new("check");
        fixture.write(
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        let path = fixture.write("src/lib.rs", "fn sample() -> u32 {let x=1;x}\n");
        let files = target_source_files(&fixture.dir, &manifest(&fixture.dir)).unwrap();
        let groups = vec![(Edition::E2021, files)];

        let check = format_files(&groups, &[], true).unwrap_err();
        assert!(check.contains("need formatting"), "{check}");
        assert!(check.contains("lib.rs"), "{check}");

        format_files(&groups, &[], false).unwrap();
        let formatted = std::fs::read_to_string(&path).unwrap();
        assert_ne!(formatted, "fn sample() -> u32 {let x=1;x}\n");

        format_files(&groups, &[], true).unwrap();
    }
}
