use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::plan::{Plan, UnitKind};
use super::{build_script, plan, Artifact};
use crate::resolver::resolve::PackageId;

const RUSTDOC_ENV_VAR: &str = "RUSTDOC";
const DEFAULT_RUSTDOC: &str = "rustdoc";
const PROC_MACRO_CRATE: &str = "proc_macro";
const OUT_DIR_ENV: &str = "OUT_DIR";
const DOT_DOC: &str = "doc";
const INDEX_HTML: &str = "index.html";
const STDERR_TAIL_LINES: usize = 5;

struct Opener {
    program: &'static str,
    arguments: &'static [&'static str],
}

fn opener() -> Opener {
    #[cfg(windows)]
    {
        Opener {
            program: "cmd",
            arguments: &["/C", "start", ""],
        }
    }
    #[cfg(target_os = "macos")]
    {
        Opener {
            program: "open",
            arguments: &[],
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Opener {
            program: "xdg-open",
            arguments: &[],
        }
    }
}

pub(crate) fn rustdoc_program() -> std::ffi::OsString {
    std::env::var_os(RUSTDOC_ENV_VAR).unwrap_or_else(|| DEFAULT_RUSTDOC.into())
}

pub(crate) fn rustdoc_arguments(
    target: &super::DocTarget,
    doc_dir: &Path,
    document_private_items: bool,
    pass_through: &[String],
) -> Vec<String> {
    let mut arguments = vec![
        "--edition".to_string(),
        target.edition.as_str().to_string(),
        "--crate-name".to_string(),
        target.crate_name.clone(),
    ];
    for directory in &target.search_dirs {
        arguments.push("-L".to_string());
        arguments.push(format!("dependency={}", directory.display()));
    }
    for (name, path) in &target.externs {
        arguments.push("--extern".to_string());
        arguments.push(format!("{name}={}", path.display()));
    }
    if target.proc_macro_crate {
        arguments.push("--extern".to_string());
        arguments.push(PROC_MACRO_CRATE.to_string());
    }
    for cfg in &target.cfgs {
        arguments.push("--cfg".to_string());
        arguments.push(cfg.clone());
    }
    if document_private_items {
        arguments.push("--document-private-items".to_string());
    }
    arguments.push("-o".to_string());
    arguments.push(doc_dir.display().to_string());
    arguments.extend(pass_through.iter().cloned());
    arguments.push(target.source.display().to_string());
    arguments
}

pub(crate) fn generate(
    docs: &[super::DocTarget],
    target_root: &Path,
    open: bool,
    document_private_items: bool,
    pass_through: &[String],
) -> Result<(), String> {
    let doc_dir = target_root.join(DOT_DOC);
    std::fs::create_dir_all(&doc_dir).map_err(|error| format!("{}: {error}", doc_dir.display()))?;
    let program = rustdoc_program();
    for target in docs {
        let mut command = Command::new(&program);
        let arguments = rustdoc_arguments(target, &doc_dir, document_private_items, pass_through);
        command.args(&arguments);
        for (key, value) in &target.env {
            command.env(key, value);
        }
        if !target.out_dir.as_os_str().is_empty() {
            command.env(OUT_DIR_ENV, &target.out_dir);
        }
        let output = command.output().map_err(|error| {
            format!("failed to run rustdoc for `{}`: {error}", target.crate_name)
        })?;
        if !output.status.success() {
            return Err(format!(
                "rustdoc for `{}` failed: {}",
                target.crate_name,
                stderr_tail(&output.stderr)
            ));
        }
    }
    if open {
        if let Some(path) = open_index_path(docs, &doc_dir) {
            let opener = opener();
            let status = Command::new(opener.program)
                .args(opener.arguments)
                .arg(&path)
                .status()
                .map_err(|error| format!("failed to open `{}`: {error}", path.display()))?;
            if !status.success() {
                return Err(format!("failed to open `{}`", path.display()));
            }
        }
    }
    Ok(())
}

pub(crate) fn open_index_path(docs: &[super::DocTarget], doc_dir: &Path) -> Option<PathBuf> {
    let target = docs
        .iter()
        .find(|target| !target.proc_macro_crate)
        .or_else(|| docs.first())?;
    Some(doc_dir.join(&target.crate_name).join(INDEX_HTML))
}

pub(super) fn collect_doc_targets(
    plan: &Plan,
    packages: &BTreeMap<PackageId, plan::PackageInfo>,
    build_script_outputs: &BTreeMap<PackageId, build_script::Output>,
    roots: &[PackageId],
    artifacts: &[Option<Artifact>],
    search_dirs: &std::collections::BTreeSet<PathBuf>,
) -> Result<Vec<super::DocTarget>, String> {
    let root_set: std::collections::BTreeSet<&PackageId> = roots.iter().collect();
    let lib_indices = super::lib_units(plan);
    let search_dirs: Vec<PathBuf> = search_dirs.iter().cloned().collect();
    let mut targets = Vec::new();
    for unit in plan.units.iter() {
        match unit.kind {
            UnitKind::Lib | UnitKind::Bin | UnitKind::ProcMacro => {}
            UnitKind::Example | UnitKind::Bench | UnitKind::Test | UnitKind::BuildScript => {
                continue
            }
        }
        if !root_set.contains(&unit.package) {
            continue;
        }
        let package = packages
            .get(&unit.package)
            .ok_or_else(|| {
                format!("missing package `{}` in the resolved graph", unit.package.name)
            })?;
        let mut externs = Vec::new();
        for (name, dependency) in &unit.externs {
            let dependency_index = *lib_indices.get(dependency).ok_or_else(|| {
                format!(
                    "internal error: dependency `{name}` of `{}` has no library unit",
                    unit.package_name
                )
            })?;
            let artifact = artifacts[dependency_index].as_ref().ok_or_else(|| {
                format!(
                    "internal error: dependency `{name}` of `{}` is not built",
                    unit.package_name
                )
            })?;
            let dependency_unit = &plan.units[dependency_index];
            externs.push((
                name.clone(),
                artifact.dir.join(&dependency_unit.output_name),
            ));
        }
        let mut cfgs: Vec<String> = unit
            .features
            .iter()
            .map(|feature| format!("feature=\"{feature}\""))
            .collect();
        let mut env = super::package_environment(package, unit);
        let mut out_dir = PathBuf::new();
        if let Some(output) = build_script_outputs.get(&unit.package) {
            cfgs.extend(output.cfgs.iter().cloned());
            env.extend(output.env.iter().cloned());
            out_dir = output.out_dir.clone();
        }
        targets.push(super::DocTarget {
            crate_name: unit.crate_name.clone(),
            source: unit.source.clone(),
            edition: unit.edition,
            externs,
            search_dirs: search_dirs.clone(),
            cfgs,
            env,
            out_dir,
            proc_macro_crate: unit.kind == UnitKind::ProcMacro,
        });
    }
    Ok(targets)
}

#[cfg(test)]
pub(crate) fn rustdoc_available() -> bool {
    Command::new(rustdoc_program())
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(STDERR_TAIL_LINES);
    lines[start..].join(", ")
}

#[cfg(test)]
mod test_support {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const CI_ENV: &str = "CI";

    pub(crate) struct Fixture {
        pub(crate) dir: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-doc-{}-{name}", std::process::id()));
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

    // without rustdoc the doc tests cannot run, CI runners must provide it
    pub(crate) fn rustdoc_or_skip() -> bool {
        if super::rustdoc_available() {
            return true;
        }
        if std::env::var_os(CI_ENV).is_some() {
            panic!("rustdoc is missing, but CI runners must provide it");
        }
        false
    }

    pub(crate) fn compile_library(source: &Path, output: &Path) {
        let status = Command::new(super::super::rustc_program())
            .arg("--edition")
            .arg("2021")
            .arg("--crate-type")
            .arg("lib")
            .arg("-o")
            .arg(output)
            .arg(source)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::super::DocTarget;
    use super::test_support::{compile_library, rustdoc_or_skip, Fixture};
    use super::{generate, open_index_path, rustdoc_arguments};
    use crate::manifest::Edition;

    fn sample_target(proc_macro: bool) -> DocTarget {
        DocTarget {
            crate_name: "demo".to_string(),
            source: PathBuf::from("src/lib.rs"),
            edition: Edition::E2021,
            externs: vec![(
                "dep_one".to_string(),
                PathBuf::from("/artifacts/libdep_one.rlib"),
            )],
            search_dirs: vec![PathBuf::from("/artifacts")],
            cfgs: vec!["feature=\"fast\"".to_string()],
            env: Vec::new(),
            out_dir: PathBuf::new(),
            proc_macro_crate: proc_macro,
        }
    }

    #[test]
    fn arguments_cover_edition_output_and_search_paths() {
        let target = sample_target(false);
        let arguments = rustdoc_arguments(&target, Path::new("/target/doc"), false, &[]);
        assert!(contains_pair(&arguments, "--edition", "2021"));
        assert!(contains_pair(&arguments, "--crate-name", "demo"));
        assert!(contains_pair(&arguments, "-L", "dependency=/artifacts"));
        assert!(contains_pair(
            &arguments,
            "--extern",
            "dep_one=/artifacts/libdep_one.rlib"
        ));
        assert!(contains_pair(&arguments, "--cfg", "feature=\"fast\""));
        assert!(contains_pair(&arguments, "-o", "/target/doc"));
        assert_eq!(arguments.last().unwrap(), "src/lib.rs");
        assert!(!arguments.contains(&"--document-private-items".to_string()));
    }

    #[test]
    fn arguments_cover_private_items_and_pass_through() {
        let target = sample_target(false);
        let arguments = rustdoc_arguments(
            &target,
            Path::new("/target/doc"),
            true,
            &["-C".to_string(), "panic=abort".to_string()],
        );
        assert!(arguments.contains(&"--document-private-items".to_string()));
        assert!(arguments.contains(&"-C".to_string()));
        assert!(arguments.contains(&"panic=abort".to_string()));
        assert_eq!(arguments.last().unwrap(), "src/lib.rs");
    }

    #[test]
    fn proc_macro_targets_get_the_unpathed_extern() {
        let target = sample_target(true);
        let arguments = rustdoc_arguments(&target, Path::new("/target/doc"), false, &[]);
        assert!(contains_pair(&arguments, "--extern", "proc_macro"));
    }

    #[test]
    fn open_index_prefers_the_first_non_proc_macro_crate() {
        let doc_dir = Path::new("/target/doc");
        let mut target = sample_target(false);
        assert_eq!(
            open_index_path(std::slice::from_ref(&target), doc_dir),
            Some(PathBuf::from("/target/doc/demo/index.html"))
        );
        target.proc_macro_crate = true;
        assert_eq!(
            open_index_path(std::slice::from_ref(&target), doc_dir),
            Some(PathBuf::from("/target/doc/demo/index.html"))
        );
        assert_eq!(open_index_path(&[], doc_dir), None);
    }

    #[test]
    fn opener_matches_the_platform() {
        let opener = super::opener();
        let expected = if cfg!(windows) {
            "cmd"
        } else if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        assert_eq!(opener.program, expected);
    }

    #[test]
    fn generates_html_documentation() {
        if !rustdoc_or_skip() {
            return;
        }
        let fixture = Fixture::new("generate");
        let helpers_dir = fixture.dir.join("helpers");
        let demo_dir = fixture.dir.join("demo");
        let target_root = fixture.dir.join("target");
        let rlibs = target_root.join("rlibs");
        fixture.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"helpers\", \"demo\"]\n",
        );
        fixture.write(
            "helpers/Cargo.toml",
            "[package]\nname = \"helpers\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        fixture.write(
            "helpers/src/lib.rs",
            "/// Helper text.\npub fn helper() -> u32 {\n    7\n}\n",
        );
        fixture.write(
            "demo/Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nhelpers = { path = \"../helpers\" }\n",
        );
        fixture.write(
            "demo/src/lib.rs",
            "pub use helpers::helper;\n\n/// Demo text.\npub fn value() -> u32 {\n    helper()\n}\n",
        );
        std::fs::create_dir_all(&rlibs).unwrap();
        compile_library(
            &helpers_dir.join("src/lib.rs"),
            &rlibs.join("libhelpers.rlib"),
        );

        let helpers = DocTarget {
            crate_name: "helpers".to_string(),
            source: helpers_dir.join("src/lib.rs"),
            edition: Edition::E2021,
            externs: Vec::new(),
            search_dirs: Vec::new(),
            cfgs: Vec::new(),
            env: Vec::new(),
            out_dir: PathBuf::new(),
            proc_macro_crate: false,
        };
        let demo = DocTarget {
            crate_name: "demo".to_string(),
            source: demo_dir.join("src/lib.rs"),
            edition: Edition::E2021,
            externs: vec![("helpers".to_string(), rlibs.join("libhelpers.rlib"))],
            search_dirs: vec![rlibs],
            cfgs: Vec::new(),
            env: Vec::new(),
            out_dir: PathBuf::new(),
            proc_macro_crate: false,
        };
        generate(&[helpers, demo], &target_root, false, false, &[]).unwrap();
        assert!(target_root.join("doc/demo/index.html").is_file());
        assert!(target_root.join("doc/helpers/index.html").is_file());
    }

    #[test]
    fn failing_rustdoc_is_fatal() {
        if !rustdoc_or_skip() {
            return;
        }
        let fixture = Fixture::new("failure");
        let target_root = fixture.dir.join("target");
        fixture.write("broken/src/lib.rs", "this is not rust\n");
        let target = DocTarget {
            crate_name: "broken".to_string(),
            source: fixture.dir.join("broken/src/lib.rs"),
            edition: Edition::E2021,
            externs: Vec::new(),
            search_dirs: Vec::new(),
            cfgs: Vec::new(),
            env: Vec::new(),
            out_dir: PathBuf::new(),
            proc_macro_crate: false,
        };
        let error = generate(&[target], &target_root, false, false, &[]).unwrap_err();
        assert!(error.contains("rustdoc for `broken` failed"), "{error}");
    }

    fn contains_pair(arguments: &[String], flag: &str, value: &str) -> bool {
        arguments
            .windows(2)
            .any(|window| window[0] == flag && window[1] == value)
    }
}
