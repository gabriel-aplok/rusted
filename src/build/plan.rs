use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::manifest::{Edition, Manifest, TargetSpec};
use crate::resolver::resolve::{BuildContext, PackageId, ResolvedDependency};

use super::Mode;

#[cfg(windows)]
const PROC_MACRO_DYLIB_EXTENSION: &str = "dll";
#[cfg(target_os = "macos")]
const PROC_MACRO_DYLIB_EXTENSION: &str = "dylib";
#[cfg(not(any(windows, target_os = "macos")))]
const PROC_MACRO_DYLIB_EXTENSION: &str = "so";

#[derive(Clone)]
pub(crate) struct PackageInfo {
    pub(crate) id: PackageId,
    pub(crate) manifest: Manifest,
    pub(crate) manifest_path: PathBuf,
    pub(crate) dir: PathBuf,
    pub(crate) features: BTreeSet<String>,
    pub(crate) dependencies: Vec<ResolvedDependency>,
}

#[derive(Clone)]
#[derive(Debug)]
pub(crate) struct Unit {
    pub(crate) package: PackageId,
    pub(crate) package_name: String,
    pub(crate) package_version: String,
    pub(crate) package_dir: PathBuf,
    pub(crate) crate_name: String,
    pub(crate) target_name: String,
    pub(crate) edition: Edition,
    pub(crate) kind: UnitKind,
    pub(crate) source: PathBuf,
    pub(crate) output_name: String,
    pub(crate) metadata_name: Option<String>,
    pub(crate) externs: Vec<(String, PackageId)>,
    pub(crate) features: Vec<String>,
    pub(crate) cap_lints: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitKind {
    Lib,
    Bin,
    Test,
    Example,
    Bench,
    BuildScript,
    ProcMacro,
}

#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) units: Vec<Unit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetList {
    None,
    All,
    Some(Vec<String>),
}

#[derive(Debug)]
pub(crate) struct TargetSelection {
    pub(crate) examples: TargetList,
    pub(crate) benches: TargetList,
}

impl TargetList {
    fn selects(&self, name: &str) -> bool {
        match self {
            TargetList::None => false,
            TargetList::All => true,
            TargetList::Some(names) => names.iter().any(|candidate| candidate == name),
        }
    }
}

impl TargetSelection {
    pub(crate) fn is_empty(&self) -> bool {
        matches!(self.examples, TargetList::None) && matches!(self.benches, TargetList::None)
    }
}

struct LibTarget {
    name: String,
    path: String,
    proc_macro: bool,
    crate_types: Vec<String>,
}

struct BinTarget {
    name: String,
    path: PathBuf,
}

struct NamedTarget {
    name: String,
    path: PathBuf,
}

pub(crate) fn plan(
    packages: &BTreeMap<PackageId, PackageInfo>,
    roots: &[PackageId],
    mode: Mode,
    selection: &TargetSelection,
) -> Result<Plan, String> {
    let root_set: BTreeSet<&PackageId> = roots.iter().collect();

    let mut lib_outputs: BTreeMap<PackageId, (String, String)> = BTreeMap::new();
    for (id, package) in packages {
        validate_package(package)?;
        if let Some(lib) = lib_target(&package.manifest, &package.dir) {
            let crate_name = normalize_name(&lib.name);
            let output_name = if lib.proc_macro {
                format!("lib{crate_name}.{PROC_MACRO_DYLIB_EXTENSION}")
            } else {
                match mode {
                    Mode::Check => format!("lib{crate_name}.rmeta"),
                    Mode::Build | Mode::Test => format!("lib{crate_name}.rlib"),
                }
            };
            lib_outputs.insert(id.clone(), (crate_name, output_name));
        }
    }

    let mut outputs: BTreeMap<String, String> = BTreeMap::new();
    let mut example_outputs: BTreeMap<String, String> = BTreeMap::new();
    let mut example_names: BTreeSet<String> = BTreeSet::new();
    let mut bench_names: BTreeSet<String> = BTreeSet::new();

    let mut units = Vec::new();
    for (id, package) in packages {
        let normal_externs = externs_for(package, &lib_outputs, mode, false, false)?;
        let build_externs = externs_for(package, &lib_outputs, mode, false, true)?;
        let dev_externs = externs_for(package, &lib_outputs, mode, true, false)?;
        let cap_lints = !root_set.contains(id);

        if let Some(script) = build_script_path(&package.dir, &package.manifest)? {
            units.push(Unit {
                package: id.clone(),
                package_name: package.manifest.name.clone(),
                package_version: package.manifest.version.clone(),
                package_dir: package.dir.clone(),
                crate_name: "build_script".to_string(),
                target_name: format!("{} (build script)", package.manifest.name),
                edition: package.manifest.edition,
                kind: UnitKind::BuildScript,
                source: script,
                output_name: "build-script".to_string(),
                metadata_name: None,
                externs: build_externs,
                features: package.features.iter().cloned().collect(),
                cap_lints,
            });
        }

        if let Some(lib) = lib_target(&package.manifest, &package.dir) {
            let source = package.dir.join(&lib.path);
            if !source.is_file() {
                return Err(format!(
                    "library source `{}` does not exist in package `{}`",
                    source.display(),
                    id.name
                ));
            }
            let (crate_name, output_name) = lib_outputs
                .get(id)
                .cloned()
                .ok_or_else(|| format!("missing library output for `{}`", id.name))?;
            let metadata_name = if !lib.proc_macro && mode == Mode::Build {
                Some(format!("lib{crate_name}.rmeta"))
            } else {
                None
            };
            units.push(Unit {
                package: id.clone(),
                package_name: package.manifest.name.clone(),
                package_version: package.manifest.version.clone(),
                package_dir: package.dir.clone(),
                crate_name,
                target_name: package.manifest.name.clone(),
                edition: package.manifest.edition,
                kind: if lib.proc_macro {
                    UnitKind::ProcMacro
                } else {
                    UnitKind::Lib
                },
                source,
                output_name,
                metadata_name,
                externs: normal_externs.clone(),
                features: package.features.iter().cloned().collect(),
                cap_lints,
            });
        }

        if !root_set.contains(id) {
            continue;
        }

        match mode {
            Mode::Build | Mode::Check => {
                let bins = if package.id.context == BuildContext::Target {
                    bin_targets(&package.dir, &package.manifest)?
                } else {
                    Vec::new()
                };
                if lib_target(&package.manifest, &package.dir).is_none() && bins.is_empty() {
                    return Err(format!(
                        "package `{}` has no library or binary targets to build",
                        id.name
                    ));
                }
                for bin in bins {
                    let crate_name = normalize_name(&bin.name);
                    let mut externs = normal_externs.clone();
                    add_own_lib(&mut externs, id, &lib_outputs, package)?;
                    let output_name = match mode {
                        Mode::Check => format!("{crate_name}.rmeta"),
                        Mode::Build | Mode::Test => bin.name.clone(),
                    };
                    if let Some(previous) = outputs.insert(output_name.clone(), id.name.clone()) {
                        return Err(format!(
                            "packages `{previous}` and `{}` both produce `{output_name}`",
                            id.name
                        ));
                    }
                    units.push(Unit {
                        package: id.clone(),
                        package_name: package.manifest.name.clone(),
                        package_version: package.manifest.version.clone(),
                        package_dir: package.dir.clone(),
                        crate_name,
                        target_name: bin.name,
                        edition: package.manifest.edition,
                        kind: UnitKind::Bin,
                        source: bin.path,
                        output_name,
                        metadata_name: None,
                        externs,
                        features: package.features.iter().cloned().collect(),
                        cap_lints,
                    });
                }
            }
            Mode::Test => {
                let target_context = package.id.context == BuildContext::Target;
                let bins = if target_context {
                    bin_targets(&package.dir, &package.manifest)?
                } else {
                    Vec::new()
                };
                let tests = if target_context {
                    test_targets(&package.dir, &package.manifest)?
                } else {
                    Vec::new()
                };
                let has_lib = lib_target(&package.manifest, &package.dir).is_some();
                if target_context && !has_lib && bins.is_empty() && tests.is_empty() {
                    return Err(format!("package `{}` has no tests to run", id.name));
                }

                if target_context {
                    if let Some(lib) = lib_target(&package.manifest, &package.dir) {
                        let source = package.dir.join(&lib.path);
                        let (lib_crate_name, _) = lib_outputs
                            .get(id)
                            .cloned()
                            .ok_or_else(|| format!("missing library output for `{}`", id.name))?;
                        let output_name = format!("lib{lib_crate_name}-test");
                        if let Some(previous) = outputs.insert(output_name.clone(), id.name.clone())
                        {
                            return Err(format!(
                                "packages `{previous}` and `{}` both produce `{output_name}`",
                                id.name
                            ));
                        }
                        units.push(Unit {
                            package: id.clone(),
                            package_name: package.manifest.name.clone(),
                            package_version: package.manifest.version.clone(),
                            package_dir: package.dir.clone(),
                            crate_name: lib_crate_name,
                            target_name: format!("{} (lib)", package.manifest.name),
                            edition: package.manifest.edition,
                            kind: UnitKind::Test,
                            source,
                            output_name,
                            metadata_name: None,
                            externs: normal_externs.clone(),
                            features: package.features.iter().cloned().collect(),
                            cap_lints,
                        });
                    }
                }

                for bin in bins {
                    let crate_name = normalize_name(&bin.name);
                    let mut externs = normal_externs.clone();
                    add_own_lib(&mut externs, id, &lib_outputs, package)?;
                    let output_name = format!("{crate_name}-test");
                    if let Some(previous) = outputs.insert(output_name.clone(), id.name.clone()) {
                        return Err(format!(
                            "packages `{previous}` and `{}` both produce `{output_name}`",
                            id.name
                        ));
                    }
                    units.push(Unit {
                        package: id.clone(),
                        package_name: package.manifest.name.clone(),
                        package_version: package.manifest.version.clone(),
                        package_dir: package.dir.clone(),
                        crate_name,
                        target_name: bin.name,
                        edition: package.manifest.edition,
                        kind: UnitKind::Test,
                        source: bin.path,
                        output_name,
                        metadata_name: None,
                        externs,
                        features: package.features.iter().cloned().collect(),
                        cap_lints,
                    });
                }

                for test in tests {
                    let crate_name = normalize_name(&test.name);
                    let mut externs = normal_externs.clone();
                    add_own_lib(&mut externs, id, &lib_outputs, package)?;
                    let output_name = format!("{crate_name}-test");
                    if let Some(previous) = outputs.insert(output_name.clone(), id.name.clone()) {
                        return Err(format!(
                            "packages `{previous}` and `{}` both produce `{output_name}`",
                            id.name
                        ));
                    }
                    units.push(Unit {
                        package: id.clone(),
                        package_name: package.manifest.name.clone(),
                        package_version: package.manifest.version.clone(),
                        package_dir: package.dir.clone(),
                        crate_name,
                        target_name: test.name,
                        edition: package.manifest.edition,
                        kind: UnitKind::Test,
                        source: test.path,
                        output_name,
                        metadata_name: None,
                        externs,
                        features: package.features.iter().cloned().collect(),
                        cap_lints,
                    });
                }
            }
        }

        let target_context = package.id.context == BuildContext::Target;

        if target_context && !matches!(selection.examples, TargetList::None) {
            for target in example_targets(&package.dir, &package.manifest)? {
                example_names.insert(target.name.clone());
                if !selection.examples.selects(&target.name) {
                    continue;
                }
                let crate_name = normalize_name(&target.name);
                let mut externs = dev_externs.clone();
                add_own_lib(&mut externs, id, &lib_outputs, package)?;
                let output_name = match mode {
                    Mode::Check => format!("{crate_name}.rmeta"),
                    Mode::Build | Mode::Test => target.name.clone(),
                };
                if let Some(previous) =
                    example_outputs.insert(output_name.clone(), id.name.clone())
                {
                    return Err(format!(
                        "packages `{previous}` and `{}` both produce `{output_name}`",
                        id.name
                    ));
                }
                units.push(Unit {
                    package: id.clone(),
                    package_name: package.manifest.name.clone(),
                    package_version: package.manifest.version.clone(),
                    package_dir: package.dir.clone(),
                    target_name: target.name.clone(),
                    crate_name,
                    edition: package.manifest.edition,
                    kind: UnitKind::Example,
                    source: target.path,
                    output_name,
                    metadata_name: None,
                    externs,
                    features: package.features.iter().cloned().collect(),
                    cap_lints,
                });
            }
        }

        if target_context && !matches!(selection.benches, TargetList::None) {
            for target in bench_targets(&package.dir, &package.manifest)? {
                bench_names.insert(target.name.clone());
                if !selection.benches.selects(&target.name) {
                    continue;
                }
                let crate_name = normalize_name(&target.name);
                let mut externs = dev_externs.clone();
                add_own_lib(&mut externs, id, &lib_outputs, package)?;
                let output_name = format!("{crate_name}-test");
                if let Some(previous) = outputs.insert(output_name.clone(), id.name.clone()) {
                    return Err(format!(
                        "packages `{previous}` and `{}` both produce `{output_name}`",
                        id.name
                    ));
                }
                units.push(Unit {
                    package: id.clone(),
                    package_name: package.manifest.name.clone(),
                    package_version: package.manifest.version.clone(),
                    package_dir: package.dir.clone(),
                    target_name: target.name.clone(),
                    crate_name,
                    edition: package.manifest.edition,
                    kind: UnitKind::Bench,
                    source: target.path,
                    output_name,
                    metadata_name: None,
                    externs,
                    features: package.features.iter().cloned().collect(),
                    cap_lints,
                });
            }
        }
    }

    validate_selection(&selection.examples, &example_names, "example")?;
    validate_selection(&selection.benches, &bench_names, "bench")?;

    Ok(Plan { units })
}

fn validate_selection(
    list: &TargetList,
    available: &BTreeSet<String>,
    kind: &str,
) -> Result<(), String> {
    let TargetList::Some(names) = list else {
        return Ok(());
    };
    for name in names {
        if available.contains(name.as_str()) {
            continue;
        }
        let found = if available.is_empty() {
            "none".to_string()
        } else {
            available.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        return Err(format!(
            "no {kind} target named `{name}` in the selected packages, available: {found}"
        ));
    }
    Ok(())
}

fn validate_package(package: &PackageInfo) -> Result<(), String> {
    let unsupported = package.manifest.unsupported_features();
    if !unsupported.is_empty() {
        return Err(format!(
            "package `{}` uses features rusted does not support yet: {}",
            package.id.name,
            unsupported.join(", ")
        ));
    }

    if package.manifest.links.is_some()
        && build_script_path(&package.dir, &package.manifest)?.is_none()
    {
        return Err(format!(
            "package `{}` declares `links` but has no build script",
            package.id.name
        ));
    }

    if let Some(lib) = lib_target(&package.manifest, &package.dir) {
        if !lib.proc_macro {
            for crate_type in &lib.crate_types {
                if crate_type != "lib" && crate_type != "rlib" {
                    return Err(format!(
                        "package `{}` uses unsupported crate type `{crate_type}`",
                        package.id.name
                    ));
                }
            }
        }
    }

    Ok(())
}

fn externs_for(
    package: &PackageInfo,
    lib_outputs: &BTreeMap<PackageId, (String, String)>,
    mode: Mode,
    dev_dependencies: bool,
    build_dependencies: bool,
) -> Result<Vec<(String, PackageId)>, String> {
    let mut externs: BTreeMap<String, PackageId> = BTreeMap::new();
    for edge in &package.dependencies {
        let include = if build_dependencies {
            edge.kind == crate::manifest::DependencyKind::Build
        } else {
            edge.kind == crate::manifest::DependencyKind::Normal
                || ((mode == Mode::Test || dev_dependencies)
                    && edge.kind == crate::manifest::DependencyKind::Dev)
        };
        if !include {
            continue;
        }
        if !lib_outputs.contains_key(&edge.package) {
            return Err(format!(
                "package `{}` depends on `{}`, but `{}` has no library target ({:?})",
                package.id.name, edge.extern_name, edge.package.name, edge.package
            ));
        }
        let name = normalize_name(&edge.extern_name);
        if let Some(previous) = externs.insert(name.clone(), edge.package.clone()) {
            if previous != edge.package {
                return Err(format!(
                    "package `{}` has two different dependencies named `{name}`",
                    package.id.name
                ));
            }
        }
    }
    Ok(externs.into_iter().collect())
}

fn add_own_lib(
    externs: &mut Vec<(String, PackageId)>,
    id: &PackageId,
    lib_outputs: &BTreeMap<PackageId, (String, String)>,
    package: &PackageInfo,
) -> Result<(), String> {
    if let Some((lib_name, _)) = lib_outputs.get(id) {
        if externs.iter().any(|(name, _)| name == lib_name) {
            return Err(format!(
                "package `{}` has a library and a dependency that both use the extern name `{lib_name}`",
                package.id.name
            ));
        }
        externs.push((lib_name.clone(), id.clone()));
    }
    Ok(())
}

fn build_script_path(dir: &Path, manifest: &Manifest) -> Result<Option<PathBuf>, String> {
    match &manifest.build {
        Some(crate::manifest::BuildScript::Disabled) => Ok(None),
        Some(crate::manifest::BuildScript::Path(path)) => {
            let path = dir.join(path);
            if path.is_file() {
                Ok(Some(path))
            } else {
                Err(format!(
                    "build script `{}` does not exist for package `{}`",
                    path.display(),
                    manifest.name
                ))
            }
        }
        None => {
            let path = dir.join("build.rs");
            if path.is_file() {
                Ok(Some(path))
            } else {
                Ok(None)
            }
        }
    }
}

fn lib_target(manifest: &Manifest, dir: &Path) -> Option<LibTarget> {
    if let Some(lib) = &manifest.lib {
        return Some(LibTarget {
            name: lib.name.clone().unwrap_or_else(|| manifest.name.clone()),
            path: lib.path.clone().unwrap_or_else(|| "src/lib.rs".to_string()),
            proc_macro: lib.proc_macro.unwrap_or(false),
            crate_types: lib.crate_types.clone(),
        });
    }
    if manifest.autodiscovery.lib == Some(false) {
        return None;
    }
    if dir.join("src/lib.rs").is_file() {
        Some(LibTarget {
            name: manifest.name.clone(),
            path: "src/lib.rs".to_string(),
            proc_macro: false,
            crate_types: Vec::new(),
        })
    } else {
        None
    }
}

fn bin_targets(dir: &Path, manifest: &Manifest) -> Result<Vec<BinTarget>, String> {
    let package_name = &manifest.name;
    let mut bins: Vec<BinTarget> = Vec::new();

    for target in &manifest.bins {
        let name = target.name.clone().unwrap_or_else(|| package_name.clone());
        let path = match &target.path {
            Some(path) => dir.join(path),
            None => default_bin_path(dir, &name, package_name)?,
        };
        bins.push(BinTarget { name, path });
    }

    if manifest.autodiscovery.bins != Some(false) {
        let main = dir.join("src/main.rs");
        if main.is_file() && !bins.iter().any(|bin| bin.path == main) {
            bins.push(BinTarget {
                name: package_name.clone(),
                path: main,
            });
        }

        let bin_dir = dir.join("src/bin");
        if bin_dir.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&bin_dir)
                .map_err(|error| format!("{}: {error}", bin_dir.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .collect();
            entries.sort();
            for path in entries {
                if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    if bins.iter().any(|bin| bin.path == path) {
                        continue;
                    }
                    let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                        continue;
                    };
                    bins.push(BinTarget {
                        name: name.to_string(),
                        path,
                    });
                } else if path.is_dir() {
                    let main = path.join("main.rs");
                    if main.is_file() && !bins.iter().any(|bin| bin.path == main) {
                        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                            continue;
                        };
                        bins.push(BinTarget {
                            name: name.to_string(),
                            path: main,
                        });
                    }
                }
            }
        }
    }

    for bin in &bins {
        if !bin.path.is_file() {
            return Err(format!(
                "binary `{}` of package `{}` points at missing source `{}`",
                bin.name,
                package_name,
                bin.path.display()
            ));
        }
    }

    Ok(bins)
}

fn default_bin_path(dir: &Path, name: &str, package_name: &str) -> Result<PathBuf, String> {
    let flat = dir.join("src/bin").join(format!("{name}.rs"));
    if flat.is_file() {
        return Ok(flat);
    }
    let nested = dir.join("src/bin").join(name).join("main.rs");
    if nested.is_file() {
        return Ok(nested);
    }
    if name == package_name {
        let main = dir.join("src/main.rs");
        if main.is_file() {
            return Ok(main);
        }
    }
    Err(format!(
        "binary `{name}` of package `{package_name}` has no `path` and no default source file"
    ))
}

fn test_targets(dir: &Path, manifest: &Manifest) -> Result<Vec<NamedTarget>, String> {
    named_targets(
        dir,
        manifest,
        "test",
        "tests",
        &manifest.tests,
        manifest.autodiscovery.tests,
    )
}

fn example_targets(dir: &Path, manifest: &Manifest) -> Result<Vec<NamedTarget>, String> {
    named_targets(
        dir,
        manifest,
        "example",
        "examples",
        &manifest.examples,
        manifest.autodiscovery.examples,
    )
}

fn bench_targets(dir: &Path, manifest: &Manifest) -> Result<Vec<NamedTarget>, String> {
    for spec in &manifest.benches {
        if spec.harness != Some(false) {
            continue;
        }
        let name = match (&spec.name, &spec.path) {
            (Some(name), _) => name.clone(),
            (None, Some(path)) => derive_target_name(Path::new(path)),
            (None, None) => {
                return Err(format!(
                    "bench target in package `{}` uses `harness = false`",
                    manifest.name
                ))
            }
        };
        return Err(format!(
            "package `{}` bench target `{name}` uses `harness = false`, which rusted does not support yet",
            manifest.name
        ));
    }
    named_targets(
        dir,
        manifest,
        "bench",
        "benches",
        &manifest.benches,
        manifest.autodiscovery.benches,
    )
}

fn named_targets(
    dir: &Path,
    manifest: &Manifest,
    kind: &str,
    directory_name: &str,
    specs: &[TargetSpec],
    autodiscover: Option<bool>,
) -> Result<Vec<NamedTarget>, String> {
    let mut targets: Vec<NamedTarget> = Vec::new();
    let package_name = &manifest.name;
    let directory = dir.join(directory_name);

    for spec in specs {
        let name = match spec.name.clone() {
            Some(name) => name,
            None => match &spec.path {
                Some(path) => derive_target_name(Path::new(path)),
                None => {
                    return Err(format!(
                        "{kind} target in package `{package_name}` needs a name or a path"
                    ))
                }
            },
        };
        let path = match &spec.path {
            Some(path) => dir.join(path),
            None => default_named_path(&directory, kind, &name)?,
        };
        targets.push(NamedTarget { name, path });
    }

    if autodiscover != Some(false) {
        let mut entries: Vec<PathBuf> = if directory.is_dir() {
            std::fs::read_dir(&directory)
                .map_err(|error| format!("{}: {error}", directory.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .collect()
        } else {
            Vec::new()
        };
        entries.sort();
        for path in entries {
            if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                if targets.iter().any(|target| target.path == path) {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                targets.push(NamedTarget {
                    name: name.to_string(),
                    path,
                });
            } else if path.is_dir() {
                let main = path.join("main.rs");
                if main.is_file() && !targets.iter().any(|target| target.path == main) {
                    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    targets.push(NamedTarget {
                        name: name.to_string(),
                        path: main,
                    });
                }
            }
        }
    }

    for target in &targets {
        if !target.path.is_file() {
            return Err(format!(
                "{kind} `{}` of package `{package_name}` points at missing source `{}`",
                target.name,
                target.path.display()
            ));
        }
    }

    Ok(targets)
}

fn derive_target_name(path: &Path) -> String {
    if path.file_name().and_then(|name| name.to_str()) == Some("main.rs") {
        if let Some(parent) = path.parent().and_then(|parent| parent.file_name()) {
            return parent.to_string_lossy().into_owned();
        }
    }
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "target".to_string())
}

fn default_named_path(directory: &Path, kind: &str, name: &str) -> Result<PathBuf, String> {
    let flat = directory.join(format!("{name}.rs"));
    if flat.is_file() {
        return Ok(flat);
    }
    let nested = directory.join(name).join("main.rs");
    if nested.is_file() {
        return Ok(nested);
    }
    Err(format!(
        "{kind} `{name}` has no path and no default source file in `{}`",
        directory.display()
    ))
}

pub(crate) fn normalize_name(name: &str) -> String {
    name.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use super::{normalize_name, plan, PackageInfo, TargetList, TargetSelection, UnitKind};
    use crate::build::Mode;
    use crate::manifest::Manifest;
    use crate::resolver::resolve::{BuildContext, PackageId, PackageSource};
    use crate::resolver::semver::Version;

    #[test]
    fn crate_names_use_underscores() {
        assert_eq!(normalize_name("foo-bar"), "foo_bar");
        assert_eq!(normalize_name("plain"), "plain");
    }

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str, manifest: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rusted-plan-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("src/lib.rs"), "pub fn value() -> u32 { 7 }\n").unwrap();
            std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
            for (file, body) in files {
                let path = dir.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, body).unwrap();
            }
            Self { dir }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const BASE_MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

    fn packages(fixture: &Fixture) -> BTreeMap<PackageId, PackageInfo> {
        let id = PackageId {
            name: "demo".to_string(),
            version: Version::parse("0.1.0").unwrap(),
            source: PackageSource::Path(fixture.dir.clone()),
            context: BuildContext::Target,
        };
        let manifest_path = fixture.dir.join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let manifest = Manifest::parse(&text, &manifest_path).unwrap();
        let mut packages = BTreeMap::new();
        packages.insert(
            id.clone(),
            PackageInfo {
                id,
                manifest,
                manifest_path,
                dir: fixture.dir.clone(),
                features: BTreeSet::new(),
                dependencies: Vec::new(),
            },
        );
        packages
    }

    fn selection(examples: TargetList, benches: TargetList) -> TargetSelection {
        TargetSelection { examples, benches }
    }

    fn example_selection() -> TargetSelection {
        selection(TargetList::All, TargetList::None)
    }

    fn bench_selection(names: &[&str]) -> TargetSelection {
        selection(
            TargetList::None,
            TargetList::Some(names.iter().map(|name| name.to_string()).collect()),
        )
    }

    #[test]
    fn examples_and_benches_need_selection() {
        let fixture = Fixture::new(
            "unselected",
            BASE_MANIFEST,
            &[
                ("examples/adder.rs", "fn main() {}\n"),
                ("benches/simple.rs", "#[test]\nfn add_works() {}\n"),
            ],
        );
        let map = packages(&fixture);
        let root = vec![map.keys().next().unwrap().clone()];
        let plan = plan(&map, &root, Mode::Build, &selection(TargetList::None, TargetList::None))
            .unwrap();
        assert!(!plan.units.iter().any(|unit| {
            matches!(unit.kind, UnitKind::Example | UnitKind::Bench)
        }));
    }

    #[test]
    fn examples_build_when_selected() {
        let fixture = Fixture::new(
            "selected",
            BASE_MANIFEST,
            &[
                ("examples/adder.rs", "fn main() {}\n"),
                ("examples/echo/main.rs", "fn main() {}\n"),
                ("benches/simple.rs", "#[test]\nfn add_works() {}\n"),
            ],
        );
        let map = packages(&fixture);
        let root = vec![map.keys().next().unwrap().clone()];
        let build = plan(&map, &root, Mode::Build, &example_selection()).unwrap();
        let examples: Vec<_> = build
            .units
            .iter()
            .filter(|unit| unit.kind == UnitKind::Example)
            .collect();
        assert_eq!(examples.len(), 2);
        let adder = examples
            .iter()
            .find(|unit| unit.target_name == "adder")
            .unwrap();
        assert_eq!(adder.crate_name, "adder");
        assert_eq!(adder.output_name, "adder");
        assert!(adder
            .externs
            .iter()
            .any(|(name, _)| name == "demo"));
        assert_eq!(adder.source, fixture.dir.join("examples/adder.rs"));
        assert!(examples.iter().any(|unit| unit.target_name == "echo"));

        let check = plan(&map, &root, Mode::Check, &example_selection()).unwrap();
        let adder = check
            .units
            .iter()
            .find(|unit| unit.kind == UnitKind::Example && unit.target_name == "adder")
            .unwrap();
        assert_eq!(adder.output_name, "adder.rmeta");

        let test = plan(&map, &root, Mode::Test, &example_selection()).unwrap();
        assert!(test
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Example));
    }

    #[test]
    fn benches_compile_like_tests() {
        let fixture = Fixture::new(
            "benches",
            BASE_MANIFEST,
            &[("benches/simple.rs", "#[test]\nfn add_works() {}\n")],
        );
        let map = packages(&fixture);
        let root = vec![map.keys().next().unwrap().clone()];
        let build = plan(&map, &root, Mode::Build, &bench_selection(&["simple"])).unwrap();
        let bench = build
            .units
            .iter()
            .find(|unit| unit.kind == UnitKind::Bench)
            .unwrap();
        assert_eq!(bench.target_name, "simple");
        assert_eq!(bench.crate_name, "simple");
        assert_eq!(bench.output_name, "simple-test");
        assert!(bench.externs.iter().any(|(name, _)| name == "demo"));

        let test = plan(
            &map,
            &root,
            Mode::Test,
            &bench_selection(&["simple"]),
        )
        .unwrap();
        assert!(test.units.iter().any(|unit| unit.kind == UnitKind::Bench));
    }

    #[test]
    fn unknown_selected_names_fail() {
        let fixture = Fixture::new(
            "unknown-name",
            BASE_MANIFEST,
            &[("examples/adder.rs", "fn main() {}\n")],
        );
        let map = packages(&fixture);
        let root = vec![map.keys().next().unwrap().clone()];
        let sel = selection(
            TargetList::Some(vec!["missing".to_string()]),
            TargetList::None,
        );
        let error = plan(&map, &root, Mode::Build, &sel).unwrap_err();
        assert!(error.contains("no example target named `missing`"), "{error}");
        assert!(error.contains("adder"), "{error}");

        let sel = selection(TargetList::None, TargetList::Some(vec!["gone".to_string()]));
        let error = plan(&map, &root, Mode::Build, &sel).unwrap_err();
        assert!(error.contains("no bench target named `gone`"), "{error}");
    }

    #[test]
    fn duplicate_examples_fail() {
        let manifest = format!(
            "{BASE_MANIFEST}[[example]]\nname = \"dup\"\npath = \"examples/one.rs\"\n[[example]]\nname = \"dup\"\npath = \"examples/two.rs\"\n"
        );
        let fixture = Fixture::new(
            "dup-examples",
            &manifest,
            &[
                ("examples/one.rs", "fn main() {}\n"),
                ("examples/two.rs", "fn main() {}\n"),
            ],
        );
        let map = packages(&fixture);
        let root = vec![map.keys().next().unwrap().clone()];
        let selection = selection(TargetList::All, TargetList::None);
        let error = plan(&map, &root, Mode::Build, &selection).unwrap_err();
        assert!(error.contains("both produce `dup`"), "{error}");
    }

    #[test]
    fn harness_false_benches_fail() {
        let manifest = format!("{BASE_MANIFEST}[[bench]]\nname = \"custom\"\nharness = false\npath = \"benches/custom.rs\"\n");
        let fixture = Fixture::new("harness-off", &manifest, &[]);
        let map = packages(&fixture);
        let root = vec![map.keys().next().unwrap().clone()];
        let error = plan(&map, &root, Mode::Build, &bench_selection(&["custom"]))
            .unwrap_err();
        assert!(error.contains("harness = false"), "{error}");
        assert!(error.contains("demo"), "{error}");
        assert!(error.contains("custom"), "{error}");
    }
}
