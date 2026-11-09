use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::resolver::resolve::{PackageId, PackageSource, Resolution};
use crate::resolver::semver::Version;
use crate::toml::Value;

pub(crate) const REGISTRY_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

#[derive(Clone, Debug)]
pub(crate) struct LockPackage {
    pub(crate) name: String,
    pub(crate) version: Version,
    pub(crate) source: Option<String>,
    pub(crate) checksum: Option<String>,
    pub(crate) dependencies: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Lockfile {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) version: u32,
    pub(crate) packages: Vec<LockPackage>,
}

impl Lockfile {
    pub(crate) fn parse(input: &str, path: &Path) -> Result<Self, String> {
        let root =
            crate::toml::parse(input).map_err(|error| format!("{}: {error}", path.display()))?;
        let version = match root.get("version") {
            Some(Value::Integer(version @ (3 | 4))) => *version as u32,
            Some(_) => return Err(format!("{}: unsupported lockfile version", path.display())),
            None => return Err(format!("{}: missing lockfile version", path.display())),
        };

        let mut packages = Vec::new();
        if let Some(value) = root.get("package") {
            let Some(entries) = value.as_array() else {
                return Err(format!("{}: `package` must be an array", path.display()));
            };
            for entry in entries {
                let Some(table) = entry.as_table() else {
                    return Err(format!(
                        "{}: every `[[package]]` must be a table",
                        path.display()
                    ));
                };
                let name = table
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{}: package is missing `name`", path.display()))?
                    .to_string();
                let version_text =
                    table
                        .get("version")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            format!("{}: package `{name}` is missing `version`", path.display())
                        })?;
                let version = Version::parse(version_text)
                    .map_err(|error| format!("{}: package `{name}`: {error}", path.display()))?;
                let source = optional_string(table, "source", path)?;
                let checksum = optional_string(table, "checksum", path)?;
                let dependencies = match table.get("dependencies") {
                    None => Vec::new(),
                    Some(Value::Array(values)) => {
                        let mut dependencies = Vec::new();
                        for value in values {
                            let Some(text) = value.as_str() else {
                                return Err(format!(
                                    "{}: package `{name}` dependencies must be strings",
                                    path.display()
                                ));
                            };
                            dependencies.push(text.to_string());
                        }
                        dependencies
                    }
                    Some(_) => {
                        return Err(format!(
                            "{}: package `{name}` dependencies must be an array",
                            path.display()
                        ))
                    }
                };
                packages.push(LockPackage {
                    name,
                    version,
                    source,
                    checksum,
                    dependencies,
                });
            }
        }

        Ok(Self { version, packages })
    }

    pub(crate) fn preferred_versions(&self, name: &str, registry: bool) -> Vec<Version> {
        let mut versions: Vec<Version> = self
            .packages
            .iter()
            .filter(|package| package.name == name && package.source.is_some() == registry)
            .map(|package| package.version.clone())
            .collect();
        versions.sort_by(|left, right| right.cmp(left));
        versions.dedup();
        versions
    }

    pub(crate) fn matches(&self, resolution: &Resolution) -> Result<(), String> {
        let mut expected = BTreeMap::new();
        for (id, package) in &resolution.packages {
            let source = match id.source {
                PackageSource::Registry => Some(REGISTRY_SOURCE.to_string()),
                PackageSource::Path(_) => None,
            };
            let entry = expected
                .entry((id.name.clone(), package.version.to_string(), source))
                .or_insert_with(|| (package.checksum.clone(), Vec::new()));
            entry.1.extend(dependency_strings(resolution, id));
        }
        for (_, dependencies) in expected.values_mut() {
            *dependencies = sorted(dependencies);
        }

        let mut actual = BTreeMap::new();
        for package in &self.packages {
            actual.insert(
                (
                    package.name.clone(),
                    package.version.to_string(),
                    package.source.clone(),
                ),
                (package.checksum.clone(), sorted(&package.dependencies)),
            );
        }

        if expected == actual {
            Ok(())
        } else {
            Err("the lockfile needs to be updated, but `--locked` was passed".to_string())
        }
    }

    pub(crate) fn render(&self) -> String {
        let mut packages = self.packages.clone();
        packages.sort_by(|left, right| {
            (&left.name, &left.version, &left.source).cmp(&(
                &right.name,
                &right.version,
                &right.source,
            ))
        });

        let mut output = String::new();
        output.push_str("# This file is automatically @generated by rusted.\n");
        output.push_str("# It is not intended for manual editing.\n");
        output.push_str("version = 4\n");

        for package in packages {
            output.push('\n');
            output.push_str("[[package]]\n");
            output.push_str(&format!("name = {}\n", toml_string(&package.name)));
            output.push_str(&format!(
                "version = {}\n",
                toml_string(&package.version.to_string())
            ));
            if let Some(source) = &package.source {
                output.push_str(&format!("source = {}\n", toml_string(source)));
            }
            if let Some(checksum) = &package.checksum {
                output.push_str(&format!("checksum = {}\n", toml_string(checksum)));
            }
            if !package.dependencies.is_empty() {
                output.push_str("dependencies = [\n");
                for dependency in sorted(&package.dependencies) {
                    output.push_str(&format!(" {},\n", toml_string(&dependency)));
                }
                output.push_str("]\n");
            }
        }

        output
    }

    pub(crate) fn write(&self, path: &Path) -> Result<(), String> {
        let text = self.render();
        std::fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub(crate) fn from_resolution(resolution: &Resolution) -> Self {
        let mut merged: BTreeMap<(String, String, Option<String>), LockPackage> = BTreeMap::new();
        for (id, package) in &resolution.packages {
            let source = match id.source {
                PackageSource::Registry => Some(REGISTRY_SOURCE.to_string()),
                PackageSource::Path(_) => None,
            };
            let key = (id.name.clone(), package.version.to_string(), source.clone());
            let dependencies = dependency_strings(resolution, id);
            match merged.get_mut(&key) {
                Some(entry) => entry.dependencies.extend(dependencies),
                None => {
                    merged.insert(
                        key,
                        LockPackage {
                            name: id.name.clone(),
                            version: package.version.clone(),
                            source,
                            checksum: package.checksum.clone(),
                            dependencies,
                        },
                    );
                }
            }
        }
        let mut packages: Vec<LockPackage> = merged.into_values().collect();
        for package in &mut packages {
            package.dependencies = sorted(&package.dependencies);
        }
        Self {
            version: 4,
            packages,
        }
    }
}

fn dependency_strings(resolution: &Resolution, id: &PackageId) -> Vec<String> {
    let Some(package) = resolution.packages.get(id) else {
        return Vec::new();
    };
    let mut versions: BTreeMap<&str, BTreeSet<&Version>> = BTreeMap::new();
    for package in resolution.packages.values() {
        versions
            .entry(package.id.name.as_str())
            .or_default()
            .insert(&package.version);
    }

    let mut dependencies = Vec::new();
    for edge in &package.dependencies {
        let dependency = &edge.package;
        let Some(target) = resolution.packages.get(dependency) else {
            continue;
        };
        let text = if versions
            .get(dependency.name.as_str())
            .map(|versions| versions.len() > 1)
            .unwrap_or(false)
        {
            format!("{} {}", dependency.name, target.version)
        } else {
            dependency.name.clone()
        };
        dependencies.push(text);
    }
    sorted(&dependencies)
}

fn sorted(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values.dedup();
    values
}

fn optional_string(
    table: &crate::toml::Table,
    key: &str,
    path: &Path,
) -> Result<Option<String>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{}: `{key}` must be a string", path.display())),
    }
}

fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::Lockfile;
    use crate::resolver::resolve::{
        BuildContext, PackageId, PackageSource, Resolution, ResolvedDependency, ResolvedPackage,
    };
    use crate::resolver::semver::Version;

    fn resolution() -> Resolution {
        let root = PackageId {
            name: "app".to_string(),
            version: Version::parse("0.1.0").unwrap(),
            source: PackageSource::Path(PathBuf::from("/tmp/app")),
            context: BuildContext::Target,
        };
        let foo = PackageId {
            name: "foo".to_string(),
            version: Version::parse("1.2.3").unwrap(),
            source: PackageSource::Registry,
            context: BuildContext::Target,
        };
        let mut packages = BTreeMap::new();
        packages.insert(
            root.clone(),
            ResolvedPackage {
                id: root.clone(),
                version: Version::parse("0.1.0").unwrap(),
                checksum: None,
                features: Default::default(),
                dependencies: vec![ResolvedDependency {
                    package: foo.clone(),
                    extern_name: "foo".to_string(),
                    kind: crate::manifest::DependencyKind::Normal,
                }],
                index: None,
                manifest: None,
                manifest_path: None,
            },
        );
        packages.insert(
            foo.clone(),
            ResolvedPackage {
                id: foo.clone(),
                version: Version::parse("1.2.3").unwrap(),
                checksum: Some(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
                ),
                features: Default::default(),
                dependencies: Vec::new(),
                index: None,
                manifest: None,
                manifest_path: None,
            },
        );
        Resolution { packages }
    }

    #[test]
    fn renders_v4_in_cargo_order() {
        let lock = Lockfile::from_resolution(&resolution());
        assert_eq!(
            lock.render(),
            "# This file is automatically @generated by rusted.\n\
             # It is not intended for manual editing.\n\
             version = 4\n\
             \n\
             [[package]]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             dependencies = [\n \
             \"foo\",\n\
             ]\n\
             \n\
             [[package]]\n\
             name = \"foo\"\n\
             version = \"1.2.3\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
             checksum = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n"
        );
    }

    #[test]
    fn renders_and_parses_v4() {
        let lock = Lockfile::from_resolution(&resolution());
        let text = lock.render();
        assert!(text.contains("version = 4"));
        assert!(text.contains("[[package]]"));
        assert!(text.contains("name = \"foo\""));
        assert!(text.contains("checksum = \"0123"));

        let parsed = Lockfile::parse(&text, std::path::Path::new("Cargo.lock")).unwrap();
        assert_eq!(parsed.version, 4);
        assert_eq!(parsed.packages.len(), 2);
        assert_eq!(
            parsed.preferred_versions("foo", true),
            vec![Version::parse("1.2.3").unwrap()]
        );
        parsed.matches(&resolution()).unwrap();
    }

    #[test]
    fn writes_to_disk() {
        let directory =
            std::env::temp_dir().join(format!("rusted-lock-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Cargo.lock");
        let lock = Lockfile::from_resolution(&resolution());
        lock.write(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed = Lockfile::parse(&text, &path).unwrap();
        parsed.matches(&resolution()).unwrap();
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn reads_version_3() {
        let text = "version = 3\n\n[[package]]\nname = \"foo\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"abc\"\n";
        let lock = Lockfile::parse(text, std::path::Path::new("Cargo.lock")).unwrap();
        assert_eq!(lock.version, 3);
        assert_eq!(lock.packages.len(), 1);
    }

    #[test]
    fn detects_outdated_lockfiles() {
        let lock = Lockfile::from_resolution(&resolution());
        let mut changed = resolution();
        let foo = PackageId {
            name: "foo".to_string(),
            version: Version::parse("1.2.3").unwrap(),
            source: PackageSource::Registry,
            context: BuildContext::Target,
        };
        changed.packages.get_mut(&foo).unwrap().version = Version::parse("1.3.0").unwrap();
        assert!(lock.matches(&changed).is_err());
    }

    #[test]
    fn rejects_invalid_lockfiles() {
        for text in [
            "",
            "version = 2\n",
            "version = 4\n[[package]]\nname = \"foo\"\n",
            "version = 4\n[[package]]\nname = \"foo\"\nversion = \"bad\"\n",
            "version = 4\n[[package]]\nname = \"foo\"\nversion = \"1.0.0\"\ndependencies = 1\n",
        ] {
            assert!(
                Lockfile::parse(text, std::path::Path::new("Cargo.lock")).is_err(),
                "{text}"
            );
        }
    }
}
