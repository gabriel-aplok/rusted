use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::build::{self, BinaryArtifact, MessageFormat, Mode};
use crate::cache;
use crate::config::{cache_root, Config, MAX_PARALLELISM};
use crate::manifest::edit::{add_dependency, remove_dependency};
use crate::manifest::select::{discover, discover_exact, ManifestFile};
use crate::registry::index::IndexClient;
use crate::report;
use crate::resolver::cfg::Cfg;
use crate::resolver::lockfile::Lockfile;
use crate::resolver::resolve::{
    self, BuildContext, PackageId, PackageSource, Resolution, RootOptions, RootPackage,
};
use crate::resolver::semver::Version;
use crate::workspace::{Package, PackageKey, Workspace};

pub const VERSION: &str = "0.1.0";

const COMMANDS: &[(&str, &str)] = &[
    ("new", "Create a new package"),
    ("init", "Create a manifest in an existing directory"),
    ("add", "Add a dependency"),
    ("remove", "Remove a dependency"),
    ("build", "Compile the workspace"),
    ("doc", "Build documentation for the workspace"),
    ("check", "Typecheck the workspace"),
    ("run", "Build and run a binary"),
    ("test", "Build and run tests"),
    ("clean", "Remove build artifacts"),
    ("update", "Update the lockfile"),
    ("tree", "Print the dependency tree"),
    ("metadata", "Print the workspace graph as JSON"),
    ("cache", "Inspect or clean the global cache"),
    ("help", "Print help"),
    ("version", "Print the version"),
];

#[derive(Debug)]
struct Failure {
    code: i32,
    message: String,
}

impl Failure {
    fn tool(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
}

impl From<build::Error> for Failure {
    fn from(error: build::Error) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    Build,
    Check,
    Doc,
    Run,
    Test,
    Clean,
    Add,
    Remove,
    Update,
    Tree,
    Metadata,
}

impl CommandKind {
    fn build_like(self) -> bool {
        matches!(
            self,
            CommandKind::Build
                | CommandKind::Check
                | CommandKind::Doc
                | CommandKind::Run
                | CommandKind::Test
        )
    }
}

#[derive(Debug, Default)]
struct Options {
    manifest_path: Option<PathBuf>,
    target_dir: Option<PathBuf>,
    open: bool,
    document_private_items: bool,
    pass_through: Vec<String>,
    document: bool,
    packages: Vec<String>,
    workspace: bool,
    release: bool,
    force: bool,
    profile: Option<String>,
    incremental: bool,
    stats: bool,
    offline: bool,
    features: BTreeSet<String>,
    all_features: bool,
    no_default_features: bool,
    locked: bool,
    frozen: bool,
    jobs: Option<usize>,
    quiet: bool,
    verbose: bool,
    bin: Option<String>,
    program_args: Vec<OsString>,
    no_run: bool,
    test_threads: Option<usize>,
    test_filter: Option<String>,
    test_args: Vec<OsString>,
    message_format: MessageFormat,
    name: Option<String>,
    optional: bool,
    dev: bool,
    build_dependency: bool,
    clean_project: bool,
    clean_store: bool,
    clean_index: bool,
    clean_source: bool,
    clean_all: bool,
    help: bool,
}

pub fn run<I>(args: I) -> i32
where
    I: IntoIterator<Item = OsString>,
{
    match execute(args) {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            failure.code
        }
    }
}

fn execute<I>(args: I) -> Result<i32, Failure>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        print_help();
        return Ok(0);
    };
    let first = first.to_string_lossy();
    let name = first.as_ref();

    match name {
        "-V" | "--version" | "version" => {
            println!("rusted {VERSION}");
            Ok(0)
        }
        "-h" | "--help" | "help" => {
            print_help();
            Ok(0)
        }
        "build" => command(CommandKind::Build, args),
        "doc" => command(CommandKind::Doc, args),
        "check" => command(CommandKind::Check, args),
        "run" => command(CommandKind::Run, args),
        "test" => command(CommandKind::Test, args),
        "clean" => command(CommandKind::Clean, args),
        "add" => command(CommandKind::Add, args),
        "remove" => command(CommandKind::Remove, args),
        "update" => command(CommandKind::Update, args),
        "tree" => command(CommandKind::Tree, args),
        "metadata" => command(CommandKind::Metadata, args),
        "cache" => cache_command(args),
        "new" => new_command(args),
        "init" => init_command(args),
        _ => Err(Failure::tool(format!(
            "unknown command `{name}`\nRun `rusted --help` for usage."
        ))),
    }
}

fn command<I>(kind: CommandKind, args: I) -> Result<i32, Failure>
where
    I: IntoIterator<Item = OsString>,
{
    let options = parse_options(kind, args)?;
    if options.help {
        print_command_help(kind);
        return Ok(0);
    }

    match kind {
        CommandKind::Clean => clean(&options),
        CommandKind::Add => add(&options),
        CommandKind::Remove => remove(&options),
        CommandKind::Update => update(&options),
        CommandKind::Tree => tree(&options),
        CommandKind::Metadata => metadata(&options),
        CommandKind::Build | CommandKind::Check | CommandKind::Doc | CommandKind::Run
        | CommandKind::Test => build_command(kind, &options),
    }
}

fn build_command(kind: CommandKind, options: &Options) -> Result<i32, Failure> {
    if options.release && options.profile.is_some() {
        return Err(Failure::tool(
            "`--release` and `--profile` cannot be used together",
        ));
    }
    let profile = options
        .profile
        .clone()
        .unwrap_or_else(|| if options.release { "release" } else { "dev" }.to_string());

    let (config, _, workspace) = load_workspace(options)?;
    let targets = select_targets(&workspace, options)?;
    let jobs = options.jobs.or(config.jobs).unwrap_or_else(default_jobs);
    if options.verbose {
        eprintln!("using {jobs} job(s)");
    }

    let mode = match kind {
        CommandKind::Check => Mode::Check,
        CommandKind::Build | CommandKind::Doc | CommandKind::Run => Mode::Build,
        CommandKind::Test => Mode::Test,
        _ => unreachable!(),
    };

    let build_options = build::Options {
        targets: &targets,
        mode,
        profile: &profile,
        force: options.force,
        incremental: options.incremental,
        jobs,
        quiet: options.quiet,
        verbose: options.verbose,
        stats: options.stats,
        message_format: options.message_format,
        features: options.features.clone(),
        all_features: options.all_features,
        no_default_features: options.no_default_features,
        locked: options.locked || options.frozen,
        target_dir: options.target_dir.as_deref(),
        profiles: &workspace.profiles,
        document: kind == CommandKind::Doc,
    };
    let result = build::run(&workspace, &config, &build_options).map_err(Failure::from)?;

    if options.document {
        return run_docs(&workspace, &config, &result, options);
    }
    if kind == CommandKind::Test {
        return run_tests(&result, options);
    }
    if kind != CommandKind::Run {
        return Ok(0);
    }

    let binary = select_binary(&workspace, &targets, &result.binaries, options)?;
    if !options.quiet {
        eprintln!("Running `{}`", binary.path.display());
    }
    let status = Command::new(&binary.path)
        .args(&options.program_args)
        .status()
        .map_err(|error| {
            Failure::tool(format!(
                "failed to run `{}`: {error}",
                binary.path.display()
            ))
        })?;
    Ok(status.code().unwrap_or(1))
}

fn run_docs(
    workspace: &Workspace,
    config: &Config,
    result: &build::BuildResult,
    options: &Options,
) -> Result<i32, Failure> {
    let target_root = build::resolve_target_root(workspace, config, options.target_dir.as_deref())
        .map_err(Failure::from)?;
    build::doc::generate(
        &result.docs,
        &target_root,
        options.open,
        options.document_private_items,
        &options.pass_through,
    )
    .map_err(Failure::tool)?;
    Ok(0)
}

fn run_tests(result: &build::BuildResult, options: &Options) -> Result<i32, Failure> {
    if options.no_run {
        return Ok(0);
    }

    let mut arguments: Vec<OsString> = Vec::new();
    if let Some(filter) = &options.test_filter {
        arguments.push(OsString::from(filter));
    }
    if let Some(threads) = options.test_threads {
        arguments.push(OsString::from("--test-threads"));
        arguments.push(OsString::from(threads.to_string()));
    }
    arguments.extend(options.test_args.iter().cloned());

    let mut failed = false;
    for test in &result.tests {
        if !options.quiet {
            println!("Running {}", test.name);
        }
        let status = Command::new(&test.path)
            .args(&arguments)
            .status()
            .map_err(|error| {
                Failure::tool(format!("failed to run `{}`: {error}", test.path.display()))
            })?;
        if !status.success() {
            failed = true;
        }
    }

    if failed {
        Ok(101)
    } else {
        Ok(0)
    }
}

fn clean(options: &Options) -> Result<i32, Failure> {
    let explicit = options.clean_project
        || options.clean_store
        || options.clean_index
        || options.clean_source
        || options.clean_all;
    let project = options.clean_project || options.clean_all || !explicit;
    let scopes = cache::CleanScopes {
        store: options.clean_store || options.clean_all,
        index: options.clean_index || options.clean_all,
        source: options.clean_source || options.clean_all,
        all: options.clean_all,
    };

    let config = if project {
        let (config, _, workspace) = load_workspace(options)?;
        build::clean(
            &workspace,
            &config,
            options.target_dir.as_deref(),
            options.quiet,
        )
        .map_err(Failure::from)?;
        config
    } else {
        Config::load().map_err(Failure::tool)?
    };
    cache::clean(&config, &scopes).map_err(Failure::tool)?;
    Ok(0)
}

type Context = (Config, ManifestFile, Workspace);

fn load_workspace(options: &Options) -> Result<Context, Failure> {
    let mut config = Config::load().map_err(Failure::tool)?;
    if options.offline || options.frozen {
        config.offline = true;
    }
    let entry = match &options.manifest_path {
        Some(path) => discover_exact(path).map_err(Failure::tool)?,
        None => {
            let directory = std::env::current_dir().map_err(|error| {
                Failure::tool(format!("cannot read the current directory: {error}"))
            })?;
            discover(&directory, config.manifest).map_err(Failure::tool)?
        }
    };
    let workspace = Workspace::load(&entry, config.manifest).map_err(Failure::tool)?;
    Ok((config, entry, workspace))
}

const LOCK_FILE: &str = "Cargo.lock";

fn add(options: &Options) -> Result<i32, Failure> {
    let name = required_name(options)?;
    if options.dev && options.build_dependency {
        return Err(Failure::tool(
            "`--dev` and `--build` cannot be used together",
        ));
    }
    let (config, entry, workspace) = load_workspace(options)?;
    let package = select_manifest_package(&workspace, &entry, options)?;
    let path = package.manifest_path.clone();

    let section = if options.build_dependency {
        "build-dependencies"
    } else if options.dev {
        "dev-dependencies"
    } else {
        "dependencies"
    };

    let mut index = IndexClient::new(&config).map_err(Failure::tool)?;
    let entries = index.entries(&name).map_err(Failure::tool)?;
    let version = entries
        .iter()
        .filter(|entry| !entry.yanked && !entry.version.is_prerelease())
        .map(|entry| &entry.version)
        .max()
        .cloned()
        .ok_or_else(|| Failure::tool(format!("no usable version found for `{name}`")))?;
    let value = dependency_value(&version, options);

    let text = read_manifest(&path)?;
    let updated = add_dependency(&text, section, &name, &value, &path).map_err(Failure::tool)?;
    write_manifest(&path, &updated)?;
    if !options.quiet {
        println!("Adding {name} v{version} to {section}");
    }

    update_lockfile(options)
}

fn remove(options: &Options) -> Result<i32, Failure> {
    let name = required_name(options)?;
    let (_, entry, workspace) = load_workspace(options)?;
    let package = select_manifest_package(&workspace, &entry, options)?;
    let path = package.manifest_path.clone();

    let text = read_manifest(&path)?;
    let updated = remove_dependency(&text, &name, &path).map_err(Failure::tool)?;
    write_manifest(&path, &updated)?;
    if !options.quiet {
        println!("Removing {name}");
    }

    update_lockfile(options)
}

fn update(options: &Options) -> Result<i32, Failure> {
    if options.frozen {
        return Err(Failure::tool("`--frozen` prevents updating the lockfile"));
    }
    let (config, _, workspace) = load_workspace(options)?;
    let (resolution, _) =
        resolve_workspace(&config, &workspace, &workspace.members, None, false, true)?;
    write_lockfile(&workspace, &resolution)
}

fn tree(options: &Options) -> Result<i32, Failure> {
    let (config, _, workspace) = load_workspace(options)?;
    let lock = read_lockfile(&workspace)?;
    let (resolution, roots) = resolve_workspace(
        &config,
        &workspace,
        &workspace.default_targets,
        lock.as_ref(),
        options.frozen,
        false,
    )?;
    print!("{}", report::tree(&resolution, &roots));
    Ok(0)
}

fn metadata(options: &Options) -> Result<i32, Failure> {
    let (config, _, workspace) = load_workspace(options)?;
    let lock = read_lockfile(&workspace)?;
    let (resolution, roots) = resolve_workspace(
        &config,
        &workspace,
        &workspace.members,
        lock.as_ref(),
        options.frozen,
        true,
    )?;
    println!(
        "{}",
        report::metadata(&resolution, &roots, &workspace.root_dir)
    );
    Ok(0)
}

fn required_name(options: &Options) -> Result<String, Failure> {
    options
        .name
        .clone()
        .ok_or_else(|| Failure::tool("a dependency name is required"))
}

fn select_manifest_package<'a>(
    workspace: &'a Workspace,
    entry: &ManifestFile,
    options: &Options,
) -> Result<&'a Package, Failure> {
    if options.packages.len() > 1 {
        return Err(Failure::tool(
            "`--package` accepts one package for this command",
        ));
    }
    if let Some(name) = options.packages.first() {
        let key = workspace
            .members
            .iter()
            .find(|key| &key.name == name)
            .ok_or_else(|| Failure::tool(format!("`{name}` is not a member of this workspace")))?;
        return workspace
            .package(key)
            .ok_or_else(|| Failure::tool(format!("missing package `{name}`")));
    }

    let dir = std::fs::canonicalize(entry.dir())
        .map_err(|error| Failure::tool(format!("{}: {error}", entry.dir().display())))?;
    workspace
        .packages
        .values()
        .find(|package| package.key.dir == dir)
        .ok_or_else(|| {
            Failure::tool(format!(
                "`{}` is a virtual workspace root, pass `--package`",
                entry.path.display()
            ))
        })
}

fn dependency_value(version: &Version, options: &Options) -> String {
    let mut fields = vec![format!("version = \"{version}\"")];
    if options.optional {
        fields.push("optional = true".to_string());
    }
    if options.no_default_features {
        fields.push("default-features = false".to_string());
    }
    if !options.features.is_empty() {
        let features: Vec<String> = options
            .features
            .iter()
            .map(|feature| format!("\"{feature}\""))
            .collect();
        fields.push(format!("features = [{}]", features.join(", ")));
    }
    if fields.len() == 1 {
        format!("\"{version}\"")
    } else {
        format!("{{ {} }}", fields.join(", "))
    }
}

fn read_manifest(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path)
        .map_err(|error| Failure::tool(format!("{}: {error}", path.display())))
}

fn write_manifest(path: &Path, text: &str) -> Result<(), Failure> {
    std::fs::write(path, text)
        .map_err(|error| Failure::tool(format!("{}: {error}", path.display())))
}

fn update_lockfile(options: &Options) -> Result<i32, Failure> {
    let (config, _, workspace) = load_workspace(options)?;
    let lock = read_lockfile(&workspace)?;
    let (resolution, _) = resolve_workspace(
        &config,
        &workspace,
        &workspace.members,
        lock.as_ref(),
        options.frozen,
        true,
    )?;
    if options.frozen {
        return Ok(0);
    }
    write_lockfile(&workspace, &resolution)
}

fn lockfile_path(workspace: &Workspace) -> PathBuf {
    workspace.root_dir.join(LOCK_FILE)
}

fn read_lockfile(workspace: &Workspace) -> Result<Option<Lockfile>, Failure> {
    let path = lockfile_path(workspace);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| Failure::tool(format!("{}: {error}", path.display())))?;
    Lockfile::parse(&text, &path)
        .map(Some)
        .map_err(Failure::tool)
}

fn write_lockfile(workspace: &Workspace, resolution: &Resolution) -> Result<i32, Failure> {
    Lockfile::from_resolution(resolution)
        .write(&lockfile_path(workspace))
        .map_err(Failure::tool)?;
    Ok(0)
}

fn resolve_workspace(
    config: &Config,
    workspace: &Workspace,
    roots: &[PackageKey],
    lock: Option<&Lockfile>,
    locked: bool,
    all_targets: bool,
) -> Result<(Resolution, Vec<PackageId>), Failure> {
    if roots.is_empty() {
        return Err(Failure::tool("this workspace has no members"));
    }

    let mut root_requests = Vec::new();
    let mut root_ids = Vec::new();
    for key in roots {
        let package = workspace
            .package(key)
            .ok_or_else(|| Failure::tool(format!("missing package `{}`", key.name)))?;
        let version = Version::parse(&package.manifest.version).map_err(Failure::tool)?;
        root_ids.push(PackageId {
            name: package.manifest.name.clone(),
            version,
            source: PackageSource::Path(package.key.dir.clone()),
            context: BuildContext::Target,
        });
        root_requests.push(RootPackage {
            manifest: package.manifest.clone(),
            manifest_path: package.manifest_path.clone(),
        });
    }

    let cache_root = cache_root(config).map_err(Failure::tool)?;
    let cfg = Cfg::cached(&cache_root.join("v1").join("cfg-cache")).map_err(Failure::tool)?;
    let mut index = IndexClient::new(config).map_err(Failure::tool)?;
    let git_context = resolve::GitContext::new(cache_root.join("v1").join("git"), config.offline);
    let options = RootOptions {
        inheritance: Some(workspace.inheritance()),
        resolver: workspace.resolver,
        dev_features: true,
        locked,
        all_targets,
        ..RootOptions::default()
    };
    let resolution = resolve::resolve(
        &root_requests,
        &mut index,
        &cfg,
        lock,
        config.manifest,
        Some(&git_context),
        &options,
    )
    .map_err(Failure::tool)?;
    Ok((resolution, root_ids))
}

fn select_targets(workspace: &Workspace, options: &Options) -> Result<Vec<PackageKey>, Failure> {
    if options.workspace && !options.packages.is_empty() {
        return Err(Failure::tool(
            "`--workspace` and `--package` cannot be used together",
        ));
    }

    if options.workspace {
        if workspace.members.is_empty() {
            return Err(Failure::tool("this workspace has no members to build"));
        }
        return Ok(workspace.members.clone());
    }

    if !options.packages.is_empty() {
        let mut targets = Vec::new();
        for name in &options.packages {
            let key = workspace
                .members
                .iter()
                .find(|key| &key.name == name)
                .ok_or_else(|| {
                    Failure::tool(format!("`{name}` is not a member of this workspace"))
                })?;
            if !targets.contains(key) {
                targets.push(key.clone());
            }
        }
        return Ok(targets);
    }

    if workspace.default_targets.is_empty() {
        return Err(Failure::tool(
            "no packages to build, use `--workspace` or `--package`",
        ));
    }
    Ok(workspace.default_targets.clone())
}

fn select_binary<'a>(
    workspace: &Workspace,
    targets: &[PackageKey],
    binaries: &'a [BinaryArtifact],
    options: &Options,
) -> Result<&'a BinaryArtifact, Failure> {
    if let Some(name) = &options.bin {
        return binaries
            .iter()
            .find(|binary| &binary.name == name)
            .ok_or_else(|| {
                Failure::tool(format!(
                    "no binary named `{name}` was built, available: {}",
                    binary_names(binaries)
                ))
            });
    }

    if targets.len() == 1 {
        if let Some(package) = workspace.package(&targets[0]) {
            if let Some(default_run) = &package.manifest.default_run {
                return binaries
                    .iter()
                    .find(|binary| &binary.name == default_run)
                    .ok_or_else(|| {
                        Failure::tool(format!(
                            "`default-run = \"{default_run}\"` does not match a built binary, available: {}",
                            binary_names(binaries)
                        ))
                    });
            }
        }
    }

    match binaries {
        [binary] => Ok(binary),
        [] => Err(Failure::tool(
            "the selected packages have no binary targets to run",
        )),
        _ => Err(Failure::tool(format!(
            "multiple binaries were built, use `--bin` or `--package`; available: {}",
            binary_names(binaries)
        ))),
    }
}

fn binary_names(binaries: &[BinaryArtifact]) -> String {
    if binaries.is_empty() {
        return "none".to_string();
    }
    let mut names: Vec<&str> = binaries.iter().map(|binary| binary.name.as_str()).collect();
    names.sort();
    names.join(", ")
}

fn parse_options<I>(kind: CommandKind, args: I) -> Result<Options, Failure>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = Options {
        document: kind == CommandKind::Doc,
        ..Options::default()
    };
    let mut expanded: Vec<OsString> = Vec::new();
    let mut after_separator = false;
    for argument in args {
        let text = argument.to_string_lossy();
        if !after_separator && text == "--" {
            after_separator = true;
            expanded.push(argument);
        } else if !after_separator && text.starts_with("--") {
            if let Some((flag, value)) = text.split_once('=') {
                expanded.push(OsString::from(flag));
                expanded.push(OsString::from(value));
            } else {
                expanded.push(argument);
            }
        } else {
            expanded.push(argument);
        }
    }
    let mut args = expanded.into_iter();
    let build_like = kind.build_like();

    while let Some(argument) = args.next() {
        let text = argument.to_string_lossy();
        match text.as_ref() {
            "-h" | "--help" => {
                options.help = true;
                return Ok(options);
            }
            "--" if kind == CommandKind::Run => {
                options.program_args.extend(args);
                break;
            }
            "--" if kind == CommandKind::Test => {
                options.test_args.extend(args);
                break;
            }
            "--" if kind == CommandKind::Doc => {
                options.pass_through.extend(
                    args.map(|argument| argument.to_string_lossy().into_owned()),
                );
                break;
            }
            "-p" | "--package"
                if build_like || kind == CommandKind::Add || kind == CommandKind::Remove =>
            {
                options.packages.push(next_value(&mut args, &text)?);
            }
            "--workspace" if build_like => options.workspace = true,
            "--release" if build_like => options.release = true,
            "--force" if build_like => options.force = true,
            "--profile" if build_like => {
                let value = next_value(&mut args, &text)?;
                options.profile = Some(parse_profile(&value)?);
            }
            "--incremental" if build_like => options.incremental = true,
            "--stats" if build_like => options.stats = true,
            "--offline" => options.offline = true,
            "--features" if build_like || kind == CommandKind::Add => {
                let value = next_value(&mut args, &text)?;
                add_features(&mut options.features, &value)?;
            }
            "--all-features" if build_like => options.all_features = true,
            "--no-default-features" if build_like || kind == CommandKind::Add => {
                options.no_default_features = true;
            }
            "--optional" if kind == CommandKind::Add => options.optional = true,
            "--dev" if kind == CommandKind::Add => options.dev = true,
            "--build" if kind == CommandKind::Add => options.build_dependency = true,
            "--locked" if build_like => options.locked = true,
            "--frozen" if kind != CommandKind::Clean => options.frozen = true,
            "--project" if kind == CommandKind::Clean => options.clean_project = true,
            "--store" if kind == CommandKind::Clean => options.clean_store = true,
            "--index" if kind == CommandKind::Clean => options.clean_index = true,
            "--source" if kind == CommandKind::Clean => options.clean_source = true,
            "--all" if kind == CommandKind::Clean => options.clean_all = true,
            "--message-format" if build_like => {
                let value = next_value(&mut args, &text)?;
                options.message_format = parse_message_format(&value)?;
            }
            "-j" | "--jobs" if build_like => {
                let value = next_value(&mut args, &text)?;
                options.jobs = Some(parse_jobs(&value)?);
            }
            "--manifest-path" => {
                options.manifest_path = Some(PathBuf::from(next_value(&mut args, &text)?));
            }
            "--target-dir" => {
                options.target_dir = Some(PathBuf::from(next_value(&mut args, &text)?));
            }
            "-q" | "--quiet" => options.quiet = true,
            "-v" | "--verbose" => options.verbose = true,
            "--bin" if kind == CommandKind::Run => {
                options.bin = Some(next_value(&mut args, &text)?);
            }
            "--no-run" if kind == CommandKind::Test => options.no_run = true,
            "--open" if kind == CommandKind::Doc => options.open = true,
            "--document-private-items" if kind == CommandKind::Doc => {
                options.document_private_items = true;
            }
            "--test-threads" if kind == CommandKind::Test => {
                let value = next_value(&mut args, &text)?;
                options.test_threads = Some(parse_test_threads(&value)?);
            }
            _ if kind == CommandKind::Test && !text.starts_with('-') => {
                if options.test_filter.is_some() {
                    return Err(Failure::tool(format!(
                        "unexpected argument `{text}` for `test`, only one test filter is supported"
                    )));
                }
                options.test_filter = Some(text.into_owned());
            }
            _ if (kind == CommandKind::Add || kind == CommandKind::Remove)
                && !text.starts_with('-') =>
            {
                if options.name.is_some() {
                    return Err(Failure::tool(format!(
                        "unexpected argument `{text}` for `{}`",
                        command_name(kind)
                    )));
                }
                options.name = Some(text.into_owned());
            }
            _ if text.starts_with('-') => {
                return Err(Failure::tool(format!(
                    "unexpected option `{text}` for `{}`",
                    command_name(kind)
                )));
            }
            _ => {
                return Err(Failure::tool(format!(
                    "unexpected argument `{text}` for `{}`",
                    command_name(kind)
                )));
            }
        }
    }

    Ok(options)
}

fn next_value(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<String, Failure> {
    let Some(value) = args.next() else {
        return Err(Failure::tool(format!("`{flag}` requires a value")));
    };
    let text = value.to_string_lossy().into_owned();
    if text.is_empty() {
        return Err(Failure::tool(format!("`{flag}` requires a value")));
    }
    Ok(text)
}

fn parse_jobs(text: &str) -> Result<usize, Failure> {
    text.parse()
        .ok()
        .filter(|jobs| *jobs >= 1 && *jobs <= MAX_PARALLELISM)
        .ok_or_else(|| {
            Failure::tool(format!(
                "`--jobs` must be an integer from 1 to {MAX_PARALLELISM}, got `{text}`"
            ))
        })
}

fn add_features(features: &mut BTreeSet<String>, text: &str) -> Result<(), Failure> {
    let mut added = 0;
    for part in text.split(|character: char| character == ',' || character.is_whitespace()) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        features.insert(part.to_string());
        added += 1;
    }
    if added == 0 {
        return Err(Failure::tool(
            "`--features` needs at least one feature name",
        ));
    }
    Ok(())
}

fn parse_message_format(text: &str) -> Result<MessageFormat, Failure> {
    match text {
        "human" => Ok(MessageFormat::Human),
        "short" => Ok(MessageFormat::Short),
        "json" => Ok(MessageFormat::Json),
        _ => Err(Failure::tool(format!(
            "`--message-format` must be `human`, `short`, or `json`, got `{text}`"
        ))),
    }
}

fn parse_test_threads(text: &str) -> Result<usize, Failure> {
    text.parse()
        .ok()
        .filter(|threads| *threads >= 1 && *threads <= MAX_PARALLELISM)
        .ok_or_else(|| {
            Failure::tool(format!(
                "`--test-threads` must be an integer from 1 to {MAX_PARALLELISM}, got `{text}`"
            ))
        })
}

fn parse_profile(text: &str) -> Result<String, Failure> {
    if text.is_empty()
        || !text.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(Failure::tool(format!(
            "`--profile` must be a simple name, got `{text}`"
        )));
    }
    Ok(text.to_string())
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|jobs| jobs.get())
        .unwrap_or(1)
}

fn command_name(kind: CommandKind) -> &'static str {
    match kind {
        CommandKind::Build => "build",
        CommandKind::Check => "check",
        CommandKind::Doc => "doc",
        CommandKind::Run => "run",
        CommandKind::Test => "test",
        CommandKind::Clean => "clean",
        CommandKind::Add => "add",
        CommandKind::Remove => "remove",
        CommandKind::Update => "update",
        CommandKind::Tree => "tree",
        CommandKind::Metadata => "metadata",
    }
}

fn cache_command<I>(args: I) -> Result<i32, Failure>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    let Some(command) = args.first().map(String::as_str) else {
        print_cache_help();
        return Ok(0);
    };
    match command {
        "-h" | "--help" | "help" => {
            print_cache_help();
            Ok(0)
        }
        "info" => cache_info(&args[1..]),
        "gc" => cache_gc(&args[1..]),
        other => Err(Failure::tool(format!(
            "unknown cache command `{other}`\nRun `rusted cache --help` for usage."
        ))),
    }
}

fn cache_info(args: &[String]) -> Result<i32, Failure> {
    let mut json = false;
    for argument in args {
        match argument.as_str() {
            "-h" | "--help" => {
                print_cache_help();
                return Ok(0);
            }
            "--json" => json = true,
            other => {
                return Err(Failure::tool(format!(
                    "unexpected argument `{other}` for `cache info`"
                )))
            }
        }
    }

    let config = Config::load().map_err(Failure::tool)?;
    let info = cache::info(&config).map_err(Failure::tool)?;
    if json {
        println!(
            "{{\"reason\":\"cache-info\",\"index\":{},\"crates\":{},\"sources\":{},\"artifacts\":{},\"total\":{}}}",
            info.index, info.crates, info.sources, info.artifacts, info.total
        );
    } else {
        println!("index     {}", cache::human_size(info.index));
        println!("crates    {}", cache::human_size(info.crates));
        println!("sources   {}", cache::human_size(info.sources));
        println!("artifacts {}", cache::human_size(info.artifacts));
        println!("total     {}", cache::human_size(info.total));
    }
    Ok(0)
}

fn cache_gc(args: &[String]) -> Result<i32, Failure> {
    let mut max_size = None;
    let mut dry_run = false;
    let mut json = false;
    let mut remote = false;
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print_cache_help();
                return Ok(0);
            }
            "--max-size" => {
                let value = args
                    .next()
                    .ok_or_else(|| Failure::tool("`--max-size` requires a value such as `5G`"))?;
                max_size = Some(cache::parse_size(value).map_err(Failure::tool)?);
            }
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            "--remote" => remote = true,
            other => {
                return Err(Failure::tool(format!(
                    "unexpected argument `{other}` for `cache gc`"
                )))
            }
        }
    }
    let max_size =
        max_size.ok_or_else(|| Failure::tool("`cache gc` requires `--max-size <SIZE>`"))?;

    let (removed, remaining) = if remote {
        let remote = crate::remote::Remote::from_env()
            .map_err(Failure::tool)?
            .ok_or_else(|| Failure::tool("`RUSTED_REMOTE_CACHE` is not set"))?;
        let report = remote.gc(max_size, dry_run).map_err(Failure::tool)?;
        (report.removed, report.remaining)
    } else {
        let config = Config::load().map_err(Failure::tool)?;
        let report = cache::gc(&config, max_size, dry_run).map_err(Failure::tool)?;
        (
            report
                .removed
                .into_iter()
                .map(|removed| (removed.key, removed.bytes))
                .collect(),
            report.remaining,
        )
    };
    let bytes: u64 = removed.iter().map(|(_, size)| size).sum();
    if json {
        let artifacts: Vec<String> = removed
            .iter()
            .map(|(key, size)| format!("{{\"key\":\"{}\",\"bytes\":{}}}", key, size))
            .collect();
        println!(
            "{{\"reason\":\"cache-gc\",\"dry_run\":{},\"removed\":{},\"bytes\":{},\"remaining\":{},\"artifacts\":[{}]}}",
            dry_run,
            removed.len(),
            bytes,
            remaining,
            artifacts.join(",")
        );
    } else {
        println!(
            "{} {} artifact(s), {}",
            if dry_run { "would remove" } else { "removed" },
            removed.len(),
            cache::human_size(bytes)
        );
        println!("remaining {}", cache::human_size(remaining));
    }
    Ok(0)
}

fn print_cache_help() {
    println!("Inspect or clean the global cache");
    println!();
    println!("Usage: rusted cache <COMMAND>");
    println!();
    println!("Commands:");
    println!("  info  Report cache sizes");
    println!("  gc    Remove least recently used artifacts");
    println!();
    println!("gc options:");
    println!("      --max-size <SIZE>  Target maximum size, such as `5G`");
    println!("      --dry-run          Report removals without deleting");
    println!("      --remote           Garbage collect remote S3 objects");
    println!("      --json             Emit JSON");
    println!();
    println!("Options:");
    println!("  -h, --help  Print help");
}

fn new_command<I>(args: I) -> Result<i32, Failure>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let Some(path) = args.next() else {
        return Err(Failure::tool("`new` needs a path"));
    };
    let text = path.to_string_lossy();
    if text == "-h" || text == "--help" {
        print_creation_help("new");
        return Ok(0);
    }
    if let Some(extra) = args.next() {
        return Err(Failure::tool(format!(
            "unexpected argument `{}` for `new`",
            extra.to_string_lossy()
        )));
    }
    create_package(Path::new(&path))
}

fn init_command<I>(args: I) -> Result<i32, Failure>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let path = match args.next() {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from("."),
    };
    let text = path.to_string_lossy();
    if text == "-h" || text == "--help" {
        print_creation_help("init");
        return Ok(0);
    }
    if let Some(extra) = args.next() {
        return Err(Failure::tool(format!(
            "unexpected argument `{}` for `init`",
            extra.to_string_lossy()
        )));
    }
    create_package(&path)
}

fn create_package(path: &Path) -> Result<i32, Failure> {
    let manifest = path.join("Cargo.toml");
    if manifest.exists() || path.join("rusted.toml").exists() {
        return Err(Failure::tool(format!(
            "`{}` already contains a manifest",
            path.display()
        )));
    }
    let name = package_name(path)?;
    std::fs::create_dir_all(path.join("src"))
        .map_err(|error| Failure::tool(format!("{}: {error}", path.display())))?;
    std::fs::write(
        &manifest,
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .map_err(|error| Failure::tool(format!("{}: {error}", manifest.display())))?;
    let main = path.join("src").join("main.rs");
    std::fs::write(&main, "fn main() {\n    println!(\"Hello, world!\");\n}\n")
        .map_err(|error| Failure::tool(format!("{}: {error}", main.display())))?;
    println!("Created {}", path.display());
    Ok(0)
}

fn package_name(path: &Path) -> Result<String, Failure> {
    let directory = if path.as_os_str().is_empty() || path == Path::new(".") {
        std::env::current_dir()
            .map_err(|error| Failure::tool(format!("cannot read the current directory: {error}")))?
    } else {
        path.to_path_buf()
    };
    let name = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| Failure::tool("cannot derive a package name from the path"))?;
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(Failure::tool(format!(
            "`{name}` is not a valid package name, pass a directory with letters, digits, `_`, or `-`"
        )));
    }
    Ok(name)
}

fn print_creation_help(name: &str) {
    if let Some((_, about)) = COMMANDS.iter().find(|(command, _)| *command == name) {
        println!("{about}");
        println!();
        println!("Usage: rusted {name} [PATH]");
        println!();
        println!("Options:");
        println!("  -h, --help  Print help");
    }
}

fn print_help() {
    println!("rusted {VERSION}");
    println!("A fast, low-disk Cargo-compatible Rust build tool.");
    println!();
    println!("Usage: rusted <COMMAND> [OPTIONS]");
    println!();
    println!("Commands:");
    for (name, about) in COMMANDS {
        println!("  {name:<10} {about}");
    }
    println!();
    println!("Options:");
    println!("  -h, --help     Print help");
    println!("  -V, --version  Print version");
}

fn print_command_help(kind: CommandKind) {
    let name = command_name(kind);
    let usage = match kind {
        CommandKind::Run => format!("Usage: rusted {name} [OPTIONS] [-- <ARGS>...]"),
        CommandKind::Test => format!("Usage: rusted {name} [OPTIONS] [TESTNAME] [-- <ARGS>...]"),
        CommandKind::Doc => format!("Usage: rusted {name} [OPTIONS] [-- <ARGS>...]"),
        CommandKind::Add | CommandKind::Remove => {
            format!("Usage: rusted {name} <NAME> [OPTIONS]")
        }
        _ => format!("Usage: rusted {name} [OPTIONS]"),
    };
    println!("{}", command_about(kind));
    println!();
    println!("{usage}");
    println!();
    println!("Options:");
    if kind.build_like() {
        println!("  -p, --package <NAME>       Build only the named workspace member, repeatable");
        println!("      --workspace            Build all workspace members");
        println!("      --release              Use the release profile");
        println!("      --force                Rebuild even when the artifact is cached");
        println!("      --profile <NAME>       Use a custom profile");
        println!("      --incremental          Enable incremental compilation");
        println!("      --stats                Print build statistics");
        println!("      --message-format <FMT> human, short, or json");
        println!("      --features <LIST>      Enable features, comma or space separated");
        println!("      --all-features         Enable every feature");
        println!("      --no-default-features  Disable default features");
        println!("      --locked               Refuse to update Cargo.lock");
        println!("      --frozen               Require the lockfile and forbid network access");
        println!("  -j, --jobs <N>             Number of parallel jobs");
    }
    if kind == CommandKind::Run {
        println!("      --bin <NAME>           Run the named binary");
    }
    if kind == CommandKind::Test {
        println!("      --no-run               Build tests without running them");
        println!("      --test-threads <N>     Pass --test-threads to the harness");
    }
    if kind == CommandKind::Add || kind == CommandKind::Remove {
        println!("  -p, --package <NAME>       Package to edit when the workspace has several");
        println!("      --frozen               Require the lockfile and forbid network access");
    }
    if kind == CommandKind::Add {
        println!("      --features <LIST>      Enable features, comma or space separated");
        println!("      --no-default-features  Disable default features");
        println!("      --optional             Mark the dependency as optional");
        println!("      --dev                  Add to dev-dependencies");
        println!("      --build                Add to build-dependencies");
    }
    if kind == CommandKind::Doc {
        println!("      --open                 Open the documentation in a browser");
        println!("      --document-private-items  Include private items in the docs");
        println!("      ARGS after `--`        Passed through to rustdoc");
    }
    if kind == CommandKind::Clean {
        println!("      --project              Remove the project target directory (default)");
        println!("      --store                Remove cached artifacts and build scripts");
        println!("      --index                Remove the registry index");
        println!("      --source               Remove downloaded crates and extracted sources");
        println!("      --all                  Remove the project target and the whole cache");
    }
    println!("      --offline              Never access the network");
    println!("      --manifest-path <PATH> Use this manifest");
    println!("      --target-dir <PATH>    Directory for build artifacts");
    println!("  -q, --quiet                Suppress progress output");
    println!("  -v, --verbose              Print rustc commands");
    println!("  -h, --help                 Print help");
}

fn command_about(kind: CommandKind) -> &'static str {
    match kind {
        CommandKind::Build => "Compile the workspace",
        CommandKind::Doc => "Build documentation for the workspace",
        CommandKind::Check => "Typecheck the workspace",
        CommandKind::Run => "Build and run a binary",
        CommandKind::Test => "Build and run tests",
        CommandKind::Clean => "Remove build artifacts",
        CommandKind::Add => "Add a dependency",
        CommandKind::Remove => "Remove a dependency",
        CommandKind::Update => "Update the lockfile",
        CommandKind::Tree => "Print the dependency tree",
        CommandKind::Metadata => "Print the workspace graph as JSON",
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{parse_options, CommandKind, COMMANDS, VERSION};
    use crate::build::MessageFormat;

    fn args(values: &[&str]) -> impl Iterator<Item = OsString> {
        values
            .iter()
            .map(|value| OsString::from(*value))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn command_names_are_unique() {
        for (index, (name, _)) in COMMANDS.iter().enumerate() {
            for (other, _) in &COMMANDS[index + 1..] {
                assert_ne!(name, other);
            }
        }
    }

    #[test]
    fn version_is_not_empty() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn parses_build_options() {
        let options = parse_options(
            CommandKind::Build,
            args(&["--release", "-j", "4", "-p", "app"]),
        )
        .unwrap();
        assert!(options.release);
        assert_eq!(options.jobs, Some(4));
        assert_eq!(options.packages, vec!["app"]);
        assert!(!options.workspace);
    }

    #[test]
    fn parses_doc_options() {
        let options = parse_options(
            CommandKind::Doc,
            args(&["--open", "--document-private-items", "--", "-C", "panic=abort"]),
        )
        .unwrap();
        assert!(options.open);
        assert!(options.document_private_items);
        assert!(options.document);
        assert_eq!(
            options.pass_through,
            vec!["-C".to_string(), "panic=abort".to_string()]
        );
        let error = parse_options(CommandKind::Build, args(&["--open"])).unwrap_err();
        assert!(error.message.contains("unexpected option `--open`"));
    }

    #[test]
    fn parses_run_arguments_after_separator() {
        let options = parse_options(
            CommandKind::Run,
            args(&["--bin", "app", "--", "one", "--two"]),
        )
        .unwrap();
        assert_eq!(options.bin.as_deref(), Some("app"));
        assert_eq!(
            options.program_args,
            vec![OsString::from("one"), OsString::from("--two")]
        );
    }

    #[test]
    fn parses_profile_and_stats() {
        let options = parse_options(
            CommandKind::Build,
            args(&["--profile", "fast", "--incremental", "--stats"]),
        )
        .unwrap();
        assert_eq!(options.profile.as_deref(), Some("fast"));
        assert!(options.incremental);
        assert!(options.stats);
    }

    #[test]
    fn rejects_bad_profile_names() {
        let error = parse_options(CommandKind::Build, args(&["--profile", "../x"])).unwrap_err();
        assert!(error.message.contains("`--profile` must be a simple name"));
    }

    #[test]
    fn parses_test_options() {
        let options = parse_options(
            CommandKind::Test,
            args(&[
                "answer",
                "--no-run",
                "--test-threads",
                "1",
                "--",
                "--nocapture",
            ]),
        )
        .unwrap();
        assert_eq!(options.test_filter.as_deref(), Some("answer"));
        assert!(options.no_run);
        assert_eq!(options.test_threads, Some(1));
        assert_eq!(options.test_args, vec![OsString::from("--nocapture")]);
    }

    #[test]
    fn parses_message_format() {
        let options =
            parse_options(CommandKind::Build, args(&["--message-format", "json"])).unwrap();
        assert_eq!(options.message_format, MessageFormat::Json);
        let error =
            parse_options(CommandKind::Build, args(&["--message-format", "xml"])).unwrap_err();
        assert!(error
            .message
            .contains("must be `human`, `short`, or `json`"));
    }

    #[test]
    fn parses_clean_scopes() {
        let options = parse_options(CommandKind::Clean, args(&["--store", "--index"])).unwrap();
        assert!(options.clean_store);
        assert!(options.clean_index);
        assert!(!options.clean_source);
        let options = parse_options(CommandKind::Clean, args(&["--all"])).unwrap();
        assert!(options.clean_all);
    }

    #[test]
    fn parses_frozen() {
        let options = parse_options(CommandKind::Build, args(&["--frozen"])).unwrap();
        assert!(options.frozen);
        assert!(!options.locked);
        let error = parse_options(CommandKind::Clean, args(&["--frozen"])).unwrap_err();
        assert!(error.message.contains("unexpected option `--frozen`"));
    }

    #[test]
    fn parses_offline() {
        let options = parse_options(CommandKind::Build, args(&["--offline"])).unwrap();
        assert!(options.offline);
    }

    #[test]
    fn parses_feature_flags() {
        let options = parse_options(
            CommandKind::Build,
            args(&[
                "--features",
                "one,two three",
                "--all-features",
                "--no-default-features",
                "--locked",
            ]),
        )
        .unwrap();
        assert!(options.features.contains("one"));
        assert!(options.features.contains("two"));
        assert!(options.features.contains("three"));
        assert!(options.all_features);
        assert!(options.no_default_features);
        assert!(options.locked);
    }

    #[test]
    fn parses_add_options() {
        let options = parse_options(
            CommandKind::Add,
            args(&[
                "serde",
                "--features",
                "derive,std",
                "--optional",
                "--no-default-features",
            ]),
        )
        .unwrap();
        assert_eq!(options.name.as_deref(), Some("serde"));
        assert!(options.optional);
        assert!(options.no_default_features);
        assert!(options.features.contains("derive"));
        assert!(options.features.contains("std"));
        assert!(!options.dev);
    }

    #[test]
    fn parses_remove_name() {
        let options = parse_options(CommandKind::Remove, args(&["serde"])).unwrap();
        assert_eq!(options.name.as_deref(), Some("serde"));
    }

    #[test]
    fn add_rejects_build_only_options() {
        let error = parse_options(CommandKind::Add, args(&["serde", "--release"])).unwrap_err();
        assert!(error.message.contains("unexpected option `--release`"));
    }

    #[test]
    fn build_rejects_add_only_options() {
        let error = parse_options(CommandKind::Build, args(&["--optional"])).unwrap_err();
        assert!(error.message.contains("unexpected option `--optional`"));
    }

    #[test]
    fn update_rejects_positional_arguments() {
        let error = parse_options(CommandKind::Update, args(&["serde"])).unwrap_err();
        assert!(error.message.contains("unexpected argument `serde`"));
    }

    #[test]
    fn dependency_values_match_the_requested_flags() {
        use super::{dependency_value, Options};
        use crate::resolver::semver::Version;

        let version = Version::parse("1.2.3").unwrap();
        let options = Options::default();
        assert_eq!(dependency_value(&version, &options), "\"1.2.3\"");

        let mut options = Options::default();
        options.optional = true;
        options.no_default_features = true;
        options.features.insert("derive".to_string());
        assert_eq!(
            dependency_value(&version, &options),
            "{ version = \"1.2.3\", optional = true, default-features = false, features = [\"derive\"] }"
        );
    }

    #[test]
    fn clean_rejects_build_options() {
        let error = parse_options(CommandKind::Clean, args(&["--release"])).unwrap_err();
        assert!(error.message.contains("unexpected option `--release`"));
    }

    #[test]
    fn rejects_unknown_options() {
        let error = parse_options(CommandKind::Build, args(&["--nope"])).unwrap_err();
        assert!(error.message.contains("unexpected option `--nope`"));
    }
}
