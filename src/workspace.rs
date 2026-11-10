#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::ManifestChoice;
use crate::manifest::select::{discover, ManifestFile, CARGO_MANIFEST_FILE, RUSTED_MANIFEST_FILE};
use crate::manifest::{
    Dependency, DependencyKind, Edition, Manifest, ProfileEntry, WorkspaceConfig,
};
use crate::resolver::semver::{Version, VersionReq};
use crate::toml::Table;

#[cfg(test)]
const MAX_DEPENDENCY_DEPTH: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PackageKey {
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
}

impl PackageKey {
    fn new(dir: &Path, name: &str) -> Result<Self, String> {
        Ok(Self {
            name: name.to_string(),
            dir: canonical(dir)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DependencyLink {
    pub(crate) kind: DependencyKind,
    pub(crate) extern_name: String,
    pub(crate) package: PackageKey,
}

#[derive(Debug)]
pub(crate) struct Package {
    pub(crate) key: PackageKey,
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest: Manifest,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) links: Vec<DependencyLink>,
}

#[derive(Debug)]
pub(crate) struct Workspace {
    pub(crate) root_dir: PathBuf,
    pub(crate) manifest_path: PathBuf,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) root_key: Option<PackageKey>,
    pub(crate) packages: BTreeMap<PackageKey, Package>,
    pub(crate) members: Vec<PackageKey>,
    pub(crate) default_targets: Vec<PackageKey>,
    pub(crate) patches: BTreeMap<String, Dependency>,
    pub(crate) replaces: BTreeMap<String, Dependency>,
    pub(crate) shared_package: Option<Table>,
    pub(crate) shared_dependencies: Option<Table>,
    pub(crate) resolver: u32,
    pub(crate) profiles: BTreeMap<String, ProfileEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct Inheritance {
    pub(crate) workspace_dir: PathBuf,
    pub(crate) patches: BTreeMap<String, Dependency>,
    pub(crate) replaces: BTreeMap<String, Dependency>,
    pub(crate) shared_package: Option<Table>,
    pub(crate) shared_dependencies: Option<Table>,
    pub(crate) member_dirs: BTreeSet<PathBuf>,
}

impl Inheritance {
    pub(crate) fn applies(&self, dir: &Path) -> bool {
        dir.starts_with(&self.workspace_dir) || self.member_dirs.contains(dir)
    }
}

impl Workspace {
    pub(crate) fn inheritance(&self) -> Inheritance {
        Inheritance {
            workspace_dir: self.root_dir.clone(),
            patches: self.patches.clone(),
            replaces: self.replaces.clone(),
            shared_package: self.shared_package.clone(),
            shared_dependencies: self.shared_dependencies.clone(),
            member_dirs: self.members.iter().map(|key| key.dir.clone()).collect(),
        }
    }
}

impl Workspace {
    pub(crate) fn load(entry: &ManifestFile, choice: ManifestChoice) -> Result<Workspace, String> {
        let root = find_root(entry, choice)?;
        let workspace = assemble(&root, choice)?;
        verify_membership(&workspace, entry.dir())?;
        Ok(workspace)
    }

    pub(crate) fn package(&self, key: &PackageKey) -> Option<&Package> {
        self.packages.get(key)
    }

    #[cfg(test)]
    pub(crate) fn build_order(
        &self,
        targets: &[PackageKey],
        include_dev: bool,
    ) -> Result<Vec<PackageKey>, String> {
        let mut state: HashMap<PackageKey, VisitState> = HashMap::new();
        let mut order = Vec::new();
        let mut stack = Vec::new();

        for target in targets {
            if !self.packages.contains_key(target) {
                return Err(format!(
                    "package `{}` is not part of the workspace loaded from `{}`",
                    target.name,
                    self.manifest_path.display()
                ));
            }
            visit(
                self,
                target,
                include_dev,
                &mut state,
                &mut order,
                &mut stack,
                0,
            )?;
        }

        Ok(order)
    }
}

enum RootFile<'a> {
    Selected(&'a ManifestFile),
    Loaded(Box<ManifestFile>),
}

impl RootFile<'_> {
    fn file(&self) -> &ManifestFile {
        match self {
            RootFile::Selected(entry) => entry,
            RootFile::Loaded(entry) => entry,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Done,
}

fn find_root(entry: &ManifestFile, choice: ManifestChoice) -> Result<RootFile<'_>, String> {
    if entry.is_workspace() {
        return Ok(RootFile::Selected(entry));
    }

    for parent in entry.dir().ancestors().skip(1) {
        let rusted = parent.join(RUSTED_MANIFEST_FILE);
        let cargo = parent.join(CARGO_MANIFEST_FILE);
        match (rusted.is_file(), cargo.is_file()) {
            (false, false) => continue,
            (true, true) => {
                return Err(format!(
                    "`{}` and `{}` both exist in {}. Delete one of them, or set `manifest = \"cargo\"` or `manifest = \"rusted\"` in the global config, or `RUSTED_MANIFEST=cargo`",
                    rusted.display(),
                    cargo.display(),
                    parent.display()
                ));
            }
            (true, false) if choice == ManifestChoice::Cargo => continue,
            (false, true) if choice == ManifestChoice::Rusted => continue,
            _ => {}
        }

        let candidate = discover(parent, choice)?;
        if candidate.is_workspace() {
            return Ok(RootFile::Loaded(Box::new(candidate)));
        }
    }

    Ok(RootFile::Selected(entry))
}

struct Loader {
    choice: ManifestChoice,
    root_canonical: PathBuf,
    shared_package: Option<Table>,
    shared_dependencies: Option<Table>,
    files: Vec<ManifestFile>,
    keys: Vec<PackageKey>,
    inside: Vec<bool>,
}

pub(crate) fn apply_inheritance(
    file: &mut ManifestFile,
    choice: ManifestChoice,
) -> Result<(), String> {
    let (workspace_dir, shared_package, shared_dependencies) = {
        let root = find_root(file, choice)?;
        let root_file = root.file();
        let workspace = root_file.manifest.workspace.as_ref();
        (
            canonical(root_file.dir())?,
            workspace.and_then(|workspace| workspace.shared_package.clone()),
            workspace.and_then(|workspace| workspace.shared_dependencies.clone()),
        )
    };
    file.manifest.inherit(
        &workspace_dir,
        shared_package.as_ref(),
        shared_dependencies.as_ref(),
    )
}

impl Loader {
    fn new(
        choice: ManifestChoice,
        root_dir: &Path,
        workspace: Option<&WorkspaceConfig>,
    ) -> Result<Self, String> {
        Ok(Self {
            choice,
            root_canonical: canonical(root_dir)?,
            shared_package: workspace.and_then(|workspace| workspace.shared_package.clone()),
            shared_dependencies: workspace
                .and_then(|workspace| workspace.shared_dependencies.clone()),
            files: Vec::new(),
            keys: Vec::new(),
            inside: Vec::new(),
        })
    }

    fn load(
        &mut self,
        dir: &Path,
        skip_without_manifest: bool,
        member: bool,
    ) -> Result<Option<usize>, String> {
        let canonical_dir = canonical(dir)?;
        if let Some(index) = self.keys.iter().position(|key| key.dir == canonical_dir) {
            return Ok(Some(index));
        }

        if skip_without_manifest && !has_manifest(dir) {
            return Ok(None);
        }

        let mut file = discover(dir, self.choice)?;
        if file.manifest.is_virtual {
            return Err(format!(
                "the manifest `{}` is a virtual workspace root and cannot be a package",
                file.path.display()
            ));
        }

        let inside = canonical_dir.starts_with(&self.root_canonical);
        if inside || member {
            file.manifest.inherit(
                &self.root_canonical,
                self.shared_package.as_ref(),
                self.shared_dependencies.as_ref(),
            )?;
        } else {
            apply_inheritance(&mut file, self.choice)?;
        }

        let key = PackageKey::new(file.dir(), &file.manifest.name)?;
        if let Some(index) = self.keys.iter().position(|known| known.dir == key.dir) {
            return Ok(Some(index));
        }
        let index = self.files.len();
        self.files.push(file);
        self.keys.push(key);
        self.inside.push(inside);
        Ok(Some(index))
    }
}

fn effective_resolver(root: &ManifestFile) -> u32 {
    let explicit = root
        .manifest
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.resolver.as_deref())
        .or(root.manifest.resolver.as_deref());
    if let Some(value) = explicit {
        return if value == "2" { 2 } else { 1 };
    }
    if root.manifest.is_virtual {
        return 1;
    }
    match root.manifest.edition {
        Edition::E2021 | Edition::E2024 => 2,
        Edition::E2015 | Edition::E2018 => 1,
    }
}

fn assemble(root: &RootFile<'_>, choice: ManifestChoice) -> Result<Workspace, String> {
    let root_file = root.file();
    let root_dir = canonical(root_file.dir())?;
    let workspace_config = root_file.manifest.workspace.as_ref();
    let resolver = effective_resolver(root_file);

    let mut loader = Loader::new(choice, &root_dir, workspace_config)?;
    let mut members: Vec<usize> = Vec::new();

    let root_is_package = !root_file.manifest.is_virtual;
    if root_is_package {
        let index = loader
            .load(root_file.dir(), false, true)?
            .ok_or_else(|| format!("`{}` has no manifest", root_dir.display()))?;
        mark_member(&loader, &mut members, index, &root_dir)?;
    }

    let excluded_paths = root_file
        .manifest
        .workspace
        .as_ref()
        .map(|config| config.exclude.clone())
        .unwrap_or_default();

    if let Some(config) = root_file.manifest.workspace.as_ref() {
        for pattern in &config.members {
            for dir in expand_pattern(pattern, &root_dir)? {
                if is_excluded(&dir, &root_dir, &excluded_paths) {
                    continue;
                }
                let index = loader.load(&dir, pattern.contains('*'), true)?;
                let Some(index) = index else { continue };
                mark_member(&loader, &mut members, index, &root_dir)?;
            }
        }
    }

    let mut links: Vec<Vec<(DependencyKind, String, usize)>> = Vec::new();
    let mut processed = 0;
    while processed < loader.files.len() {
        resolve_links(
            &mut loader,
            processed,
            &root_dir,
            &excluded_paths,
            &mut members,
            &mut links,
        )?;
        processed += 1;
    }

    let mut packages = BTreeMap::new();
    let files = std::mem::take(&mut loader.files);
    for (index, file) in files.into_iter().enumerate() {
        let key = loader.keys[index].clone();
        let resolved = links[index]
            .iter()
            .map(|(kind, extern_name, target)| DependencyLink {
                kind: *kind,
                extern_name: extern_name.clone(),
                package: loader.keys[*target].clone(),
            })
            .collect();
        let package = Package {
            key: key.clone(),
            manifest_path: file.path.clone(),
            manifest: file.manifest,
            links: resolved,
        };
        packages.insert(key, package);
    }

    let member_keys: Vec<PackageKey> = members
        .iter()
        .map(|index| loader.keys[*index].clone())
        .collect();
    check_duplicate_names(&member_keys, &root_dir)?;
    check_native_links(&packages)?;

    let root_key = if root_is_package {
        loader.keys.first().cloned()
    } else {
        None
    };

    let mut default_targets = Vec::new();
    match workspace_config.and_then(|config| config.default_members.as_ref()) {
        Some(patterns) => {
            for pattern in patterns {
                for dir in expand_pattern(pattern, &root_dir)? {
                    if is_excluded(&dir, &root_dir, &excluded_paths) {
                        continue;
                    }
                    let canonical_dir = canonical(&dir)?;
                    let key = loader
                        .keys
                        .iter()
                        .find(|key| key.dir == canonical_dir)
                        .ok_or_else(|| {
                            format!(
                                "`{}` is listed in `default-members` but is not a member of the workspace at `{}`",
                                dir.display(),
                                root_dir.display()
                            )
                        })?
                        .clone();
                    if !default_targets.contains(&key) {
                        default_targets.push(key);
                    }
                }
            }
        }
        None => match &root_key {
            Some(key) => default_targets.push(key.clone()),
            None => default_targets = member_keys.clone(),
        },
    };

    let patches = absolute_source_overrides(&root_file.manifest.patch, &root_dir);
    let replaces = absolute_source_overrides(&root_file.manifest.replace, &root_dir);

    Ok(Workspace {
        root_dir,
        manifest_path: root_file.path.clone(),
        root_key,
        packages,
        members: member_keys,
        default_targets,
        shared_package: workspace_config.and_then(|config| config.shared_package.clone()),
        shared_dependencies: workspace_config.and_then(|config| config.shared_dependencies.clone()),
        resolver,
        profiles: root_file.manifest.profiles.clone(),
        patches,
        replaces,
    })
}

// Makes patch and replace paths absolute against the workspace root, since
// the resolver consumes them from anywhere in the graph.
fn absolute_source_overrides(
    overrides: &BTreeMap<String, Dependency>,
    root_dir: &Path,
) -> BTreeMap<String, Dependency> {
    overrides
        .iter()
        .map(|(name, dependency)| {
            let mut dependency = dependency.clone();
            if let (Some(path), false) = (&mut dependency.path, dependency.git.is_some()) {
                *path = root_dir.join(path.as_str()).display().to_string();
            }
            (name.clone(), dependency)
        })
        .collect()
}

impl Inheritance {
    // Patch and replace entries redirect a crates.io dependency to a local
    // source. Replace keys may pin a version as `name:version`; those only
    // apply when the dependency requirement accepts that version.
    pub(crate) fn redirect_for(&self, package: &str, requirement: &str) -> Option<Dependency> {
        if let Some(patched) = self.patches.get(package) {
            return Some(patched.clone());
        }
        // Replace keys may pin the version as `name:version`; a pinned entry
        // applies only when the consumer requirement accepts that version.
        let requirement = VersionReq::parse(requirement).ok();
        for (key, dependency) in &self.replaces {
            let (name, pinned) = match key.rsplit_once(':') {
                Some((name, version)) if !version.is_empty() => (name, Some(version)),
                _ => (key.as_str(), None),
            };
            if name != package {
                continue;
            }
            if let Some(version) = pinned {
                let accepted = match (&requirement, Version::parse(version)) {
                    (Some(request), Ok(version)) => request.matches(&version),
                    _ => false,
                };
                if !accepted {
                    continue;
                }
            }
            return Some(dependency.clone());
        }
        None
    }
}

fn mark_member(
    loader: &Loader,
    members: &mut Vec<usize>,
    index: usize,
    root_dir: &Path,
) -> Result<(), String> {
    if !members.contains(&index) {
        members.push(index);
    }
    check_member_shape(loader, index, root_dir)
}

fn check_member_shape(loader: &Loader, index: usize, root_dir: &Path) -> Result<(), String> {
    let dir = loader.keys[index].dir.clone();
    if dir == loader.root_canonical {
        return Ok(());
    }
    if loader.files[index].manifest.workspace.is_some() {
        return Err(format!(
            "member `{}` declares its own `[workspace]` table and cannot be a member of `{}`",
            dir.display(),
            root_dir.display()
        ));
    }
    Ok(())
}

fn resolve_links(
    loader: &mut Loader,
    index: usize,
    root_dir: &Path,
    excluded_paths: &[String],
    members: &mut Vec<usize>,
    links: &mut Vec<Vec<(DependencyKind, String, usize)>>,
) -> Result<(), String> {
    while links.len() < loader.files.len() {
        links.push(Vec::new());
    }
    let package_dir = loader.keys[index].dir.clone();
    let package_name = loader.keys[index].name.clone();

    let mut pending: Vec<(DependencyKind, String, Dependency)> = Vec::new();
    let manifest = &loader.files[index].manifest;
    for (kind, dependencies) in manifest.dependency_sections() {
        for (name, dependency) in dependencies {
            pending.push((kind, name.to_string(), dependency.clone()));
        }
    }
    for (_cfg, kind, dependencies) in manifest.target_dependency_sections() {
        for (name, dependency) in dependencies {
            pending.push((kind, name.to_string(), dependency.clone()));
        }
    }

    let mut resolved = Vec::new();
    for (kind, name, dependency) in pending {
        if dependency.workspace {
            return Err(format!(
                "dependency `{name}` of `{package_name}` still has `workspace = true` after inheritance"
            ));
        }
        if dependency.git.is_some() {
            continue;
        }
        let Some(path_text) = &dependency.path else {
            continue;
        };

        let joined = package_dir.join(path_text);
        let target_dir = canonical(&joined).map_err(|error| {
            format!("path dependency `{path_text}` of `{package_name}` does not resolve: {error}")
        })?;

        let target = loader
            .load(&target_dir, false, false)?
            .ok_or_else(|| format!("`{}` has no manifest", target_dir.display()))?;

        let target_name = loader.files[target].manifest.name.clone();
        let target_manifest_path = loader.files[target].path.display().to_string();
        let expected = dependency.package.as_deref().unwrap_or(name.as_str());
        if expected != target_name {
            return Err(format!(
                "`{package_name}` declares dependency `{name}` but `{target_manifest_path}` declares package `{target_name}`. Set `package = \"{target_name}\"` on the dependency, or align the names"
            ));
        }

        if loader.inside[target] && is_excluded(&target_dir, root_dir, excluded_paths) {
            return Err(format!(
                "path dependency `{path_text}` of `{package_name}` is excluded from the workspace at `{}`",
                root_dir.display()
            ));
        }

        if loader.inside[target] {
            mark_member(loader, members, target, root_dir)?;
        }

        resolved.push((kind, name, target));
    }

    while links.len() < loader.files.len() {
        links.push(Vec::new());
    }
    links[index] = resolved;
    Ok(())
}

fn verify_membership(workspace: &Workspace, entry_dir: &Path) -> Result<(), String> {
    let dir = canonical(entry_dir)?;
    if dir == workspace.root_dir {
        return Ok(());
    }
    if workspace.packages.keys().any(|key| key.dir == dir) {
        return Ok(());
    }
    Err(format!(
        "`{}` is not a member of the workspace loaded from `{}`",
        dir.display(),
        workspace.manifest_path.display()
    ))
}

fn check_duplicate_names(members: &[PackageKey], root_dir: &Path) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &PackageKey> = BTreeMap::new();
    for key in members {
        if let Some(previous) = seen.insert(key.name.as_str(), key) {
            return Err(format!(
                "duplicate package name `{}` in the workspace at `{}`: `{}` and `{}` both declare it",
                key.name,
                root_dir.display(),
                previous.dir.display(),
                key.dir.display()
            ));
        }
    }
    Ok(())
}

fn check_native_links(packages: &BTreeMap<PackageKey, Package>) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &PackageKey> = BTreeMap::new();
    for (key, package) in packages {
        let Some(native_name) = package.manifest.links.as_deref() else {
            continue;
        };
        if let Some(previous) = seen.get(native_name) {
            if previous.dir != key.dir {
                return Err(format!(
                    "packages `{}` and `{}` both link to native library `{native_name}`",
                    previous.name, key.name
                ));
            }
        } else {
            seen.insert(native_name, key);
        }
    }
    Ok(())
}

fn expand_pattern(pattern: &str, root_dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !pattern.contains('*') {
        return Ok(vec![root_dir.join(pattern)]);
    }
    if pattern.contains("**") || !pattern.ends_with('*') {
        return Err(format!(
            "only a trailing `*` is supported in workspace patterns, got `{pattern}`"
        ));
    }

    let base_text = pattern.trim_end_matches('*');
    let base_dir = root_dir.join(base_text);
    if !base_dir.is_dir() {
        return Err(format!(
            "workspace member `{pattern}` matched no directories"
        ));
    }
    let mut directories = Vec::new();
    let read = std::fs::read_dir(&base_dir)
        .map_err(|error| format!("workspace member `{pattern}`: {error}"))?;
    for item in read {
        let item = item.map_err(|error| format!("workspace member `{pattern}`: {error}"))?;
        let path = item.path();
        if path.is_dir() {
            directories.push(path);
        }
    }
    directories.sort();
    if directories.is_empty() {
        return Err(format!(
            "workspace member `{pattern}` matched no directories"
        ));
    }
    Ok(directories)
}

fn is_excluded(dir: &Path, root_dir: &Path, exclude: &[String]) -> bool {
    exclude.iter().any(|pattern| {
        let target = root_dir.join(pattern);
        if target.is_dir() {
            return dir.starts_with(&target);
        }
        if pattern.contains('*') {
            return pattern_matches(pattern, dir, root_dir);
        }
        dir == target
    })
}

fn pattern_matches(pattern: &str, dir: &Path, root_dir: &Path) -> bool {
    let Ok(relative) = dir.strip_prefix(root_dir) else {
        return false;
    };
    let relative_text = relative.to_string_lossy();
    let base = pattern.trim_end_matches('*').trim_end_matches('/');
    let Some(rest) = relative_text.strip_prefix(base) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('/') else {
        return false;
    };
    !rest.contains('/')
}

#[cfg(test)]
fn visit(
    workspace: &Workspace,
    key: &PackageKey,
    include_dev: bool,
    state: &mut HashMap<PackageKey, VisitState>,
    order: &mut Vec<PackageKey>,
    stack: &mut Vec<PackageKey>,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_DEPENDENCY_DEPTH {
        return Err(format!(
            "dependency chain is deeper than {MAX_DEPENDENCY_DEPTH}"
        ));
    }

    match state.get(key) {
        Some(VisitState::Done) => return Ok(()),
        Some(VisitState::Visiting) => {
            let start = stack.iter().position(|item| item == key).unwrap_or(0);
            let cycle = stack[start..]
                .iter()
                .map(|item| item.name.clone())
                .chain(std::iter::once(key.name.clone()))
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(format!("dependency cycle: {cycle}"));
        }
        None => {}
    }

    state.insert(key.clone(), VisitState::Visiting);
    stack.push(key.clone());

    if let Some(package) = workspace.packages.get(key) {
        for link in &package.links {
            if link.kind == DependencyKind::Dev && !include_dev {
                continue;
            }
            visit(
                workspace,
                &link.package,
                include_dev,
                state,
                order,
                stack,
                depth + 1,
            )?;
        }
    }

    state.insert(key.clone(), VisitState::Done);
    stack.pop();
    order.push(key.clone());
    Ok(())
}

fn has_manifest(dir: &Path) -> bool {
    dir.join(RUSTED_MANIFEST_FILE).is_file() || dir.join(CARGO_MANIFEST_FILE).is_file()
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Workspace;
    use crate::config::ManifestChoice;
    use crate::manifest::select::{discover_exact, CARGO_MANIFEST_FILE, RUSTED_MANIFEST_FILE};

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("rusted-ws-{}-{name}", std::process::id()));
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

    fn package(name: &str, extra: &str) -> String {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{extra}")
    }

    #[test]
    fn loads_a_path_dependency_workspace() {
        let fixture = Fixture::new("path-ws");
        fixture.write(
            CARGO_MANIFEST_FILE,
            &format!(
                "{}\n[workspace]\n",
                package("app", "[dependencies]\ncore = { path = \"core\" }")
            ),
        );
        fixture.write(
            "core/Cargo.toml",
            &package("core", "[dependencies]\nutil = { path = \"../util\" }"),
        );
        fixture.write("util/Cargo.toml", &package("util", ""));

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        assert_eq!(workspace.members.len(), 3);
        assert_eq!(workspace.default_targets.len(), 1);
        assert_eq!(workspace.default_targets[0].name, "app");

        let order = workspace
            .build_order(&workspace.default_targets, false)
            .unwrap();
        let names: Vec<&str> = order.iter().map(|key| key.name.as_str()).collect();
        assert_eq!(names, vec!["util", "core", "app"]);

        let app = workspace
            .package(workspace.default_targets.first().unwrap())
            .unwrap();
        assert_eq!(app.manifest.name, "app");
        assert_eq!(app.manifest_path, fixture.dir.join(CARGO_MANIFEST_FILE));
        assert_eq!(app.key.name, "app");
        assert_eq!(app.links.len(), 1);
        assert_eq!(app.links[0].extern_name, "core");
    }

    #[test]
    fn loads_a_virtual_workspace_with_globs_excludes_and_mixed_manifests() {
        let fixture = Fixture::new("virtual-ws");
        fixture.write(
            CARGO_MANIFEST_FILE,
            "[workspace]\nmembers = [\"a/*\", \"standalone\"]\nexclude = [\"a/two\"]\n",
        );
        fixture.write("a/one/rusted.toml", &package("one", ""));
        fixture.write("a/two/Cargo.toml", &package("two", ""));
        fixture.write("a/skip/readme.txt", "not a package");
        fixture.write("standalone/rusted.toml", &package("standalone", ""));

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        let names: Vec<&str> = workspace
            .members
            .iter()
            .map(|key| key.name.as_str())
            .collect();
        assert_eq!(names, vec!["one", "standalone"]);
        assert_eq!(workspace.default_targets.len(), 2);

        let order = workspace
            .build_order(&workspace.default_targets, false)
            .unwrap();
        assert_eq!(order.len(), 2);
    }

    #[test]
    fn cyclic_path_dependencies_fail() {
        let fixture = Fixture::new("cycle-ws");
        fixture.write(
            CARGO_MANIFEST_FILE,
            &format!(
                "{}\n[workspace]\n",
                package("app", "[dependencies]\nb = { path = \"b\" }")
            ),
        );
        fixture.write(
            "b/Cargo.toml",
            &package("b", "[dependencies]\napp = { path = \"..\" }"),
        );

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        let error = workspace
            .build_order(&workspace.default_targets, false)
            .unwrap_err();
        assert!(error.contains("dependency cycle"), "{error}");
    }

    #[test]
    fn an_empty_glob_fails() {
        let fixture = Fixture::new("empty-glob");
        fixture.write(
            CARGO_MANIFEST_FILE,
            "[workspace]\nmembers = [\"ghost/*\"]\n",
        );

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let error = Workspace::load(&entry, ManifestChoice::Auto).unwrap_err();
        assert!(error.contains("matched no directories"), "{error}");
    }

    #[test]
    fn a_member_manifest_attaches_to_the_parent_workspace() {
        let fixture = Fixture::new("walk-up");
        fixture.write(CARGO_MANIFEST_FILE, "[workspace]\nmembers = [\"member\"]\n");
        let member_manifest = fixture.write("member/Cargo.toml", &package("member", ""));

        let entry = discover_exact(&member_manifest).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        assert_eq!(workspace.members.len(), 1);
        assert_eq!(workspace.default_targets[0].name, "member");
        assert!(workspace.root_key.is_none());
    }

    #[test]
    fn duplicate_member_names_fail() {
        let fixture = Fixture::new("dup-ws");
        fixture.write(
            CARGO_MANIFEST_FILE,
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        );
        fixture.write("a/Cargo.toml", &package("dup", ""));
        fixture.write("b/Cargo.toml", &package("dup", ""));

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let error = Workspace::load(&entry, ManifestChoice::Auto).unwrap_err();
        assert!(error.contains("duplicate package name `dup`"), "{error}");
    }

    #[test]
    fn package_fields_inherit_from_the_workspace() {
        let fixture = Fixture::new("inherit-package");
        fixture.write(
            CARGO_MANIFEST_FILE,
            "[package]\nname = \"app\"\nversion.workspace = true\nedition.workspace = true\n\
             authors.workspace = true\nlicense.workspace = true\n\n\
             [workspace]\nmembers = []\n\n\
             [workspace.package]\nversion = \"2.3.4\"\nedition = \"2021\"\n\
             authors = [\"A\", \"B\"]\nlicense = \"MIT\"\n",
        );

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        let app = workspace.package(&workspace.members[0]).unwrap();
        assert_eq!(app.manifest.version, "2.3.4");
        assert_eq!(app.manifest.edition, crate::manifest::Edition::E2021);
        assert_eq!(app.manifest.authors, vec!["A", "B"]);
        assert_eq!(app.manifest.license.as_deref(), Some("MIT"));
        assert!(app.manifest.inherited.is_empty());
    }

    #[test]
    fn workspace_dependencies_inherit_and_merge_features() {
        let fixture = Fixture::new("inherit-deps");
        fixture.write(
            CARGO_MANIFEST_FILE,
            &format!(
                "{}\n[workspace]\nmembers = []\n\n\
                 [workspace.dependencies]\nserde = {{ version = \"1\", features = [\"std\"] }}\n\
                 core = {{ path = \"core\" }}\n",
                package(
                    "app",
                    "[dependencies]\nserde = { workspace = true, features = [\"derive\"] }\n\
                     core = { workspace = true }"
                )
            ),
        );
        fixture.write("core/Cargo.toml", &package("core", ""));

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        let app = workspace.package(&workspace.default_targets[0]).unwrap();

        let serde = &app.manifest.dependencies["serde"];
        assert_eq!(serde.requirement.as_deref(), Some("1"));
        assert_eq!(serde.features, vec!["derive", "std"]);
        assert!(!serde.workspace);

        let core = &app.manifest.dependencies["core"];
        let path = core.path.as_deref().unwrap();
        assert!(path.ends_with("core"), "{path}");
        assert!(std::path::Path::new(path).is_absolute(), "{path}");

        let order = workspace
            .build_order(&workspace.default_targets, false)
            .unwrap();
        let names: Vec<&str> = order.iter().map(|key| key.name.as_str()).collect();
        assert_eq!(names, vec!["core", "app"]);
    }

    #[test]
    fn default_members_select_the_build_set() {
        let fixture = Fixture::new("default-members");
        fixture.write(
            CARGO_MANIFEST_FILE,
            &format!(
                "{}\n[workspace]\nmembers = [\"tool\"]\ndefault-members = [\"tool\"]\n",
                package("app", "")
            ),
        );
        fixture.write("tool/Cargo.toml", &package("tool", ""));

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        assert_eq!(workspace.members.len(), 2);
        let names: Vec<&str> = workspace
            .default_targets
            .iter()
            .map(|key| key.name.as_str())
            .collect();
        assert_eq!(names, vec!["tool"]);
    }

    #[test]
    fn path_dependencies_outside_the_workspace_are_loaded() {
        let fixture = Fixture::new("outside-path-dep");
        fixture.write(
            "ws/Cargo.toml",
            &format!(
                "{}\n[workspace]\nmembers = []\n",
                package("app", "[dependencies]\next = { path = \"../ext\" }")
            ),
        );
        fixture.write("ext/Cargo.toml", &package("ext", ""));

        let entry = discover_exact(&fixture.dir.join("ws/Cargo.toml")).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        assert_eq!(workspace.members.len(), 1);
        assert!(workspace.packages.keys().any(|key| key.name == "ext"));

        let order = workspace
            .build_order(&workspace.default_targets, false)
            .unwrap();
        let names: Vec<&str> = order.iter().map(|key| key.name.as_str()).collect();
        assert_eq!(names, vec!["ext", "app"]);
    }

    #[test]
    fn members_outside_the_root_are_supported() {
        let fixture = Fixture::new("outside-member");
        fixture.write("ws/Cargo.toml", "[workspace]\nmembers = [\"../shared\"]\n");
        fixture.write("shared/Cargo.toml", &package("shared", ""));

        let entry = discover_exact(&fixture.dir.join("ws/Cargo.toml")).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();
        let names: Vec<&str> = workspace
            .members
            .iter()
            .map(|key| key.name.as_str())
            .collect();
        assert_eq!(names, vec!["shared"]);
    }

    #[test]
    fn both_manifest_files_in_an_ancestor_fail_the_walk_up() {
        let fixture = Fixture::new("walk-up-both");
        fixture.write(CARGO_MANIFEST_FILE, "[workspace]\nmembers = [\"nested\"]\n");
        fixture.write(
            RUSTED_MANIFEST_FILE,
            "[workspace]\nmembers = [\"nested\"]\n",
        );
        fixture.write("nested/Cargo.toml", &package("nested", ""));

        let entry = discover_exact(&fixture.dir.join("nested/Cargo.toml")).unwrap();
        let error = Workspace::load(&entry, ManifestChoice::Auto).unwrap_err();
        assert!(error.contains("both exist"), "{error}");
    }

    #[test]
    fn an_excluded_path_dependency_fails() {
        let fixture = Fixture::new("excluded-dep");
        fixture.write(
            CARGO_MANIFEST_FILE,
            &format!(
                "{}\n[workspace]\nexclude = [\"core\"]\n",
                package("app", "[dependencies]\ncore = { path = \"core\" }")
            ),
        );
        fixture.write("core/Cargo.toml", &package("core", ""));

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let error = Workspace::load(&entry, ManifestChoice::Auto).unwrap_err();
        assert!(error.contains("excluded from the workspace"), "{error}");
    }

    #[test]
    fn dev_dependencies_are_optional_in_the_build_order() {
        let fixture = Fixture::new("dev-deps");
        fixture.write(
            CARGO_MANIFEST_FILE,
            &format!(
                "{}\n[workspace]\n",
                package("app", "[dev-dependencies]\nhelper = { path = \"helper\" }")
            ),
        );
        fixture.write("helper/Cargo.toml", &package("helper", ""));

        let entry = discover_exact(&fixture.dir.join(CARGO_MANIFEST_FILE)).unwrap();
        let workspace = Workspace::load(&entry, ManifestChoice::Auto).unwrap();

        let without_dev = workspace
            .build_order(&workspace.default_targets, false)
            .unwrap();
        assert_eq!(without_dev.len(), 1);
        assert_eq!(without_dev[0].name, "app");

        let with_dev = workspace
            .build_order(&workspace.default_targets, true)
            .unwrap();
        let names: Vec<&str> = with_dev.iter().map(|key| key.name.as_str()).collect();
        assert_eq!(names, vec!["helper", "app"]);
    }
}
