mod build_script;
pub(crate) mod doc;
mod feature_split;
pub(crate) mod fingerprint;
pub(crate) mod fmt;
mod output;
pub(crate) mod plan;
mod rustc;
pub(crate) mod store;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use crate::config::Config;
use crate::manifest::{DependencyKind, Manifest, ProfileEntry};
use crate::registry::download::{CrateStore, Request};
use crate::registry::index::IndexClient;
use crate::resolver::cfg::Cfg;
use crate::resolver::git;
use crate::resolver::lockfile::Lockfile;
use crate::resolver::resolve::{
    self, BuildContext, GitContext, PackageId, PackageSource, Resolution, RootOptions, RootPackage,
};
use crate::resolver::semver::Version;
use crate::workspace::{PackageKey, Workspace};

use build_script::Output as BuildScriptOutput;
use fingerprint::{artifact_key, rustc_identity, source_hash, RustcIdentity};
use plan::{PackageInfo, TargetSelection, Unit, UnitKind};
use rustc::Flags;
use store::{Artifact, Store};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Build,
    Check,
    Test,
}

const EXAMPLES_DIR: &str = "examples";
const CFG_CACHE_DIR: &str = "cfg-cache";

struct ProfileDirs {
    host: PathBuf,
    target: PathBuf,
}

impl ProfileDirs {
    fn new(target_root: &Path, target: Option<&str>, profile: &str) -> Self {
        Self {
            host: profile_dir(target_root, target, true, profile),
            target: profile_dir(target_root, target, false, profile),
        }
    }

    fn for_unit(&self, unit: &Unit) -> &Path {
        if unit.host {
            &self.host
        } else {
            &self.target
        }
    }
}

fn platform_dir(target_root: &Path, target: Option<&str>) -> PathBuf {
    match target {
        Some(triple) => target_root.join(triple),
        None => target_root.to_path_buf(),
    }
}

fn profile_dir(target_root: &Path, target: Option<&str>, host: bool, profile: &str) -> PathBuf {
    let base = match (target, host) {
        (Some(triple), false) => target_root.join(triple),
        _ => target_root.to_path_buf(),
    };
    base.join(profile_dir_name(profile))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MessageFormat {
    Human,
    Short,
    Json,
}

impl Default for MessageFormat {
    fn default() -> Self {
        Self::Human
    }
}

pub(crate) struct Error {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl Error {
    fn tool(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }

    fn compile(message: impl Into<String>) -> Self {
        Self {
            code: 101,
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinaryKind {
    Bin,
    Example,
    Bench,
}

pub(crate) struct BinaryArtifact {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) kind: BinaryKind,
}

pub(crate) struct BuildResult {
    pub(crate) binaries: Vec<BinaryArtifact>,
    pub(crate) tests: Vec<TestArtifact>,
    pub(crate) docs: Vec<DocTarget>,
}

pub(crate) struct DocTarget {
    pub(crate) crate_name: String,
    pub(crate) source: PathBuf,
    pub(crate) edition: crate::manifest::Edition,
    pub(crate) externs: Vec<(String, PathBuf)>,
    pub(crate) search_dirs: Vec<PathBuf>,
    pub(crate) cfgs: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) out_dir: PathBuf,
    pub(crate) proc_macro_crate: bool,
    pub(crate) target: Option<String>,
}

pub(crate) struct TestArtifact {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

pub(crate) struct Options<'a> {
    pub(crate) targets: &'a [PackageKey],
    pub(crate) mode: Mode,
    pub(crate) selection: &'a TargetSelection,
    pub(crate) profile: &'a str,
    pub(crate) target: Option<&'a str>,
    pub(crate) force: bool,
    pub(crate) incremental: bool,
    pub(crate) jobs: usize,
    pub(crate) quiet: bool,
    pub(crate) verbose: bool,
    pub(crate) stats: bool,
    pub(crate) message_format: MessageFormat,
    pub(crate) features: BTreeSet<String>,
    pub(crate) all_features: bool,
    pub(crate) no_default_features: bool,
    pub(crate) locked: bool,
    pub(crate) target_dir: Option<&'a Path>,
    pub(crate) profiles: &'a BTreeMap<String, ProfileEntry>,
    pub(crate) document: bool,
    pub(crate) rustc_program: Option<std::ffi::OsString>,
    pub(crate) artifact_salt: Option<String>,
    pub(crate) rustc_extra_args: Vec<String>,
}

enum Preparation {
    Hit(Artifact),
    Miss(Prepared),
}

struct Prepared {
    key: String,
    externs: Vec<(String, PathBuf)>,
    flags: Flags,
    incremental_dir: Option<PathBuf>,
    cfgs: Vec<String>,
    env: Vec<(String, String)>,
    args: Vec<String>,
    out_dir: Option<PathBuf>,
}

struct Schedule {
    pipeline: bool,
    meta_remaining: Vec<usize>,
    full_remaining: Vec<usize>,
    paths: Vec<Option<PathBuf>>,
    keys: Vec<Option<String>>,
    scheduled: Vec<bool>,
    artifact_complete: Vec<bool>,
    link_ready: Vec<bool>,
    dependencies: Vec<Vec<usize>>,
    dependents: Vec<Vec<usize>>,
    ready: VecDeque<usize>,
}

impl Schedule {
    fn new(
        plan: &plan::Plan,
        mode: Mode,
        dependencies: &[Vec<usize>],
        dependents: &[Vec<usize>],
    ) -> Self {
        let mut schedule = Self {
            pipeline: std::env::var_os("RUSTED_NO_PIPELINE").is_none(),
            meta_remaining: dependencies.iter().map(|deps| deps.len()).collect(),
            full_remaining: dependencies.iter().map(|deps| deps.len()).collect(),
            paths: (0..plan.units.len()).map(|_| None).collect(),
            keys: (0..plan.units.len()).map(|_| None).collect(),
            scheduled: (0..plan.units.len()).map(|_| false).collect(),
            artifact_complete: (0..plan.units.len()).map(|_| false).collect(),
            link_ready: (0..plan.units.len()).map(|_| false).collect(),
            dependencies: dependencies.to_vec(),
            dependents: dependents.to_vec(),
            ready: VecDeque::new(),
        };
        for index in 0..plan.units.len() {
            if schedule.can_start(plan, mode, index) {
                schedule.scheduled[index] = true;
                schedule.ready.push_back(index);
            }
        }
        schedule
    }

    fn can_start(&self, plan: &plan::Plan, mode: Mode, index: usize) -> bool {
        if self.pipeline && plan.units[index].kind == UnitKind::Lib && mode == Mode::Build {
            self.meta_remaining[index] == 0
        } else {
            self.full_remaining[index] == 0
        }
    }

    fn metadata_ready(&mut self, index: usize, path: PathBuf, plan: &plan::Plan, mode: Mode) {
        self.paths[index] = Some(path);
        for position in 0..self.dependents[index].len() {
            let dependent = self.dependents[index][position];
            self.meta_remaining[dependent] = self.meta_remaining[dependent].saturating_sub(1);
            if !self.scheduled[dependent] && self.can_start(plan, mode, dependent) {
                self.scheduled[dependent] = true;
                self.ready.push_back(dependent);
            }
        }
    }

    fn completed(
        &mut self,
        index: usize,
        path: PathBuf,
        plan: &plan::Plan,
        mode: Mode,
        announced: &mut [bool],
    ) {
        if !announced[index] {
            announced[index] = true;
            self.metadata_ready(index, path.clone(), plan, mode);
        }
        self.paths[index] = Some(path);
        self.artifact_complete[index] = true;
        self.mark_link_ready(index, plan, mode);
    }

    fn mark_link_ready(&mut self, index: usize, plan: &plan::Plan, mode: Mode) {
        if self.link_ready[index] || !self.artifact_complete[index] {
            return;
        }
        if !self.dependencies[index]
            .iter()
            .all(|dependency| self.link_ready[*dependency])
        {
            return;
        }
        self.link_ready[index] = true;
        for position in 0..self.dependents[index].len() {
            let dependent = self.dependents[index][position];
            self.full_remaining[dependent] = self.full_remaining[dependent].saturating_sub(1);
            if !self.scheduled[dependent] && self.can_start(plan, mode, dependent) {
                self.scheduled[dependent] = true;
                self.ready.push_back(dependent);
            }
            self.mark_link_ready(dependent, plan, mode);
        }
    }
}

struct Job {
    program: std::ffi::OsString,
    unit: Unit,
    flags: Flags,
    mode: Mode,
    externs: Vec<(String, PathBuf)>,
    search_dirs: Vec<PathBuf>,
    key: String,
    incremental_dir: Option<PathBuf>,
    cfgs: Vec<String>,
    env: Vec<(String, String)>,
    args: Vec<String>,
    out_dir: Option<PathBuf>,
    force: bool,
    verbose: bool,
    format: MessageFormat,
    store: Store,
    index: usize,
    metadata_sender: Option<mpsc::Sender<(usize, PathBuf)>>,
}

enum JobError {
    Tool(String),
    Compile {
        message: String,
        diagnostics: rustc::Diagnostics,
    },
}

struct JobOutcome {
    artifact: Artifact,
    compiled: bool,
    fresh: bool,
    bytes: u64,
    diagnostics: rustc::Diagnostics,
}

pub(crate) fn run(
    workspace: &Workspace,
    config: &Config,
    options: &Options<'_>,
) -> Result<BuildResult, Error> {
    let target_root = resolve_target_root(workspace, config, options.target_dir)?;
    std::fs::create_dir_all(&target_root)
        .map_err(|error| Error::tool(format!("{}: {error}", target_root.display())))?;
    let profile_dirs = ProfileDirs::new(&target_root, options.target, options.profile);
    std::fs::create_dir_all(&profile_dirs.host)
        .map_err(|error| Error::tool(format!("{}: {error}", profile_dirs.host.display())))?;
    std::fs::create_dir_all(&profile_dirs.target)
        .map_err(|error| Error::tool(format!("{}: {error}", profile_dirs.target.display())))?;

    let store = Store::open(config).map_err(Error::tool)?;
    let hash_cache_path = store.root().join("hash-cache");
    let mut file_hash_cache = fingerprint::SourceHashCache::load(&hash_cache_path);
    let remote = if config.offline {
        None
    } else {
        match crate::remote::Remote::from_env() {
            Ok(remote) => remote,
            Err(error) => {
                if !options.quiet {
                    eprintln!("warning: remote cache disabled: {error}");
                }
                None
            }
        }
    };
    let identity = rustc_identity().map_err(Error::tool)?;

    let mut root_requests = Vec::new();
    let mut root_ids = Vec::new();
    for key in options.targets {
        let package = workspace.package(key).ok_or_else(|| {
            Error::tool(format!(
                "missing package `{}` in the workspace graph",
                key.name
            ))
        })?;
        let dir = package.key.dir.clone();
        let version = Version::parse(&package.manifest.version).map_err(Error::tool)?;
        root_ids.push(PackageId {
            name: package.manifest.name.clone(),
            version,
            source: PackageSource::Path(dir),
            context: BuildContext::Target,
        });
        root_requests.push(RootPackage {
            manifest: package.manifest.clone(),
            manifest_path: package.manifest_path.clone(),
        });
    }

    let lock_path = workspace.root_dir.join("Cargo.lock");
    let lock = if lock_path.is_file() {
        let text = std::fs::read_to_string(&lock_path)
            .map_err(|error| Error::tool(format!("{}: {error}", lock_path.display())))?;
        Some(Lockfile::parse(&text, &lock_path).map_err(Error::tool)?)
    } else {
        None
    };

    let cfg =
        Cfg::cached(&store.root().join(CFG_CACHE_DIR), options.target).map_err(Error::tool)?;
    let mut index = IndexClient::new(config).map_err(Error::tool)?;
    let crate_store = CrateStore::new(config).map_err(Error::tool)?;
    let git_root = store.root().join("git");
    let git_context = GitContext::new(git_root.clone(), config.offline);
    let root_options = RootOptions {
        features: options.features.clone(),
        all_features: options.all_features,
        no_default_features: options.no_default_features,
        locked: false,
        inheritance: Some(workspace.inheritance()),
        resolver: workspace.resolver,
        dev_features: options.mode == Mode::Test,
        all_targets: false,
    };
    let resolution = resolve::resolve(
        &root_requests,
        &mut index,
        &cfg,
        lock.as_ref(),
        config.manifest,
        Some(&git_context),
        &root_options,
    )
    .map_err(Error::tool)?;

    let lock_options = RootOptions {
        locked: options.locked,
        all_targets: true,
        dev_features: true,
        ..root_options.clone()
    };
    if options.locked {
        resolve::resolve(
            &root_requests,
            &mut index,
            &cfg,
            lock.as_ref(),
            config.manifest,
            Some(&git_context),
            &lock_options,
        )
        .map_err(Error::tool)?;
    } else {
        let lock_resolution = resolve::resolve(
            &root_requests,
            &mut index,
            &cfg,
            lock.as_ref(),
            config.manifest,
            Some(&git_context),
            &lock_options,
        )
        .map_err(Error::tool)?;
        Lockfile::from_resolution(&lock_resolution)
            .write(&lock_path)
            .map_err(Error::tool)?;
    }

    let needed = needed_packages(&resolution, &root_ids, options.mode, options.selection);
    check_rust_versions(&resolution, &needed, identity.release())?;

    let mut registry_requests: BTreeMap<PackageId, Request> = BTreeMap::new();
    for (id, package) in &resolution.packages {
        if !needed.contains(id) {
            continue;
        }
        if matches!(id.source, PackageSource::Registry) {
            let checksum = package.checksum.clone().ok_or_else(|| {
                Error::tool(format!("registry package `{}` has no checksum", id.name))
            })?;
            let url = index
                .download_url(&id.name, &package.version)
                .map_err(Error::tool)?;
            registry_requests.insert(
                id.clone(),
                Request {
                    name: id.name.clone(),
                    version: package.version.clone(),
                    checksum,
                    url,
                },
            );
        }
    }
    let requests: Vec<Request> = registry_requests.values().cloned().collect();
    crate_store
        .fetch_many(&requests, options.jobs)
        .map_err(Error::tool)?;

    let mut packages = BTreeMap::new();
    for (id, package) in &resolution.packages {
        if !needed.contains(id) {
            continue;
        }
        let (manifest, manifest_path, dir) = match &id.source {
            PackageSource::Path(dir) => {
                let manifest = package.manifest.clone().ok_or_else(|| {
                    Error::tool(format!("path package `{}` has no manifest", id.name))
                })?;
                let manifest_path = package.manifest_path.clone().ok_or_else(|| {
                    Error::tool(format!("path package `{}` has no manifest path", id.name))
                })?;
                (manifest, manifest_path, dir.clone())
            }
            PackageSource::Git { url, rev, .. } => {
                let dir = git::checkout_dir(&git_root, rev);
                if !dir.join("Cargo.toml").is_file() {
                    return Err(Error::tool(format!(
                        "git checkout for `{url}` at revision `{rev}` is missing from `{}`, rerun the build",
                        dir.display()
                    )));
                }
                let manifest_path = dir.join("Cargo.toml");
                let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
                    Error::tool(format!("{}: {error}", manifest_path.display()))
                })?;
                let manifest = Manifest::parse(&text, &manifest_path).map_err(Error::tool)?;
                if manifest.name != id.name {
                    return Err(Error::tool(format!(
                        "git checkout for `{url}` declares package `{}`, expected `{}`",
                        manifest.name, id.name
                    )));
                }
                let version = Version::parse(&manifest.version).map_err(Error::tool)?;
                if version != package.version {
                    return Err(Error::tool(format!(
                        "git checkout for `{url}` declares version `{}`, expected `{}`",
                        version, package.version
                    )));
                }
                (manifest, manifest_path, dir)
            }
            PackageSource::Registry => {
                let request = registry_requests
                    .get(id)
                    .ok_or_else(|| Error::tool(format!("missing download for `{}`", id.name)))?;
                let dir = crate_store.source(request).map_err(Error::tool)?;
                let manifest_path = dir.join("Cargo.toml");
                let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
                    Error::tool(format!("{}: {error}", manifest_path.display()))
                })?;
                let manifest = Manifest::parse(&text, &manifest_path).map_err(Error::tool)?;
                if manifest.name != id.name {
                    return Err(Error::tool(format!(
                        "registry archive for `{}` declares package `{}`",
                        id.name, manifest.name
                    )));
                }
                let version = Version::parse(&manifest.version).map_err(Error::tool)?;
                if version != package.version {
                    return Err(Error::tool(format!(
                        "registry archive for `{}` declares version `{}`, expected `{}`",
                        id.name, version, package.version
                    )));
                }
                (manifest, manifest_path, dir)
            }
        };
        packages.insert(
            id.clone(),
            PackageInfo {
                id: id.clone(),
                manifest,
                manifest_path,
                dir,
                features: package.features.clone(),
                dependencies: package
                    .dependencies
                    .iter()
                    .filter(|edge| needed.contains(&edge.package))
                    .cloned()
                    .collect(),
            },
        );
    }

    feature_split::apply(&mut packages, &root_ids);
    let plan = plan::plan(
        &packages,
        &root_ids,
        options.mode,
        options.selection,
        options.target.is_some(),
    )
    .map_err(Error::tool)?;
    let build_script_units = build_script_units(&plan);
    let mut build_script_outputs: BTreeMap<PackageId, BuildScriptOutput> = BTreeMap::new();
    let excluded = [canonical(&target_root)?, canonical(store.root())?];

    if !options.quiet {
        let mut seen = BTreeSet::new();
        for unit in &plan.units {
            if !seen.insert(&unit.package) {
                continue;
            }
            if let Some(package) = packages.get(&unit.package) {
                for warning in &package.manifest.warnings {
                    eprintln!("warning: {}: {warning}", package.manifest_path.display());
                }
            }
        }
    }

    let lib_units = lib_units(&plan);
    let (dependencies, dependents) =
        dependency_graph(&plan, &lib_units, &build_script_units, &packages)?;
    let mut schedule = Schedule::new(&plan, options.mode, &dependencies, &dependents);
    let mut announced: Vec<bool> = (0..plan.units.len()).map(|_| false).collect();
    let mut artifacts: Vec<Option<Artifact>> = (0..plan.units.len()).map(|_| None).collect();
    let mut search_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut binaries = Vec::new();
    let mut tests = Vec::new();
    let mut source_hashes: HashMap<PackageId, [u8; 32]> = HashMap::new();
    let mut printed: BTreeSet<PackageId> = BTreeSet::new();

    let jobs = options.jobs.max(1);
    let (sender, receiver) = mpsc::channel::<(usize, Result<JobOutcome, JobError>)>();
    let (metadata_sender, metadata_receiver) = mpsc::channel::<(usize, PathBuf)>();
    let mut outstanding = 0usize;
    let mut failed: Option<Error> = None;
    let mut rustc_calls = 0usize;
    let mut cache_hits = 0usize;
    let mut bytes_written = 0u64;
    let mut warnings = 0usize;
    let mut json_lines: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let start = Instant::now();

    while outstanding > 0 || !schedule.ready.is_empty() {
        while let Ok((index, path)) = metadata_receiver.try_recv() {
            if !announced[index] {
                announced[index] = true;
                schedule.metadata_ready(index, path, &plan, options.mode);
            }
        }
        if failed.is_none() {
            while outstanding < jobs {
                let Some(index) = schedule.ready.pop_front() else {
                    break;
                };

                let preparation = prepare(
                    index,
                    &plan,
                    &packages,
                    config,
                    options,
                    &lib_units,
                    &mut schedule.keys,
                    &schedule.paths,
                    &mut source_hashes,
                    &excluded,
                    &identity,
                    &store,
                    &profile_dirs,
                    &build_script_outputs,
                    &build_script_units,
                    remote.as_ref(),
                    &mut file_hash_cache,
                );
                let preparation = match preparation {
                    Ok(preparation) => preparation,
                    Err(error) => {
                        failed = Some(error);
                        break;
                    }
                };

                match preparation {
                    Preparation::Hit(artifact) => {
                        cache_hits += 1;
                        if options.verbose && !options.quiet {
                            let unit = &plan.units[index];
                            eprintln!("Fresh {} v{}", unit.package_name, unit.package_version);
                        }
                        emit_artifact(
                            index,
                            &artifact,
                            true,
                            &plan,
                            options.message_format,
                            &mut json_lines,
                        );
                        if let Err(error) = finish_unit(
                            index,
                            artifact,
                            &plan,
                            &packages,
                            config,
                            options,
                            &cfg,
                            &identity,
                            &source_hashes,
                            &store,
                            &profile_dirs,
                            &mut build_script_outputs,
                            &mut artifacts,
                            &mut search_dirs,
                            &mut binaries,
                            &mut tests,
                            &mut schedule,
                            &dependents,
                            &mut announced,
                        ) {
                            failed = Some(error);
                            break;
                        }
                    }
                    Preparation::Miss(prepared) => {
                        let unit = &plan.units[index];
                        if printed.insert(unit.package.clone())
                            && !options.quiet
                            && options.message_format != MessageFormat::Json
                        {
                            println!(
                                "{} {} v{}",
                                if options.mode == Mode::Check {
                                    "Checking"
                                } else {
                                    "Compiling"
                                },
                                unit.package_name,
                                unit.package_version
                            );
                        }

                        let job = Job {
                            program: options
                                .rustc_program
                                .clone()
                                .unwrap_or_else(|| rustc_program()),
                            unit: unit.clone(),
                            flags: prepared.flags,
                            mode: options.mode,
                            externs: prepared.externs,
                            search_dirs: search_dirs.iter().cloned().collect(),
                            key: prepared.key,
                            incremental_dir: prepared.incremental_dir,
                            cfgs: prepared.cfgs,
                            env: prepared.env,
                            args: prepared.args,
                            out_dir: prepared.out_dir,
                            force: options.force,
                            verbose: options.verbose,
                            format: options.message_format,
                            store: store.clone(),
                            index,
                            metadata_sender: if schedule.pipeline && unit.metadata_name.is_some() {
                                Some(metadata_sender.clone())
                            } else {
                                None
                            },
                        };
                        let sender = sender.clone();
                        let spawned = std::thread::Builder::new()
                            .name(format!("rustc-{index}"))
                            .spawn(move || {
                                let result =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        execute_job(job)
                                    }));
                                let result = match result {
                                    Ok(result) => result,
                                    Err(_) => {
                                        Err(JobError::Tool("rustc worker panicked".to_string()))
                                    }
                                };
                                if sender.send((index, result)).is_err() {
                                    return;
                                }
                            });
                        match spawned {
                            Ok(_) => outstanding += 1,
                            Err(error) => {
                                failed = Some(Error::tool(format!(
                                    "failed to start a rustc worker: {error}"
                                )));
                                break;
                            }
                        }
                    }
                }
            }
        }

        if outstanding == 0 {
            if schedule.ready.is_empty() || failed.is_some() {
                break;
            }
            continue;
        }

        let received = match receiver.recv_timeout(std::time::Duration::from_millis(2)) {
            Ok(received) => received,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                failed = Some(Error::tool("rustc workers stopped unexpectedly"));
                break;
            }
        };
        outstanding -= 1;
        let (index, result) = received;
        match result {
            Ok(outcome) => {
                let JobOutcome {
                    artifact,
                    compiled,
                    fresh,
                    bytes,
                    diagnostics,
                } = outcome;
                if let Err(error) = emit_diagnostics(
                    index,
                    &diagnostics,
                    options.message_format,
                    &plan,
                    &mut warnings,
                    &mut json_lines,
                ) {
                    if failed.is_none() {
                        failed = Some(error);
                    }
                }
                if compiled {
                    rustc_calls += 1;
                    bytes_written += bytes;
                    if let Some(remote) = &remote {
                        if let Err(error) = remote.push(&store, &artifact.key) {
                            if !options.quiet {
                                eprintln!("warning: remote cache push failed: {error}");
                            }
                        }
                    }
                }
                if fresh {
                    cache_hits += 1;
                    if options.verbose && !options.quiet {
                        let unit = &plan.units[index];
                        eprintln!("Fresh {} v{}", unit.package_name, unit.package_version);
                    }
                }
                emit_artifact(
                    index,
                    &artifact,
                    fresh,
                    &plan,
                    options.message_format,
                    &mut json_lines,
                );
                if let Err(error) = finish_unit(
                    index,
                    artifact,
                    &plan,
                    &packages,
                    config,
                    options,
                    &cfg,
                    &identity,
                    &source_hashes,
                    &store,
                    &profile_dirs,
                    &mut build_script_outputs,
                    &mut artifacts,
                    &mut search_dirs,
                    &mut binaries,
                    &mut tests,
                    &mut schedule,
                    &dependents,
                    &mut announced,
                ) {
                    if failed.is_none() {
                        failed = Some(error);
                    }
                }
            }
            Err(JobError::Tool(message)) => {
                if failed.is_none() {
                    failed = Some(Error::tool(message));
                }
            }
            Err(JobError::Compile {
                message,
                diagnostics,
            }) => {
                if let Err(error) = emit_diagnostics(
                    index,
                    &diagnostics,
                    options.message_format,
                    &plan,
                    &mut warnings,
                    &mut json_lines,
                ) {
                    if failed.is_none() {
                        failed = Some(error);
                    }
                }
                if failed.is_none() {
                    failed = Some(Error::compile(message));
                }
            }
        }
    }
    drop(sender);
    file_hash_cache
        .save(&hash_cache_path)
        .map_err(Error::tool)?;

    for lines in json_lines.values() {
        for line in lines {
            println!("{line}");
        }
    }

    if failed.is_none() && artifacts.iter().any(|artifact| artifact.is_none()) {
        return Err(Error::tool(
            "dependency graph contains a cycle or a missing artifact",
        ));
    }

    let success = failed.is_none();
    if options.message_format == MessageFormat::Json {
        if options.stats {
            println!(
                "{}",
                output::build_stats(
                    rustc_calls,
                    cache_hits,
                    bytes_written,
                    start.elapsed().as_millis()
                )
            );
        }
        println!("{}", output::build_finished(success, warnings));
    }

    if let Some(error) = failed {
        if options.message_format != MessageFormat::Json && warnings > 0 && !options.quiet {
            eprintln!(
                "warning: {warnings} {} emitted",
                if warnings == 1 { "warning" } else { "warnings" }
            );
        }
        return Err(error);
    }

    if !options.quiet && options.message_format != MessageFormat::Json {
        println!(
            "Finished {} target(s) in {:.2}s",
            options.profile,
            start.elapsed().as_secs_f64()
        );
    }
    if warnings > 0 && options.message_format != MessageFormat::Json && !options.quiet {
        eprintln!(
            "warning: {warnings} {} emitted",
            if warnings == 1 { "warning" } else { "warnings" }
        );
    }
    if options.stats && options.message_format != MessageFormat::Json {
        println!(
            "stats: rustc_calls={rustc_calls} cache_hits={cache_hits} bytes_written={bytes_written} wall_ms={}",
            start.elapsed().as_millis()
        );
    }

    let docs = if options.document {
        doc::collect_doc_targets(
            &plan,
            &packages,
            &build_script_outputs,
            &root_ids,
            &artifacts,
            &search_dirs,
            options.target,
        )
        .map_err(Error::tool)?
    } else {
        Vec::new()
    };

    Ok(BuildResult {
        binaries,
        tests,
        docs,
    })
}

#[allow(clippy::too_many_arguments)]
fn prepare(
    index: usize,
    plan: &plan::Plan,
    packages: &BTreeMap<PackageId, PackageInfo>,
    config: &Config,
    options: &Options<'_>,
    lib_units: &BTreeMap<(PackageId, bool), usize>,
    keys: &mut [Option<String>],
    paths: &[Option<PathBuf>],
    source_hashes: &mut HashMap<PackageId, [u8; 32]>,
    excluded: &[PathBuf],
    identity: &RustcIdentity,
    store: &Store,
    profile_dirs: &ProfileDirs,
    build_script_outputs: &BTreeMap<PackageId, BuildScriptOutput>,
    build_script_units: &BTreeMap<PackageId, usize>,
    remote: Option<&crate::remote::Remote>,
    file_hash_cache: &mut fingerprint::SourceHashCache,
) -> Result<Preparation, Error> {
    let unit = &plan.units[index];
    let package = packages.get(&unit.package).ok_or_else(|| {
        Error::tool(format!(
            "missing package `{}` in the resolved graph",
            unit.package.name
        ))
    })?;
    let flags = Flags::resolve(
        config,
        options.profiles,
        options.profile,
        options.incremental,
        &unit.package_name,
        unit.kind,
        options.target,
    )
    .map_err(Error::tool)?;

    let mut externs = Vec::new();
    let mut dependency_keys = Vec::new();
    for entry in &unit.externs {
        let dependency_index = *lib_units
            .get(&(entry.package.clone(), entry.host))
            .ok_or_else(|| {
                Error::tool(format!(
                    "internal error: dependency `{}` of `{}` has no library unit",
                    entry.name, unit.package_name
                ))
            })?;
        let path = paths[dependency_index].as_ref().ok_or_else(|| {
            Error::tool(format!(
                "internal error: dependency `{}` of `{}` is not built",
                entry.name, unit.package_name
            ))
        })?;
        let key = keys[dependency_index].as_ref().ok_or_else(|| {
            Error::tool(format!(
                "internal error: dependency `{}` of `{}` has no key",
                entry.name, unit.package_name
            ))
        })?;
        externs.push((entry.name.clone(), path.clone()));
        dependency_keys.push((entry.name.clone(), key.clone()));
    }

    let source = match source_hashes.get(&unit.package) {
        Some(hash) => *hash,
        None => {
            let unit_exclusions: &[PathBuf] = match unit.package.source {
                PackageSource::Path(_) => excluded,
                PackageSource::Registry | PackageSource::Git { .. } => &[],
            };
            let hash = source_hash(&unit.package_dir, unit_exclusions, file_hash_cache)
                .map_err(Error::tool)?;
            source_hashes.insert(unit.package.clone(), hash);
            hash
        }
    };
    let build_output = if unit.kind == UnitKind::BuildScript {
        None
    } else {
        let output = build_script_outputs.get(&unit.package);
        if build_script_units.contains_key(&unit.package) && output.is_none() {
            return Err(Error::tool(format!(
                "internal error: build script output for `{}` is missing",
                unit.package.name
            )));
        }
        output
    };
    let key = artifact_key(
        unit,
        &flags,
        identity,
        options.mode,
        &source,
        build_output.map(|output| output.key.as_str()),
        &dependency_keys,
        options.target,
        options.artifact_salt.as_deref(),
    );
    keys[index] = Some(key.clone());
    let require_nonzero = !requires_metadata_only(options.mode, unit.kind);
    let outputs = [(unit.output_name.clone(), require_nonzero)];

    if !options.force {
        if let Some(artifact) = store.lookup(&key, &outputs) {
            store.touch(&artifact).map_err(Error::tool)?;
            return Ok(Preparation::Hit(artifact));
        }
        if let Some(remote) = remote {
            match remote.fetch(store, &key) {
                Ok(true) => {
                    if let Some(artifact) = store.lookup(&key, &outputs) {
                        store.touch(&artifact).map_err(Error::tool)?;
                        return Ok(Preparation::Hit(artifact));
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    if !options.quiet {
                        eprintln!("warning: remote cache fetch failed: {error}");
                    }
                }
            }
        }
    }

    let incremental_dir = if flags.incremental() {
        Some(profile_dirs.for_unit(unit).join("incremental"))
    } else {
        None
    };
    let mut env = package_environment(package, unit);
    let (cfgs, mut args, out_dir) = match build_output {
        Some(output) => {
            env.extend(output.env.clone());
            (
                output.cfgs.clone(),
                output.args.clone(),
                Some(output.out_dir.clone()),
            )
        }
        None => (Vec::new(), Vec::new(), None),
    };
    args.extend(options.rustc_extra_args.iter().cloned());

    Ok(Preparation::Miss(Prepared {
        key,
        externs,
        flags,
        incremental_dir,
        cfgs,
        env,
        args,
        out_dir,
    }))
}

fn package_environment(package: &PackageInfo, unit: &Unit) -> Vec<(String, String)> {
    let manifest = &package.manifest;
    let mut environment = Vec::new();
    environment.push((
        "CARGO_MANIFEST_DIR".to_string(),
        package.dir.display().to_string(),
    ));
    environment.push((
        "CARGO_MANIFEST_PATH".to_string(),
        package.manifest_path.display().to_string(),
    ));
    environment.push(("CARGO_CRATE_NAME".to_string(), unit.crate_name.clone()));
    environment.push(("CARGO_PKG_NAME".to_string(), manifest.name.clone()));
    environment.push(("CARGO_PKG_VERSION".to_string(), manifest.version.clone()));

    if let Ok(version) = Version::parse(&manifest.version) {
        environment.push((
            "CARGO_PKG_VERSION_MAJOR".to_string(),
            version.major.to_string(),
        ));
        environment.push((
            "CARGO_PKG_VERSION_MINOR".to_string(),
            version.minor.to_string(),
        ));
        environment.push((
            "CARGO_PKG_VERSION_PATCH".to_string(),
            version.patch.to_string(),
        ));
    }
    let pre = manifest
        .version
        .split_once('-')
        .map(|(_, rest)| rest.split('+').next().unwrap_or(""))
        .unwrap_or("");
    environment.push(("CARGO_PKG_VERSION_PRE".to_string(), pre.to_string()));

    let optional = |value: &Option<String>| value.clone().unwrap_or_default();
    environment.push(("CARGO_PKG_AUTHORS".to_string(), manifest.authors.join(": ")));
    environment.push((
        "CARGO_PKG_DESCRIPTION".to_string(),
        optional(&manifest.description),
    ));
    environment.push((
        "CARGO_PKG_HOMEPAGE".to_string(),
        optional(&manifest.homepage),
    ));
    environment.push((
        "CARGO_PKG_REPOSITORY".to_string(),
        optional(&manifest.repository),
    ));
    environment.push(("CARGO_PKG_LICENSE".to_string(), optional(&manifest.license)));
    environment.push((
        "CARGO_PKG_LICENSE_FILE".to_string(),
        optional(&manifest.license_file),
    ));
    environment.push(("CARGO_PKG_README".to_string(), optional(&manifest.readme)));
    if let Some(rust_version) = &manifest.rust_version {
        environment.push(("CARGO_PKG_RUST_VERSION".to_string(), rust_version.clone()));
    }
    if let Some(links) = &manifest.links {
        environment.push(("CARGO_MANIFEST_LINKS".to_string(), links.clone()));
    }
    if unit.kind == UnitKind::Bin {
        environment.push(("CARGO_BIN_NAME".to_string(), unit.target_name.clone()));
    }

    environment
}

fn execute_job(job: Job) -> Result<JobOutcome, JobError> {
    let require_nonzero = !requires_metadata_only(job.mode, job.unit.kind);
    let outputs = [(job.unit.output_name.clone(), require_nonzero)];

    if !job.force {
        if let Some(artifact) = job.store.lookup(&job.key, &outputs) {
            job.store.touch(&artifact).map_err(JobError::Tool)?;
            return Ok(JobOutcome {
                artifact,
                compiled: false,
                fresh: true,
                bytes: 0,
                diagnostics: rustc::Diagnostics {
                    stderr: Vec::new(),
                    warnings: 0,
                },
            });
        }
    }

    let _lock = job.store.lock(&job.key).map_err(JobError::Tool)?;

    if !job.force {
        if let Some(artifact) = job.store.lookup(&job.key, &outputs) {
            job.store.touch(&artifact).map_err(JobError::Tool)?;
            return Ok(JobOutcome {
                artifact,
                compiled: false,
                fresh: true,
                bytes: 0,
                diagnostics: rustc::Diagnostics {
                    stderr: Vec::new(),
                    warnings: 0,
                },
            });
        }
    }

    let temp = job.store.temp_dir(&job.key).map_err(JobError::Tool)?;
    let output = temp.join(&job.unit.output_name);
    let metadata_output = job
        .metadata_sender
        .as_ref()
        .and_then(|_| job.unit.metadata_name.as_ref().map(|name| temp.join(name)));
    let metadata_store = job.metadata_sender.as_ref().and_then(|_| {
        job.unit
            .metadata_name
            .as_ref()
            .map(|name| job.store.root().join("metadata").join(&job.key).join(name))
    });
    let outcome = rustc::compile(
        &job.program,
        job.index,
        &job.unit,
        &job.key,
        &job.flags,
        job.mode,
        &job.externs,
        &job.search_dirs,
        &job.cfgs,
        &job.env,
        &job.args,
        job.out_dir.as_deref(),
        &output,
        job.incremental_dir.as_deref(),
        metadata_output.as_deref(),
        metadata_store.as_deref(),
        job.metadata_sender.as_ref(),
        job.format,
        job.verbose,
    )
    .map_err(JobError::Tool)?;
    let diagnostics = match outcome {
        rustc::CompileOutcome::Success(diagnostics) => diagnostics,
        rustc::CompileOutcome::Failure {
            message,
            diagnostics,
        } => {
            if let Err(cleanup) = std::fs::remove_dir_all(&temp) {
                return Err(JobError::Compile {
                    message: format!(
                        "{message}; also failed to clean {}: {cleanup}",
                        temp.display()
                    ),
                    diagnostics,
                });
            }
            return Err(JobError::Compile {
                message,
                diagnostics,
            });
        }
    };

    let meta = format!(
        "key={}\npackage={} {}\ntarget={}\nmode={}\nprofile={}\n",
        job.key,
        job.unit.package_name,
        job.unit.package_version,
        job.unit.target_name,
        match job.mode {
            Mode::Build => "build",
            Mode::Check => "check",
            Mode::Test => "test",
        },
        job.flags.cache_key_material()
    );
    let artifact = job
        .store
        .commit(&job.key, &temp, &meta)
        .map_err(JobError::Tool)?;
    let bytes = std::fs::metadata(artifact.dir.join(&job.unit.output_name))
        .map_err(|error| {
            JobError::Tool(format!(
                "{}: {error}",
                artifact.dir.join(&job.unit.output_name).display()
            ))
        })?
        .len();

    Ok(JobOutcome {
        artifact,
        compiled: true,
        fresh: false,
        bytes,
        diagnostics,
    })
}

#[allow(clippy::too_many_arguments)]
fn complete(
    index: usize,
    artifact: Artifact,
    plan: &plan::Plan,
    mode: Mode,
    profile_dirs: &ProfileDirs,
    store: &Store,
    artifacts: &mut [Option<Artifact>],
    search_dirs: &mut BTreeSet<PathBuf>,
    binaries: &mut Vec<BinaryArtifact>,
    tests: &mut Vec<TestArtifact>,
    schedule: &mut Schedule,
    _dependents: &[Vec<usize>],
    announced: &mut [bool],
) -> Result<(), Error> {
    let unit = &plan.units[index];
    let profile_dir = profile_dirs.for_unit(unit);

    match (mode, unit.kind) {
        (Mode::Build, UnitKind::Bin) => {
            let destination = profile_dir.join(&unit.target_name);
            store
                .link_into(&artifact.dir.join(&unit.output_name), &destination)
                .map_err(Error::tool)?;
            binaries.push(BinaryArtifact {
                name: unit.target_name.clone(),
                path: destination,
                kind: BinaryKind::Bin,
            });
        }
        (Mode::Build, UnitKind::Example) => {
            let destination = profile_dir.join(EXAMPLES_DIR).join(&unit.target_name);
            store
                .link_into(&artifact.dir.join(&unit.output_name), &destination)
                .map_err(Error::tool)?;
            binaries.push(BinaryArtifact {
                name: unit.target_name.clone(),
                path: destination,
                kind: BinaryKind::Example,
            });
        }
        (Mode::Build, UnitKind::Bench) => {
            binaries.push(BinaryArtifact {
                name: unit.target_name.clone(),
                path: artifact.dir.join(&unit.output_name),
                kind: BinaryKind::Bench,
            });
        }
        (Mode::Test, UnitKind::Test) => {
            tests.push(TestArtifact {
                name: unit.target_name.clone(),
                path: artifact.dir.join(&unit.output_name),
            });
        }
        (Mode::Test, UnitKind::Bench) => {
            tests.push(TestArtifact {
                name: unit.target_name.clone(),
                path: artifact.dir.join(&unit.output_name),
            });
        }
        _ => {}
    }
    if unit.kind == UnitKind::Lib || unit.kind == UnitKind::ProcMacro {
        search_dirs.insert(artifact.dir.clone());
    }
    let path = artifact.dir.join(&unit.output_name);
    artifacts[index] = Some(artifact);
    schedule.completed(index, path, plan, mode, announced);

    Ok(())
}

fn requires_metadata_only(mode: Mode, kind: UnitKind) -> bool {
    mode == Mode::Check && matches!(kind, UnitKind::Bin | UnitKind::Example | UnitKind::Bench)
}

fn needed_packages(
    resolution: &Resolution,
    roots: &[PackageId],
    mode: Mode,
    selection: &TargetSelection,
) -> BTreeSet<PackageId> {
    let with_dev = mode == Mode::Test || !selection.is_empty();
    let mut needed = BTreeSet::new();
    let mut pending: Vec<PackageId> = roots.to_vec();
    while let Some(id) = pending.pop() {
        if !needed.insert(id.clone()) {
            continue;
        }
        let Some(package) = resolution.packages.get(&id) else {
            continue;
        };
        for edge in &package.dependencies {
            if edge.kind == DependencyKind::Dev && !with_dev {
                continue;
            }
            pending.push(edge.package.clone());
        }
    }
    needed
}

fn check_rust_versions(
    resolution: &Resolution,
    needed: &BTreeSet<PackageId>,
    active: &str,
) -> Result<(), Error> {
    let Some(active_version) = parse_version_triple(active) else {
        return Ok(());
    };
    for (id, package) in &resolution.packages {
        if !needed.contains(id) {
            continue;
        }
        let required = package
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.rust_version.as_deref())
            .or_else(|| {
                package
                    .index
                    .as_ref()
                    .and_then(|entry| entry.rust_version.as_deref())
            });
        let Some(required) = required else {
            continue;
        };
        let Some(required_version) = parse_version_triple(required) else {
            return Err(Error::tool(format!(
                "package `{} {}` has invalid `rust-version` `{required}`",
                id.name, package.version
            )));
        };
        if active_version < required_version {
            return Err(Error::tool(format!(
                "package `{} {}` requires rustc {required} or newer, but rustc {active} is active",
                id.name, package.version
            )));
        }
    }
    Ok(())
}

fn parse_version_triple(text: &str) -> Option<(u64, u64, u64)> {
    let core = text
        .split(|character| character == '-' || character == '+')
        .next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Some((major, minor, patch))
}

fn lib_units(plan: &plan::Plan) -> BTreeMap<(PackageId, bool), usize> {
    let mut units = BTreeMap::new();
    for (index, unit) in plan.units.iter().enumerate() {
        if unit.kind == UnitKind::Lib || unit.kind == UnitKind::ProcMacro {
            units.insert((unit.package.clone(), unit.host), index);
        }
    }
    units
}

fn build_script_units(plan: &plan::Plan) -> BTreeMap<PackageId, usize> {
    let mut units = BTreeMap::new();
    for (index, unit) in plan.units.iter().enumerate() {
        if unit.kind == UnitKind::BuildScript {
            units.insert(unit.package.clone(), index);
        }
    }
    units
}

fn dependency_graph(
    plan: &plan::Plan,
    lib_units: &BTreeMap<(PackageId, bool), usize>,
    build_script_units: &BTreeMap<PackageId, usize>,
    packages: &BTreeMap<PackageId, PackageInfo>,
) -> Result<(Vec<Vec<usize>>, Vec<Vec<usize>>), Error> {
    let mut dependencies = vec![Vec::new(); plan.units.len()];
    for (index, unit) in plan.units.iter().enumerate() {
        for entry in &unit.externs {
            let dependency_index = *lib_units
                .get(&(entry.package.clone(), entry.host))
                .ok_or_else(|| {
                    Error::tool(format!(
                        "internal error: dependency `{}` of `{}` has no library unit",
                        entry.name, unit.package_name
                    ))
                })?;
            if !dependencies[index].contains(&dependency_index) {
                dependencies[index].push(dependency_index);
            }
        }
    }

    for (id, package) in packages {
        let Some(build_script) = build_script_units.get(id).copied() else {
            continue;
        };
        for (index, unit) in plan.units.iter().enumerate() {
            if unit.package == *id
                && index != build_script
                && !dependencies[index].contains(&build_script)
            {
                dependencies[index].push(build_script);
            }
        }
        for edge in &package.dependencies {
            if let Some(dependency_script) = build_script_units.get(&edge.package).copied() {
                if dependency_script != build_script
                    && !dependencies[build_script].contains(&dependency_script)
                {
                    dependencies[build_script].push(dependency_script);
                }
            }
        }
    }

    let mut dependents = vec![Vec::new(); plan.units.len()];
    for (index, deps) in dependencies.iter().enumerate() {
        for dependency in deps {
            dependents[*dependency].push(index);
        }
    }

    Ok((dependencies, dependents))
}

#[allow(clippy::too_many_arguments)]
fn finish_unit(
    index: usize,
    artifact: Artifact,
    plan: &plan::Plan,
    packages: &BTreeMap<PackageId, PackageInfo>,
    config: &Config,
    options: &Options<'_>,
    cfg: &Cfg,
    identity: &RustcIdentity,
    source_hashes: &HashMap<PackageId, [u8; 32]>,
    store: &Store,
    profile_dirs: &ProfileDirs,
    build_script_outputs: &mut BTreeMap<PackageId, BuildScriptOutput>,
    artifacts: &mut [Option<Artifact>],
    search_dirs: &mut BTreeSet<PathBuf>,
    binaries: &mut Vec<BinaryArtifact>,
    tests: &mut Vec<TestArtifact>,
    schedule: &mut Schedule,
    _dependents: &[Vec<usize>],
    announced: &mut [bool],
) -> Result<(), Error> {
    let unit = &plan.units[index];
    if unit.kind == UnitKind::BuildScript {
        let package = packages.get(&unit.package).ok_or_else(|| {
            Error::tool(format!(
                "missing package `{}` in the resolved graph",
                unit.package.name
            ))
        })?;
        let flags = Flags::resolve(
            config,
            options.profiles,
            options.profile,
            options.incremental,
            &unit.package_name,
            unit.kind,
            options.target,
        )
        .map_err(Error::tool)?;
        let source = source_hashes.get(&unit.package).copied().ok_or_else(|| {
            Error::tool(format!(
                "internal error: source hash for `{}` is missing",
                unit.package.name
            ))
        })?;
        let binary = artifact.dir.join(&unit.output_name);
        let output = build_script::run(
            unit,
            package,
            &binary,
            &artifact.key,
            &source,
            &flags,
            cfg,
            identity.host(),
            options.profile,
            options.jobs,
            store,
            packages,
            build_script_outputs,
        )
        .map_err(Error::compile)?;
        build_script_outputs.insert(unit.package.clone(), output);
    }

    complete(
        index,
        artifact,
        plan,
        options.mode,
        profile_dirs,
        store,
        artifacts,
        search_dirs,
        binaries,
        tests,
        schedule,
        _dependents,
        announced,
    )
}

fn emit_diagnostics(
    index: usize,
    diagnostics: &rustc::Diagnostics,
    format: MessageFormat,
    plan: &plan::Plan,
    warnings: &mut usize,
    lines: &mut BTreeMap<usize, Vec<String>>,
) -> Result<(), Error> {
    *warnings += diagnostics.warnings;
    match format {
        MessageFormat::Human | MessageFormat::Short => {
            use std::io::Write;
            std::io::stderr()
                .write_all(&diagnostics.stderr)
                .map_err(|error| Error::tool(format!("failed to write diagnostics: {error}")))?;
        }
        MessageFormat::Json => {
            let unit = &plan.units[index];
            let package_id = format!("{} {}", unit.package_name, unit.package_version);
            for line in diagnostics.stderr.split(|byte| *byte == b'\n') {
                if line.is_empty() {
                    continue;
                }
                let Ok(text) = std::str::from_utf8(line) else {
                    continue;
                };
                if crate::util::json::parse(text).is_ok() {
                    lines
                        .entry(index)
                        .or_default()
                        .push(output::compiler_message(&package_id, text));
                } else {
                    eprintln!("{text}");
                }
            }
        }
    }
    Ok(())
}

fn emit_artifact(
    index: usize,
    artifact: &Artifact,
    fresh: bool,
    plan: &plan::Plan,
    format: MessageFormat,
    lines: &mut BTreeMap<usize, Vec<String>>,
) {
    if format != MessageFormat::Json {
        return;
    }
    let unit = &plan.units[index];
    let package_id = format!("{} {}", unit.package_name, unit.package_version);
    let kind = match unit.kind {
        UnitKind::Lib => "lib",
        UnitKind::Bin => "bin",
        UnitKind::Test => "test",
        UnitKind::Example => "example",
        UnitKind::Bench => "bench",
        UnitKind::BuildScript => "custom-build",
        UnitKind::ProcMacro => "proc-macro",
    };
    let filename = artifact.dir.join(&unit.output_name);
    lines
        .entry(index)
        .or_default()
        .push(output::compiler_artifact(
            &package_id,
            &unit.target_name,
            kind,
            &filename.to_string_lossy(),
            fresh,
        ));
}

pub(crate) fn rustc_program() -> std::ffi::OsString {
    std::env::var_os(RUSTC_ENV).unwrap_or_else(|| DEFAULT_RUSTC.into())
}

pub(crate) const CLIPPY_ARTIFACT_SALT: &str = "clippy";

const RUSTC_ENV: &str = "RUSTC";
const DEFAULT_RUSTC: &str = "rustc";
const CLIPPY_DRIVER: &str = "clippy-driver";
const CLIPPY_NAME_PREFIX: &str = "clippy";
const PATH_ENV: &str = "PATH";
const CARGO_HOME_ENV: &str = "CARGO_HOME";
const CARGO_BIN_DIR: &str = "bin";
const DOT_CARGO_DIR: &str = ".cargo";

#[cfg(windows)]
const EXECUTABLE_SUFFIX: &str = ".exe";
#[cfg(not(windows))]
const EXECUTABLE_SUFFIX: &str = "";

pub(crate) fn clippy_driver() -> Result<PathBuf, String> {
    let name = format!("{CLIPPY_DRIVER}{EXECUTABLE_SUFFIX}");
    let rustc_env = std::env::var_os(RUSTC_ENV);
    let path_match = std::env::var_os(PATH_ENV);
    let bin_dirs = cargo_bin_dirs();
    clippy_driver_from(&name, &rustc_env, path_match.as_deref(), &bin_dirs)
}

fn clippy_driver_from(
    name: &str,
    rustc_env: Option<&std::ffi::OsStr>,
    path: Option<&std::ffi::OsStr>,
    bin_dirs: &[PathBuf],
) -> Result<PathBuf, String> {
    if let Some(rustc) = rustc_env {
        let path = PathBuf::from(rustc);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if name.starts_with(CLIPPY_NAME_PREFIX) {
            return Ok(path);
        }
    }
    if let Some(path) = path {
        let found = std::env::split_paths(path)
            .map(|directory| directory.join(name))
            .find(|candidate| rustc::is_executable(candidate));
        if let Some(found) = found {
            return Ok(found);
        }
    }
    for directory in bin_dirs {
        let candidate = directory.join(name);
        if rustc::is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "{CLIPPY_DRIVER} was not found on `PATH` or in the cargo home directory, install the clippy component with `rustup component add clippy`"
    ))
}

fn cargo_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(cargo_home) = std::env::var_os(CARGO_HOME_ENV) {
        dirs.push(PathBuf::from(cargo_home).join(CARGO_BIN_DIR));
    }
    if let Ok(home) = crate::config::home() {
        dirs.push(home.join(DOT_CARGO_DIR).join(CARGO_BIN_DIR));
    }
    dirs
}

pub(crate) fn clean(
    workspace: &Workspace,
    config: &Config,
    target_dir_override: Option<&Path>,
    quiet: bool,
) -> Result<(), Error> {
    let target_root = resolve_target_root(workspace, config, target_dir_override)?;
    if target_root.exists() {
        std::fs::remove_dir_all(&target_root)
            .map_err(|error| Error::tool(format!("{}: {error}", target_root.display())))?;
        if !quiet {
            println!("Removed {}", target_root.display());
        }
    }
    Ok(())
}

pub(crate) fn resolve_target_root(
    workspace: &Workspace,
    config: &Config,
    target_dir_override: Option<&Path>,
) -> Result<PathBuf, Error> {
    match target_dir_override {
        Some(path) if path.is_absolute() => Ok(path.to_path_buf()),
        Some(path) => {
            let current = std::env::current_dir().map_err(|error| {
                Error::tool(format!("cannot read the current directory: {error}"))
            })?;
            Ok(current.join(path))
        }
        None if config.target_dir.is_absolute() => Ok(config.target_dir.clone()),
        None => Ok(workspace.root_dir.join(&config.target_dir)),
    }
}

fn profile_dir_name(profile: &str) -> &str {
    if profile == "dev" {
        "debug"
    } else {
        profile
    }
}

fn canonical(path: &Path) -> Result<PathBuf, Error> {
    path.canonicalize()
        .map_err(|error| Error::tool(format!("{}: {error}", path.display())))
}

#[cfg(test)]
mod layout_tests {
    use super::{platform_dir, profile_dir};

    #[test]
    fn target_layout_prefixes_the_triple_for_target_units() {
        let root = std::path::Path::new("/target");
        assert_eq!(
            platform_dir(root, Some("aarch64-unknown-linux-gnu")),
            std::path::PathBuf::from("/target/aarch64-unknown-linux-gnu")
        );
        assert_eq!(
            platform_dir(root, None),
            std::path::PathBuf::from("/target")
        );
    }

    #[test]
    fn profile_dirs_split_host_and_target_platforms() {
        let root = std::path::Path::new("/target");
        let triple = Some("aarch64-unknown-linux-gnu");
        assert_eq!(
            profile_dir(root, triple, false, "dev"),
            std::path::PathBuf::from("/target/aarch64-unknown-linux-gnu/debug")
        );
        assert_eq!(
            profile_dir(root, triple, true, "dev"),
            std::path::PathBuf::from("/target/debug")
        );
        assert_eq!(
            profile_dir(root, None, true, "release"),
            std::path::PathBuf::from("/target/release")
        );
        assert_eq!(
            profile_dir(root, None, false, "dev"),
            std::path::PathBuf::from("/target/debug")
        );
    }
}

#[cfg(test)]
mod clippy_tests {
    use super::clippy_driver_from;

    #[test]
    fn clippy_driver_error_suggests_the_clippy_component() {
        let missing = clippy_driver_from("clippy-driver", None, None, &[]);
        let error = missing.unwrap_err();

        assert!(
            error.contains("install the clippy component with `rustup component add clippy`"),
            "{error}"
        );
    }
}
