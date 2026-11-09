use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::manifest::{Edition, Manifest};
use crate::resolver::resolve::{BuildContext, PackageId, ResolvedDependency};

use super::Mode;

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitKind {
    Lib,
    Bin,
    Test,
    BuildScript,
    ProcMacro,
}

pub(crate) struct Plan {
    pub(crate) units: Vec<Unit>,
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

struct TestTarget {
    name: String,
    path: PathBuf,
}

pub(crate) fn plan(
    packages: &BTreeMap<PackageId, PackageInfo>,
    roots: &[PackageId],
    mode: Mode,
) -> Result<Plan, String> {
    let root_set: BTreeSet<&PackageId> = roots.iter().collect();

    let mut lib_outputs: BTreeMap<PackageId, (String, String)> = BTreeMap::new();
    for (id, package) in packages {
        validate_package(package)?;
        if let Some(lib) = lib_target(&package.manifest, &package.dir) {
            let crate_name = normalize_name(&lib.name);
            let output_name = if lib.proc_macro {
                format!("lib{crate_name}.so")
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

    let mut units = Vec::new();
    for (id, package) in packages {
        let normal_externs = externs_for(package, &lib_outputs, mode, false)?;
        let build_externs = externs_for(package, &lib_outputs, mode, true)?;
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
    }

    Ok(Plan { units })
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
    build_dependencies: bool,
) -> Result<Vec<(String, PackageId)>, String> {
    let mut externs: BTreeMap<String, PackageId> = BTreeMap::new();
    for edge in &package.dependencies {
        let include = if build_dependencies {
            edge.kind == crate::manifest::DependencyKind::Build
        } else {
            edge.kind == crate::manifest::DependencyKind::Normal
                || (mode == Mode::Test && edge.kind == crate::manifest::DependencyKind::Dev)
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

fn test_targets(dir: &Path, manifest: &Manifest) -> Result<Vec<TestTarget>, String> {
    let mut tests: Vec<TestTarget> = Vec::new();
    let package_name = &manifest.name;

    for target in &manifest.tests {
        let name = match target.name.clone() {
            Some(name) => name,
            None => match &target.path {
                Some(path) => derive_test_name(Path::new(path)),
                None => {
                    return Err(format!(
                        "test target in package `{package_name}` needs a name or a path"
                    ))
                }
            },
        };
        let path = match &target.path {
            Some(path) => dir.join(path),
            None => default_test_path(dir, &name)?,
        };
        tests.push(TestTarget { name, path });
    }

    if manifest.autodiscovery.tests != Some(false) {
        let directory = dir.join("tests");
        if directory.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&directory)
                .map_err(|error| format!("{}: {error}", directory.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .collect();
            entries.sort();
            for path in entries {
                if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                    if tests.iter().any(|test| test.path == path) {
                        continue;
                    }
                    let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                        continue;
                    };
                    tests.push(TestTarget {
                        name: name.to_string(),
                        path,
                    });
                } else if path.is_dir() {
                    let main = path.join("main.rs");
                    if main.is_file() && !tests.iter().any(|test| test.path == main) {
                        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                            continue;
                        };
                        tests.push(TestTarget {
                            name: name.to_string(),
                            path: main,
                        });
                    }
                }
            }
        }
    }

    for test in &tests {
        if !test.path.is_file() {
            return Err(format!(
                "test `{}` of package `{package_name}` points at missing source `{}`",
                test.name,
                test.path.display()
            ));
        }
    }

    Ok(tests)
}

fn derive_test_name(path: &Path) -> String {
    if path.file_name().and_then(|name| name.to_str()) == Some("main.rs") {
        if let Some(parent) = path.parent().and_then(|parent| parent.file_name()) {
            return parent.to_string_lossy().into_owned();
        }
    }
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "test".to_string())
}

fn default_test_path(package_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let flat = package_dir.join("tests").join(format!("{name}.rs"));
    if flat.is_file() {
        return Ok(flat);
    }
    let nested = package_dir.join("tests").join(name).join("main.rs");
    if nested.is_file() {
        return Ok(nested);
    }
    Err(format!(
        "test `{name}` has no path and no default source file in `{}`",
        package_dir.display()
    ))
}

pub(crate) fn normalize_name(name: &str) -> String {
    name.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::normalize_name;

    #[test]
    fn crate_names_use_underscores() {
        assert_eq!(normalize_name("foo-bar"), "foo_bar");
        assert_eq!(normalize_name("plain"), "plain");
    }
}
