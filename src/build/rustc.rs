use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::config::Config;
use crate::manifest::ProfileEntry;
use crate::profile::{DebugInfo, Linker, Lto, OptLevel, Panic, Profile, Strip};

use super::plan::{Unit, UnitKind};
use super::{MessageFormat, Mode};

pub(crate) struct Diagnostics {
    pub(crate) stderr: Vec<u8>,
    pub(crate) warnings: usize,
}

pub(crate) enum CompileOutcome {
    Success(Diagnostics),
    Failure {
        message: String,
        diagnostics: Diagnostics,
    },
}

#[derive(Clone)]
pub(crate) struct Flags {
    opt_level: &'static str,
    debug_info: &'static str,
    strip: Option<&'static str>,
    codegen_units: u32,
    debug_assertions: bool,
    overflow_checks: bool,
    lto: Option<&'static str>,
    panic: Option<&'static str>,
    incremental: bool,
    linker_choice: Option<Linker>,
    linker: Option<String>,
}

impl Flags {
    pub(crate) fn opt_level(&self) -> &'static str {
        self.opt_level
    }

    pub(crate) fn debug_enabled(&self) -> bool {
        self.debug_info != "0"
    }

    pub(crate) fn incremental(&self) -> bool {
        self.incremental
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            opt_level: "0",
            debug_info: "0",
            strip: Some("debuginfo"),
            codegen_units: 16,
            debug_assertions: true,
            overflow_checks: true,
            lto: None,
            panic: None,
            incremental: false,
            linker_choice: None,
            linker: None,
        }
    }

    pub(crate) fn cache_key_material(&self) -> String {
        format!(
            "opt={};debug={};strip={:?};cgu={};debug-assertions={};overflow-checks={};lto={:?};panic={:?};incremental={};linker={:?}",
            self.opt_level,
            self.debug_info,
            self.strip,
            self.codegen_units,
            self.debug_assertions,
            self.overflow_checks,
            self.lto,
            self.panic,
            self.incremental,
            self.linker
        )
    }

    pub(crate) fn resolve(
        config: &Config,
        profiles: &BTreeMap<String, ProfileEntry>,
        profile_name: &str,
        force_incremental: bool,
        package_name: &str,
        kind: UnitKind,
    ) -> Result<Self, String> {
        let release = profile_name == "release";
        let mut flags = if release {
            Self {
                opt_level: "3",
                debug_info: "0",
                strip: Some("debuginfo"),
                codegen_units: 16,
                debug_assertions: false,
                overflow_checks: false,
                lto: None,
                panic: None,
                incremental: false,
                linker_choice: None,
                linker: None,
            }
        } else {
            Self {
                opt_level: "0",
                debug_info: "0",
                strip: Some("debuginfo"),
                codegen_units: 16,
                debug_assertions: true,
                overflow_checks: true,
                lto: None,
                panic: None,
                incremental: false,
                linker_choice: None,
                linker: None,
            }
        };
        flags.linker_choice = config.linker.clone();
        flags.incremental = profile_name == "dev";

        if let Some(profile) = config.profiles.get(profile_name) {
            flags.merge(profile);
        }
        if let Some(entry) = profiles.get(profile_name) {
            flags.merge(&entry.settings);
            if let Some(settings) = entry.package.get("*") {
                flags.merge(settings);
            }
            if let Some(settings) = entry.package.get(package_name) {
                flags.merge(settings);
            }
            if kind == UnitKind::BuildScript || kind == UnitKind::ProcMacro {
                if let Some(settings) = &entry.build_override {
                    flags.merge(settings);
                }
            }
        }
        if force_incremental {
            flags.incremental = true;
        }
        flags.linker = resolve_linker(flags.linker_choice.as_ref())?;
        Ok(flags)
    }

    fn merge(&mut self, profile: &Profile) {
        if let Some(level) = profile.opt_level {
            self.opt_level = opt_level(level);
        }
        if let Some(debug) = profile.debug {
            self.debug_info = debug_info(debug);
        }
        if let Some(strip) = profile.strip {
            self.strip = match strip {
                Strip::None => None,
                Strip::Debuginfo => Some("debuginfo"),
                Strip::Symbols => Some("symbols"),
            };
        }
        if let Some(units) = profile.codegen_units {
            self.codegen_units = units;
        }
        if let Some(value) = profile.debug_assertions {
            self.debug_assertions = value;
        }
        if let Some(value) = profile.overflow_checks {
            self.overflow_checks = value;
        }
        if let Some(lto) = profile.lto {
            self.lto = match lto {
                Lto::Off => None,
                Lto::Thin => Some("thin"),
                Lto::Fat => Some("fat"),
                Lto::ThinLocal => Some("thin-local"),
            };
        }
        if let Some(panic) = profile.panic {
            self.panic = Some(match panic {
                Panic::Unwind => "unwind",
                Panic::Abort => "abort",
            });
        }
        if let Some(value) = profile.incremental {
            self.incremental = value;
        }
        if let Some(linker) = &profile.linker {
            self.linker_choice = Some(linker.clone());
        }
    }

    fn apply(&self, command: &mut Command) {
        command
            .arg("-C")
            .arg(format!("opt-level={}", self.opt_level));
        command
            .arg("-C")
            .arg(format!("debuginfo={}", self.debug_info));
        if let Some(strip) = self.strip {
            command.arg("-C").arg(format!("strip={strip}"));
        }
        command
            .arg("-C")
            .arg(format!("codegen-units={}", self.codegen_units));
        command.arg("-C").arg(format!(
            "debug-assertions={}",
            if self.debug_assertions { "yes" } else { "no" }
        ));
        command.arg("-C").arg(format!(
            "overflow-checks={}",
            if self.overflow_checks { "yes" } else { "no" }
        ));
        if let Some(lto) = self.lto {
            command.arg("-C").arg(format!("lto={lto}"));
        } else {
            command.arg("-C").arg("embed-bitcode=no");
        }
        if let Some(panic) = self.panic {
            command.arg("-C").arg(format!("panic={panic}"));
        }
        if let Some(linker) = &self.linker {
            command.arg("-C").arg(format!("linker={linker}"));
        }
    }
}

pub(crate) fn program() -> std::ffi::OsString {
    super::rustc_program()
}

fn resolve_linker(choice: Option<&Linker>) -> Result<Option<String>, String> {
    match choice {
        Some(Linker::System) => Ok(None),
        Some(Linker::Path(path)) => {
            let absolute = std::path::absolute(path)
                .map_err(|error| format!("linker path `{path}`: {error}"))?;
            Ok(Some(absolute.display().to_string()))
        }
        Some(Linker::Mold) => find_linker(&["mold", "ld.mold"]).map(Some).ok_or_else(|| {
            "`linker = \"mold\"` was requested, but mold is not on PATH".to_string()
        }),
        Some(Linker::Lld) => find_linker(&["ld.lld", "lld"])
            .map(Some)
            .ok_or_else(|| "`linker = \"lld\"` was requested, but lld is not on PATH".to_string()),
        None => Ok(find_linker(&["mold", "ld.mold"]).or_else(|| find_linker(&["ld.lld", "lld"]))),
    }
}

fn find_linker(names: &[&str]) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for name in names {
            let candidate = directory.join(name);
            if is_executable(&candidate) {
                return Some(candidate.display().to_string());
            }
        }
    }
    None
}

pub(crate) fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

pub(crate) fn compile(
    program: &std::ffi::OsStr,
    unit_index: usize,
    unit: &Unit,
    unit_metadata: &str,
    flags: &Flags,
    mode: Mode,
    externs: &[(String, PathBuf)],
    search_dirs: &[PathBuf],
    cfgs: &[String],
    env: &[(String, String)],
    extra_args: &[String],
    out_dir: Option<&Path>,
    output: &Path,
    incremental_dir: Option<&Path>,
    metadata_output: Option<&Path>,
    metadata_store: Option<&Path>,
    metadata_ready: Option<&mpsc::Sender<(usize, PathBuf)>>,
    format: MessageFormat,
    verbose: bool,
) -> Result<CompileOutcome, String> {
    let mut command = Command::new(program);

    command
        .arg("-C")
        .arg(format!("metadata={unit_metadata}"))
        .arg("--edition")
        .arg(unit.edition.as_str())
        .arg("--crate-name")
        .arg(&unit.crate_name);
    match unit.kind {
        UnitKind::Lib => {
            command.arg("--crate-type").arg("lib");
        }
        UnitKind::Bin | UnitKind::Example | UnitKind::BuildScript => {
            command.arg("--crate-type").arg("bin");
        }
        UnitKind::Test | UnitKind::Bench => {
            command.arg("--test");
        }
        UnitKind::ProcMacro => {
            command.arg("--crate-type").arg("proc-macro");
            // rustc supplies the proc_macro crate from the sysroot when it is
            // reached through the extern prelude with no explicit path.
            command.arg("--extern").arg("proc_macro");
        }
    }

    flags.apply(&mut command);
    if let Some(directory) = incremental_dir {
        command
            .arg("-C")
            .arg(format!("incremental={}", directory.display()));
    }
    for directory in search_dirs {
        command
            .arg("-L")
            .arg(format!("dependency={}", directory.display()));
    }
    for (name, path) in externs {
        command
            .arg("--extern")
            .arg(format!("{name}={}", path.display()));
    }
    if unit.cap_lints {
        command.arg("--cap-lints").arg("allow");
    }
    for feature in &unit.features {
        command.arg("--cfg").arg(format!("feature=\"{feature}\""));
    }
    for setting in cfgs {
        command.arg("--cfg").arg(setting);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    if let Some(out_dir) = out_dir {
        command.env("OUT_DIR", out_dir);
    }
    for argument in extra_args {
        command.arg(argument);
    }

    match mode {
        Mode::Check if unit.kind != UnitKind::BuildScript && unit.kind != UnitKind::ProcMacro => {
            command.arg("--emit=metadata");
            command.arg("-o").arg(output);
        }
        Mode::Build if metadata_output.is_some() => {
            let metadata = metadata_output.expect("metadata output is set");
            command.arg("--emit").arg(format!(
                "metadata={},link={}",
                metadata.display(),
                output.display()
            ));
        }
        Mode::Build | Mode::Check | Mode::Test => {
            command.arg("-o").arg(output);
        }
    }

    match format {
        MessageFormat::Human => {}
        MessageFormat::Short => {
            command.arg("--error-format=short");
        }
        MessageFormat::Json => {
            command.arg("--error-format=json");
            command.arg("--color").arg("never");
        }
    }

    command.arg(&unit.source);
    command.current_dir(&unit.package_dir);

    if verbose {
        eprintln!("{}", render(&command));
    }
    log_invocation(&command)?;

    let output = match (metadata_output, metadata_store, metadata_ready) {
        (Some(metadata), Some(store), Some(sender)) => {
            run_pipelined(command, metadata, store, sender, unit_index, unit)?
        }
        _ => command
            .output()
            .map_err(|error| format!("failed to run rustc for `{}`: {error}", unit.package_name))?,
    };
    let warnings = match format {
        MessageFormat::Json => count_json_warnings(&output.stderr),
        MessageFormat::Human | MessageFormat::Short => count_text_warnings(&output.stderr),
    };
    let diagnostics = Diagnostics {
        stderr: output.stderr,
        warnings,
    };

    if !output.status.success() {
        return Ok(CompileOutcome::Failure {
            message: format!(
                "could not compile `{}` (exit status {})",
                unit.package_name, output.status
            ),
            diagnostics,
        });
    }

    Ok(CompileOutcome::Success(diagnostics))
}

fn run_pipelined(
    mut command: Command,
    metadata: &Path,
    metadata_store: &Path,
    sender: &mpsc::Sender<(usize, PathBuf)>,
    unit_index: usize,
    unit: &Unit,
) -> Result<std::process::Output, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to run rustc for `{}`: {error}", unit.package_name))?;
    let mut announced = false;
    let status = loop {
        if !announced {
            if let Ok(first) = std::fs::metadata(metadata) {
                if first.len() > 0 {
                    std::thread::sleep(Duration::from_millis(2));
                    if let Ok(second) = std::fs::metadata(metadata) {
                        if second.len() == first.len() {
                            if let Some(parent) = metadata_store.parent() {
                                std::fs::create_dir_all(parent)
                                    .map_err(|error| format!("{}: {error}", parent.display()))?;
                            }
                            let temp_path = metadata_store
                                .with_extension(format!("{}.tmp", std::process::id()));
                            std::fs::copy(metadata, &temp_path)
                                .map_err(|error| format!("{}: {error}", temp_path.display()))?;
                            std::fs::rename(&temp_path, metadata_store).map_err(|error| {
                                format!("{}: {error}", metadata_store.display())
                            })?;
                            let _ = sender.send((unit_index, metadata_store.to_path_buf()));
                            announced = true;
                        }
                    }
                }
            }
        }
        match child
            .try_wait()
            .map_err(|error| format!("failed to wait for rustc: {error}"))?
        {
            Some(status) => break status,
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_end(&mut stdout)
            .map_err(|error| format!("failed to read rustc stdout: {error}"))?;
    }
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_end(&mut stderr)
            .map_err(|error| format!("failed to read rustc stderr: {error}"))?;
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn count_json_warnings(stderr: &[u8]) -> usize {
    stderr
        .split(|byte| *byte == b'\n')
        .filter(|line| {
            let Ok(text) = std::str::from_utf8(line) else {
                return false;
            };
            let Ok(value) = crate::util::json::parse(text) else {
                return false;
            };
            if value
                .get("$message_type")
                .and_then(crate::util::json::Value::as_str)
                != Some("diagnostic")
                || value
                    .get("level")
                    .and_then(crate::util::json::Value::as_str)
                    != Some("warning")
            {
                return false;
            }
            let message = value
                .get("message")
                .and_then(crate::util::json::Value::as_str)
                .unwrap_or("");
            !is_text_summary(message)
        })
        .count()
}

fn is_text_summary(text: &str) -> bool {
    let text = text.trim();
    let text = text
        .strip_prefix("warning: ")
        .or_else(|| text.rsplit(": warning: ").next())
        .unwrap_or(text);
    let Some((number, rest)) = text.split_once(' ') else {
        return false;
    };
    number.parse::<usize>().is_ok()
        && (rest.trim() == "warning emitted" || rest.trim() == "warnings emitted")
}

fn count_text_warnings(stderr: &[u8]) -> usize {
    stderr
        .split(|byte| *byte == b'\n')
        .filter(|line| {
            let Ok(text) = std::str::from_utf8(line) else {
                return false;
            };
            let text = text.trim_start();
            (text.starts_with("warning:") || text.contains(": warning:")) && !is_text_summary(text)
        })
        .count()
}

fn log_invocation(command: &Command) -> Result<(), String> {
    let Some(path) = std::env::var_os("RUSTED_RUSTC_LOG") else {
        return Ok(());
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("{}: {error}", std::path::Path::new(&path).display()))?;
    use std::io::Write;
    writeln!(file, "{}", render(command))
        .map_err(|error| format!("{}: {error}", std::path::Path::new(&path).display()))
}

fn render(command: &Command) -> String {
    let mut text = command.get_program().to_string_lossy().into_owned();
    for argument in command.get_args() {
        text.push(' ');
        text.push_str(&argument.to_string_lossy());
    }
    text
}

fn opt_level(level: OptLevel) -> &'static str {
    match level {
        OptLevel::O0 => "0",
        OptLevel::O1 => "1",
        OptLevel::O2 => "2",
        OptLevel::O3 => "3",
        OptLevel::SizeS => "s",
        OptLevel::SizeZ => "z",
    }
}

fn debug_info(debug: DebugInfo) -> &'static str {
    match debug {
        DebugInfo::None => "0",
        DebugInfo::LineTablesOnly => "line-tables-only",
        DebugInfo::Full => "2",
    }
}

#[cfg(test)]
mod tests {
    use super::Flags;

    #[test]
    fn linker_changes_the_cache_key() {
        let left = Flags::for_test();
        let mut right = Flags::for_test();
        right.linker = Some("/usr/bin/mold".to_string());
        assert_ne!(left.cache_key_material(), right.cache_key_material());
    }
}
