use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::profile::{Linker, Profile};
use crate::toml::{self, Table, Value};

const CONFIG_DIR_ENV: &str = "XDG_CONFIG_HOME";
const CACHE_DIR_ENV: &str = "XDG_CACHE_HOME";
const HOME_ENV: &str = "HOME";
const USERPROFILE_ENV: &str = "USERPROFILE";
const APPDATA_ENV: &str = "APPDATA";
const LOCALAPPDATA_ENV: &str = "LOCALAPPDATA";
const MANIFEST_ENV: &str = "RUSTED_MANIFEST";
const JOBS_ENV: &str = "RUSTED_JOBS";
const OFFLINE_ENV: &str = "RUSTED_OFFLINE";
const CUSTOM_CACHE_DIR_ENV: &str = "RUSTED_CACHE_DIR";
const TARGET_DIR_ENV: &str = "RUSTED_TARGET_DIR";
const CONFIG_FILE: &str = "rusted/config.toml";
const CACHE_DIR_NAME: &str = "rusted";
const DEFAULT_TARGET_DIR: &str = "target";

pub(crate) const MAX_PARALLELISM: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManifestChoice {
    Auto,
    Cargo,
    Rusted,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Config {
    pub(crate) manifest: ManifestChoice,
    pub(crate) cache_dir: Option<PathBuf>,
    pub(crate) jobs: Option<usize>,
    pub(crate) offline: bool,
    pub(crate) target_dir: PathBuf,
    pub(crate) registry_index: Option<String>,
    pub(crate) profiles: BTreeMap<String, Profile>,
    pub(crate) linker: Option<Linker>,
}

impl Config {
    pub(crate) fn load() -> Result<Self, String> {
        let mut config = Self::fresh();
        let path = config_path()?;

        if path.exists() {
            let input = std::fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let table =
                toml::parse(&input).map_err(|error| format!("{}: {error}", path.display()))?;
            let mut warnings = Vec::new();
            config.receive(&table, &path, &mut warnings)?;
        }

        config.override_from_env()?;
        Ok(config)
    }

    fn fresh() -> Self {
        Self {
            manifest: ManifestChoice::Auto,
            cache_dir: None,
            jobs: None,
            offline: false,
            target_dir: PathBuf::from(DEFAULT_TARGET_DIR),
            registry_index: None,
            profiles: BTreeMap::new(),
            linker: None,
        }
    }

    fn receive(
        &mut self,
        table: &Table,
        source: &Path,
        warnings: &mut Vec<String>,
    ) -> Result<(), String> {
        for (key, value) in table.iter() {
            match key {
                "manifest" => {
                    let Value::String(text) = value else {
                        return Err("`manifest` must be a string".into());
                    };
                    self.manifest = parse_choice(text, "manifest")?;
                }
                "cache_dir" => self.cache_dir = Some(parse_path(value, "cache_dir")?),
                "jobs" => self.jobs = Some(parse_jobs(value, "jobs")?),
                "offline" => self.offline = parse_bool(value, "offline")?,
                "target_dir" => self.target_dir = parse_path(value, "target_dir")?,
                "profile" => self.receive_profiles(value, warnings)?,
                "registry" => self.receive_registry(value, warnings)?,
                "linker" => {
                    let Value::String(text) = value else {
                        return Err("`linker` must be `system`, `lld`, `mold`, or a path".into());
                    };
                    self.linker = Some(Linker::parse(text));
                }
                _ => warnings.push(format!("unknown key `{key}` in {source:?}")),
            }
        }
        Ok(())
    }

    fn receive_profiles(
        &mut self,
        value: &Value,
        warnings: &mut Vec<String>,
    ) -> Result<(), String> {
        let Value::Table(table) = value else {
            return Err("`profile` must be a table".into());
        };

        for (name, value) in table.iter() {
            let Value::Table(entry) = value else {
                return Err(format!("`profile.{name}` must be a table"));
            };
            let (profile, unknown) = Profile::from_table(entry)?;
            for field in unknown {
                warnings.push(format!(
                    "unknown field `{field}` in `profile.{name}` of the global config, package overrides live in manifests"
                ));
            }
            self.profiles.insert(name.to_string(), profile);
        }
        Ok(())
    }

    fn receive_registry(
        &mut self,
        value: &Value,
        warnings: &mut Vec<String>,
    ) -> Result<(), String> {
        let Value::Table(table) = value else {
            return Err("`registry` must be a table".into());
        };

        for (key, value) in table.iter() {
            match key {
                "index" => {
                    let Value::String(text) = value else {
                        return Err("`registry.index` must be a string".into());
                    };
                    self.registry_index = Some(text.clone());
                }
                _ => warnings.push(format!("unknown key `{key}` in `registry`")),
            }
        }
        Ok(())
    }

    fn override_from_env(&mut self) -> Result<(), String> {
        if let Some(text) = env_value(MANIFEST_ENV)? {
            self.manifest = parse_choice(&text, MANIFEST_ENV)?;
        }
        if let Some(text) = env_value(JOBS_ENV)? {
            self.jobs = Some(
                text.parse()
                    .ok()
                    .filter(|jobs| *jobs >= 1 && *jobs <= MAX_PARALLELISM)
                    .ok_or(format!(
                        "`{JOBS_ENV}` must be an integer from 1 to {MAX_PARALLELISM}, got `{text}`"
                    ))?,
            );
        }
        if let Some(text) = env_value(OFFLINE_ENV)? {
            match text.as_str() {
                "1" | "true" => self.offline = true,
                "0" | "false" => self.offline = false,
                _ => {
                    return Err(format!(
                        "`{OFFLINE_ENV}` must be `1`, `0`, `true`, or `false`"
                    ))
                }
            }
        }
        if let Some(text) = env_value(CUSTOM_CACHE_DIR_ENV)? {
            self.cache_dir = Some(PathBuf::from(text));
        }
        if let Some(text) = env_value(TARGET_DIR_ENV)? {
            self.target_dir = PathBuf::from(text);
        }
        Ok(())
    }
}

pub(crate) fn config_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join(CONFIG_FILE))
}

pub(crate) fn cache_root(config: &Config) -> Result<PathBuf, String> {
    match &config.cache_dir {
        Some(path) if path.is_absolute() => Ok(path.clone()),
        Some(path) => {
            let current = std::env::current_dir()
                .map_err(|error| format!("cannot read the current directory: {error}"))?;
            Ok(current.join(path))
        }
        None => cache_home(),
    }
}

pub(crate) fn cache_home() -> Result<PathBuf, String> {
    let base = match std::env::var_os(CACHE_DIR_ENV) {
        Some(dir) => PathBuf::from(dir),
        None if cfg!(windows) => match std::env::var_os(LOCALAPPDATA_ENV) {
            Some(dir) => PathBuf::from(dir),
            None => home()?.join(".cache"),
        },
        None => home()?.join(".cache"),
    };
    Ok(base.join(CACHE_DIR_NAME))
}

fn config_dir() -> Result<PathBuf, String> {
    let base = match std::env::var_os(CONFIG_DIR_ENV) {
        Some(dir) => PathBuf::from(dir),
        None if cfg!(windows) => match std::env::var_os(APPDATA_ENV) {
            Some(dir) => PathBuf::from(dir),
            None => home()?.join(".config"),
        },
        None => home()?.join(".config"),
    };
    Ok(base)
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os(HOME_ENV)
        .or_else(|| std::env::var_os(USERPROFILE_ENV))
        .map(PathBuf::from)
        .ok_or_else(|| format!("neither `{HOME_ENV}` nor `{USERPROFILE_ENV}` is set"))
}

fn env_value(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(text) if !text.trim().is_empty() => Ok(Some(text)),
        Ok(_) => Ok(None),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("{name}: {error}")),
    }
}

fn parse_bool(value: &Value, key: &str) -> Result<bool, String> {
    let Value::Boolean(value) = value else {
        return Err(format!("`{key}` must be a boolean"));
    };
    Ok(*value)
}

fn parse_path(value: &Value, key: &str) -> Result<PathBuf, String> {
    let Value::String(text) = value else {
        return Err(format!("`{key}` must be a string"));
    };
    Ok(PathBuf::from(text))
}

fn parse_jobs(value: &Value, key: &str) -> Result<usize, String> {
    let Value::Integer(jobs) = value else {
        return Err(format!(
            "`{key}` must be an integer from 1 to {MAX_PARALLELISM}"
        ));
    };
    usize::try_from(*jobs)
        .ok()
        .filter(|jobs| *jobs >= 1 && *jobs <= MAX_PARALLELISM)
        .ok_or(format!(
            "`{key}` must be an integer from 1 to {MAX_PARALLELISM}"
        ))
}

fn parse_choice(text: &str, key: &str) -> Result<ManifestChoice, String> {
    match text {
        "auto" => Ok(ManifestChoice::Auto),
        "cargo" => Ok(ManifestChoice::Cargo),
        "rusted" => Ok(ManifestChoice::Rusted),
        _ => Err(format!("`{key}` must be `auto`, `cargo`, or `rusted`")),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{cache_home, config_path, Config, ManifestChoice};
    use crate::profile::{Lto, OptLevel};

    fn table(input: &str) -> crate::toml::Table {
        crate::toml::parse(input).unwrap()
    }

    fn config(input: &str) -> (Config, Vec<String>) {
        let table = table(input);
        let mut config = Config::fresh();
        let mut warnings = Vec::new();
        config
            .receive(&table, Path::new("config.toml"), &mut warnings)
            .unwrap();
        (config, warnings)
    }

    #[test]
    fn fresh_defaults() {
        let config = Config::fresh();
        assert_eq!(config.manifest, ManifestChoice::Auto);
        assert!(!config.offline);
        assert_eq!(config.target_dir, std::path::PathBuf::from("target"));
        assert_eq!(config.jobs, None);
        assert!(config.cache_dir.is_none());
        assert_eq!(config.profiles.len(), 0);
    }

    #[test]
    fn parses_config_values() {
        let (config, warnings) = config(
            "manifest = \"cargo\"\njobs = 4\noffline = true\ntarget_dir = \".rusted\"\ncache_dir = \"/tmp/rusted-store\"\n\n[profile.release]\nopt-level = 3\nlto = \"thin\"\n",
        );

        assert_eq!(config.manifest, ManifestChoice::Cargo);
        assert_eq!(config.jobs, Some(4));
        assert!(config.offline);
        assert_eq!(config.target_dir, std::path::PathBuf::from(".rusted"));
        assert_eq!(
            config.cache_dir,
            Some(std::path::PathBuf::from("/tmp/rusted-store"))
        );

        let release = config.profiles.get("release").unwrap();
        assert_eq!(release.opt_level, Some(OptLevel::O3));
        assert_eq!(release.lto, Some(Lto::Thin));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn parses_registry_index() {
        let (config, warnings) = config("[registry]\nindex = \"https://index.crates.io\"\n");
        assert_eq!(
            config.registry_index.as_deref(),
            Some("https://index.crates.io")
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn unknown_keys_become_warnings() {
        let (_, warnings) = config("nonsense = 1\n");
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("unknown key `nonsense`")));
    }

    #[test]
    fn invalid_values_fail() {
        let mut config = Config::fresh();
        let mut warnings = Vec::new();
        let error = config
            .receive(&table("jobs = 0"), Path::new("config.toml"), &mut warnings)
            .unwrap_err();
        assert!(error.contains("`jobs` must be"), "{error}");

        let error = config
            .receive(
                &table("manifest = \"nope\""),
                Path::new("config.toml"),
                &mut warnings,
            )
            .unwrap_err();
        assert!(error.contains("`manifest` must be"), "{error}");

        let error = config
            .receive(
                &table("offline = \"yes\""),
                Path::new("config.toml"),
                &mut warnings,
            )
            .unwrap_err();
        assert!(error.contains("`offline` must be"), "{error}");
    }

    #[test]
    fn loads_from_disk_with_env_overrides() {
        let vars = [
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "RUSTED_MANIFEST",
            "RUSTED_JOBS",
            "RUSTED_OFFLINE",
            "RUSTED_TARGET_DIR",
            "RUSTED_CACHE_DIR",
        ];
        let originals: Vec<(String, Option<std::ffi::OsString>)> = vars
            .iter()
            .map(|name| (name.to_string(), std::env::var_os(name)))
            .collect();

        let config_root =
            std::env::temp_dir().join(format!("rusted-config-{}", std::process::id()));
        let cache_root = std::env::temp_dir().join(format!("rusted-cache-{}", std::process::id()));
        let config_dir = config_root.join("rusted");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "jobs = 2\nmanifest = \"cargo\"\n",
        )
        .unwrap();

        std::env::set_var("XDG_CONFIG_HOME", &config_root);
        std::env::set_var("XDG_CACHE_HOME", &cache_root);
        std::env::remove_var("RUSTED_MANIFEST");
        std::env::remove_var("RUSTED_JOBS");
        std::env::remove_var("RUSTED_OFFLINE");
        std::env::remove_var("RUSTED_TARGET_DIR");
        std::env::remove_var("RUSTED_CACHE_DIR");

        assert_eq!(config_path().unwrap(), config_dir.join("config.toml"));
        assert_eq!(cache_home().unwrap(), cache_root.join("rusted"));

        let config = Config::load().unwrap();
        assert_eq!(config.jobs, Some(2));
        assert_eq!(config.manifest, ManifestChoice::Cargo);
        assert!(!config.offline);

        std::env::set_var("RUSTED_JOBS", "8");
        std::env::set_var("RUSTED_OFFLINE", "1");
        std::env::set_var("RUSTED_MANIFEST", "rusted");
        std::env::set_var("RUSTED_TARGET_DIR", ".out");
        let config = Config::load().unwrap();
        assert_eq!(config.jobs, Some(8));
        assert!(config.offline);
        assert_eq!(config.manifest, ManifestChoice::Rusted);
        assert_eq!(config.target_dir, std::path::PathBuf::from(".out"));

        for (name, value) in originals {
            match value {
                Some(value) => std::env::set_var(&name, value),
                None => std::env::remove_var(&name),
            }
        }
        let _ = std::fs::remove_dir_all(&config_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }
}
