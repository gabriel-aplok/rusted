use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::resolver::cfg::Cfg;
use crate::resolver::resolve::PackageId;
use crate::util::hash::{to_hex, Sha256};
use crate::util::lock::Lock;

use super::plan::{PackageInfo, Unit};
use super::rustc::Flags;
use super::store::Store;

pub(crate) struct Output {
    pub(crate) key: String,
    pub(crate) out_dir: PathBuf,
    pub(crate) cfgs: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) args: Vec<String>,
    pub(crate) metadata: BTreeMap<String, String>,
}

struct Directives {
    cfgs: Vec<String>,
    env: Vec<(String, String)>,
    args: Vec<String>,
    metadata: BTreeMap<String, String>,
    rerun_changed: Vec<String>,
    rerun_env: Vec<String>,
    warnings: Vec<String>,
}

impl Directives {
    fn new() -> Self {
        Self {
            cfgs: Vec::new(),
            env: Vec::new(),
            args: Vec::new(),
            metadata: BTreeMap::new(),
            rerun_changed: Vec::new(),
            rerun_env: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn into_output(self, base: &str, inputs: &str, out_dir: PathBuf) -> Output {
        Output {
            key: format!("{base}:{inputs}"),
            out_dir,
            cfgs: self.cfgs,
            env: self.env,
            args: self.args,
            metadata: self.metadata,
        }
    }
}

pub(crate) fn run(
    unit: &Unit,
    package: &PackageInfo,
    binary: &Path,
    build_script_key: &str,
    source_hash: &[u8; 32],
    flags: &Flags,
    cfg: &Cfg,
    profile: &str,
    jobs: usize,
    store: &Store,
    packages: &BTreeMap<PackageId, PackageInfo>,
    outputs: &BTreeMap<PackageId, Output>,
) -> Result<Output, String> {
    let dependency_hash = dependency_hash(package, packages, outputs);
    let base = base_key(
        unit,
        build_script_key,
        source_hash,
        flags,
        cfg,
        profile,
        &dependency_hash,
    );
    let base_dir = store.root().join("build-scripts").join(&base);
    let lock_path = store
        .root()
        .join("locks")
        .join(format!("build-script-{base}.lock"));
    let _lock =
        Lock::acquire(&lock_path).map_err(|error| format!("{}: {error}", lock_path.display()))?;

    if let Some((directives, stored_inputs)) = read_cache(&base_dir)? {
        let current = hash_inputs(
            &package.dir,
            &directives.rerun_changed,
            &directives.rerun_env,
        )?;
        if current == stored_inputs {
            return Ok(directives.into_output(&base, &current, base_dir.join("out")));
        }
    }

    if base_dir.exists() {
        fs::remove_dir_all(&base_dir)
            .map_err(|error| format!("{}: {error}", base_dir.display()))?;
    }
    let out_dir = base_dir.join("out");
    fs::create_dir_all(&out_dir).map_err(|error| format!("{}: {error}", out_dir.display()))?;

    let environment = environment(
        package, flags, cfg, profile, jobs, &out_dir, packages, outputs,
    );
    let output = Command::new(binary)
        .current_dir(&package.dir)
        .envs(environment)
        .output()
        .map_err(|error| {
            format!(
                "failed to run build script for `{}`: {error}",
                package.id.name
            )
        })?;

    std::io::stderr()
        .write_all(&output.stderr)
        .map_err(|error| format!("failed to write build script stderr: {error}"))?;

    if !output.status.success() {
        let _ = fs::remove_dir_all(&base_dir);
        return Err(format!(
            "build script for `{}` failed with {}",
            package.id.name, output.status
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let directives = parse_directives(&stdout, package)?;
    for warning in &directives.warnings {
        eprintln!("warning: {}: {warning}", package.id.name);
    }

    let inputs = hash_inputs(
        &package.dir,
        &directives.rerun_changed,
        &directives.rerun_env,
    )?;
    write_cache(&base_dir, &directives, &inputs)?;

    Ok(directives.into_output(&base, &inputs, out_dir))
}

fn base_key(
    unit: &Unit,
    build_script_key: &str,
    source_hash: &[u8; 32],
    flags: &Flags,
    cfg: &Cfg,
    profile: &str,
    dependency_hash: &str,
) -> String {
    let mut hasher = Sha256::new();
    field(&mut hasher, "rusted-build-script-v1");
    field(&mut hasher, build_script_key);
    field(&mut hasher, &unit.package_name);
    field(&mut hasher, &unit.package_version);
    for feature in &unit.features {
        field(&mut hasher, feature);
    }
    field(&mut hasher, &flags.cache_key_material());
    field(&mut hasher, cfg.target());
    field(&mut hasher, profile);
    field(&mut hasher, &to_hex(source_hash));
    field(&mut hasher, dependency_hash);
    to_hex(&hasher.finish())
}

fn dependency_hash(
    package: &PackageInfo,
    packages: &BTreeMap<PackageId, PackageInfo>,
    outputs: &BTreeMap<PackageId, Output>,
) -> String {
    let mut hasher = Sha256::new();
    for edge in &package.dependencies {
        let Some(target) = packages.get(&edge.package) else {
            continue;
        };
        let Some(output) = outputs.get(&edge.package) else {
            continue;
        };
        field(&mut hasher, &edge.extern_name);
        if let Some(links) = &target.manifest.links {
            field(&mut hasher, links);
        }
        for (key, value) in &output.metadata {
            field(&mut hasher, key);
            field(&mut hasher, value);
        }
    }
    to_hex(&hasher.finish())
}

#[allow(clippy::too_many_arguments)]
fn environment(
    package: &PackageInfo,
    flags: &Flags,
    cfg: &Cfg,
    profile: &str,
    jobs: usize,
    out_dir: &Path,
    packages: &BTreeMap<PackageId, PackageInfo>,
    outputs: &BTreeMap<PackageId, Output>,
) -> Vec<(String, String)> {
    let mut environment = Vec::new();
    let manifest = &package.manifest;

    environment.push(("OUT_DIR".to_string(), out_dir.display().to_string()));
    environment.push((
        "CARGO_MANIFEST_DIR".to_string(),
        package.dir.display().to_string(),
    ));
    environment.push((
        "CARGO_MANIFEST_PATH".to_string(),
        package.manifest_path.display().to_string(),
    ));
    environment.push(("CARGO_PKG_NAME".to_string(), manifest.name.clone()));
    environment.push(("CARGO_PKG_VERSION".to_string(), manifest.version.clone()));

    if let Ok(version) = crate::resolver::semver::Version::parse(&manifest.version) {
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

    for (key, value) in cfg.environment() {
        environment.push((key, value));
    }
    for feature in &package.features {
        environment.push((
            format!(
                "CARGO_FEATURE_{}",
                feature.to_ascii_uppercase().replace('-', "_")
            ),
            "1".to_string(),
        ));
    }

    environment.push(("HOST".to_string(), cfg.target().to_string()));
    environment.push(("TARGET".to_string(), cfg.target().to_string()));
    environment.push((
        "PROFILE".to_string(),
        if profile == "dev" {
            "debug".to_string()
        } else {
            profile.to_string()
        },
    ));
    environment.push(("OPT_LEVEL".to_string(), flags.opt_level().to_string()));
    environment.push((
        "DEBUG".to_string(),
        if flags.debug_enabled() {
            "true".to_string()
        } else {
            "false".to_string()
        },
    ));
    environment.push(("NUM_JOBS".to_string(), jobs.to_string()));
    if let Ok(current) = std::env::current_exe() {
        let path = current.display().to_string();
        environment.push(("CARGO".to_string(), path.clone()));
        environment.push(("RUSTED".to_string(), path));
    }
    environment.push((
        "RUSTC".to_string(),
        super::rustc_program().to_string_lossy().into_owned(),
    ));
    environment.push(("RUSTDOC".to_string(), "rustdoc".to_string()));

    if let Some(links) = &package.manifest.links {
        environment.push(("CARGO_MANIFEST_LINKS".to_string(), links.clone()));
    }

    for edge in &package.dependencies {
        let Some(target) = packages.get(&edge.package) else {
            continue;
        };
        let Some(links) = &target.manifest.links else {
            continue;
        };
        let Some(output) = outputs.get(&edge.package) else {
            continue;
        };
        let prefix = links.to_ascii_uppercase().replace('-', "_");
        for (key, value) in &output.metadata {
            environment.push((
                format!(
                    "DEP_{prefix}_{}",
                    key.to_ascii_uppercase().replace('-', "_")
                ),
                value.clone(),
            ));
        }
    }

    environment
}

fn parse_directives(stdout: &str, package: &PackageInfo) -> Result<Directives, String> {
    let mut directives = Directives::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(directive) = line
            .strip_prefix("cargo::")
            .or_else(|| line.strip_prefix("cargo:"))
        else {
            continue;
        };
        let Some((key, value)) = directive.split_once('=') else {
            directives
                .warnings
                .push(format!("unknown build directive `{directive}`"));
            continue;
        };
        match key {
            "rustc-cfg" => directives.cfgs.push(value.to_string()),
            "rustc-env" => match value.split_once('=') {
                Some((name, value)) => directives.env.push((name.to_string(), value.to_string())),
                None => {
                    return Err(format!(
                        "build script for `{}` emitted invalid rustc-env `{value}`",
                        package.id.name
                    ))
                }
            },
            "rustc-link-lib" => {
                directives.args.push("-l".to_string());
                directives.args.push(value.to_string());
            }
            "rustc-link-search" => {
                directives.args.push("-L".to_string());
                directives.args.push(value.to_string());
            }
            "rustc-link-arg" | "rustc-cdylib-link-arg" => {
                directives.args.push("-C".to_string());
                directives.args.push(format!("link-arg={value}"));
            }
            "rustc-check-cfg" => {
                directives.args.push(format!("--check-cfg={value}"));
            }
            "rustc-flags" => {
                let mut tokens = value.split_whitespace();
                while let Some(token) = tokens.next() {
                    match token {
                        "-l" | "-L" => {
                            if let Some(argument) = tokens.next() {
                                directives.args.push(token.to_string());
                                directives.args.push(argument.to_string());
                            }
                        }
                        other => directives
                            .warnings
                            .push(format!("unsupported rustc-flags token `{other}`")),
                    }
                }
            }
            "metadata" => match value.split_once('=') {
                Some((name, value)) => {
                    directives
                        .metadata
                        .insert(name.to_string(), value.to_string());
                }
                None => directives
                    .warnings
                    .push(format!("invalid metadata `{value}`")),
            },
            "warning" => directives.warnings.push(value.to_string()),
            "rerun-if-changed" => directives.rerun_changed.push(value.to_string()),
            "rerun-if-env-changed" => directives.rerun_env.push(value.to_string()),
            _ => directives
                .warnings
                .push(format!("unknown build directive `{key}`")),
        }
    }

    Ok(directives)
}

fn hash_inputs(
    package_dir: &Path,
    changed: &[String],
    env_names: &[String],
) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for path in changed {
        field(&mut hasher, "changed");
        field(&mut hasher, path);
        let full = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            package_dir.join(path)
        };
        hash_path(&full, &mut hasher)?;
    }
    for name in env_names {
        field(&mut hasher, "env");
        field(&mut hasher, name);
        match std::env::var(name) {
            Ok(value) => field(&mut hasher, &value),
            Err(_) => field(&mut hasher, "unset"),
        }
    }
    Ok(to_hex(&hasher.finish()))
}

fn hash_path(path: &Path, hasher: &mut Sha256) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            field(hasher, "missing");
            Ok(())
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
        Ok(metadata) if metadata.is_dir() => {
            field(hasher, "dir");
            let mut entries: Vec<PathBuf> = fs::read_dir(path)
                .map_err(|error| format!("{}: {error}", path.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .collect();
            entries.sort();
            for entry in entries {
                field(
                    hasher,
                    &entry.file_name().unwrap_or_default().to_string_lossy(),
                );
                hash_path(&entry, hasher)?;
            }
            Ok(())
        }
        Ok(_) => {
            field(hasher, "file");
            let mut file =
                fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
            let mut buffer = [0u8; crate::util::READ_BUFFER];
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            Ok(())
        }
    }
}

fn read_cache(directory: &Path) -> Result<Option<(Directives, String)>, String> {
    if !directory.join("complete").is_file() || !directory.join("out").is_dir() {
        return Ok(None);
    }
    let directives_path = directory.join("directives");
    let inputs_path = directory.join("inputs");
    let directives = match fs::read_to_string(&directives_path) {
        Ok(text) => parse_cached(&text)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", directives_path.display())),
    };
    let inputs = match fs::read_to_string(&inputs_path) {
        Ok(text) => text.trim().to_string(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", inputs_path.display())),
    };
    Ok(Some((directives, inputs)))
}

fn write_cache(directory: &Path, directives: &Directives, inputs: &str) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    let mut text = String::new();
    for value in &directives.cfgs {
        text.push_str(&format!("cfg\t{}\n", escape(value)));
    }
    for (key, value) in &directives.env {
        text.push_str(&format!("env\t{}\t{}\n", escape(key), escape(value)));
    }
    for argument in &directives.args {
        text.push_str(&format!("arg\t{}\n", escape(argument)));
    }
    for (key, value) in &directives.metadata {
        text.push_str(&format!("metadata\t{}\t{}\n", escape(key), escape(value)));
    }
    for path in &directives.rerun_changed {
        text.push_str(&format!("changed\t{}\n", escape(path)));
    }
    for name in &directives.rerun_env {
        text.push_str(&format!("env-change\t{}\n", escape(name)));
    }
    fs::write(directory.join("directives"), text)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    fs::write(directory.join("inputs"), inputs)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    fs::write(directory.join("complete"), b"")
        .map_err(|error| format!("{}: {error}", directory.display()))
}

fn parse_cached(text: &str) -> Result<Directives, String> {
    let mut directives = Directives::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let Some(kind) = fields.next() else {
            continue;
        };
        match kind {
            "cfg" => directives.cfgs.push(unescape(fields.next().unwrap_or(""))?),
            "env" => {
                let key = unescape(fields.next().unwrap_or(""))?;
                let value = unescape(fields.next().unwrap_or(""))?;
                directives.env.push((key, value));
            }
            "arg" => directives.args.push(unescape(fields.next().unwrap_or(""))?),
            "metadata" => {
                let key = unescape(fields.next().unwrap_or(""))?;
                let value = unescape(fields.next().unwrap_or(""))?;
                directives.metadata.insert(key, value);
            }
            "changed" => directives
                .rerun_changed
                .push(unescape(fields.next().unwrap_or(""))?),
            "env-change" => directives
                .rerun_env
                .push(unescape(fields.next().unwrap_or(""))?),
            _ => {}
        }
    }
    Ok(directives)
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            character => out.push(character),
        }
    }
    out
}

fn unescape(value: &str) -> Result<String, String> {
    let mut out = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            _ => return Err("invalid escape in build directive cache".to_string()),
        }
    }
    Ok(out)
}

fn field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.as_bytes());
    hasher.update(&[0]);
}

#[cfg(test)]
mod tests {
    use super::{escape, unescape};

    #[test]
    fn escapes_and_unescapes() {
        let text = "a\tb\nc\\d";
        assert_eq!(unescape(&escape(text)).unwrap(), text);
    }
}
