use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::config::{cache_root, Config};
use crate::registry::{DEFAULT_INDEX, RETRY_ATTEMPTS, RETRY_DELAY_MS};
use crate::resolver::semver::Version;
use crate::util::json::{self, Value};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub(crate) struct IndexDependency {
    pub(crate) name: String,
    pub(crate) requirement: String,
    pub(crate) features: Vec<String>,
    pub(crate) optional: bool,
    pub(crate) default_features: bool,
    pub(crate) target: Option<String>,
    pub(crate) kind: String,
    pub(crate) registry: Option<String>,
    pub(crate) package: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexEntry {
    pub(crate) name: String,
    pub(crate) version: Version,
    pub(crate) dependencies: Vec<IndexDependency>,
    pub(crate) checksum: String,
    pub(crate) features: BTreeMap<String, Vec<String>>,
    pub(crate) yanked: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) rust_version: Option<String>,
}

#[derive(Debug)]
pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
    pub(crate) etag: Option<String>,
}

pub(crate) trait Fetcher {
    fn get(&self, url: &str, etag: Option<&str>) -> Result<HttpResponse, String>;
}

pub(crate) struct IndexClient {
    root: PathBuf,
    index_url: String,
    offline: bool,
    fetcher: Box<dyn Fetcher>,
    download_base: Option<String>,
}

impl IndexClient {
    pub(crate) fn new(config: &Config) -> Result<Self, String> {
        let root = cache_root(config)?.join("v1").join("index");
        fs::create_dir_all(&root).map_err(|error| format!("{}: {error}", root.display()))?;
        let index_url = config
            .registry_index
            .clone()
            .unwrap_or_else(|| DEFAULT_INDEX.to_string())
            .trim_end_matches('/')
            .to_string();

        Ok(Self {
            root,
            index_url,
            offline: config.offline,
            fetcher: Box::new(CurlFetcher),
            download_base: None,
        })
    }

    #[cfg(test)]
    fn with_fetcher(root: PathBuf, offline: bool, fetcher: Box<dyn Fetcher>) -> Self {
        Self {
            root,
            index_url: "https://index.example.invalid".to_string(),
            offline,
            fetcher,
            download_base: None,
        }
    }

    pub(crate) fn entries(&mut self, name: &str) -> Result<Vec<IndexEntry>, String> {
        let path = index_path(name)?;
        let cache_name = format!("{path}.json");
        let body = self
            .cached_fetch(&path, &cache_name)
            .map_err(|error| format!("crate `{name}`: {error}"))?;
        match body {
            Some(body) => parse_entries(&body, name),
            None => Ok(Vec::new()),
        }
    }

    pub(crate) fn download_url(&mut self, name: &str, version: &Version) -> Result<String, String> {
        if self.download_base.is_none() {
            let body = self
                .cached_fetch("config.json", "config.json")?
                .ok_or_else(|| "registry index has no config.json".to_string())?;
            let text = std::str::from_utf8(&body)
                .map_err(|error| format!("registry config is not UTF-8: {error}"))?;
            let value = json::parse(text)
                .map_err(|error| format!("registry config is invalid JSON: {error}"))?;
            let base = required_string(&value, "dl")?;
            self.download_base = Some(base.to_string());
        }

        let base = self.download_base.as_deref().unwrap_or_default();
        if base.contains("{crate}") || base.contains("{version}") {
            Ok(base
                .replace("{crate}", name)
                .replace("{version}", &version.to_string()))
        } else {
            Ok(format!(
                "{}/{}/{}-{}.crate",
                base.trim_end_matches('/'),
                name,
                name,
                version
            ))
        }
    }

    fn cached_fetch(
        &mut self,
        relative: &str,
        cache_name: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let cache_path = self.root.join(cache_name);
        let etag_path = self.root.join(format!("{cache_name}.etag"));

        if self.offline {
            return match fs::read(&cache_path) {
                Ok(body) => Ok(Some(body)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
                    "`{relative}` is not cached and offline mode is enabled"
                )),
                Err(error) => Err(format!("{}: {error}", cache_path.display())),
            };
        }

        let etag = read_optional(&etag_path)?;
        let url = format!("{}/{}", self.index_url, relative);
        let response = self.fetch_with_retry(&url, etag.as_deref())?;

        match response.status {
            200 => {
                write_atomic(&cache_path, &response.body)?;
                write_etag(&etag_path, response.etag.as_deref())?;
                Ok(Some(response.body))
            }
            304 => {
                let body = fs::read(&cache_path)
                    .map_err(|error| format!("{}: {error}", cache_path.display()))?;
                Ok(Some(body))
            }
            404 => Ok(None),
            status => Err(format!("request to `{url}` returned HTTP {status}")),
        }
    }

    fn fetch_with_retry(&self, url: &str, etag: Option<&str>) -> Result<HttpResponse, String> {
        let mut delay = Duration::from_millis(RETRY_DELAY_MS);
        let mut last_error = String::new();
        for attempt in 1..=RETRY_ATTEMPTS {
            match self.fetcher.get(url, etag) {
                Ok(response) if retryable(response.status) && attempt < RETRY_ATTEMPTS => {
                    std::thread::sleep(delay);
                    delay *= 2;
                    last_error = format!("HTTP {}", response.status);
                }
                Ok(response) => return Ok(response),
                Err(error) if attempt < RETRY_ATTEMPTS => {
                    std::thread::sleep(delay);
                    delay *= 2;
                    last_error = error;
                }
                Err(error) => return Err(error),
            }
        }
        Err(format!("request to `{url}` failed: {last_error}"))
    }
}

fn retryable(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

pub(crate) fn index_path(name: &str) -> Result<String, String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(format!("invalid crate name `{name}`"));
    }
    let name = name.to_ascii_lowercase();
    let path = match name.len() {
        0 => return Err("empty crate name".to_string()),
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    };
    Ok(path)
}

fn parse_entries(body: &[u8], requested: &str) -> Result<Vec<IndexEntry>, String> {
    let text = std::str::from_utf8(body)
        .map_err(|error| format!("index for `{requested}` is not UTF-8: {error}"))?;
    let mut entries = Vec::new();

    for (line_number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value = json::parse(line)
            .map_err(|error| format!("index line {}: {error}", line_number + 1))?;
        let entry = parse_entry(&value)
            .map_err(|error| format!("index line {}: {error}", line_number + 1))?;
        if !entry.name.eq_ignore_ascii_case(requested) {
            return Err(format!(
                "index line {} names `{}`, expected `{requested}`",
                line_number + 1,
                entry.name
            ));
        }
        entries.push(entry);
    }

    entries.sort_by(|left, right| left.version.cmp(&right.version));
    Ok(entries)
}

fn parse_entry(value: &Value) -> Result<IndexEntry, String> {
    let name = required_string(value, "name")?.to_string();
    let version = Version::parse(required_string(value, "vers")?)
        .map_err(|error| format!("invalid version: {error}"))?;
    let checksum = required_string(value, "cksum")?.to_string();
    let yanked = value
        .get("yanked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rust_version = optional_string(value, "rust_version")?;

    let mut features = BTreeMap::new();
    if let Some(value) = value.get("features") {
        features.extend(parse_features(value)?);
    }
    if let Some(value) = value.get("features2") {
        features.extend(parse_features(value)?);
    }

    let dependencies = match value.get("deps") {
        None => Vec::new(),
        Some(Value::Array(dependencies)) => dependencies
            .iter()
            .map(parse_dependency)
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("`deps` must be an array".to_string()),
    };

    Ok(IndexEntry {
        name,
        version,
        dependencies,
        checksum,
        features,
        yanked,
        rust_version,
    })
}

fn parse_dependency(value: &Value) -> Result<IndexDependency, String> {
    Ok(IndexDependency {
        name: required_string(value, "name")?.to_string(),
        requirement: required_string(value, "req")?.to_string(),
        features: string_list(value, "features")?.unwrap_or_default(),
        optional: value
            .get("optional")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        default_features: value
            .get("default_features")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        target: optional_string(value, "target")?,
        kind: optional_string(value, "kind")?.unwrap_or_else(|| "normal".to_string()),
        registry: optional_string(value, "registry")?,
        package: optional_string(value, "package")?,
    })
}

fn parse_features(value: &Value) -> Result<BTreeMap<String, Vec<String>>, String> {
    let fields = value
        .as_object()
        .ok_or_else(|| "`features` must be an object".to_string())?;
    let mut features = BTreeMap::new();
    for (name, value) in fields {
        let values = value
            .as_array()
            .ok_or_else(|| format!("feature `{name}` must be an array"))?;
        let mut items = Vec::new();
        for value in values {
            let item = value
                .as_str()
                .ok_or_else(|| format!("feature `{name}` must contain strings"))?;
            items.push(item.to_string());
        }
        features.insert(name.clone(), items);
    }
    Ok(features)
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string `{key}`"))
}

fn optional_string(value: &Value, key: &str) -> Result<Option<String>, String> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(Value::Null) => Ok(None),
        Some(_) => Err(format!("`{key}` must be a string or null")),
    }
}

fn string_list(value: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::Array(values)) => {
            let mut items = Vec::new();
            for value in values {
                let item = value
                    .as_str()
                    .ok_or_else(|| format!("`{key}` must contain strings"))?;
                items.push(item.to_string());
            }
            Ok(Some(items))
        }
        Some(_) => Err(format!("`{key}` must be an array of strings")),
    }
}

struct CurlFetcher;

impl Fetcher for CurlFetcher {
    fn get(&self, url: &str, etag: Option<&str>) -> Result<HttpResponse, String> {
        let directory = std::env::temp_dir().join(format!(
            "rusted-curl-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        let guard = TempDir(directory.clone());

        let body_path = directory.join("body");
        let headers_path = directory.join("headers");

        let mut command = Command::new("curl");
        command
            .arg("--silent")
            .arg("--show-error")
            .arg("--location")
            .arg("--connect-timeout")
            .arg("10")
            .arg("--max-time")
            .arg("60")
            .arg("--dump-header")
            .arg(&headers_path)
            .arg("--output")
            .arg(&body_path)
            .arg("--write-out")
            .arg("%{http_code}");
        if let Some(etag) = etag {
            command
                .arg("--header")
                .arg(format!("If-None-Match: {etag}"));
        }
        command.arg(url);

        let output = command
            .output()
            .map_err(|error| format!("failed to run curl: {error}"))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(format!("curl request to `{url}` failed: {}", error.trim()));
        }
        let status_text = String::from_utf8_lossy(&output.stdout);
        let status = status_text
            .trim()
            .parse::<u16>()
            .map_err(|_| format!("curl returned an invalid status `{}`", status_text.trim()))?;
        let body = read_bytes_optional(&body_path)?;
        let headers = read_bytes_optional(&headers_path)?;
        let headers = String::from_utf8_lossy(&headers);
        let etag = parse_etag(&headers);

        drop(guard);
        Ok(HttpResponse { status, body, etag })
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn parse_etag(headers: &str) -> Option<String> {
    let mut etag = None;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("etag") {
                etag = Some(value.trim().to_string());
            }
        }
    }
    etag
}

fn read_bytes_optional(path: &Path) -> Result<Vec<u8>, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cache");
    let temp = path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(error) = fs::write(&temp, bytes) {
        let _ = fs::remove_file(&temp);
        return Err(format!("{}: {error}", temp.display()));
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(format!("{}: {error}", path.display()));
    }
    Ok(())
}

fn write_etag(path: &Path, etag: Option<&str>) -> Result<(), String> {
    match etag {
        Some(etag) => write_atomic(path, etag.as_bytes()),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::rc::Rc;

    use super::{index_path, Fetcher, HttpResponse, IndexClient};
    use crate::config::{Config, ManifestChoice};
    use crate::resolver::semver::Version;

    #[derive(Default)]
    struct FakeState {
        responses: VecDeque<Result<HttpResponse, String>>,
        calls: Vec<(String, Option<String>)>,
    }

    #[derive(Clone, Default)]
    struct FakeFetcher {
        state: Rc<RefCell<FakeState>>,
    }

    impl FakeFetcher {
        fn with(responses: Vec<Result<HttpResponse, String>>) -> Self {
            let fetcher = Self::default();
            fetcher.state.borrow_mut().responses = responses.into();
            fetcher
        }

        fn calls(&self) -> Vec<(String, Option<String>)> {
            self.state.borrow().calls.clone()
        }
    }

    impl Fetcher for FakeFetcher {
        fn get(&self, url: &str, etag: Option<&str>) -> Result<HttpResponse, String> {
            let mut state = self.state.borrow_mut();
            state
                .calls
                .push((url.to_string(), etag.map(str::to_string)));
            state
                .responses
                .pop_front()
                .unwrap_or_else(|| Err("no fake response".to_string()))
        }
    }

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-index-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn response(status: u16, body: &[u8], etag: Option<&str>) -> Result<HttpResponse, String> {
        Ok(HttpResponse {
            status,
            body: body.to_vec(),
            etag: etag.map(str::to_string),
        })
    }

    fn make_client(
        fixture: &Fixture,
        offline: bool,
        fetcher: FakeFetcher,
    ) -> (IndexClient, FakeFetcher) {
        (
            IndexClient::with_fetcher(
                fixture.dir.join("index"),
                offline,
                Box::new(fetcher.clone()),
            ),
            fetcher,
        )
    }

    #[test]
    fn index_paths_follow_the_sparse_scheme() {
        assert_eq!(index_path("a").unwrap(), "1/a");
        assert_eq!(index_path("ab").unwrap(), "2/ab");
        assert_eq!(index_path("abc").unwrap(), "3/a/abc");
        assert_eq!(index_path("serde").unwrap(), "se/rd/serde");
        assert_eq!(index_path("cargo-util").unwrap(), "ca/rg/cargo-util");
        assert_eq!(index_path("Serde").unwrap(), "se/rd/serde");
        assert!(index_path("").is_err());
        assert!(index_path("bad/name").is_err());
    }

    #[test]
    fn parses_real_index_fixtures() {
        let body = include_str!("../../tests/fixtures/index/serde.jsonl");
        let entries = super::parse_entries(body.as_bytes(), "serde").unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.name, "serde");
        assert_eq!(entry.version, Version::parse("1.0.229").unwrap());
        assert_eq!(entry.checksum.len(), 64);
        assert!(!entry.yanked);
        assert!(entry.rust_version.is_some());
        assert!(entry.features.contains_key("derive"));

        let dependency = entry
            .dependencies
            .iter()
            .find(|dependency| dependency.name == "serde_core")
            .unwrap();
        assert_eq!(dependency.requirement, "=1.0.229");
        assert!(!dependency.optional);
        assert!(!dependency.default_features);
        assert!(dependency.features.contains(&"result".to_string()));
        assert!(dependency.target.is_none());
        assert_eq!(dependency.kind, "normal");
        assert!(dependency.registry.is_none());
        assert!(dependency.package.is_none());

        let body = include_str!("../../tests/fixtures/index/cfg-if.jsonl");
        let entries = super::parse_entries(body.as_bytes(), "cfg-if").unwrap();
        assert!(entries.len() > 5);
        assert!(entries
            .windows(2)
            .all(|pair| pair[0].version < pair[1].version));
    }

    #[test]
    fn merges_features2_into_features() {
        let line = r#"{"name":"x","vers":"1.0.0","deps":[],"cksum":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","features":{"old":[]},"features2":{"new":["dep:y"]},"yanked":false}"#;
        let entries = super::parse_entries(line.as_bytes(), "x").unwrap();
        let entry = &entries[0];
        assert!(entry.features.contains_key("old"));
        assert_eq!(entry.features.get("new"), Some(&vec!["dep:y".to_string()]));
    }

    #[test]
    fn caches_bodies_and_revalidates_with_etags() {
        let fixture = Fixture::new("etag");
        let body = include_str!("../../tests/fixtures/index/cfg-if.jsonl").as_bytes();
        let fetcher = FakeFetcher::with(vec![
            response(200, body, Some("etag-1")),
            response(304, b"", None),
        ]);
        let (mut client, fetcher) = make_client(&fixture, false, fetcher);

        let first = client.entries("cfg-if").unwrap();
        assert!(!first.is_empty());
        let second = client.entries("cfg-if").unwrap();
        assert_eq!(first.len(), second.len());

        let calls = fetcher.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1, None);
        assert_eq!(calls[1].1, Some("etag-1".to_string()));
    }

    #[test]
    fn missing_entries_return_empty() {
        let fixture = Fixture::new("missing");
        let fetcher = FakeFetcher::with(vec![response(404, b"", None)]);
        let (mut client, _) = make_client(&fixture, false, fetcher);
        assert!(client.entries("does-not-exist").unwrap().is_empty());
    }

    #[test]
    fn offline_reads_the_cache_and_rejects_uncached_names() {
        let fixture = Fixture::new("offline");
        let body = include_str!("../../tests/fixtures/index/cfg-if.jsonl").as_bytes();
        let fetcher = FakeFetcher::with(vec![response(200, body, Some("etag-1"))]);
        let (mut client, _) = make_client(&fixture, false, fetcher);
        assert!(!client.entries("cfg-if").unwrap().is_empty());

        let offline = FakeFetcher::with(Vec::new());
        let (mut client, _) = make_client(&fixture, true, offline);
        assert!(!client.entries("cfg-if").unwrap().is_empty());

        let error = client.entries("serde").unwrap_err();
        assert!(error.contains("offline mode"), "{error}");
    }

    #[test]
    fn config_json_provides_the_download_template() {
        let fixture = Fixture::new("config");
        let fetcher = FakeFetcher::with(vec![response(
            200,
            br#"{"dl":"https://static.crates.io/crates"}"#,
            Some("cfg-1"),
        )]);
        let (mut client, fetcher) = make_client(&fixture, false, fetcher);
        let version = Version::parse("1.0.0").unwrap();
        let url = client.download_url("serde", &version).unwrap();
        assert_eq!(
            url,
            "https://static.crates.io/crates/serde/serde-1.0.0.crate"
        );
        let url = client.download_url("serde", &version).unwrap();
        assert_eq!(
            url,
            "https://static.crates.io/crates/serde/serde-1.0.0.crate"
        );
        assert_eq!(fetcher.calls().len(), 1);
    }

    #[test]
    fn config_json_supports_placeholders() {
        let fixture = Fixture::new("config-template");
        let fetcher = FakeFetcher::with(vec![response(
            200,
            br#"{"dl":"https://example.com/{crate}/{version}"}"#,
            None,
        )]);
        let (mut client, _) = make_client(&fixture, false, fetcher);
        let version = Version::parse("2.3.4").unwrap();
        assert_eq!(
            client.download_url("foo", &version).unwrap(),
            "https://example.com/foo/2.3.4"
        );
    }

    #[test]
    fn retries_transient_statuses() {
        let fixture = Fixture::new("retry");
        let body = include_str!("../../tests/fixtures/index/cfg-if.jsonl").as_bytes();
        let fetcher = FakeFetcher::with(vec![
            response(503, b"", None),
            response(200, body, Some("etag-1")),
        ]);
        let (mut client, fetcher) = make_client(&fixture, false, fetcher);
        assert!(!client.entries("cfg-if").unwrap().is_empty());
        assert_eq!(fetcher.calls().len(), 2);
    }

    #[test]
    #[ignore]
    fn fetches_the_live_index() {
        let fixture = Fixture::new("live");
        let config = Config {
            manifest: ManifestChoice::Auto,
            cache_dir: Some(fixture.dir.clone()),
            jobs: None,
            offline: false,
            target_dir: PathBuf::from("target"),
            registry_index: None,
            profiles: Default::default(),
            linker: None,
        };
        let mut client = IndexClient::new(&config).unwrap();
        let entries = client.entries("serde").unwrap();
        assert!(!entries.is_empty());
        let latest = entries.last().unwrap();
        assert_eq!(latest.name, "serde");
        let url = client.download_url("serde", &latest.version).unwrap();
        assert!(url.contains("serde-"), "{url}");
    }
}
