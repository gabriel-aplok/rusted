use std::collections::BTreeMap;
use std::collections::{BTreeSet, VecDeque};

use super::plan::PackageInfo;
use crate::manifest::{Dependency, Manifest};
use crate::resolver::resolve::{BuildContext, PackageId, ResolvedDependency};

// The sparse index does not expose `proc-macro`, so the resolver cannot split
// feature contexts during resolution. This pass runs after registry manifests
// are parsed and rebuilds feature sets the way Cargo resolver 2 behaves: the
// target context walks the workspace closure without entering a proc macro,
// and every proc-macro dependency subtree becomes a set of Build-context units
// fed by host edges only. The lockfile graph is untouched because its entries
// merge by name, version, and source.
pub(crate) fn apply(packages: &mut BTreeMap<PackageId, PackageInfo>, roots: &[PackageId]) {
    let proc_macros: Vec<PackageId> = packages
        .iter()
        .filter(|(id, package)| {
            id.context == BuildContext::Target && is_proc_macro(&package.manifest)
        })
        .map(|(id, _)| id.clone())
        .collect();
    if proc_macros.is_empty() {
        return;
    }

    let pm_build_ids: BTreeSet<PackageId> = proc_macros
        .iter()
        .map(|id| with_context(id, BuildContext::Build))
        .collect();

    // Seed target features from the resolved workspace roots.
    let mut target_features: BTreeMap<PackageId, BTreeSet<String>> = BTreeMap::new();
    for id in roots {
        if let Some(package) = packages.get(id) {
            target_features
                .entry(id.clone())
                .or_default()
                .extend(package.features.iter().cloned());
        }
    }
    propagate(
        packages,
        &mut target_features,
        roots.to_vec(),
        &proc_macros,
        BuildContext::Target,
    );

    // Seed host features from every edge that points at a proc macro.
    let mut host_features: BTreeMap<PackageId, BTreeSet<String>> = BTreeMap::new();
    let seeds: Vec<PackageId> = proc_macros.clone();
    for (id, package) in &*packages {
        if proc_macros.contains(id) {
            continue;
        }
        for edge in &package.dependencies {
            let Some(pm) = proc_macros.iter().find(|pm| **pm == edge.package) else {
                continue;
            };
            let parent_active = target_features
                .get(id)
                .cloned()
                .unwrap_or_else(|| package.features.clone());
            host_features
                .entry(with_context(pm, BuildContext::Build))
                .or_default()
                .extend(incoming(package, edge, &parent_active));
        }
    }
    propagate(
        packages,
        &mut host_features,
        seeds,
        &[],
        BuildContext::Build,
    );

    // Apply. Order matters: host copies insert first, then pm target nodes
    // move to Build, then target feature sets lose the merged host features.
    let mut insertions: Vec<(PackageId, PackageInfo)> = Vec::new();
    let mut removals: Vec<PackageId> = Vec::new();

    let host_keys: Vec<PackageId> = host_features.keys().cloned().collect();
    for host_id in &host_keys {
        let origin = with_context(host_id, BuildContext::Target);
        if pm_build_ids.contains(host_id) {
            // Proc macro crates move entirely to the host side.
            continue;
        }
        let Some(source) = packages.get(&origin) else {
            continue;
        };
        let mut copy = source.clone();
        copy.id = host_id.clone();
        copy.features = host_features[host_id].clone();
        copy.dependencies = rewritten_deps(source, &pm_build_ids, &host_keys);
        insertions.push((host_id.clone(), copy));
        if target_features.get(&origin).is_none() {
            // The node was merged into a single entry by the resolver but the
            // target side never reaches it: it belongs to the host only.
            removals.push(origin);
        }
    }

    for pm in &proc_macros {
        let host_id = with_context(pm, BuildContext::Build);
        let Some(mut package) = packages.remove(pm) else {
            continue;
        };
        package.id = host_id.clone();
        package.features = host_features
            .get(&host_id)
            .cloned()
            .unwrap_or_else(|| package.features.clone());
        package.dependencies = rewritten_deps(&package, &pm_build_ids, &host_keys);
        insertions.push((host_id, package));
    }

    for id in removals {
        packages.remove(&id);
    }
    for (id, package) in insertions {
        packages.insert(id, package);
    }

    // Shared crates lose the merged host features on the target side.
    for (id, package) in packages.iter_mut() {
        if id.context != BuildContext::Target {
            continue;
        }
        if let Some(target_set) = target_features.get(id) {
            package.features = target_set.clone();
        }
        // Target edges that pointed at a moved proc macro follow the host copy.
        for edge in &mut package.dependencies {
            if proc_macros.contains(&edge.package) {
                edge.package = with_context(&edge.package, BuildContext::Build);
            }
        }
    }
}

fn rewritten_deps(
    package: &PackageInfo,
    pm_build_ids: &BTreeSet<PackageId>,
    host_keys: &[PackageId],
) -> Vec<ResolvedDependency> {
    package
        .dependencies
        .iter()
        .map(|edge| {
            let mut edge = edge.clone();
            if pm_build_ids.contains(&edge.package) {
                edge.package = with_context(&edge.package, BuildContext::Build);
            } else if host_keys.contains(&with_context(&edge.package, BuildContext::Build)) {
                edge.package = with_context(&edge.package, BuildContext::Build);
            }
            edge
        })
        .collect()
}

// One breadth-first expansion over the pristine graph. A side stops at the
// blocked nodes (proc macros block the target side) and keeps its feature sets
// keyed by the context of its own ids.
fn propagate(
    packages: &BTreeMap<PackageId, PackageInfo>,
    features: &mut BTreeMap<PackageId, BTreeSet<String>>,
    mut queue: Vec<PackageId>,
    stop: &[PackageId],
    context: BuildContext,
) {
    let mut queued: BTreeSet<PackageId> = queue.iter().cloned().collect();

    while let Some(id) = queue.pop() {
        let Some(package) = packages.get(&id) else {
            continue;
        };
        let parent_features = features.get(&id).cloned().unwrap_or_default();
        for edge in &package.dependencies {
            if stop.contains(&edge.package) {
                continue;
            }
            let Some(child) = packages.get(&edge.package) else {
                continue;
            };
            let child_side_id = with_context(&child.id, context);
            let entry = features.entry(child_side_id.clone()).or_default();
            let incoming = incoming(package, edge, &parent_features);
            let mut changed = false;
            for item in incoming {
                changed |= entry.insert(item);
            }
            changed |= expand_named(&child.manifest, entry);
            if changed && queued.insert(child_side_id.clone()) {
                queue.push(child.id.clone());
            }
        }
    }
}

fn incoming(
    parent: &PackageInfo,
    edge: &ResolvedDependency,
    parent_features: &BTreeSet<String>,
) -> Vec<String> {
    let mut requested: Vec<String> = Vec::new();
    if let Some(declaration) = declaration_for(parent, &edge.extern_name) {
        for feature in &declaration.features {
            requested.push(feature.clone());
        }
        if declaration.default_features.unwrap_or(true) && has_default(parent) {
            requested.push("default".to_string());
        }
    }

    for dep in optional_keys(parent) {
        if parent_features.contains(&dep) {
            requested.push(dep);
        }
    }

    for feature in parent_features {
        if let Some(items) = parent.manifest.features.get(feature) {
            for item in items {
                if let Some((dep, pushed)) = item.split_once('/') {
                    let dep = dep.trim_end_matches('?');
                    if dep == edge.extern_name || matches_key(parent, dep, &edge.package.name) {
                        requested.push(pushed.to_string());
                    }
                }
            }
        }
    }

    requested
}

fn expand_named(manifest: &Manifest, active: &mut BTreeSet<String>) -> bool {
    let mut changed = false;
    let mut queue: VecDeque<String> = active.iter().cloned().collect();
    while let Some(name) = queue.pop_front() {
        if item_unusable(&name) {
            continue;
        }
        if let Some(items) = manifest.features.get(&name) {
            for item in items {
                if !item_unusable(item) && active.insert(item.clone()) {
                    queue.push_back(item.clone());
                    changed = true;
                }
            }
        } else if optional(manifest, &name) && active.insert(name.clone()) {
            queue.push_back(name);
        }
    }
    changed
}

fn declaration_for(parent: &PackageInfo, key: &str) -> Option<Dependency> {
    let mut declaration = None;
    for (_, table) in parent.manifest.dependency_sections() {
        if let Some(entry) = table.get(key) {
            declaration = Some(entry.clone());
        }
    }
    for (_, _, table) in parent.manifest.target_dependency_sections() {
        if let Some(entry) = table.get(key) {
            declaration = Some(entry.clone());
        }
    }
    declaration
}

fn optional_keys(parent: &PackageInfo) -> Vec<String> {
    let mut keys = Vec::new();
    for (kind, table) in parent.manifest.dependency_sections() {
        if kind == crate::manifest::DependencyKind::Dev {
            continue;
        }
        for (key, dependency) in table.iter() {
            if dependency.optional {
                keys.push(key.clone());
            }
        }
    }
    keys
}

fn matches_key(parent: &PackageInfo, dep: &str, package: &str) -> bool {
    for (_, table) in parent.manifest.dependency_sections() {
        if let Some(entry) = table.get(dep) {
            if entry.package.as_deref() == Some(package) {
                return true;
            }
        }
    }
    false
}

fn has_default(parent: &PackageInfo) -> bool {
    parent.manifest.features.contains_key("default")
}

fn optional(manifest: &Manifest, key: &str) -> bool {
    for (kind, table) in manifest.dependency_sections() {
        if kind == crate::manifest::DependencyKind::Dev {
            continue;
        }
        if table
            .get(key)
            .map(|dependency| dependency.optional)
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

fn item_unusable(name: &str) -> bool {
    name.contains('/') || name.starts_with("dep:")
}

fn is_proc_macro(manifest: &Manifest) -> bool {
    manifest
        .lib
        .as_ref()
        .map(|lib| lib.proc_macro == Some(true))
        .unwrap_or(false)
}

fn with_context(id: &PackageId, context: BuildContext) -> PackageId {
    let mut id = id.clone();
    id.context = context;
    id
}
