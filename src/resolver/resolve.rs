use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::config::ManifestChoice;
use crate::manifest::select::{discover, CARGO_MANIFEST_FILE};
use crate::manifest::{DependencyKind, GitSource, Manifest};
use crate::registry::index::IndexEntry;
use crate::resolver::cfg::Cfg;
use crate::resolver::git::{self, GitRefSelection};
use crate::resolver::lockfile::Lockfile;
use crate::resolver::semver::{Version, VersionReq};
use crate::workspace::{apply_inheritance, Inheritance};

const MAX_STEPS: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PackageSource {
    Registry,
    Path(PathBuf),
    Git {
        url: String,
        reference: Option<GitRefSelection>,
        rev: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum BuildContext {
    #[default]
    Target,
    Build,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PackageId {
    pub(crate) name: String,
    pub(crate) version: Version,
    pub(crate) source: PackageSource,
    pub(crate) context: BuildContext,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedDependency {
    pub(crate) package: PackageId,
    pub(crate) extern_name: String,
    pub(crate) kind: DependencyKind,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedPackage {
    pub(crate) id: PackageId,
    pub(crate) version: Version,
    pub(crate) checksum: Option<String>,
    pub(crate) features: BTreeSet<String>,
    pub(crate) dependencies: Vec<ResolvedDependency>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) index: Option<IndexEntry>,
    pub(crate) manifest: Option<Manifest>,
    pub(crate) manifest_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Resolution {
    pub(crate) packages: BTreeMap<PackageId, ResolvedPackage>,
}

pub(crate) trait Registry {
    fn entries(&mut self, name: &str) -> Result<Vec<IndexEntry>, String>;
}

impl Registry for crate::registry::index::IndexClient {
    fn entries(&mut self, name: &str) -> Result<Vec<IndexEntry>, String> {
        crate::registry::index::IndexClient::entries(self, name)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RootPackage {
    pub(crate) manifest: Manifest,
    pub(crate) manifest_path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct RootOptions {
    pub(crate) features: BTreeSet<String>,
    pub(crate) all_features: bool,
    pub(crate) no_default_features: bool,
    pub(crate) locked: bool,
    pub(crate) inheritance: Option<Inheritance>,
    pub(crate) resolver: u32,
    pub(crate) dev_features: bool,
    pub(crate) all_targets: bool,
}

impl Default for RootOptions {
    fn default() -> Self {
        Self {
            features: BTreeSet::new(),
            all_features: false,
            no_default_features: false,
            locked: false,
            inheritance: None,
            resolver: 1,
            dev_features: true,
            all_targets: false,
        }
    }
}

#[derive(Clone)]
struct DepMeta {
    key: String,
    package: String,
    requirement: String,
    source: DepSource,
    target: Option<String>,
    kind: DependencyKind,
    optional: bool,
    default_features: bool,
    features: Vec<String>,
    dev: bool,
}

#[derive(Clone)]
enum DepSource {
    Registry,
    Path(PathBuf),
    Git(GitSource),
}

pub(crate) struct GitContext {
    pub(crate) root: PathBuf,
    pub(crate) offline: bool,
    cache: std::sync::Mutex<BTreeMap<(String, Option<GitRefSelection>), String>>,
}

impl GitContext {
    pub(crate) fn new(root: PathBuf, offline: bool) -> Self {
        Self {
            root,
            offline,
            cache: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    fn resolve_ref(
        &self,
        url: &str,
        reference: Option<&GitRefSelection>,
    ) -> Result<String, String> {
        let cached = self
            .cache
            .lock()
            .unwrap()
            .get(&(url.to_string(), reference.cloned()))
            .cloned();
        if let Some(rev) = cached {
            return Ok(rev);
        }
        let rev = git::resolve_ref(url, reference)?;
        self.cache
            .lock()
            .unwrap()
            .insert((url.to_string(), reference.cloned()), rev.clone());
        Ok(rev)
    }
}

#[derive(Clone)]
struct Meta {
    name: String,
    version: Version,
    checksum: Option<String>,
    index: Option<IndexEntry>,
    manifest: Option<Manifest>,
    manifest_path: Option<PathBuf>,
    dir: PathBuf,
    features: BTreeMap<String, Vec<String>>,
    deps: Vec<DepMeta>,
}

impl Meta {
    fn from_manifest(
        manifest: &Manifest,
        manifest_path: &Path,
        include_dev: bool,
    ) -> Result<Self, String> {
        let version = Version::parse(&manifest.version).map_err(|error| {
            format!(
                "`{}` has invalid version `{}`: {error}",
                manifest.name, manifest.version
            )
        })?;
        let dir = manifest_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()
            .map_err(|error| format!("{}: {error}", manifest_path.display()))?;

        let mut deps = Vec::new();
        for (kind, dependencies) in manifest.dependency_sections() {
            if kind == DependencyKind::Dev && !include_dev {
                continue;
            }
            collect_manifest_deps(&manifest.name, &dir, kind, None, dependencies, &mut deps)?;
        }
        for (cfg, kind, dependencies) in manifest.target_dependency_sections() {
            if kind == DependencyKind::Dev && !include_dev {
                continue;
            }
            collect_manifest_deps(
                &manifest.name,
                &dir,
                kind,
                Some(cfg.to_string()),
                dependencies,
                &mut deps,
            )?;
        }

        Ok(Self {
            name: manifest.name.clone(),
            version,
            checksum: None,
            index: None,
            manifest: Some(manifest.clone()),
            manifest_path: Some(manifest_path.to_path_buf()),
            dir,
            features: manifest.features.clone(),
            deps,
        })
    }

    fn from_index(entry: &IndexEntry) -> Result<Self, String> {
        let mut deps = Vec::new();
        for dependency in &entry.dependencies {
            if dependency.kind == "dev" {
                continue;
            }
            if let Some(registry) = &dependency.registry {
                if registry != "crates-io" {
                    return Err(format!(
                        "package `{}` needs unsupported registry `{registry}`",
                        entry.name
                    ));
                }
            }
            let kind = match dependency.kind.as_str() {
                "normal" => DependencyKind::Normal,
                "build" => DependencyKind::Build,
                _ => {
                    return Err(format!(
                        "package `{}` has unsupported dependency kind `{}`",
                        entry.name, dependency.kind
                    ))
                }
            };
            deps.push(DepMeta {
                key: dependency.name.clone(),
                package: dependency
                    .package
                    .clone()
                    .unwrap_or_else(|| dependency.name.clone()),
                requirement: dependency.requirement.clone(),
                source: DepSource::Registry,
                target: dependency.target.clone(),
                kind,
                optional: dependency.optional,
                default_features: dependency.default_features,
                features: dependency.features.clone(),
                dev: false,
            });
        }

        Ok(Self {
            name: entry.name.clone(),
            version: entry.version.clone(),
            checksum: Some(entry.checksum.clone()),
            index: Some(entry.clone()),
            manifest: None,
            manifest_path: None,
            dir: PathBuf::new(),
            features: entry.features.clone(),
            deps,
        })
    }
}

fn collect_manifest_deps(
    package_name: &str,
    package_dir: &Path,
    kind: DependencyKind,
    target: Option<String>,
    dependencies: &BTreeMap<String, crate::manifest::Dependency>,
    out: &mut Vec<DepMeta>,
) -> Result<(), String> {
    for (key, dependency) in dependencies {
        if dependency.workspace {
            return Err(format!(
                "dependency `{key}` of `{package_name}` still has `workspace = true` after inheritance"
            ));
        }

        let requirement = dependency
            .requirement
            .clone()
            .unwrap_or_else(|| "*".to_string());
        let package = dependency.package.clone().unwrap_or_else(|| key.clone());
        if let Some(git) = &dependency.git {
            out.push(DepMeta {
                key: key.clone(),
                package,
                requirement,
                source: DepSource::Git(git.clone()),
                target: target.clone(),
                kind,
                optional: dependency.optional,
                default_features: dependency.default_features.unwrap_or(true),
                features: dependency.features.clone(),
                dev: kind == DependencyKind::Dev,
            });
            continue;
        }
        if let Some(registry) = &dependency.registry {
            if registry != "crates-io" {
                return Err(format!(
                    "dependency `{key}` of `{package_name}` uses unsupported registry `{registry}`"
                ));
            }
        }

        let source = match &dependency.path {
            Some(path) => DepSource::Path(package_dir.join(path)),
            None => {
                if dependency.requirement.is_none() {
                    return Err(format!(
                        "dependency `{key}` of `{package_name}` needs a version"
                    ));
                }
                DepSource::Registry
            }
        };

        out.push(DepMeta {
            key: key.clone(),
            package,
            requirement,
            source,
            target: target.clone(),
            kind,
            optional: dependency.optional,
            default_features: dependency.default_features.unwrap_or(true),
            features: dependency.features.clone(),
            dev: kind == DependencyKind::Dev,
        });
    }
    Ok(())
}

#[derive(Clone)]
struct Selected {
    meta: Meta,
    features: BTreeSet<String>,
    expanded: BTreeSet<String>,
    enabled_deps: BTreeSet<(String, DependencyKind)>,
    weak_features: Vec<(String, String)>,
    dependencies: Vec<ResolvedDependency>,
}

impl Selected {
    fn new(meta: Meta) -> Self {
        Self {
            meta,
            features: BTreeSet::new(),
            expanded: BTreeSet::new(),
            enabled_deps: BTreeSet::new(),
            weak_features: Vec::new(),
            dependencies: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct State {
    packages: BTreeMap<PackageId, Selected>,
}

#[derive(Clone)]
enum Task {
    Feature {
        package: PackageId,
        feature: String,
    },
    Dependency {
        parent: PackageId,
        key: String,
        kind: DependencyKind,
        extra_features: Vec<String>,
    },
}

struct Candidate {
    id: PackageId,
    meta: Meta,
    features: Vec<String>,
}

enum Outcome {
    Done,
    New(Vec<Candidate>),
}

struct Context<'a> {
    registry: &'a mut dyn Registry,
    cfg: &'a Cfg,
    lock: Option<&'a Lockfile>,
    manifest_choice: ManifestChoice,
    git: Option<&'a GitContext>,
    inheritance: Option<&'a Inheritance>,
    resolver: u32,
    dev_features: bool,
    all_targets: bool,
    cache: HashMap<String, Vec<IndexEntry>>,
    steps: usize,
}

impl Context<'_> {
    fn entries(&mut self, name: &str) -> Result<Vec<IndexEntry>, String> {
        if let Some(entries) = self.cache.get(name) {
            return Ok(entries.clone());
        }
        let entries = self.registry.entries(name)?;
        self.cache.insert(name.to_string(), entries.clone());
        Ok(entries)
    }
}

pub(crate) fn resolve(
    roots: &[RootPackage],
    registry: &mut dyn Registry,
    cfg: &Cfg,
    lock: Option<&Lockfile>,
    manifest_choice: ManifestChoice,
    git: Option<&GitContext>,
    options: &RootOptions,
) -> Result<Resolution, String> {
    let mut state = State {
        packages: BTreeMap::new(),
    };
    let mut queue = VecDeque::new();

    for root in roots {
        let meta = Meta::from_manifest(&root.manifest, &root.manifest_path, true)?;
        let id = PackageId {
            name: meta.name.clone(),
            version: meta.version.clone(),
            source: PackageSource::Path(meta.dir.clone()),
            context: BuildContext::Target,
        };
        if state.packages.contains_key(&id) {
            return Err(format!("duplicate root package `{}`", id.name));
        }
        state
            .packages
            .insert(id.clone(), Selected::new(meta.clone()));

        let mut requested = BTreeSet::new();
        if options.all_features {
            for feature in meta.features.keys() {
                requested.insert(feature.clone());
            }
            for dependency in &meta.deps {
                if dependency.optional
                    && !meta.features.contains_key(&dependency.key)
                    && implicit_optional_dependency(&meta, &dependency.key)
                {
                    requested.insert(dependency.key.clone());
                }
            }
        } else {
            for feature in &options.features {
                if !is_known_feature(&meta, feature) {
                    return Err(format!(
                        "feature `{feature}` is not defined in `{} {}`",
                        meta.name, meta.version
                    ));
                }
                requested.insert(feature.clone());
            }
            if !options.no_default_features && meta.features.contains_key("default") {
                requested.insert("default".to_string());
            }
        }
        for feature in requested {
            queue.push_back(Task::Feature {
                package: id.clone(),
                feature,
            });
        }
        enqueue_non_optional(
            cfg,
            options.resolver,
            options.all_targets,
            &mut state,
            &mut queue,
            &id,
        )?;
    }

    let mut context = Context {
        registry,
        cfg,
        lock,
        manifest_choice,
        git,
        inheritance: options.inheritance.as_ref(),
        resolver: options.resolver,
        dev_features: options.dev_features,
        all_targets: options.all_targets,
        cache: HashMap::new(),
        steps: 0,
    };
    let state = solve(&mut context, state, queue)?;

    let mut packages = BTreeMap::new();
    for (id, selected) in state.packages {
        packages.insert(
            id.clone(),
            ResolvedPackage {
                id,
                version: selected.meta.version.clone(),
                checksum: selected.meta.checksum.clone(),
                features: selected.features,
                dependencies: selected.dependencies,
                index: selected.meta.index,
                manifest: selected.meta.manifest,
                manifest_path: selected.meta.manifest_path,
            },
        );
    }
    let resolution = Resolution { packages };

    if options.locked {
        let lock = lock.ok_or("`--locked` requires an existing Cargo.lock")?;
        lock.matches(&resolution)?;
    }

    Ok(resolution)
}

fn solve(
    context: &mut Context<'_>,
    mut state: State,
    mut queue: VecDeque<Task>,
) -> Result<State, String> {
    while let Some(task) = queue.pop_front() {
        context.steps += 1;
        if context.steps > MAX_STEPS {
            return Err(format!(
                "resolution exceeded {MAX_STEPS} steps, the dependency graph is too complex"
            ));
        }

        match task {
            Task::Feature { package, feature } => {
                process_feature(context, &mut state, &mut queue, &package, &feature)?;
            }
            Task::Dependency {
                parent,
                key,
                kind,
                extra_features,
            } => match process_dependency(
                context,
                &mut state,
                &mut queue,
                &parent,
                &key,
                kind,
                &extra_features,
            )? {
                Outcome::Done => {}
                Outcome::New(candidates) => {
                    let mut last_error = String::new();
                    for candidate in candidates {
                        let mut next_state = state.clone();
                        let mut next_queue = queue.clone();
                        add_edge(&mut next_state, &parent, &candidate.id, &key, kind);
                        next_state
                            .packages
                            .insert(candidate.id.clone(), Selected::new(candidate.meta));
                        for feature in candidate.features {
                            next_queue.push_back(Task::Feature {
                                package: candidate.id.clone(),
                                feature,
                            });
                        }
                        enqueue_non_optional(
                            context.cfg,
                            context.resolver,
                            context.all_targets,
                            &mut next_state,
                            &mut next_queue,
                            &candidate.id,
                        )?;
                        match solve(context, next_state, next_queue) {
                            Ok(state) => return Ok(state),
                            Err(error) => last_error = error,
                        }
                    }
                    return Err(last_error);
                }
            },
        }
    }
    Ok(state)
}

fn process_dependency(
    context: &mut Context<'_>,
    state: &mut State,
    queue: &mut VecDeque<Task>,
    parent: &PackageId,
    key: &str,
    kind: DependencyKind,
    extra_features: &[String],
) -> Result<Outcome, String> {
    let mut parent_meta = state
        .packages
        .get(parent)
        .ok_or_else(|| format!("internal error: parent `{}` is missing", parent.name))?
        .meta
        .clone();
    if let Some(inheritance) = context.inheritance {
        apply_source_overrides(inheritance, &mut parent_meta.deps)?;
        state.packages.get_mut(parent).unwrap().meta.deps = parent_meta.deps.clone();
    }
    let dependency = parent_meta
        .deps
        .iter()
        .find(|dependency| dependency.key == key && dependency.kind == kind)
        .cloned()
        .ok_or_else(|| {
            format!(
                "internal error: `{}` has no `{kind:?}` dependency `{key}`",
                parent_meta.name
            )
        })?;

    if !target_matches(
        context.cfg,
        context.resolver,
        context.all_targets,
        &dependency.target,
    )? {
        return Ok(Outcome::Done);
    }

    match &dependency.source {
        DepSource::Path(path) => {
            let directory = path
                .canonicalize()
                .map_err(|error| format!("path dependency `{}`: {error}", path.display()))?;
            let package_source = PackageSource::Path(directory.clone());
            resolve_path_like(
                state,
                queue,
                parent,
                key,
                &dependency,
                extra_features,
                context,
                package_source,
                &directory,
            )
        }
        DepSource::Git(git_source) => {
            let git = context.git.ok_or_else(|| {
                format!(
                    "dependency `{key}` of `{}` is a git dependency, but no git context is available",
                    parent_meta.name
                )
            })?;
            let url = git::canonical_url(&git_source.url);
            let reference = git_reference(git_source).map_err(|error| {
                format!("dependency `{key}` of `{}`: {error}", parent_meta.name)
            })?;
            let rev = match locked_git_rev(context.lock, &url, reference.as_ref()) {
                Some(rev) => rev,
                None if git.offline => {
                    return Err(format!(
                        "git dependency `{key}` of `{}` points at `{url}`, which is not pinned in the lockfile and offline mode is enabled",
                        parent_meta.name
                    ));
                }
                None => git.resolve_ref(&url, reference.as_ref())?,
            };
            let directory = git::checkout_dir(&git.root, &rev);
            if git.offline && !directory.join(CARGO_MANIFEST_FILE).is_file() {
                return Err(format!(
                    "git checkout for `{url}` at revision `{rev}` is missing and offline mode is enabled",
                ));
            }
            let directory = git::checkout(&git.root, &url, &rev)?;
            let directory = directory
                .canonicalize()
                .map_err(|error| format!("git checkout for `{}`: {error}", directory.display()))?;
            let package_source = PackageSource::Git {
                url,
                reference: reference.clone(),
                rev: rev.clone(),
            };
            resolve_path_like(
                state,
                queue,
                parent,
                key,
                &dependency,
                extra_features,
                context,
                package_source,
                &directory,
            )
        }
        DepSource::Registry => {
            let name = dependency.package.clone();
            let build_context = dependency_context(parent, kind);
            let requirement = VersionReq::parse(&dependency.requirement).map_err(|error| {
                format!(
                    "dependency `{key}` of `{}` has invalid requirement `{}`: {error}",
                    parent_meta.name, dependency.requirement
                )
            })?;
            let existing = state
                .packages
                .iter()
                .filter(|(id, selected)| {
                    id.name == name
                        && id.source == PackageSource::Registry
                        && id.context == build_context
                        && requirement.matches(&selected.meta.version)
                })
                .map(|(id, _)| id.clone())
                .max_by(|left, right| left.version.cmp(&right.version));
            if let Some(id) = existing {
                merge_selected(
                    state,
                    queue,
                    &id,
                    &dependency,
                    extra_features,
                    context.resolver,
                    context.dev_features,
                );
                add_edge(state, parent, &id, key, dependency.kind);
                return Ok(Outcome::Done);
            }

            let entries = context.entries(&name)?;
            if entries.is_empty() {
                return Err(format!(
                    "package `{name}` required by `{} {}` was not found in the registry",
                    parent_meta.name, parent_meta.version
                ));
            }
            let locked = context
                .lock
                .map(|lock| lock.preferred_versions(&name, true))
                .unwrap_or_default();
            for existing_id in state.packages.keys() {
                if existing_id.name != name
                    || existing_id.source != PackageSource::Registry
                    || existing_id.context != build_context
                    || requirement.matches(&existing_id.version)
                {
                    continue;
                }
                let compatible_available = entries.iter().any(|entry| {
                    same_compatibility_class(&existing_id.version, &entry.version)
                        && requirement.matches(&entry.version)
                        && (!entry.yanked || locked.contains(&entry.version))
                });
                if compatible_available {
                    return Err(format!(
                        "conflicting requirements for `{name}`: selected {} does not satisfy `{}` and one compatible version must serve both",
                        existing_id.version, dependency.requirement
                    ));
                }
            }
            let mut versions: Vec<IndexEntry> = entries
                .into_iter()
                .filter(|entry| {
                    requirement.matches(&entry.version)
                        && (!entry.yanked || locked.contains(&entry.version))
                })
                .collect();
            versions.sort_by(|left, right| right.version.cmp(&left.version));
            let mut ordered = Vec::new();
            for locked_version in &locked {
                if let Some(index) = versions
                    .iter()
                    .position(|entry| &entry.version == locked_version)
                {
                    ordered.push(versions.remove(index));
                }
            }
            ordered.extend(versions);
            if ordered.is_empty() {
                return Err(format!(
                    "no version of `{name}` matches `{}` required by `{} {}`",
                    dependency.requirement, parent_meta.name, parent_meta.version
                ));
            }

            let mut candidates = Vec::new();
            let mut last_error = None;
            for entry in ordered {
                match Meta::from_index(&entry) {
                    Ok(meta) => {
                        let features = features_for(
                            &meta,
                            &dependency,
                            extra_features,
                            context.resolver,
                            context.dev_features,
                        );
                        candidates.push(Candidate {
                            id: PackageId {
                                name: name.clone(),
                                version: meta.version.clone(),
                                source: PackageSource::Registry,
                                context: build_context,
                            },
                            meta,
                            features,
                        });
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            if candidates.is_empty() {
                return Err(last_error
                    .unwrap_or_else(|| format!("no usable version of `{name}` was found")));
            }
            Ok(Outcome::New(candidates))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_path_like(
    state: &mut State,
    queue: &mut VecDeque<Task>,
    parent: &PackageId,
    key: &str,
    dependency: &DepMeta,
    extra_features: &[String],
    context: &Context<'_>,
    package_source: PackageSource,
    directory: &Path,
) -> Result<Outcome, String> {
    let parent_meta = state
        .packages
        .get(parent)
        .ok_or_else(|| format!("internal error: parent `{}` is missing", parent.name))?
        .meta
        .clone();
    let mut file = discover(directory, context.manifest_choice)?;
    if file.manifest.is_virtual {
        return Err(format!(
            "`{}` points at a virtual workspace",
            directory.display()
        ));
    }
    match context
        .inheritance
        .filter(|inheritance| inheritance.applies(directory))
    {
        Some(inheritance) => file.manifest.inherit(
            &inheritance.workspace_dir,
            inheritance.shared_package.as_ref(),
            inheritance.shared_dependencies.as_ref(),
        )?,
        None => apply_inheritance(&mut file, context.manifest_choice)?,
    }
    if file.manifest.name != dependency.package {
        return Err(format!(
            "`{}` depends on `{key}` as package `{}`, but `{}` declares package `{}`",
            parent_meta.name,
            dependency.package,
            file.path.display(),
            file.manifest.name
        ));
    }
    let meta = Meta::from_manifest(&file.manifest, &file.path, false)?;
    if dependency.requirement != "*" {
        check_requirement(dependency, &meta.version, &parent_meta)?;
    }
    let build_context = dependency_context(parent, dependency.kind);
    let id = PackageId {
        name: meta.name.clone(),
        version: meta.version.clone(),
        source: package_source,
        context: build_context,
    };
    if state.packages.contains_key(&id) {
        check_requirement(dependency, &state.packages[&id].meta.version, &parent_meta)?;
        merge_selected(
            state,
            queue,
            &id,
            dependency,
            extra_features,
            context.resolver,
            context.dev_features,
        );
        add_edge(state, parent, &id, key, dependency.kind);
        return Ok(Outcome::Done);
    }
    let features = features_for(
        &meta,
        dependency,
        extra_features,
        context.resolver,
        context.dev_features,
    );
    Ok(Outcome::New(vec![Candidate { id, meta, features }]))
}

fn git_reference(source: &GitSource) -> Result<Option<GitRefSelection>, String> {
    match (&source.branch, &source.tag, &source.rev) {
        (None, None, None) => Ok(None),
        (Some(branch), None, None) => Ok(Some(GitRefSelection::Branch(branch.clone()))),
        (None, Some(tag), None) => Ok(Some(GitRefSelection::Tag(tag.clone()))),
        (None, None, Some(rev)) => Ok(Some(GitRefSelection::Rev(rev.clone()))),
        _ => Err("a git dependency cannot combine `branch`, `tag`, and `rev`".to_string()),
    }
}

fn locked_git_rev(
    lock: Option<&Lockfile>,
    url: &str,
    reference: Option<&GitRefSelection>,
) -> Option<String> {
    let lock = lock?;
    let refspec = match reference {
        Some(GitRefSelection::Branch(branch)) => format!("?branch={branch}"),
        Some(GitRefSelection::Tag(tag)) => format!("?tag={tag}"),
        Some(GitRefSelection::Rev(rev)) => format!("?rev={rev}"),
        None => String::new(),
    };
    let prefix = format!("git+{url}{refspec}#");
    lock.packages.iter().find_map(|package| {
        let sha = package.source.as_ref()?.strip_prefix(&prefix)?;
        Some(sha.to_string())
    })
}

fn check_requirement(dependency: &DepMeta, version: &Version, parent: &Meta) -> Result<(), String> {
    if dependency.requirement == "*" {
        return Ok(());
    }
    let requirement = VersionReq::parse(&dependency.requirement).map_err(|error| {
        format!(
            "dependency `{}` of `{}` has invalid requirement `{}`: {error}",
            dependency.key, parent.name, dependency.requirement
        )
    })?;
    if requirement.matches(version) {
        Ok(())
    } else {
        Err(format!(
            "`{} {}` requires `{} {}`, but `{}` is selected",
            parent.name, parent.version, dependency.key, dependency.requirement, version
        ))
    }
}

fn requested_features(dependency: &DepMeta, extra: &[String]) -> Vec<String> {
    let mut features: BTreeSet<String> = dependency.features.iter().cloned().collect();
    features.extend(extra.iter().cloned());
    features.into_iter().collect()
}

fn features_for(
    meta: &Meta,
    dependency: &DepMeta,
    extra: &[String],
    resolver: u32,
    dev_features: bool,
) -> Vec<String> {
    if resolver >= 2 && !dev_features && dependency.dev {
        return Vec::new();
    }
    let mut features: BTreeSet<String> =
        requested_features(dependency, extra).into_iter().collect();
    if dependency.default_features && meta.features.contains_key("default") {
        features.insert("default".to_string());
    }
    features.into_iter().collect()
}

fn merge_selected(
    state: &mut State,
    queue: &mut VecDeque<Task>,
    id: &PackageId,
    dependency: &DepMeta,
    extra_features: &[String],
    resolver: u32,
    dev_features: bool,
) {
    let selected = state
        .packages
        .get(id)
        .expect("selected package disappeared");
    let features = features_for(
        &selected.meta,
        dependency,
        extra_features,
        resolver,
        dev_features,
    );
    for feature in features {
        queue.push_back(Task::Feature {
            package: id.clone(),
            feature,
        });
    }
}

fn enqueue_non_optional(
    cfg: &Cfg,
    resolver: u32,
    all_targets: bool,
    state: &mut State,
    queue: &mut VecDeque<Task>,
    id: &PackageId,
) -> Result<(), String> {
    let deps = state
        .packages
        .get(id)
        .ok_or_else(|| format!("internal error: `{}` is missing", id.name))?
        .meta
        .deps
        .clone();
    for dependency in deps {
        if dependency.optional {
            continue;
        }
        if !target_matches(cfg, resolver, all_targets, &dependency.target)? {
            continue;
        }
        let selected = state
            .packages
            .get_mut(id)
            .ok_or_else(|| format!("internal error: `{}` is missing", id.name))?;
        if selected
            .enabled_deps
            .insert((dependency.key.clone(), dependency.kind))
        {
            queue.push_back(Task::Dependency {
                parent: id.clone(),
                key: dependency.key,
                kind: dependency.kind,
                extra_features: Vec::new(),
            });
        }
    }
    Ok(())
}

fn process_feature(
    context: &mut Context<'_>,
    state: &mut State,
    queue: &mut VecDeque<Task>,
    package: &PackageId,
    feature: &str,
) -> Result<(), String> {
    let meta = state
        .packages
        .get(package)
        .ok_or_else(|| format!("internal error: `{}` is missing", package.name))?
        .meta
        .clone();
    if state
        .packages
        .get(package)
        .map(|selected| selected.expanded.contains(feature))
        .unwrap_or(false)
    {
        return Ok(());
    }

    let items = match meta.features.get(feature) {
        Some(items) => items.clone(),
        None => {
            if implicit_optional_dependency(&meta, feature) {
                if let Some(selected) = state.packages.get_mut(package) {
                    selected.features.insert(feature.to_string());
                    selected.expanded.insert(feature.to_string());
                }
                activate_optional(state, queue, package, feature, false)?;
                return Ok(());
            }
            return Err(format!(
                "feature `{feature}` was not found in `{} {}`",
                meta.name, meta.version
            ));
        }
    };

    if let Some(selected) = state.packages.get_mut(package) {
        selected.features.insert(feature.to_string());
        selected.expanded.insert(feature.to_string());
    }

    for item in items {
        process_feature_item(context, state, queue, package, &meta, &item)?;
    }
    Ok(())
}

fn process_feature_item(
    _context: &mut Context<'_>,
    state: &mut State,
    queue: &mut VecDeque<Task>,
    package: &PackageId,
    meta: &Meta,
    item: &str,
) -> Result<(), String> {
    if let Some(key) = item.strip_prefix("dep:") {
        let dependency = meta
            .deps
            .iter()
            .find(|dependency| dependency.key == key)
            .ok_or_else(|| {
                format!(
                    "feature of `{} {}` references unknown dependency `{key}`",
                    meta.name, meta.version
                )
            })?;
        if !dependency.optional {
            return Err(format!(
                "`dep:{key}` in `{} {}` requires an optional dependency",
                meta.name, meta.version
            ));
        }
        activate_optional(state, queue, package, key, false)?;
        return Ok(());
    }

    if let Some((dependency_key, feature)) = item.split_once('/') {
        let weak = dependency_key.ends_with('?');
        let dependency_key = dependency_key.trim_end_matches('?');
        if !meta
            .deps
            .iter()
            .any(|dependency| dependency.key == dependency_key)
        {
            return Err(format!(
                "feature of `{} {}` references unknown dependency `{dependency_key}`",
                meta.name, meta.version
            ));
        }
        let dependency_kind = meta
            .deps
            .iter()
            .find(|dependency| dependency.key == dependency_key)
            .map(|dependency| dependency.kind)
            .unwrap_or(DependencyKind::Normal);
        if weak {
            let optional = meta
                .deps
                .iter()
                .find(|dependency| dependency.key == dependency_key)
                .map(|dependency| dependency.optional)
                .unwrap_or(false);
            if !optional {
                return Err(format!(
                    "feature of `{} {}` uses `{dependency_key}?/{feature}`, but `{dependency_key}` is not optional",
                    meta.name, meta.version
                ));
            }
            let enabled = state
                .packages
                .get(package)
                .map(|selected| {
                    selected
                        .enabled_deps
                        .iter()
                        .any(|(key, _)| key == dependency_key)
                })
                .unwrap_or(false);
            if enabled {
                queue.push_back(Task::Dependency {
                    parent: package.clone(),
                    key: dependency_key.to_string(),
                    kind: dependency_kind,
                    extra_features: vec![feature.to_string()],
                });
            } else if let Some(selected) = state.packages.get_mut(package) {
                selected
                    .weak_features
                    .push((dependency_key.to_string(), feature.to_string()));
            }
        } else {
            activate_optional(state, queue, package, dependency_key, true)?;
            queue.push_back(Task::Dependency {
                parent: package.clone(),
                key: dependency_key.to_string(),
                kind: dependency_kind,
                extra_features: vec![feature.to_string()],
            });
        }
        return Ok(());
    }

    if meta.features.contains_key(item) {
        queue.push_back(Task::Feature {
            package: package.clone(),
            feature: item.to_string(),
        });
        return Ok(());
    }
    if implicit_optional_dependency(meta, item) {
        activate_optional(state, queue, package, item, true)?;
        return Ok(());
    }
    Err(format!(
        "feature of `{} {}` references `{item}`, which is not a feature or optional dependency",
        meta.name, meta.version
    ))
}

fn activate_optional(
    state: &mut State,
    queue: &mut VecDeque<Task>,
    parent: &PackageId,
    key: &str,
    implicit_feature: bool,
) -> Result<(), String> {
    let dependency = state
        .packages
        .get(parent)
        .and_then(|selected| {
            selected
                .meta
                .deps
                .iter()
                .find(|dependency| dependency.key == key)
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "internal error: `{}` has no dependency `{key}`",
                parent.name
            )
        })?;
    if !dependency.optional {
        return Ok(());
    }
    if implicit_feature {
        if let Some(selected) = state.packages.get_mut(parent) {
            selected.features.insert(key.to_string());
        }
    }
    let newly_enabled = state
        .packages
        .get_mut(parent)
        .map(|selected| {
            selected
                .enabled_deps
                .insert((key.to_string(), dependency.kind))
        })
        .unwrap_or(false);
    if !newly_enabled {
        return Ok(());
    }

    queue.push_back(Task::Dependency {
        parent: parent.clone(),
        key: key.to_string(),
        kind: dependency.kind,
        extra_features: Vec::new(),
    });
    let weak = state
        .packages
        .get(parent)
        .map(|selected| selected.weak_features.clone())
        .unwrap_or_default();
    for (dependency_key, feature) in weak {
        if dependency_key == key {
            let weak_kind = state
                .packages
                .get(parent)
                .and_then(|selected| {
                    selected
                        .meta
                        .deps
                        .iter()
                        .find(|dependency| dependency.key == dependency_key)
                        .map(|dependency| dependency.kind)
                })
                .unwrap_or(DependencyKind::Normal);
            queue.push_back(Task::Dependency {
                parent: parent.clone(),
                key: dependency_key,
                kind: weak_kind,
                extra_features: vec![feature],
            });
        }
    }
    Ok(())
}

fn is_known_feature(meta: &Meta, feature: &str) -> bool {
    meta.features.contains_key(feature) || implicit_optional_dependency(meta, feature)
}

fn implicit_optional_dependency(meta: &Meta, key: &str) -> bool {
    if !meta
        .deps
        .iter()
        .any(|dependency| dependency.key == key && dependency.optional)
    {
        return false;
    }
    !meta.features.values().flatten().any(|item| {
        item.strip_prefix("dep:")
            .map(|name| name == key)
            .unwrap_or(false)
    })
}

fn dependency_context(parent: &PackageId, kind: DependencyKind) -> BuildContext {
    if parent.context == BuildContext::Build || kind == DependencyKind::Build {
        BuildContext::Build
    } else {
        BuildContext::Target
    }
}

fn same_compatibility_class(left: &Version, right: &Version) -> bool {
    if left.major == 0 && right.major == 0 {
        left.minor == right.minor
    } else {
        left.major == right.major
    }
}

// Redirects Registry-source deps of a package through patch overrides and
// replace entries. Consumer requirements, features, and optionality are kept,
// so a patched version must still satisfy every dependency requirement.
fn apply_source_overrides(inheritance: &Inheritance, deps: &mut [DepMeta]) -> Result<(), String> {
    for dependency in deps.iter_mut() {
        if !matches!(dependency.source, DepSource::Registry) {
            continue;
        }
        let Some(patched) = inheritance.redirect_for(&dependency.package, &dependency.requirement)
        else {
            continue;
        };
        if patched.path.is_none() && patched.git.is_none() {
            return Err(format!(
                "override for `{}` must set a `path` or `git` source",
                dependency.package
            ));
        }
        if patched.workspace {
            return Err(format!(
                "override for `{}` cannot use `workspace = true`",
                dependency.package
            ));
        }
        let source = match (&patched.path, &patched.git) {
            (Some(path), None) => DepSource::Path(PathBuf::from(path)),
            (None, Some(git)) => DepSource::Git(git.clone()),
            _ => DepSource::Registry,
        };
        let mut features = dependency.features.clone();
        features.extend(patched.features.iter().cloned());
        dependency.source = source;
        dependency.features = features;
    }
    Ok(())
}

fn target_matches(
    cfg: &Cfg,
    resolver: u32,
    all_targets: bool,
    target: &Option<String>,
) -> Result<bool, String> {
    if all_targets || resolver < 2 {
        return Ok(true);
    }
    match target {
        Some(expression) => cfg.matches(expression),
        None => Ok(true),
    }
}

fn add_edge(
    state: &mut State,
    parent: &PackageId,
    target: &PackageId,
    extern_name: &str,
    kind: DependencyKind,
) {
    if let Some(selected) = state.packages.get_mut(parent) {
        if !selected
            .dependencies
            .iter()
            .any(|edge| edge.package == *target && edge.extern_name == extern_name)
        {
            selected.dependencies.push(ResolvedDependency {
                package: target.clone(),
                extern_name: extern_name.to_string(),
                kind,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use super::{
        resolve, BuildContext, GitContext, PackageSource, Registry, RootOptions, RootPackage,
    };
    use crate::config::ManifestChoice;
    use crate::manifest::Manifest;
    use crate::registry::index::{IndexDependency, IndexEntry};
    use crate::resolver::cfg::Cfg;
    use crate::resolver::git::test_support::{git_or_skip, TestRepo};
    use crate::resolver::git::GitRefSelection;
    use crate::resolver::lockfile::{LockPackage, Lockfile, REGISTRY_SOURCE};
    use crate::resolver::resolve::PackageId;
    use crate::resolver::semver::Version;

    struct FakeRegistry {
        entries: BTreeMap<String, Vec<IndexEntry>>,
    }

    impl FakeRegistry {
        fn new() -> Self {
            Self {
                entries: BTreeMap::new(),
            }
        }

        fn add(&mut self, entry: IndexEntry) {
            self.entries
                .entry(entry.name.clone())
                .or_default()
                .push(entry);
        }
    }

    impl Registry for FakeRegistry {
        fn entries(&mut self, name: &str) -> Result<Vec<IndexEntry>, String> {
            Ok(self.entries.get(name).cloned().unwrap_or_default())
        }
    }

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-resolve-{}-{name}", std::process::id()));
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

        fn root(&self, manifest: &str) -> RootPackage {
            let path = self.write("Cargo.toml", manifest);
            RootPackage {
                manifest: Manifest::parse(manifest, &path).unwrap(),
                manifest_path: path,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn cfg() -> Cfg {
        Cfg::new(
            ["unix".to_string()],
            [
                ("target_os".to_string(), "linux".to_string()),
                ("target_arch".to_string(), "x86_64".to_string()),
            ],
            "x86_64-unknown-linux-gnu",
        )
    }

    fn entry(
        name: &str,
        version: &str,
        deps: Vec<IndexDependency>,
        features: BTreeMap<String, Vec<String>>,
        yanked: bool,
    ) -> IndexEntry {
        IndexEntry {
            name: name.to_string(),
            version: Version::parse(version).unwrap(),
            dependencies: deps,
            checksum: format!("{:0>64}", name),
            features,
            yanked,
            rust_version: None,
        }
    }

    fn dep(
        name: &str,
        requirement: &str,
        optional: bool,
        default_features: bool,
        features: Vec<&str>,
        target: Option<&str>,
    ) -> IndexDependency {
        IndexDependency {
            name: name.to_string(),
            requirement: requirement.to_string(),
            features: features.into_iter().map(str::to_string).collect(),
            optional,
            default_features,
            target: target.map(str::to_string),
            kind: "normal".to_string(),
            registry: None,
            package: None,
        }
    }

    fn features(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(name, values)| {
                (
                    name.to_string(),
                    values.iter().map(|value| value.to_string()).collect(),
                )
            })
            .collect()
    }

    fn options() -> RootOptions {
        RootOptions::default()
    }

    fn require<'a>(resolution: &'a super::Resolution, name: &str) -> &'a super::ResolvedPackage {
        resolution
            .packages
            .values()
            .find(|package| package.id.name == name)
            .unwrap_or_else(|| panic!("missing package {name}"))
    }

    #[test]
    fn uses_a_locked_yanked_version() {
        let fixture = Fixture::new("yanked-locked");
        let root = fixture
            .root("[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"1\"\n");
        let mut registry = FakeRegistry::new();
        registry.add(entry("foo", "1.0.0", vec![], BTreeMap::new(), true));

        let error = resolve(
            &[root.clone()],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap_err();
        assert!(error.contains("no version of `foo` matches"), "{error}");

        let lock = Lockfile {
            version: 4,
            packages: vec![LockPackage {
                name: "foo".to_string(),
                version: Version::parse("1.0.0").unwrap(),
                source: Some(REGISTRY_SOURCE.to_string()),
                checksum: Some(format!("{:0>64}", "foo")),
                dependencies: Vec::new(),
            }],
        };
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            Some(&lock),
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert_eq!(
            require(&resolution, "foo").version,
            Version::parse("1.0.0").unwrap()
        );
    }

    #[test]
    fn build_dependency_features_do_not_unify_with_normal_features() {
        let fixture = Fixture::new("build-context");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
             [dependencies]\nshared = { version = \"1\", features = [\"normal\"] }\n\
             [build-dependencies]\nshared = { version = \"1\", features = [\"build\"] }\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "shared",
            "1.0.0",
            vec![],
            features(&[("normal", &[]), ("build", &[])]),
            false,
        ));
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();

        let normal = resolution
            .packages
            .values()
            .find(|package| {
                package.id.name == "shared" && package.id.context == BuildContext::Target
            })
            .unwrap();
        let build = resolution
            .packages
            .values()
            .find(|package| {
                package.id.name == "shared" && package.id.context == BuildContext::Build
            })
            .unwrap();
        assert!(normal.features.contains("normal"));
        assert!(!normal.features.contains("build"));
        assert!(build.features.contains("build"));
        assert!(!build.features.contains("normal"));

        let lock = Lockfile::from_resolution(&resolution);
        assert_eq!(
            lock.packages
                .iter()
                .filter(|package| package.name == "shared")
                .count(),
            1
        );
    }

    #[test]
    fn unifies_compatible_requirements_by_backtracking() {
        let fixture = Fixture::new("unify-compatible");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\na = \"1\"\nb = \"1\"\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "a",
            "1.0.0",
            vec![dep("foo", "^1.2", false, true, vec![], None)],
            BTreeMap::new(),
            false,
        ));
        registry.add(entry(
            "b",
            "1.0.0",
            vec![dep("foo", "<1.5", false, true, vec![], None)],
            BTreeMap::new(),
            false,
        ));
        for version in ["1.1.0", "1.4.0", "1.9.0"] {
            registry.add(entry("foo", version, vec![], BTreeMap::new(), false));
        }

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        let versions: Vec<String> = resolution
            .packages
            .values()
            .filter(|package| package.id.name == "foo")
            .map(|package| package.version.to_string())
            .collect();
        assert_eq!(versions, vec!["1.4.0"]);
    }

    #[test]
    fn reports_unmergeable_compatible_requirements() {
        let fixture = Fixture::new("unify-impossible");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\na = \"1\"\nb = \"1\"\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "a",
            "1.0.0",
            vec![dep("foo", "^1.9", false, true, vec![], None)],
            BTreeMap::new(),
            false,
        ));
        registry.add(entry(
            "b",
            "1.0.0",
            vec![dep("foo", "<1.5", false, true, vec![], None)],
            BTreeMap::new(),
            false,
        ));
        for version in ["1.4.0", "1.9.0"] {
            registry.add(entry("foo", version, vec![], BTreeMap::new(), false));
        }

        let error = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap_err();
        assert!(error.contains("conflicting requirements"), "{error}");
    }

    #[test]
    fn picks_the_highest_matching_version_and_transitives() {
        let fixture = Fixture::new("highest");
        let root = fixture
            .root("[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"1\"\n");
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "foo",
            "1.0.0",
            vec![dep("bar", "^1", false, true, vec![], None)],
            BTreeMap::new(),
            false,
        ));
        registry.add(entry("foo", "1.1.0", vec![], BTreeMap::new(), true));
        registry.add(entry("foo", "2.0.0", vec![], BTreeMap::new(), false));
        registry.add(entry("bar", "1.0.0", vec![], BTreeMap::new(), false));
        registry.add(entry("bar", "1.2.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();

        assert_eq!(
            require(&resolution, "foo").version,
            Version::parse("1.0.0").unwrap()
        );
        assert_eq!(
            require(&resolution, "bar").version,
            Version::parse("1.2.0").unwrap()
        );
        assert!(require(&resolution, "foo").index.is_some());
        assert!(require(&resolution, "foo").manifest.is_none());
    }

    #[test]
    fn backtracks_to_a_lower_version_on_a_missing_transitive() {
        let fixture = Fixture::new("backtrack");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \">=1, <3\"\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "foo",
            "2.0.0",
            vec![dep("missing", "^1", false, true, vec![], None)],
            BTreeMap::new(),
            false,
        ));
        registry.add(entry("foo", "1.0.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert_eq!(
            require(&resolution, "foo").version,
            Version::parse("1.0.0").unwrap()
        );
    }

    #[test]
    fn resolves_optional_dependencies_through_features() {
        let fixture = Fixture::new("features");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nbar = { version = \"1\", features = [\"extra\"] }\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "bar",
            "1.0.0",
            vec![dep("qux", "^1", true, true, vec![], None)],
            features(&[("extra", &["dep:qux"]), ("default", &[])]),
            false,
        ));
        registry.add(entry("qux", "1.0.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(require(&resolution, "qux").version == Version::parse("1.0.0").unwrap());
        assert!(require(&resolution, "bar").features.contains("extra"));
    }

    #[test]
    fn default_features_activate_optional_dependencies() {
        let fixture = Fixture::new("default-features");
        let root = fixture
            .root("[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nbar = \"1\"\n");
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "bar",
            "1.0.0",
            vec![dep("qux", "^1", true, true, vec![], None)],
            features(&[("default", &["dep:qux"])]),
            false,
        ));
        registry.add(entry("qux", "1.0.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "qux"));
        assert!(require(&resolution, "bar").features.contains("default"));
    }

    #[test]
    fn implicit_optional_dependency_features_are_recorded() {
        let fixture = Fixture::new("implicit-feature");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nbar = { version = \"1\", features = [\"derive\"] }\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "bar",
            "1.0.0",
            vec![dep("qux", "^1", true, true, vec![], None)],
            features(&[("derive", &["qux"]), ("default", &[])]),
            false,
        ));
        registry.add(entry("qux", "1.0.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(require(&resolution, "bar").features.contains("qux"));
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "qux"));
    }

    #[test]
    fn skips_optional_dependencies_without_features() {
        let fixture = Fixture::new("optional-off");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nbar = { version = \"1\", default-features = false }\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "bar",
            "1.0.0",
            vec![dep("qux", "^1", true, true, vec![], None)],
            features(&[("default", &["dep:qux"])]),
            false,
        ));
        registry.add(entry("qux", "1.0.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(resolution
            .packages
            .values()
            .all(|package| package.id.name != "qux"));
    }

    #[test]
    fn unifies_features_across_dependents() {
        let fixture = Fixture::new("unify");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\na = \"1\"\nb = \"1\"\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "a",
            "1.0.0",
            vec![dep("foo", "^1", false, true, vec!["one"], None)],
            BTreeMap::new(),
            false,
        ));
        registry.add(entry(
            "b",
            "1.0.0",
            vec![dep("foo", "^1", false, true, vec!["two"], None)],
            BTreeMap::new(),
            false,
        ));
        registry.add(entry(
            "foo",
            "1.0.0",
            vec![],
            features(&[("one", &[]), ("two", &[]), ("default", &[])]),
            false,
        ));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        let foo = require(&resolution, "foo");
        assert!(foo.features.contains("one"));
        assert!(foo.features.contains("two"));
    }

    #[test]
    fn weak_features_apply_only_when_the_dependency_is_enabled() {
        let fixture = Fixture::new("weak");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = { version = \"1\", features = [\"weak\"] }\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "foo",
            "1.0.0",
            vec![dep("bar", "^1", true, true, vec![], None)],
            features(&[("weak", &["bar?/x"]), ("default", &[])]),
            false,
        ));
        registry.add(entry(
            "bar",
            "1.0.0",
            vec![],
            features(&[("x", &[]), ("default", &[])]),
            false,
        ));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(resolution
            .packages
            .values()
            .all(|package| package.id.name != "bar"));

        let fixture = Fixture::new("weak-strong");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = { version = \"1\", features = [\"strong\"] }\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "foo",
            "1.0.0",
            vec![dep("bar", "^1", true, true, vec![], None)],
            features(&[("strong", &["dep:bar", "bar?/x"]), ("default", &[])]),
            false,
        ));
        registry.add(entry(
            "bar",
            "1.0.0",
            vec![],
            features(&[("x", &[]), ("default", &[])]),
            false,
        ));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(require(&resolution, "bar").features.contains("x"));
    }

    #[test]
    fn resolver_two_does_not_unify_dev_features_for_normal_builds() {
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
                        [dependencies]\nshared = \"1\"\n\
                        [dev-dependencies]\nshared = { version = \"1\", features = [\"dev\"] }\n";

        let fixture = Fixture::new("dev-features-build");
        let root = fixture.root(manifest);
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "shared",
            "1.0.0",
            vec![],
            features(&[("dev", &[]), ("default", &[])]),
            false,
        ));
        let mut build = options();
        build.resolver = 2;
        build.dev_features = false;
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &build,
        )
        .unwrap();
        assert!(!require(&resolution, "shared").features.contains("dev"));

        let fixture = Fixture::new("dev-features-test");
        let root = fixture.root(manifest);
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "shared",
            "1.0.0",
            vec![],
            features(&[("dev", &[]), ("default", &[])]),
            false,
        ));
        let mut tests = options();
        tests.resolver = 2;
        tests.dev_features = true;
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &tests,
        )
        .unwrap();
        assert!(require(&resolution, "shared").features.contains("dev"));

        let fixture = Fixture::new("dev-features-resolver1");
        let root = fixture.root(manifest);
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "shared",
            "1.0.0",
            vec![],
            features(&[("dev", &[]), ("default", &[])]),
            false,
        ));
        let mut resolver1 = options();
        resolver1.resolver = 1;
        resolver1.dev_features = false;
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &resolver1,
        )
        .unwrap();
        assert!(require(&resolution, "shared").features.contains("dev"));
    }

    #[test]
    fn dep_syntax_disables_the_implicit_optional_feature() {
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
                        [features]\nfull = [\"dep:extra\"]\n\
                        [dependencies]\nextra = { version = \"1\", optional = true }\n";

        let fixture = Fixture::new("dep-syntax");
        let root = fixture.root(manifest);
        let mut registry = FakeRegistry::new();
        registry.add(entry("extra", "1.0.0", vec![], BTreeMap::new(), false));
        let mut implicit = options();
        implicit.features.insert("extra".to_string());
        let error = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &implicit,
        )
        .unwrap_err();
        assert!(error.contains("feature `extra` is not defined"), "{error}");

        let fixture = Fixture::new("dep-syntax-full");
        let root = fixture.root(manifest);
        let mut full = options();
        full.features.insert("full".to_string());
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &full,
        )
        .unwrap();
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "extra"));
        let app = require(&resolution, "app");
        assert!(app.features.contains("full"));
    }

    #[test]
    fn feature_cycles_terminate() {
        let fixture = Fixture::new("feature-cycle");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
             [features]\na = [\"b\"]\nb = [\"a\"]\n",
        );
        let mut registry = FakeRegistry::new();
        let mut options = options();
        options.features.insert("a".to_string());
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options,
        )
        .unwrap();
        let app = require(&resolution, "app");
        assert!(app.features.contains("a"));
        assert!(app.features.contains("b"));
    }

    #[test]
    fn weak_features_require_an_optional_dependency() {
        let fixture = Fixture::new("weak-required");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
             [features]\nw = [\"foo?/x\"]\n\
             [dependencies]\nfoo = \"1\"\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "foo",
            "1.0.0",
            vec![],
            features(&[("x", &[])]),
            false,
        ));
        let mut options = options();
        options.features.insert("w".to_string());
        let error = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options,
        )
        .unwrap_err();
        assert!(error.contains("is not optional"), "{error}");
    }

    #[test]
    fn rejects_git_dependencies() {
        let fixture = Fixture::new("git");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = { git = \"https://example.com/foo\" }\n",
        );
        let mut registry = FakeRegistry::new();
        let error = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap_err();
        assert!(error.contains("git dependency"), "{error}");
    }

    #[test]
    fn includes_build_dependencies() {
        let fixture = Fixture::new("build-deps");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[build-dependencies]\nbuildtool = \"1\"\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry("buildtool", "1.0.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "buildtool"));
    }

    #[test]
    fn dependency_features_activate_and_enable_named_features() {
        let fixture = Fixture::new("dep-feature");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = { version = \"1\", features = [\"x\"] }\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "foo",
            "1.0.0",
            vec![dep("bar", "^1", true, true, vec![], None)],
            features(&[("x", &["bar/y"]), ("default", &[])]),
            false,
        ));
        registry.add(entry(
            "bar",
            "1.0.0",
            vec![],
            features(&[("y", &[]), ("default", &[])]),
            false,
        ));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(require(&resolution, "bar").features.contains("y"));
    }

    #[test]
    fn all_features_enables_every_feature_and_optional_dependency() {
        let fixture = Fixture::new("all-features");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[features]\ndefault = []\nfull = [\"dep:extra\"]\n[dependencies]\nextra = { version = \"1\", optional = true }\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry("extra", "1.0.0", vec![], BTreeMap::new(), false));

        let mut options = RootOptions::default();
        options.all_features = true;
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options,
        )
        .unwrap();
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "extra"));
        assert!(require(&resolution, "app").features.contains("full"));
    }

    #[test]
    fn resolves_path_dependencies_and_their_registry_dependencies() {
        let fixture = Fixture::new("path");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nlib = { path = \"lib\" }\n",
        );
        fixture.write(
            "lib/Cargo.toml",
            "[package]\nname = \"lib\"\nversion = \"0.2.0\"\n[dependencies]\nbaz = \"^1\"\n",
        );
        fixture.write("lib/src/lib.rs", "pub fn value() {}\n");
        let mut registry = FakeRegistry::new();
        registry.add(entry("baz", "1.1.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        let lib = require(&resolution, "lib");
        assert_eq!(lib.version, Version::parse("0.2.0").unwrap());
        assert!(lib.manifest.is_some());
        assert!(lib.manifest_path.is_some());
        assert!(lib.index.is_none());
        assert_eq!(
            require(&resolution, "baz").version,
            Version::parse("1.1.0").unwrap()
        );
    }

    #[test]
    fn filters_target_dependencies() {
        let fixture = Fixture::new("targets");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[target.'cfg(unix)'.dependencies]\nunixdep = \"1\"\n[target.'cfg(windows)'.dependencies]\nwindep = \"1\"\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry("unixdep", "1.0.0", vec![], BTreeMap::new(), false));
        registry.add(entry("windep", "1.0.0", vec![], BTreeMap::new(), false));

        let mut resolver2 = options();
        resolver2.resolver = 2;
        let resolution = resolve(
            &[root.clone()],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &resolver2,
        )
        .unwrap();
        assert_eq!(resolution.packages.len(), 2);
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "unixdep"));
        assert!(!resolution
            .packages
            .values()
            .any(|package| package.id.name == "windep"));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert_eq!(resolution.packages.len(), 3);
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "windep"));
    }

    #[test]
    fn resolves_incompatible_versions_of_the_same_package() {
        let fixture = Fixture::new("multi-version");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\na = \"1\"\nb = \"1\"\n",
        );
        let mut registry = FakeRegistry::new();
        registry.add(entry(
            "a",
            "1.0.0",
            vec![dep("foo", "^1", false, true, vec![], None)],
            BTreeMap::new(),
            false,
        ));
        registry.add(entry(
            "b",
            "1.0.0",
            vec![dep("foo", "^2", false, true, vec![], None)],
            BTreeMap::new(),
            false,
        ));
        registry.add(entry("foo", "1.0.0", vec![], BTreeMap::new(), false));
        registry.add(entry("foo", "2.0.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        let versions: Vec<String> = resolution
            .packages
            .values()
            .filter(|package| package.id.name == "foo")
            .map(|package| package.version.to_string())
            .collect();
        assert_eq!(versions, vec!["1.0.0", "2.0.0"]);
    }

    #[test]
    fn prefers_the_lockfile_version_and_enforces_locked() {
        let fixture = Fixture::new("lock");
        let mut registry = FakeRegistry::new();
        registry.add(entry("foo", "1.0.0", vec![], BTreeMap::new(), false));
        registry.add(entry("foo", "1.1.0", vec![], BTreeMap::new(), false));

        let checksum = format!("{:0>64}", "foo");
        let lock_text = format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"foo\",\n]\n\n[[package]]\nname = \"foo\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{checksum}\"\n"
        );
        let lock = Lockfile::parse(&lock_text, Path::new("Cargo.lock")).unwrap();

        let resolution = resolve(
            &[fixture.root(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"1\"\n",
            )],
            &mut registry,
            &cfg(),
            Some(&lock),
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert_eq!(
            require(&resolution, "foo").version,
            Version::parse("1.0.0").unwrap()
        );

        let mut locked_options = options();
        locked_options.locked = true;
        resolve(
            &[fixture.root(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"1\"\n",
            )],
            &mut registry,
            &cfg(),
            Some(&lock),
            ManifestChoice::Auto,
            None,
            &locked_options,
        )
        .unwrap();

        let mut changed_options = options();
        changed_options.locked = true;
        let error = resolve(
            &[fixture.root(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"1\"\n",
            )],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &changed_options,
        )
        .unwrap_err();
        assert!(error.contains("--locked"), "{error}");
    }

    #[test]
    fn includes_dev_dependencies_of_roots_only() {
        let fixture = Fixture::new("dev");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nlib = { path = \"lib\" }\n[dev-dependencies]\ndevtool = \"1\"\n",
        );
        fixture.write(
            "lib/Cargo.toml",
            "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n[dev-dependencies]\nlibdev = \"1\"\n",
        );
        fixture.write("lib/src/lib.rs", "");
        let mut registry = FakeRegistry::new();
        registry.add(entry("devtool", "1.0.0", vec![], BTreeMap::new(), false));
        registry.add(entry("libdev", "1.0.0", vec![], BTreeMap::new(), false));

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options(),
        )
        .unwrap();
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "devtool"));
        assert!(!resolution
            .packages
            .values()
            .any(|package| package.id.name == "libdev"));
    }

    #[test]
    fn applies_root_feature_flags() {
        let fixture = Fixture::new("root-features");
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[features]\ndefault = []\nfull = [\"dep:extra\"]\n[dependencies]\nextra = { version = \"1\", optional = true }\n";
        let root = fixture.root(manifest);
        let mut registry = FakeRegistry::new();
        registry.add(entry("extra", "1.0.0", vec![], BTreeMap::new(), false));

        let mut options = RootOptions::default();
        options.features.insert("full".to_string());
        options.no_default_features = true;
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options,
        )
        .unwrap();
        assert!(resolution
            .packages
            .values()
            .any(|package| package.id.name == "extra"));
        assert!(require(&resolution, "app").features.contains("full"));
    }

    #[test]
    fn resolution_ids_include_sources() {
        let id = PackageId {
            name: "foo".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            source: PackageSource::Registry,
            context: BuildContext::Target,
        };
        assert_eq!(id.name, "foo");
        let mut set = BTreeSet::new();
        set.insert(id);
        assert_eq!(set.len(), 1);
    }

    fn git_crate_manifest() -> &'static str {
        "[package]\nname = \"foo\"\nversion = \"1.2.3\"\n"
    }

    fn git_dep_field(url: &str, extra: &str) -> String {
        let field = if extra.is_empty() {
            format!("git = \"{url}\"")
        } else {
            format!("git = \"{url}\", {extra}")
        };
        format!("foo = {{ {field} }}")
    }

    #[test]
    fn resolves_git_dependencies_end_to_end() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("resolve-e2e");
        repo.write("Cargo.toml", git_crate_manifest());
        repo.write("src/lib.rs", "pub fn value() {}\n");
        let head = repo.commit("init");

        let fixture = Fixture::new("git-e2e");
        let root = fixture.root(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\n{}\n",
            git_dep_field(&repo.url(), "")
        ));
        let mut registry = FakeRegistry::new();
        let cache = Fixture::new("git-e2e-cache");
        let git_context = GitContext::new(cache.dir.clone(), false);

        let resolution = resolve(
            &[root.clone()],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            Some(&git_context),
            &options(),
        )
        .unwrap();
        let foo = require(&resolution, "foo");
        match &foo.id.source {
            PackageSource::Git {
                url,
                reference,
                rev,
            } => {
                assert_eq!(url, &repo.url());
                assert!(reference.is_none());
                assert_eq!(rev, &head);
            }
            other => panic!("expected a git source, got {other:?}"),
        }

        let again = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            Some(&git_context),
            &options(),
        )
        .unwrap();
        let again_foo = require(&again, "foo");
        assert_eq!(again_foo.id.source, foo.id.source);

        let lock = Lockfile::from_resolution(&resolution);
        assert!(lock
            .render()
            .contains(&format!("source = \"git+{}#{head}\"", repo.url())));
    }

    #[test]
    fn git_requirement_mismatch_errors() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("resolve-mismatch");
        repo.write("Cargo.toml", git_crate_manifest());
        repo.write("src/lib.rs", "pub fn value() {}\n");
        repo.commit("init");

        let fixture = Fixture::new("git-mismatch");
        let root = fixture.root(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\n{}\n",
            git_dep_field(&repo.url(), "version = \"2\"")
        ));
        let mut registry = FakeRegistry::new();
        let cache = Fixture::new("git-mismatch-cache");
        let git_context = GitContext::new(cache.dir.clone(), false);
        let error = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            Some(&git_context),
            &options(),
        )
        .unwrap_err();
        assert!(error.contains("requires `foo 2`"), "{error}");
    }

    #[test]
    fn locked_git_dependency_uses_the_pinned_rev() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("resolve-locked");
        repo.write("Cargo.toml", git_crate_manifest());
        repo.write("src/lib.rs", "// first\n");
        let first = repo.commit("first");
        repo.write("src/lib.rs", "// second\n");
        let second = repo.commit("second");
        assert_ne!(first, second);

        let fixture = Fixture::new("git-locked");
        let root = fixture.root(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\n{}\n",
            git_dep_field(&repo.url(), "")
        ));
        let lock_text = format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"foo\",\n]\n\n[[package]]\nname = \"foo\"\nversion = \"1.2.3\"\nsource = \"git+{}#{}\"\n",
            repo.url(),
            first
        );
        let lock = Lockfile::parse(&lock_text, Path::new("Cargo.lock")).unwrap();
        let mut registry = FakeRegistry::new();
        let cache = Fixture::new("git-locked-cache");
        let git_context = GitContext::new(cache.dir.clone(), false);
        let mut locked = options();
        locked.locked = true;
        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            Some(&lock),
            ManifestChoice::Auto,
            Some(&git_context),
            &locked,
        )
        .unwrap();
        let rev = match &require(&resolution, "foo").id.source {
            PackageSource::Git { rev, .. } => rev.clone(),
            other => panic!("expected a git source, got {other:?}"),
        };
        assert_eq!(rev, first);
    }

    #[test]
    fn offline_git_dependencies_need_a_lock_or_a_checkout() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("resolve-offline");
        repo.write("Cargo.toml", git_crate_manifest());
        repo.write("src/lib.rs", "pub fn value() {}\n");
        let head = repo.commit("init");

        let fixture = Fixture::new("git-offline");
        let root = fixture.root(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\n{}\n",
            git_dep_field(&repo.url(), "")
        ));
        let mut registry = FakeRegistry::new();
        let cache = Fixture::new("git-offline-cache");
        let git_context = GitContext::new(cache.dir.clone(), true);
        let error = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            Some(&git_context),
            &options(),
        )
        .unwrap_err();
        assert!(error.contains("offline"), "{error}");
        assert!(error.contains(&repo.url()), "{error}");

        let lock_text = format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"foo\",\n]\n\n[[package]]\nname = \"foo\"\nversion = \"1.2.3\"\nsource = \"git+{}#{}\"\n",
            repo.url(),
            head
        );
        let lock = Lockfile::parse(&lock_text, Path::new("Cargo.lock")).unwrap();
        let fixture = Fixture::new("git-offline-locked");
        let root = fixture.root(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\n{}\n",
            git_dep_field(&repo.url(), "")
        ));
        let error = resolve(
            &[root],
            &mut registry,
            &cfg(),
            Some(&lock),
            ManifestChoice::Auto,
            Some(&git_context),
            &options(),
        )
        .unwrap_err();
        assert!(error.contains("offline"), "{error}");
    }

    #[test]
    fn tag_and_branch_git_deps_select_different_revs() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("resolve-refs");
        repo.write("Cargo.toml", git_crate_manifest());
        repo.write("src/lib.rs", "// first\n");
        let first = repo.commit("first");
        repo.tag("v1.0.0");
        repo.write("src/lib.rs", "// second\n");
        let second = repo.commit("second");
        assert_ne!(first, second);
        repo.git(&["branch", "dev"]);

        let tag_fixture = Fixture::new("git-tag");
        let tag_root = tag_fixture.root(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\n{}\n",
            git_dep_field(&repo.url(), "tag = \"v1.0.0\"")
        ));
        let branch_fixture = Fixture::new("git-branch");
        let branch_root = branch_fixture.root(&format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\n{}\n",
            git_dep_field(&repo.url(), "branch = \"dev\"")
        ));
        let mut registry = FakeRegistry::new();
        let cache = Fixture::new("git-refs-cache");
        let git_context = GitContext::new(cache.dir.clone(), false);

        let tag_resolution = resolve(
            &[tag_root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            Some(&git_context),
            &options(),
        )
        .unwrap();
        let branch_resolution = resolve(
            &[branch_root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            Some(&git_context),
            &options(),
        )
        .unwrap();
        let tag_rev = match &require(&tag_resolution, "foo").id.source {
            PackageSource::Git { rev, .. } => rev.clone(),
            other => panic!("expected a git source, got {other:?}"),
        };
        let branch_rev = match &require(&branch_resolution, "foo").id.source {
            PackageSource::Git { rev, .. } => rev.clone(),
            other => panic!("expected a git source, got {other:?}"),
        };
        assert_eq!(tag_rev, first);
        assert_eq!(branch_rev, second);

        let tag_source = crate::resolver::git::source_string(
            &repo.url(),
            Some(&GitRefSelection::Tag("v1.0.0".to_string())),
            &tag_rev,
        );
        let branch_source = crate::resolver::git::source_string(
            &repo.url(),
            Some(&GitRefSelection::Branch("dev".to_string())),
            &branch_rev,
        );
        assert_ne!(tag_source, branch_source);
    }

    #[test]
    fn patch_overrides_redirect_registry_sources() {
        let fixture = Fixture::new("patch-redirect");
        let root = fixture
            .root("[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"1\"\n");
        fixture.write(
            "patched/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"1.0.5\"\nedition = \"2021\"\n",
        );
        let dependency = crate::manifest::Dependency {
            path: Some(
                root.manifest_path
                    .parent()
                    .unwrap()
                    .join("patched")
                    .display()
                    .to_string(),
            ),
            ..crate::manifest::Dependency::default()
        };
        let mut patches = BTreeMap::new();
        patches.insert("foo".to_string(), dependency);
        let inheritance = crate::workspace::Inheritance {
            workspace_dir: root.manifest_path.parent().unwrap().to_path_buf(),
            shared_package: None,
            shared_dependencies: None,
            patches,
            replaces: BTreeMap::new(),
            member_dirs: BTreeSet::new(),
        };
        let mut registry = FakeRegistry::new();
        registry.add(entry("foo", "1.0.0", vec![], BTreeMap::new(), false));
        let mut options = options();
        options.inheritance = Some(inheritance);

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options,
        )
        .unwrap();

        let foo = require(&resolution, "foo");
        assert_eq!(foo.version, Version::parse("1.0.5").unwrap());
        assert!(matches!(foo.id.source, PackageSource::Path(_)));
    }

    #[test]
    fn patch_overrides_must_satisfy_consumer_requirements() {
        let fixture = Fixture::new("patch-mismatch");
        let root = fixture
            .root("[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"1\"\n");
        fixture.write(
            "patched/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"2.0.0\"\nedition = \"2021\"\n",
        );
        let dependency = crate::manifest::Dependency {
            path: Some(
                root.manifest_path
                    .parent()
                    .unwrap()
                    .join("patched")
                    .display()
                    .to_string(),
            ),
            ..crate::manifest::Dependency::default()
        };
        let mut patches = BTreeMap::new();
        patches.insert("foo".to_string(), dependency);
        let inheritance = crate::workspace::Inheritance {
            workspace_dir: root.manifest_path.parent().unwrap().to_path_buf(),
            shared_package: None,
            shared_dependencies: None,
            patches,
            replaces: BTreeMap::new(),
            member_dirs: BTreeSet::new(),
        };
        let mut registry = FakeRegistry::new();
        registry.add(entry("foo", "1.0.0", vec![], BTreeMap::new(), false));
        let mut options = options();
        options.inheritance = Some(inheritance);

        let error = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options,
        )
        .err()
        .expect("a patch outside the consumer requirement must fail");
        assert!(error.contains("foo"), "{error}");
    }

    #[test]
    fn replace_entries_redirect_pinned_versions() {
        let fixture = Fixture::new("replace-redirect");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"^0.1\"\n",
        );
        fixture.write(
            "fork/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"0.1.9\"\nedition = \"2021\"\n",
        );
        let dependency = crate::manifest::Dependency {
            path: Some(
                root.manifest_path
                    .parent()
                    .unwrap()
                    .join("fork")
                    .display()
                    .to_string(),
            ),
            ..crate::manifest::Dependency::default()
        };
        let mut replaces = BTreeMap::new();
        replaces.insert("foo:0.1.9".to_string(), dependency);
        let inheritance = crate::workspace::Inheritance {
            workspace_dir: root.manifest_path.parent().unwrap().to_path_buf(),
            shared_package: None,
            shared_dependencies: None,
            patches: BTreeMap::new(),
            replaces,
            member_dirs: BTreeSet::new(),
        };
        let mut registry = FakeRegistry::new();
        registry.add(entry("foo", "0.1.9", vec![], BTreeMap::new(), false));
        let mut options = options();
        options.inheritance = Some(inheritance);

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options,
        )
        .unwrap();

        let foo = require(&resolution, "foo");
        assert!(matches!(foo.id.source, PackageSource::Path(_)));
    }

    #[test]
    fn replace_pinned_versions_respect_the_consumer_requirement() {
        let fixture = Fixture::new("replace-mismatch");
        let root = fixture.root(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = \"^0.2\"\n",
        );
        fixture.write(
            "fork/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"0.1.9\"\nedition = \"2021\"\n",
        );
        let dependency = crate::manifest::Dependency {
            path: Some(
                root.manifest_path
                    .parent()
                    .unwrap()
                    .join("fork")
                    .display()
                    .to_string(),
            ),
            ..crate::manifest::Dependency::default()
        };
        let mut replaces = BTreeMap::new();
        replaces.insert("foo:0.1.9".to_string(), dependency);
        let inheritance = crate::workspace::Inheritance {
            workspace_dir: root.manifest_path.parent().unwrap().to_path_buf(),
            shared_package: None,
            shared_dependencies: None,
            patches: BTreeMap::new(),
            replaces,
            member_dirs: BTreeSet::new(),
        };
        let mut registry = FakeRegistry::new();
        registry.add(entry("foo", "0.2.0", vec![], BTreeMap::new(), false));
        let mut options = options();
        options.inheritance = Some(inheritance);

        let resolution = resolve(
            &[root],
            &mut registry,
            &cfg(),
            None,
            ManifestChoice::Auto,
            None,
            &options,
        )
        .unwrap();

        let foo = require(&resolution, "foo");
        assert_eq!(foo.version, Version::parse("0.2.0").unwrap());
        assert!(matches!(foo.id.source, PackageSource::Registry));
    }
}
