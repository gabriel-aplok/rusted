use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::profile::Profile;
use crate::toml::{Table, Value};

const PACKAGE_KEYS: &[&str] = &[
    "name",
    "version",
    "edition",
    "rust-version",
    "description",
    "documentation",
    "homepage",
    "repository",
    "readme",
    "keywords",
    "categories",
    "license",
    "license-file",
    "authors",
    "publish",
    "default-run",
    "build",
    "links",
    "resolver",
    "autolib",
    "autobins",
    "autoexamples",
    "autotests",
    "autobenches",
    "metadata",
    "include",
    "exclude",
];

const TARGET_KEYS: &[&str] = &[
    "name",
    "path",
    "crate-type",
    "proc-macro",
    "test",
    "bench",
    "doctest",
    "harness",
    "required-features",
];

const DEPENDENCY_KEYS: &[&str] = &[
    "version",
    "path",
    "registry",
    "optional",
    "default-features",
    "default_features",
    "features",
    "package",
    "workspace",
    "git",
    "branch",
    "tag",
    "rev",
];

const WORKSPACE_KEYS: &[&str] = &[
    "members",
    "exclude",
    "default-members",
    "resolver",
    "package",
    "dependencies",
];

const INHERITED_KEYS: &[&str] = &[
    "version",
    "authors",
    "description",
    "documentation",
    "edition",
    "homepage",
    "keywords",
    "categories",
    "license",
    "license-file",
    "publish",
    "readme",
    "repository",
    "rust-version",
    "include",
    "exclude",
];

const ROOT_KEYS: &[&str] = &[
    "package",
    "lib",
    "bin",
    "example",
    "test",
    "bench",
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
    "target",
    "features",
    "workspace",
    "profile",
    "patch",
    "replace",
    "lints",
    "badges",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DependencyKind {
    Normal,
    Dev,
    Build,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edition {
    E2015,
    E2018,
    E2021,
    E2024,
}

impl Edition {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        match text {
            "2015" => Ok(Self::E2015),
            "2018" => Ok(Self::E2018),
            "2021" => Ok(Self::E2021),
            "2024" => Ok(Self::E2024),
            _ => Err(format!("unsupported edition `{text}`")),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::E2015 => "2015",
            Self::E2018 => "2018",
            Self::E2021 => "2021",
            Self::E2024 => "2024",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BuildScript {
    Disabled,
    Path(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PublishPolicy {
    Allowed,
    Never,
    Registries(Vec<String>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AutoTargets {
    pub(crate) lib: Option<bool>,
    pub(crate) bins: Option<bool>,
    pub(crate) examples: Option<bool>,
    pub(crate) tests: Option<bool>,
    pub(crate) benches: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitSource {
    pub(crate) url: String,
    pub(crate) branch: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) rev: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Dependency {
    pub(crate) requirement: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) registry: Option<String>,
    pub(crate) optional: bool,
    pub(crate) default_features: Option<bool>,
    pub(crate) features: Vec<String>,
    pub(crate) package: Option<String>,
    pub(crate) git: Option<GitSource>,
    pub(crate) workspace: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TargetSpec {
    pub(crate) name: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) crate_types: Vec<String>,
    pub(crate) proc_macro: Option<bool>,
    pub(crate) test: Option<bool>,
    pub(crate) bench: Option<bool>,
    pub(crate) doctest: Option<bool>,
    pub(crate) harness: Option<bool>,
    pub(crate) required_features: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TargetDeps {
    pub(crate) dependencies: BTreeMap<String, Dependency>,
    pub(crate) dev_dependencies: BTreeMap<String, Dependency>,
    pub(crate) build_dependencies: BTreeMap<String, Dependency>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Workspace {
    pub(crate) members: Vec<String>,
    pub(crate) exclude: Vec<String>,
    pub(crate) default_members: Option<Vec<String>>,
    pub(crate) resolver: Option<String>,
    pub(crate) shared_package: Option<Table>,
    pub(crate) shared_dependencies: Option<Table>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ProfileEntry {
    pub(crate) settings: Profile,
    pub(crate) package: BTreeMap<String, Profile>,
    pub(crate) build_override: Option<Profile>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Manifest {
    pub(crate) is_virtual: bool,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) edition: Edition,
    pub(crate) rust_version: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) documentation: Option<String>,
    pub(crate) homepage: Option<String>,
    pub(crate) repository: Option<String>,
    pub(crate) readme: Option<String>,
    pub(crate) keywords: Vec<String>,
    pub(crate) categories: Vec<String>,
    pub(crate) authors: Vec<String>,
    pub(crate) license: Option<String>,
    pub(crate) license_file: Option<String>,
    pub(crate) publish: Option<PublishPolicy>,
    pub(crate) default_run: Option<String>,
    pub(crate) build: Option<BuildScript>,
    pub(crate) links: Option<String>,
    pub(crate) resolver: Option<String>,
    pub(crate) autodiscovery: AutoTargets,
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
    pub(crate) lib: Option<TargetSpec>,
    pub(crate) bins: Vec<TargetSpec>,
    pub(crate) examples: Vec<TargetSpec>,
    pub(crate) tests: Vec<TargetSpec>,
    pub(crate) benches: Vec<TargetSpec>,
    pub(crate) dependencies: BTreeMap<String, Dependency>,
    pub(crate) dev_dependencies: BTreeMap<String, Dependency>,
    pub(crate) build_dependencies: BTreeMap<String, Dependency>,
    pub(crate) targets: BTreeMap<String, TargetDeps>,
    pub(crate) features: BTreeMap<String, Vec<String>>,
    pub(crate) workspace: Option<Workspace>,
    pub(crate) profiles: BTreeMap<String, ProfileEntry>,
    pub(crate) lints: Option<Table>,
    pub(crate) badges: Option<Table>,
    pub(crate) metadata: Option<Table>,
    pub(crate) patch: Option<Table>,
    pub(crate) replace: Option<Table>,
    pub(crate) warnings: Vec<String>,
    pub(crate) inherited: BTreeSet<&'static str>,
}

pub(crate) fn map(root: &Table) -> Result<Manifest, String> {
    let mut warnings = Vec::new();

    let is_virtual = root.get("package").is_none();
    let empty = Table::new();
    let package: &Table = match root.get("package") {
        None if root.get("workspace").is_some() => &empty,
        None => return Err("missing `[package]` table".into()),
        Some(Value::Table(table)) => table,
        Some(_) => return Err("`[package]` must be a table".into()),
    };
    let (package_table, inherited) = strip_inherited(package);
    let package = &package_table;
    unknown_keys(package, PACKAGE_KEYS, "package", &mut warnings);

    let name = match get_string(package, "package.name", "name")? {
        Some(name) => name,
        None if is_virtual => String::new(),
        None => return Err("`[package]` is missing `name`".into()),
    };
    let version = match get_string(package, "package.version", "version")? {
        Some(version) => version,
        None if is_virtual || inherited.contains("version") => String::new(),
        None => return Err("`[package]` is missing `version`".into()),
    };
    let edition = match package.get("edition") {
        None => Edition::E2015,
        Some(Value::String(text)) => Edition::parse(text)?,
        Some(_) => return Err("`package.edition` must be a string".into()),
    };

    let build = match package.get("build") {
        None => None,
        Some(Value::Boolean(false)) => Some(BuildScript::Disabled),
        Some(Value::Boolean(true)) => return Err("`package.build` cannot be `true`".into()),
        Some(Value::String(path)) => Some(BuildScript::Path(path.clone())),
        Some(_) => return Err("`package.build` must be a string or a boolean".into()),
    };

    let publish = parse_publish(package, "package.publish")?;

    let metadata = match package.get("metadata") {
        None => None,
        Some(Value::Table(table)) => Some(table.clone()),
        Some(_) => return Err("`package.metadata` must be a table".into()),
    };

    let autodiscovery = AutoTargets {
        lib: get_bool(package, "package.autolib", "autolib")?,
        bins: get_bool(package, "package.autobins", "autobins")?,
        examples: get_bool(package, "package.autoexamples", "autoexamples")?,
        tests: get_bool(package, "package.autotests", "autotests")?,
        benches: get_bool(package, "package.autobenches", "autobenches")?,
    };

    let lib = parse_optional_target(root, "lib", &mut warnings)?;
    let bins = parse_target_list(root, "bin", &mut warnings)?;
    let examples = parse_target_list(root, "example", &mut warnings)?;
    let tests = parse_target_list(root, "test", &mut warnings)?;
    let benches = parse_target_list(root, "bench", &mut warnings)?;

    let dependencies = parse_dependencies(root, "dependencies", "dependencies", &mut warnings)?;
    let dev_dependencies =
        parse_dependencies(root, "dev-dependencies", "dev-dependencies", &mut warnings)?;
    let build_dependencies = parse_dependencies(
        root,
        "build-dependencies",
        "build-dependencies",
        &mut warnings,
    )?;

    let targets = parse_target_sections(root, &mut warnings)?;
    let features = parse_features(root)?;
    let workspace = match root.get("workspace") {
        None => None,
        Some(Value::Table(table)) => {
            let (parsed, unknown) = parse_workspace(table)?;
            for key in unknown {
                warnings.push(format!("unknown field `{key}` in `workspace`"));
            }
            Some(parsed)
        }
        Some(_) => return Err("`[workspace]` must be a table".into()),
    };

    let profiles = parse_profiles(root, &mut warnings)?;

    let patch = parse_optional_table(root, "patch")?;
    let replace = parse_optional_table(root, "replace")?;
    let lints = parse_optional_table(root, "lints")?;
    let badges = parse_optional_table(root, "badges")?;

    unknown_keys(root, ROOT_KEYS, "manifest root", &mut warnings);

    if is_virtual {
        for key in [
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
            "target",
            "features",
            "lib",
            "bin",
            "example",
            "test",
            "bench",
        ] {
            if root.get(key).is_some() {
                return Err(format!("a virtual manifest cannot contain `[{key}]`"));
            }
        }
    }

    Ok(Manifest {
        is_virtual,
        name: name.clone(),
        version: version.clone(),
        edition,
        rust_version: get_string(package, "package.rust-version", "rust-version")?,
        description: get_string(package, "package.description", "description")?,
        documentation: get_string(package, "package.documentation", "documentation")?,
        homepage: get_string(package, "package.homepage", "homepage")?,
        repository: get_string(package, "package.repository", "repository")?,
        readme: parse_readme(package)?,
        keywords: get_string_list(package, "package.keywords", "keywords")?.unwrap_or_default(),
        categories: get_string_list(package, "package.categories", "categories")?
            .unwrap_or_default(),
        authors: get_string_list(package, "package.authors", "authors")?.unwrap_or_default(),
        license: get_string(package, "package.license", "license")?,
        license_file: get_string(package, "package.license-file", "license-file")?,
        publish,
        default_run: get_string(package, "package.default-run", "default-run")?,
        build,
        links: get_string(package, "package.links", "links")?,
        resolver: package
            .get("resolver")
            .map(|value| parse_resolver(value, "package.resolver"))
            .transpose()?,
        autodiscovery,
        include: get_string_list(package, "package.include", "include")?.unwrap_or_default(),
        exclude: get_string_list(package, "package.exclude", "exclude")?.unwrap_or_default(),
        lib,
        bins,
        examples,
        tests,
        benches,
        dependencies,
        dev_dependencies,
        build_dependencies,
        targets,
        features,
        workspace,
        profiles,
        lints,
        badges,
        metadata,
        patch,
        replace,
        warnings,
        inherited,
    })
}

impl Manifest {
    pub(crate) fn dependency_sections(
        &self,
    ) -> impl Iterator<Item = (DependencyKind, &BTreeMap<String, Dependency>)> {
        [
            (DependencyKind::Normal, &self.dependencies),
            (DependencyKind::Dev, &self.dev_dependencies),
            (DependencyKind::Build, &self.build_dependencies),
        ]
        .into_iter()
    }

    pub(crate) fn target_dependency_sections(
        &self,
    ) -> impl Iterator<Item = (&str, DependencyKind, &BTreeMap<String, Dependency>)> {
        self.targets.iter().flat_map(|(cfg, deps)| {
            [
                (cfg.as_str(), DependencyKind::Normal, &deps.dependencies),
                (cfg.as_str(), DependencyKind::Dev, &deps.dev_dependencies),
                (
                    cfg.as_str(),
                    DependencyKind::Build,
                    &deps.build_dependencies,
                ),
            ]
        })
    }

    pub(crate) fn inherit(
        &mut self,
        workspace_dir: &Path,
        shared_package: Option<&Table>,
        shared_dependencies: Option<&Table>,
    ) -> Result<(), String> {
        if !self.inherited.is_empty() {
            let shared = shared_package.ok_or_else(|| {
                "`[package]` fields inherit with `workspace = true`, but the workspace has no `[workspace.package]`".to_string()
            })?;
            let inherited: Vec<&'static str> = self.inherited.iter().copied().collect();
            for field in inherited {
                self.inherit_package_field(field, shared)?;
            }
            self.inherited.clear();
        }

        inherit_dependencies(&mut self.dependencies, shared_dependencies, workspace_dir)?;
        inherit_dependencies(
            &mut self.dev_dependencies,
            shared_dependencies,
            workspace_dir,
        )?;
        inherit_dependencies(
            &mut self.build_dependencies,
            shared_dependencies,
            workspace_dir,
        )?;
        for deps in self.targets.values_mut() {
            inherit_dependencies(&mut deps.dependencies, shared_dependencies, workspace_dir)?;
            inherit_dependencies(
                &mut deps.dev_dependencies,
                shared_dependencies,
                workspace_dir,
            )?;
            inherit_dependencies(
                &mut deps.build_dependencies,
                shared_dependencies,
                workspace_dir,
            )?;
        }
        Ok(())
    }

    fn inherit_package_field(&mut self, field: &str, shared: &Table) -> Result<(), String> {
        if shared.get(field).is_none() {
            return Err(missing_inherited(field));
        }
        match field {
            "version" => self.version = inherited_string(shared, field)?,
            "edition" => self.edition = Edition::parse(&inherited_string(shared, field)?)?,
            "rust-version" => self.rust_version = Some(inherited_string(shared, field)?),
            "description" => self.description = Some(inherited_string(shared, field)?),
            "documentation" => self.documentation = Some(inherited_string(shared, field)?),
            "homepage" => self.homepage = Some(inherited_string(shared, field)?),
            "repository" => self.repository = Some(inherited_string(shared, field)?),
            "license" => self.license = Some(inherited_string(shared, field)?),
            "license-file" => self.license_file = Some(inherited_string(shared, field)?),
            "authors" => self.authors = inherited_list(shared, field)?,
            "keywords" => self.keywords = inherited_list(shared, field)?,
            "categories" => self.categories = inherited_list(shared, field)?,
            "include" => self.include = inherited_list(shared, field)?,
            "exclude" => self.exclude = inherited_list(shared, field)?,
            "publish" => self.publish = parse_publish(shared, "workspace.package.publish")?,
            "readme" => self.readme = parse_readme(shared)?,
            _ => return Err(format!("internal error: unknown inherited field `{field}`")),
        }
        Ok(())
    }
}

fn strip_inherited(package: &Table) -> (Table, BTreeSet<&'static str>) {
    let mut kept = Table::new();
    let mut inherited = BTreeSet::new();
    for (key, value) in package.iter() {
        let known = INHERITED_KEYS.iter().find(|known| **known == key);
        let marks = matches!(
            value,
            Value::Table(table) if table.get("workspace") == Some(&Value::Boolean(true))
        );
        if let (Some(known), true) = (known, marks) {
            inherited.insert(*known);
            continue;
        }
        kept.insert(key.to_string(), value.clone())
            .expect("package keys are unique");
    }
    (kept, inherited)
}

fn inherit_dependencies(
    dependencies: &mut BTreeMap<String, Dependency>,
    shared: Option<&Table>,
    workspace_dir: &Path,
) -> Result<(), String> {
    for (name, entry) in dependencies.iter_mut() {
        if !entry.workspace {
            continue;
        }
        let shared = shared.ok_or_else(|| {
            format!(
                "dependency `{name}` inherits with `workspace = true`, but the workspace has no `[workspace.dependencies]`"
            )
        })?;
        let value = shared.get(name).ok_or_else(|| {
            format!(
                "dependency `{name}` inherits from `[workspace.dependencies]`, but `{name}` is not defined there"
            )
        })?;
        let (mut base, _) = dependency(name, value, "workspace.dependencies")?;
        if base.workspace {
            return Err(format!(
                "`workspace.dependencies.{name}` cannot set `workspace = true`"
            ));
        }
        if entry.requirement.is_some()
            || entry.path.is_some()
            || entry.registry.is_some()
            || entry.git.is_some()
            || entry.package.is_some()
        {
            return Err(format!(
                "dependency `{name}` inherits from the workspace and cannot set its own source fields"
            ));
        }
        if let Some(path) = &base.path {
            let joined = workspace_dir.join(path);
            let absolute = std::path::absolute(&joined)
                .map_err(|error| format!("workspace dependency `{name}` path `{path}`: {error}"))?;
            base.path = Some(absolute.to_string_lossy().into_owned());
        }
        let features = std::mem::take(&mut entry.features);
        let default_features = entry.default_features;
        let optional = entry.optional;
        *entry = base;
        entry.features.extend(features);
        entry.features.sort();
        entry.features.dedup();
        if default_features.is_some() {
            entry.default_features = default_features;
        }
        entry.optional = entry.optional || optional;
    }
    Ok(())
}

fn missing_inherited(field: &str) -> String {
    format!("`workspace.package.{field}` is not set, but a member inherits it")
}

fn inherited_string(shared: &Table, field: &str) -> Result<String, String> {
    get_string(shared, &format!("workspace.package.{field}"), field)?
        .ok_or_else(|| missing_inherited(field))
}

fn inherited_list(shared: &Table, field: &str) -> Result<Vec<String>, String> {
    get_string_list(shared, &format!("workspace.package.{field}"), field)?
        .ok_or_else(|| missing_inherited(field))
}

fn parse_publish(table: &Table, context: &str) -> Result<Option<PublishPolicy>, String> {
    match table.get("publish") {
        None => Ok(None),
        Some(Value::Boolean(true)) => Ok(Some(PublishPolicy::Allowed)),
        Some(Value::Boolean(false)) => Ok(Some(PublishPolicy::Never)),
        Some(Value::Array(values)) => {
            let mut registries = Vec::new();
            for value in values {
                match value {
                    Value::String(registry) => registries.push(registry.clone()),
                    _ => {
                        return Err(format!(
                            "`{context}` must be a boolean or an array of strings"
                        ))
                    }
                }
            }
            Ok(Some(PublishPolicy::Registries(registries)))
        }
        Some(_) => Err(format!(
            "`{context}` must be a boolean or an array of strings"
        )),
    }
}

fn parse_readme(package: &Table) -> Result<Option<String>, String> {
    match package.get("readme") {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(Value::Boolean(false)) => Ok(None),
        Some(Value::Boolean(true)) => Err("`package.readme` cannot be `true`".into()),
        Some(_) => Err("`package.readme` must be a string or a boolean".into()),
    }
}

fn parse_optional_table(root: &Table, key: &str) -> Result<Option<Table>, String> {
    match root.get(key) {
        None => Ok(None),
        Some(Value::Table(table)) => Ok(Some(table.clone())),
        Some(_) => Err(format!("`[{key}]` must be a table")),
    }
}

fn parse_optional_target(
    root: &Table,
    key: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<TargetSpec>, String> {
    match root.get(key) {
        None => Ok(None),
        Some(Value::Table(table)) => {
            let (target, unknown) = parse_target(table)?;
            for field in unknown {
                warnings.push(format!("unknown field `{field}` in `{key}`"));
            }
            Ok(Some(target))
        }
        Some(_) => Err(format!("`[{key}]` must be a table")),
    }
}

fn parse_target_list(
    root: &Table,
    key: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<TargetSpec>, String> {
    match root.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => {
            let mut targets = Vec::new();
            for (index, value) in values.iter().enumerate() {
                let Value::Table(table) = value else {
                    return Err(format!("`{key}` entries must be tables"));
                };
                let (target, unknown) = parse_target(table)?;
                for field in unknown {
                    warnings.push(format!("unknown field `{field}` in `{key}.{index}`"));
                }
                targets.push(target);
            }
            Ok(targets)
        }
        Some(_) => Err(format!("`[{key}]` must be an array of tables")),
    }
}

fn parse_target(table: &Table) -> Result<(TargetSpec, Vec<String>), String> {
    let mut target = TargetSpec::default();

    target.name = get_string(table, "target", "name")?;
    target.path = get_string(table, "target", "path")?;
    target.crate_types = get_string_list(table, "target", "crate-type")?.unwrap_or_default();
    target.required_features =
        get_string_list(table, "target", "required-features")?.unwrap_or_default();
    target.proc_macro = get_bool(table, "target", "proc-macro")?;
    target.test = get_bool(table, "target", "test")?;
    target.bench = get_bool(table, "target", "bench")?;
    target.doctest = get_bool(table, "target", "doctest")?;
    target.harness = get_bool(table, "target", "harness")?;

    let unknown = table
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !TARGET_KEYS.contains(&key.as_str()))
        .collect();
    Ok((target, unknown))
}

fn parse_dependencies(
    root: &Table,
    key: &str,
    section: &str,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<String, Dependency>, String> {
    let table = match root.get(key) {
        None => return Ok(BTreeMap::new()),
        Some(Value::Table(table)) => table,
        Some(_) => return Err(format!("`[{section}]` must be a table")),
    };

    let mut dependencies = BTreeMap::new();
    for (name, value) in table.iter() {
        let (dependency, unknown) = dependency(name, value, section)?;
        for field in unknown {
            warnings.push(format!("unknown field `{field}` in `{section}.{name}`"));
        }
        dependencies.insert(name.to_string(), dependency);
    }
    Ok(dependencies)
}

fn parse_target_sections(
    root: &Table,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<String, TargetDeps>, String> {
    let table = match root.get("target") {
        None => return Ok(BTreeMap::new()),
        Some(Value::Table(table)) => table,
        Some(_) => return Err("`[target]` must be a table".into()),
    };

    let mut targets = BTreeMap::new();
    for (cfg, value) in table.iter() {
        let Value::Table(entry) = value else {
            return Err(format!("`target.{cfg}` must be a table"));
        };
        let mut deps = TargetDeps::default();
        for (kind, value) in entry.iter() {
            match &kind[..] {
                "dependencies" | "dev-dependencies" | "build-dependencies" => {
                    let section = format!("target.{cfg}.{kind}");
                    let mut map = BTreeMap::new();
                    let Value::Table(entries) = value else {
                        return Err(format!("`{section}` must be a table"));
                    };
                    for (name, value) in entries.iter() {
                        let (dependency, unknown) = dependency(name, value, &section)?;
                        for field in unknown {
                            warnings.push(format!("unknown field `{field}` in `{section}.{name}`"));
                        }
                        map.insert(name.to_string(), dependency);
                    }
                    match &kind[..] {
                        "dependencies" => deps.dependencies = map,
                        "dev-dependencies" => deps.dev_dependencies = map,
                        _ => deps.build_dependencies = map,
                    }
                }
                _ => warnings.push(format!("unknown field `{kind}` in `target.{cfg}`")),
            }
        }
        targets.insert(cfg.to_string(), deps);
    }
    Ok(targets)
}

fn parse_features(root: &Table) -> Result<BTreeMap<String, Vec<String>>, String> {
    let table = match root.get("features") {
        None => return Ok(BTreeMap::new()),
        Some(Value::Table(table)) => table,
        Some(_) => return Err("`[features]` must be a table".into()),
    };

    let mut features = BTreeMap::new();
    for (name, _) in table.iter() {
        let Some(values) = get_string_list(table, &format!("features.{name}"), name)? else {
            return Err(format!("`features.{name}` must be an array of strings"));
        };
        features.insert(name.to_string(), values);
    }
    Ok(features)
}

fn parse_workspace(table: &Table) -> Result<(Workspace, Vec<String>), String> {
    let mut workspace = Workspace::default();

    workspace.members = parse_member_list(table, "members")?;
    workspace.exclude = parse_member_list(table, "exclude")?;
    workspace.default_members = match table.get("default-members") {
        None => None,
        Some(_) => Some(parse_member_list(table, "default-members")?),
    };
    workspace.resolver = table
        .get("resolver")
        .map(|value| parse_resolver(value, "workspace.resolver"))
        .transpose()?;
    workspace.shared_package = table.get("package").and_then(Value::as_table).cloned();
    workspace.shared_dependencies = table.get("dependencies").and_then(Value::as_table).cloned();

    let unknown = table
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !WORKSPACE_KEYS.contains(&key.as_str()))
        .collect();
    Ok((workspace, unknown))
}

fn parse_member_list(table: &Table, key: &str) -> Result<Vec<String>, String> {
    match table.get(key) {
        None => Ok(Vec::new()),
        Some(Value::String(member)) => Ok(vec![member.clone()]),
        Some(Value::Array(values)) => {
            let mut members = Vec::new();
            for value in values {
                match value {
                    Value::String(member) => members.push(member.clone()),
                    _ => return Err(format!("`workspace.{key}` must be an array of strings")),
                }
            }
            Ok(members)
        }
        Some(_) => Err(format!(
            "`workspace.{key}` must be a string or an array of strings"
        )),
    }
}

fn parse_profiles(
    root: &Table,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<String, ProfileEntry>, String> {
    let table = match root.get("profile") {
        None => return Ok(BTreeMap::new()),
        Some(Value::Table(table)) => table,
        Some(_) => return Err("`[profile]` must be a table".into()),
    };

    let mut profiles = BTreeMap::new();
    for (name, value) in table.iter() {
        let Value::Table(entry) = value else {
            return Err(format!("`profile.{name}` must be a table"));
        };
        let (settings, unknown) = Profile::from_table(entry)?;
        let mut item = ProfileEntry {
            settings,
            package: BTreeMap::new(),
            build_override: None,
        };
        for field in unknown {
            match field.as_str() {
                "package" => {
                    let Some(table) = entry.get("package").and_then(Value::as_table) else {
                        return Err("`profile.package` must be a table".into());
                    };
                    for (package, value) in table.iter() {
                        let Value::Table(package_table) = value else {
                            return Err(format!(
                                "`profile.{name}.package.{package}` must be a table"
                            ));
                        };
                        let (profile, unknown) = Profile::from_table(package_table)?;
                        for field in unknown {
                            warnings.push(format!(
                                "unknown field `{field}` in `profile.{name}.package.{package}`"
                            ));
                        }
                        item.package.insert(package.to_string(), profile);
                    }
                }
                "build-override" => {
                    let Some(table) = entry.get("build-override").and_then(Value::as_table) else {
                        return Err("`profile.build-override` must be a table".into());
                    };
                    let (profile, unknown) = Profile::from_table(table)?;
                    for field in unknown {
                        warnings.push(format!(
                            "unknown field `{field}` in `profile.{name}.build-override`"
                        ));
                    }
                    item.build_override = Some(profile);
                }
                _ => warnings.push(format!("unknown field `{field}` in `profile.{name}`")),
            }
        }
        profiles.insert(name.to_string(), item);
    }
    Ok(profiles)
}

fn dependency(
    name: &str,
    value: &Value,
    section: &str,
) -> Result<(Dependency, Vec<String>), String> {
    let mut item = Dependency::default();

    let Value::Table(table) = value else {
        match value {
            Value::String(requirement) => {
                item.requirement = Some(requirement.clone());
                return Ok((item, Vec::new()));
            }
            _ => return Err(format!("`{section}.{name}` must be a string or a table")),
        }
    };

    let path = |key: &str| format!("{section}.{name}.{key}");

    item.requirement = get_string(table, &path("version"), "version")?;
    item.path = get_string(table, &path("path"), "path")?;
    item.registry = get_string(table, &path("registry"), "registry")?;
    item.optional = get_bool(table, &path("optional"), "optional")?.unwrap_or(false);
    item.workspace = get_bool(table, &path("workspace"), "workspace")?.unwrap_or(false);
    item.default_features = match table
        .get("default-features")
        .or_else(|| table.get("default_features"))
    {
        None => None,
        Some(Value::Boolean(value)) => Some(*value),
        Some(_) => return Err(format!("`{}` must be a boolean", path("default-features"))),
    };
    item.features = get_string_list(table, &path("features"), "features")?.unwrap_or_default();
    item.package = get_string(table, &path("package"), "package")?;

    if let Some(url) = get_string(table, &path("git"), "git")? {
        item.git = Some(GitSource {
            url,
            branch: get_string(table, &path("branch"), "branch")?,
            tag: get_string(table, &path("tag"), "tag")?,
            rev: get_string(table, &path("rev"), "rev")?,
        });
    } else if table.get("branch").is_some()
        || table.get("tag").is_some()
        || table.get("rev").is_some()
    {
        return Err(format!(
            "`{}` has `branch`, `tag`, or `rev` without `git`",
            path("git")
        ));
    }

    let unknown = table
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !DEPENDENCY_KEYS.contains(&key.as_str()))
        .collect();
    Ok((item, unknown))
}

fn unknown_keys(table: &Table, known: &[&str], context: &str, warnings: &mut Vec<String>) {
    for (key, _) in table.iter() {
        if !known.contains(&key) {
            warnings.push(format!("unknown key `{key}` in {context}"));
        }
    }
}

fn get_string(table: &Table, context: &str, key: &str) -> Result<Option<String>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("`{context}` must be a string")),
    }
}

fn get_bool(table: &Table, context: &str, key: &str) -> Result<Option<bool>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::Boolean(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("`{context}` must be a boolean")),
    }
}

fn get_string_list(table: &Table, context: &str, key: &str) -> Result<Option<Vec<String>>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::Array(values)) => {
            let mut items = Vec::new();
            for value in values {
                match value {
                    Value::String(text) => items.push(text.clone()),
                    _ => return Err(format!("`{context}` must be an array of strings")),
                }
            }
            Ok(Some(items))
        }
        Some(_) => Err(format!("`{context}` must be an array of strings")),
    }
}

fn parse_resolver(value: &Value, context: &str) -> Result<String, String> {
    let Value::String(text) = value else {
        return Err(format!("`{context}` must be `1` or `2`"));
    };
    match text.as_str() {
        "1" | "2" => Ok(text.clone()),
        _ => Err(format!("`{context}` must be `1` or `2`")),
    }
}
