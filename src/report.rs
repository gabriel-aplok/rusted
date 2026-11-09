use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::manifest::DependencyKind;
use crate::resolver::lockfile::REGISTRY_SOURCE;
use crate::resolver::resolve::{
    BuildContext, PackageId, PackageSource, Resolution, ResolvedPackage,
};
use crate::util::json::escape;

pub(crate) fn tree(resolution: &Resolution, roots: &[PackageId]) -> String {
    let mut roots = roots.to_vec();
    roots.sort();
    let mut output = String::new();
    for (index, root) in roots.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let mut path = Vec::new();
        print_node(resolution, root, "", true, true, &mut path, &mut output);
    }
    output
}

fn print_node(
    resolution: &Resolution,
    id: &PackageId,
    prefix: &str,
    last: bool,
    root: bool,
    path: &mut Vec<PackageId>,
    output: &mut String,
) {
    let Some(package) = resolution.packages.get(id) else {
        return;
    };
    let mut label = format!("{} v{}", package.id.name, package.version);
    if !package.features.is_empty() {
        let features: Vec<&str> = package.features.iter().map(String::as_str).collect();
        label.push_str(&format!(" [{}]", features.join(", ")));
    }
    if package.id.context == BuildContext::Build {
        label.push_str(" (build)");
    }
    if path.contains(id) {
        label.push_str(" (cycle)");
    }

    if root {
        output.push_str(&label);
    } else {
        output.push_str(prefix);
        output.push_str(if last { "`-- " } else { "|-- " });
        output.push_str(&label);
    }
    output.push('\n');

    if path.contains(id) {
        return;
    }
    path.push(id.clone());

    let mut children: BTreeSet<&PackageId> = BTreeSet::new();
    for edge in &package.dependencies {
        children.insert(&edge.package);
    }
    let mut children: Vec<&PackageId> = children.into_iter().collect();
    children.sort_by(|left, right| {
        let left = resolution.packages.get(*left);
        let right = resolution.packages.get(*right);
        match (left, right) {
            (Some(left), Some(right)) => {
                (&left.id.name, &left.version).cmp(&(&right.id.name, &right.version))
            }
            _ => std::cmp::Ordering::Equal,
        }
    });

    let child_prefix = if root {
        String::new()
    } else {
        format!("{prefix}{}", if last { "    " } else { "|   " })
    };
    for (index, child) in children.iter().enumerate() {
        print_node(
            resolution,
            child,
            &child_prefix,
            index + 1 == children.len(),
            false,
            path,
            output,
        );
    }
    path.pop();
}

pub(crate) fn metadata(
    resolution: &Resolution,
    roots: &[PackageId],
    workspace_root: &Path,
) -> String {
    let mut groups: BTreeMap<(String, String, String), Vec<&ResolvedPackage>> = BTreeMap::new();
    for package in resolution.packages.values() {
        groups
            .entry((
                package.id.name.clone(),
                package.version.to_string(),
                source_string(&package.id.source),
            ))
            .or_default()
            .push(package);
    }

    let mut members: Vec<String> = roots
        .iter()
        .map(|id| format!("{} {}", id.name, version_of(resolution, id)))
        .collect();
    members.sort();

    let mut output = String::new();
    output.push('{');
    output.push_str(&format!(
        "\"version\":1,\"workspace_root\":\"{}\",",
        escape(&workspace_root.display().to_string())
    ));
    output.push_str("\"workspace_members\":[");
    for (index, member) in members.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(&format!("\"{}\"", escape(member)));
    }
    output.push_str("],\"packages\":[");
    for (index, group) in groups.values().enumerate() {
        if index > 0 {
            output.push(',');
        }
        let package = group[0];
        output.push_str("{\"name\":\"");
        output.push_str(&escape(&package.id.name));
        output.push_str("\",\"version\":\"");
        output.push_str(&escape(&package.version.to_string()));
        output.push_str("\",\"id\":\"");
        output.push_str(&escape(&package_id(resolution, &package.id)));
        output.push_str("\",\"source\":\"");
        output.push_str(&escape(&source_string(&package.id.source)));
        output.push_str("\",\"manifest_path\":\"");
        output.push_str(&escape(
            &package
                .manifest_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        ));
        output.push_str("\",\"targets\":[");
        for (target_index, target) in targets(package).iter().enumerate() {
            if target_index > 0 {
                output.push(',');
            }
            output.push_str(&format!(
                "{{\"name\":\"{}\",\"kind\":[\"{}\"],\"src_path\":\"{}\"}}",
                escape(&target.name),
                target.kind,
                escape(&target.path)
            ));
        }
        output.push_str("],\"features\":[");
        let mut features: BTreeSet<&str> = BTreeSet::new();
        for package in group {
            features.extend(package.features.iter().map(String::as_str));
        }
        for (feature_index, feature) in features.iter().enumerate() {
            if feature_index > 0 {
                output.push(',');
            }
            output.push_str(&format!("\"{}\"", escape(feature)));
        }
        output.push_str("],\"dependencies\":[");
        let mut dependencies: BTreeSet<(&str, String, &str)> = BTreeSet::new();
        for package in group {
            for edge in &package.dependencies {
                let version = version_of(resolution, &edge.package);
                dependencies.insert((&edge.package.name, version, kind_name(edge.kind)));
            }
        }
        for (dependency_index, (name, version, kind)) in dependencies.iter().enumerate() {
            if dependency_index > 0 {
                output.push(',');
            }
            output.push_str(&format!(
                "{{\"name\":\"{}\",\"version\":\"{}\",\"kind\":\"{}\"}}",
                escape(name),
                escape(version),
                kind
            ));
        }
        output.push_str("],\"kinds\":[");
        let mut kinds: BTreeSet<&str> = BTreeSet::new();
        for package in group {
            for edge in &package.dependencies {
                kinds.insert(kind_name(edge.kind));
            }
        }
        for (kind_index, kind) in kinds.iter().enumerate() {
            if kind_index > 0 {
                output.push(',');
            }
            output.push_str(&format!("\"{kind}\""));
        }
        output.push_str("]}");
    }
    output.push_str("]}");
    output
}

fn source_string(source: &PackageSource) -> String {
    match source {
        PackageSource::Registry => REGISTRY_SOURCE.to_string(),
        PackageSource::Path(directory) => format!("path+{}", directory.display()),
        PackageSource::Git {
            url,
            reference,
            rev,
        } => crate::resolver::git::source_string(url, reference.as_ref(), rev),
    }
}

struct TargetInfo {
    name: String,
    kind: &'static str,
    path: String,
}

fn package_id(resolution: &Resolution, id: &PackageId) -> String {
    format!(
        "{} {} ({})",
        id.name,
        version_of(resolution, id),
        source_string(&id.source)
    )
}

fn targets(package: &ResolvedPackage) -> Vec<TargetInfo> {
    let Some(manifest) = &package.manifest else {
        return Vec::new();
    };
    let dir = package
        .manifest_path
        .as_ref()
        .and_then(|path| path.parent())
        .unwrap_or_else(|| Path::new("."));

    let mut targets = Vec::new();
    if manifest.lib.is_some() || dir.join("src/lib.rs").is_file() {
        let spec = manifest.lib.as_ref();
        let name = spec
            .and_then(|spec| spec.name.clone())
            .unwrap_or_else(|| manifest.name.replace('-', "_"));
        let path = spec
            .and_then(|spec| spec.path.clone())
            .unwrap_or_else(|| "src/lib.rs".to_string());
        targets.push(TargetInfo {
            name,
            kind: "lib",
            path: dir.join(path).display().to_string(),
        });
    }

    let mut bins: Vec<(String, String)> = manifest
        .bins
        .iter()
        .map(|spec| {
            (
                spec.name.clone().unwrap_or_else(|| manifest.name.clone()),
                spec.path
                    .clone()
                    .unwrap_or_else(|| "src/main.rs".to_string()),
            )
        })
        .collect();
    if bins.is_empty() && dir.join("src/main.rs").is_file() {
        bins.push((manifest.name.clone(), "src/main.rs".to_string()));
    }
    for (name, path) in bins {
        targets.push(TargetInfo {
            name,
            kind: "bin",
            path: dir.join(path).display().to_string(),
        });
    }

    for spec in &manifest.tests {
        if let Some(name) = &spec.name {
            let path = spec
                .path
                .clone()
                .unwrap_or_else(|| format!("tests/{name}.rs"));
            targets.push(TargetInfo {
                name: name.clone(),
                kind: "test",
                path: dir.join(path).display().to_string(),
            });
        }
    }
    for spec in &manifest.benches {
        if let Some(name) = &spec.name {
            let path = spec
                .path
                .clone()
                .unwrap_or_else(|| format!("benches/{name}.rs"));
            targets.push(TargetInfo {
                name: name.clone(),
                kind: "bench",
                path: dir.join(path).display().to_string(),
            });
        }
    }

    targets.sort_by(|left, right| {
        (left.kind, &left.name, &left.path).cmp(&(right.kind, &right.name, &right.path))
    });
    targets
}

fn version_of(resolution: &Resolution, id: &PackageId) -> String {
    resolution
        .packages
        .get(id)
        .map(|package| package.version.to_string())
        .unwrap_or_default()
}

fn kind_name(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Normal => "normal",
        DependencyKind::Dev => "dev",
        DependencyKind::Build => "build",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{metadata, tree};
    use crate::manifest::{DependencyKind, Manifest};
    use crate::resolver::resolve::{
        BuildContext, PackageId, PackageSource, Resolution, ResolvedDependency, ResolvedPackage,
    };
    use crate::resolver::semver::Version;
    use crate::util::json;

    fn package(
        name: &str,
        version: &str,
        source: PackageSource,
        dependencies: Vec<ResolvedDependency>,
        features: &[&str],
    ) -> ResolvedPackage {
        let manifest_path = if matches!(source, PackageSource::Path(_)) {
            Some(PathBuf::from(format!("/tmp/{name}/Cargo.toml")))
        } else {
            None
        };
        let manifest = if let Some(path) = &manifest_path {
            Some(
                Manifest::parse(
                    &format!(
                        "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n[lib]\npath = \"src/lib.rs\"\n"
                    ),
                    path,
                )
                .unwrap(),
            )
        } else {
            None
        };
        let id = PackageId {
            name: name.to_string(),
            version: Version::parse(version).unwrap(),
            source,
            context: BuildContext::Target,
        };
        ResolvedPackage {
            id,
            version: Version::parse(version).unwrap(),
            checksum: None,
            features: features.iter().map(|feature| feature.to_string()).collect(),
            dependencies,
            index: None,
            manifest,
            manifest_path,
        }
    }

    fn resolution() -> (Resolution, Vec<PackageId>) {
        let app = PackageId {
            name: "app".to_string(),
            version: Version::parse("0.1.0").unwrap(),
            source: PackageSource::Path(PathBuf::from("/tmp/app")),
            context: BuildContext::Target,
        };
        let foo = PackageId {
            name: "foo".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            source: PackageSource::Registry,
            context: BuildContext::Target,
        };
        let bar = PackageId {
            name: "bar".to_string(),
            version: Version::parse("2.0.0").unwrap(),
            source: PackageSource::Registry,
            context: BuildContext::Target,
        };
        let mut packages = BTreeMap::new();
        packages.insert(
            app.clone(),
            package(
                "app",
                "0.1.0",
                PackageSource::Path(PathBuf::from("/tmp/app")),
                vec![ResolvedDependency {
                    package: foo.clone(),
                    extern_name: "foo".to_string(),
                    kind: DependencyKind::Normal,
                }],
                &[],
            ),
        );
        packages.insert(
            foo.clone(),
            package(
                "foo",
                "1.0.0",
                PackageSource::Registry,
                vec![ResolvedDependency {
                    package: bar.clone(),
                    extern_name: "bar".to_string(),
                    kind: DependencyKind::Normal,
                }],
                &["std"],
            ),
        );
        packages.insert(
            bar.clone(),
            package("bar", "2.0.0", PackageSource::Registry, Vec::new(), &[]),
        );
        (Resolution { packages }, vec![app])
    }

    #[test]
    fn prints_a_stable_tree() {
        let (resolution, roots) = resolution();
        let output = tree(&resolution, &roots);
        assert_eq!(
            output,
            "app v0.1.0\n`-- foo v1.0.0 [std]\n    `-- bar v2.0.0\n"
        );
    }

    #[test]
    fn prints_metadata_json() {
        let (resolution, roots) = resolution();
        let output = metadata(&resolution, &roots, std::path::Path::new("/tmp/app"));
        let value = json::parse(&output).unwrap();
        assert_eq!(value.get("version").and_then(json::Value::as_u64), Some(1));
        assert_eq!(
            value.get("workspace_root").and_then(json::Value::as_str),
            Some("/tmp/app")
        );
        let packages = value
            .get("packages")
            .and_then(json::Value::as_array)
            .unwrap();
        assert_eq!(packages.len(), 3);
        let foo = packages
            .iter()
            .find(|package| package.get("name").and_then(json::Value::as_str) == Some("foo"))
            .unwrap();
        assert_eq!(
            foo.get("features")
                .and_then(json::Value::as_array)
                .map(|features| features.len()),
            Some(1)
        );

        let app = packages
            .iter()
            .find(|package| package.get("name").and_then(json::Value::as_str) == Some("app"))
            .unwrap();
        assert_eq!(
            app.get("id").and_then(json::Value::as_str),
            Some("app 0.1.0 (path+/tmp/app)")
        );
        let targets = app.get("targets").and_then(json::Value::as_array).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].get("name").and_then(json::Value::as_str),
            Some("app")
        );
        assert_eq!(
            targets[0]
                .get("kind")
                .and_then(json::Value::as_array)
                .map(|kind| kind.len()),
            Some(1)
        );
        let dependencies = app
            .get("dependencies")
            .and_then(json::Value::as_array)
            .unwrap();
        assert_eq!(dependencies.len(), 1);
        assert_eq!(
            dependencies[0].get("name").and_then(json::Value::as_str),
            Some("foo")
        );
        assert_eq!(
            dependencies[0].get("kind").and_then(json::Value::as_str),
            Some("normal")
        );
    }
}
