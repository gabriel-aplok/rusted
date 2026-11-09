use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::config::{cache_root, Config};
use crate::registry::{RETRY_ATTEMPTS, RETRY_DELAY_MS};
use crate::resolver::semver::Version;
use crate::util::hash::{to_hex, Sha256};
use crate::util::lock::Lock;

use super::extract::ensure_extracted;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) name: String,
    pub(crate) version: Version,
    pub(crate) checksum: String,
    pub(crate) url: String,
}

pub(crate) trait Downloader: Send + Sync {
    fn download(&self, url: &str, destination: &Path) -> Result<(), String>;
}

pub(crate) struct CrateStore {
    root: PathBuf,
    offline: bool,
    downloader: Box<dyn Downloader>,
}

impl CrateStore {
    pub(crate) fn new(config: &Config) -> Result<Self, String> {
        Self::from_parts(
            cache_root(config)?.join("v1"),
            config.offline,
            Box::new(CurlDownloader),
        )
    }

    fn from_parts(
        root: PathBuf,
        offline: bool,
        downloader: Box<dyn Downloader>,
    ) -> Result<Self, String> {
        for directory in ["crates", "src", "locks", "tmp"] {
            let path = root.join(directory);
            fs::create_dir_all(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        }
        Ok(Self {
            root,
            offline,
            downloader,
        })
    }

    pub(crate) fn fetch(&self, request: &Request) -> Result<PathBuf, String> {
        let tarball = self
            .root
            .join("crates")
            .join(format!("{}-{}.crate", request.name, request.version));

        if tarball.is_file() {
            if checksum_matches(&tarball, &request.checksum)? {
                return Ok(tarball);
            }
            if self.offline {
                return Err(format!(
                    "cached crate `{} {}` has a bad checksum and offline mode is enabled",
                    request.name, request.version
                ));
            }
            remove_file_if_exists(&tarball)?;
        }

        if self.offline {
            return Err(format!(
                "crate `{} {}` is not cached and offline mode is enabled",
                request.name, request.version
            ));
        }

        let lock_path = self.lock_path("crate", request);
        let _lock = Lock::acquire(&lock_path)
            .map_err(|error| format!("{}: {error}", lock_path.display()))?;

        if tarball.is_file() {
            if checksum_matches(&tarball, &request.checksum)? {
                return Ok(tarball);
            }
            remove_file_if_exists(&tarball)?;
        }

        let temp = self.temp_path("crates", request);
        self.download_with_retry(request, &temp)?;
        if !checksum_matches(&temp, &request.checksum)? {
            remove_file_if_exists(&temp)?;
            return Err(format!(
                "checksum mismatch for `{} {}`",
                request.name, request.version
            ));
        }
        fs::rename(&temp, &tarball).map_err(|error| {
            let _ = fs::remove_file(&temp);
            format!("{}: {error}", tarball.display())
        })?;
        Ok(tarball)
    }

    pub(crate) fn source(&self, request: &Request) -> Result<PathBuf, String> {
        let expected = self
            .root
            .join("src")
            .join(format!("{}-{}", request.name, request.version));
        if expected.join("Cargo.toml").is_file() {
            return Ok(expected);
        }
        let tarball = self.fetch(request)?;
        ensure_extracted(&self.root, request, &tarball)
    }

    pub(crate) fn fetch_many(
        &self,
        requests: &[Request],
        jobs: usize,
    ) -> Result<Vec<PathBuf>, String> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let workers = jobs.max(1).min(requests.len());
        let queue = Mutex::new((0..requests.len()).collect::<VecDeque<_>>());
        let results = Mutex::new((0..requests.len()).map(|_| None).collect::<Vec<_>>());
        let error = Mutex::new(None::<String>);

        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let index = {
                        let mut queue = queue.lock().unwrap();
                        queue.pop_front()
                    };
                    let Some(index) = index else {
                        break;
                    };
                    if error.lock().unwrap().is_some() {
                        break;
                    }
                    match self.fetch(&requests[index]) {
                        Ok(path) => {
                            results.lock().unwrap()[index] = Some(path);
                        }
                        Err(message) => {
                            *error.lock().unwrap() = Some(message);
                            break;
                        }
                    }
                });
            }
        });

        if let Some(error) = error.into_inner().unwrap() {
            return Err(error);
        }
        results
            .into_inner()
            .unwrap()
            .into_iter()
            .map(|path| path.ok_or_else(|| "internal error: missing download result".to_string()))
            .collect()
    }

    fn download_with_retry(&self, request: &Request, destination: &Path) -> Result<(), String> {
        let mut delay = Duration::from_millis(RETRY_DELAY_MS);
        let mut last_error = String::new();
        for attempt in 1..=RETRY_ATTEMPTS {
            remove_file_if_exists(destination)?;
            match self.downloader.download(&request.url, destination) {
                Ok(()) => return Ok(()),
                Err(error) if attempt < RETRY_ATTEMPTS => {
                    remove_file_if_exists(destination)?;
                    std::thread::sleep(delay);
                    delay *= 2;
                    last_error = error;
                }
                Err(error) => {
                    remove_file_if_exists(destination)?;
                    return Err(error);
                }
            }
        }
        Err(last_error)
    }

    fn lock_path(&self, kind: &str, request: &Request) -> PathBuf {
        self.root.join("locks").join(format!(
            "registry-{kind}-{}-{}.lock",
            request.name, request.version
        ))
    }

    fn temp_path(&self, directory: &str, request: &Request) -> PathBuf {
        self.root.join(directory).join(format!(
            ".{}-{}-{}-{}.tmp",
            request.name,
            request.version,
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

struct CurlDownloader;

impl Downloader for CurlDownloader {
    fn download(&self, url: &str, destination: &Path) -> Result<(), String> {
        let output = Command::new("curl")
            .arg("--silent")
            .arg("--show-error")
            .arg("--fail")
            .arg("--location")
            .arg("--connect-timeout")
            .arg("10")
            .arg("--max-time")
            .arg("300")
            .arg("--output")
            .arg(destination)
            .arg(url)
            .output()
            .map_err(|error| format!("failed to run curl: {error}"))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(format!("download from `{url}` failed: {}", error.trim()));
        }
        Ok(())
    }
}

fn checksum_matches(path: &Path, expected: &str) -> Result<bool, String> {
    let mut hasher = Sha256::new();
    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
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
    Ok(to_hex(&hasher.finish()).eq_ignore_ascii_case(expected))
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{CrateStore, Downloader, Request};
    use crate::resolver::semver::Version;
    use crate::util::hash::{sha256, to_hex};

    const GOOD: &[u8] = include_bytes!("../../tests/fixtures/crates/good-1.0.0.crate");
    const TRAVERSAL: &[u8] = include_bytes!("../../tests/fixtures/crates/traversal-1.0.0.crate");
    const SYMLINK: &[u8] = include_bytes!("../../tests/fixtures/crates/symlink-1.0.0.crate");

    struct FakeState {
        calls: Vec<String>,
        payloads: HashMap<String, Vec<u8>>,
        fail_first: bool,
        delay: Duration,
        current: usize,
        max: usize,
    }

    #[derive(Clone)]
    struct FakeDownloader {
        state: Arc<Mutex<FakeState>>,
    }

    impl FakeDownloader {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    calls: Vec::new(),
                    payloads: HashMap::new(),
                    fail_first: false,
                    delay: Duration::ZERO,
                    current: 0,
                    max: 0,
                })),
            }
        }

        fn with_payload(self, url: &str, bytes: &[u8]) -> Self {
            self.state
                .lock()
                .unwrap()
                .payloads
                .insert(url.to_string(), bytes.to_vec());
            self
        }

        fn with_delay(self, delay: Duration) -> Self {
            self.state.lock().unwrap().delay = delay;
            self
        }

        fn fail_first(self) -> Self {
            self.state.lock().unwrap().fail_first = true;
            self
        }

        fn calls(&self) -> Vec<String> {
            self.state.lock().unwrap().calls.clone()
        }

        fn max_concurrency(&self) -> usize {
            self.state.lock().unwrap().max
        }
    }

    impl Downloader for FakeDownloader {
        fn download(&self, url: &str, destination: &Path) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            state.calls.push(url.to_string());
            state.current += 1;
            state.max = state.max.max(state.current);
            if state.fail_first {
                state.fail_first = false;
                state.current -= 1;
                return Err("fake download failure".to_string());
            }
            let delay = state.delay;
            let bytes = state
                .payloads
                .get(url)
                .cloned()
                .unwrap_or_else(|| url.as_bytes().to_vec());
            drop(state);

            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            let result = std::fs::write(destination, bytes)
                .map_err(|error| format!("{}: {error}", destination.display()));
            let mut state = self.state.lock().unwrap();
            state.current -= 1;
            result
        }
    }

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-download-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn store(&self, offline: bool, downloader: FakeDownloader) -> CrateStore {
            CrateStore::from_parts(self.dir.clone(), offline, Box::new(downloader)).unwrap()
        }

        fn seed(&self, request: &Request, bytes: &[u8]) {
            let path = self
                .dir
                .join("crates")
                .join(format!("{}-{}.crate", request.name, request.version));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn make_request(name: &str, version: &str, bytes: &[u8], url: &str) -> Request {
        Request {
            name: name.to_string(),
            version: Version::parse(version).unwrap(),
            checksum: to_hex(&sha256(bytes)),
            url: url.to_string(),
        }
    }

    #[test]
    fn new_uses_the_global_cache_root() {
        let fixture = Fixture::new("new");
        let config = crate::config::Config {
            manifest: crate::config::ManifestChoice::Auto,
            cache_dir: Some(fixture.dir.clone()),
            jobs: None,
            offline: true,
            target_dir: PathBuf::from("target"),
            registry_index: None,
            profiles: Default::default(),
            linker: None,
        };
        let store = CrateStore::new(&config).unwrap();
        assert!(fixture.dir.join("v1/crates").is_dir());
        let request = make_request("absent", "1.0.0", b"x", "https://example/absent");
        let error = store.fetch(&request).unwrap_err();
        assert!(error.contains("offline"), "{error}");
    }

    #[test]
    #[ignore]
    fn downloads_and_extracts_the_live_cfg_if_crate() {
        let fixture = Fixture::new("live");
        let config = crate::config::Config {
            manifest: crate::config::ManifestChoice::Auto,
            cache_dir: Some(fixture.dir.clone()),
            jobs: None,
            offline: false,
            target_dir: PathBuf::from("target"),
            registry_index: None,
            profiles: Default::default(),
            linker: None,
        };
        let store = CrateStore::new(&config).unwrap();
        let request = Request {
            name: "cfg-if".to_string(),
            version: Version::parse("1.0.5").unwrap(),
            checksum: "4e7648175b45a9a48536d676f68d918270699102aa8dab5496df06904c914600"
                .to_string(),
            url: "https://static.crates.io/crates/cfg-if/cfg-if-1.0.5.crate".to_string(),
        };
        let source = store.source(&request).unwrap();
        assert!(source.join("Cargo.toml").is_file());
        assert!(source.join("src/lib.rs").is_file());

        let mut offline_config = config;
        offline_config.offline = true;
        let offline = CrateStore::new(&offline_config).unwrap();
        assert_eq!(offline.source(&request).unwrap(), source);
    }

    #[test]
    fn downloads_and_verifies_once() {
        let fixture = Fixture::new("download");
        let downloader = FakeDownloader::new().with_payload("https://example/good", b"hello");
        let observer = downloader.clone();
        let store = fixture.store(false, downloader);
        let request = make_request("good", "1.0.0", b"hello", "https://example/good");

        let tarball = store.fetch(&request).unwrap();
        assert_eq!(std::fs::read(&tarball).unwrap(), b"hello");
        assert_eq!(store.fetch(&request).unwrap(), tarball);
        assert_eq!(observer.calls().len(), 1);
    }

    #[test]
    fn rejects_checksum_mismatch_and_removes_temp() {
        let fixture = Fixture::new("checksum");
        let downloader = FakeDownloader::new().with_payload("https://example/bad", b"bad");
        let store = fixture.store(false, downloader);
        let request = make_request("bad", "1.0.0", b"good", "https://example/bad");

        let error = store.fetch(&request).unwrap_err();
        assert!(error.contains("checksum mismatch"), "{error}");
        let crates = fixture.dir.join("crates");
        let entries: Vec<_> = std::fs::read_dir(crates).unwrap().collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn retries_transient_download_failures() {
        let fixture = Fixture::new("retry");
        let downloader = FakeDownloader::new()
            .with_payload("https://example/retry", b"data")
            .fail_first();
        let observer = downloader.clone();
        let store = fixture.store(false, downloader);
        let request = make_request("retry", "1.0.0", b"data", "https://example/retry");
        store.fetch(&request).unwrap();
        assert_eq!(observer.calls().len(), 2);
    }

    #[test]
    fn offline_uses_the_cache_and_rejects_misses() {
        let fixture = Fixture::new("offline");
        let online = fixture.store(
            false,
            FakeDownloader::new().with_payload("https://example/offline", b"cached"),
        );
        let request = make_request("offline", "1.0.0", b"cached", "https://example/offline");
        online.fetch(&request).unwrap();

        let offline_downloader = FakeDownloader::new();
        let offline = fixture.store(true, offline_downloader.clone());
        offline.fetch(&request).unwrap();
        assert_eq!(offline_downloader.calls().len(), 0);

        let missing = make_request("missing", "1.0.0", b"absent", "https://example/missing");
        let error = offline.fetch(&missing).unwrap_err();
        assert!(error.contains("offline"), "{error}");
    }

    #[test]
    fn extracts_a_crate_and_shares_the_source() {
        let fixture = Fixture::new("extract");
        let request = make_request("good", "1.0.0", GOOD, "https://example/good");
        fixture.seed(&request, GOOD);

        let first = fixture.store(false, FakeDownloader::new());
        let source = first.source(&request).unwrap();
        assert!(source.join("Cargo.toml").is_file());
        assert!(source.join("src/lib.rs").is_file());
        assert!(source.starts_with(fixture.dir.join("src")));

        let second_downloader = FakeDownloader::new();
        let second = fixture.store(false, second_downloader.clone());
        assert_eq!(second.source(&request).unwrap(), source);
        assert_eq!(second_downloader.calls().len(), 0);

        let tarball = fixture.dir.join("crates").join("good-1.0.0.crate");
        std::fs::remove_file(&tarball).unwrap();
        let offline = fixture.store(true, FakeDownloader::new());
        assert_eq!(offline.source(&request).unwrap(), source);
    }

    #[test]
    fn rejects_traversal_and_unsafe_symlink_archives() {
        let fixture = Fixture::new("unsafe");
        let traversal = make_request("traversal", "1.0.0", TRAVERSAL, "https://example/traversal");
        fixture.seed(&traversal, TRAVERSAL);
        let store = fixture.store(false, FakeDownloader::new());
        let error = store.source(&traversal).unwrap_err();
        assert!(error.contains("parent path"), "{error}");
        assert!(!fixture.dir.join("src/traversal-1.0.0").exists());

        let symlink = make_request("symlink", "1.0.0", SYMLINK, "https://example/symlink");
        fixture.seed(&symlink, SYMLINK);
        let error = store.source(&symlink).unwrap_err();
        assert!(error.contains("unsafe symlink"), "{error}");
        assert!(!fixture.dir.join("src/symlink-1.0.0").exists());
    }

    #[test]
    fn fetch_many_respects_the_job_limit() {
        let fixture = Fixture::new("parallel");
        let downloader = FakeDownloader::new().with_delay(Duration::from_millis(20));
        let observer = downloader.clone();
        let store = fixture.store(false, downloader);
        let requests: Vec<Request> = (0..4)
            .map(|index| {
                let url = format!("https://example/{index}");
                Request {
                    name: format!("crate{index}"),
                    version: Version::parse("1.0.0").unwrap(),
                    checksum: to_hex(&sha256(url.as_bytes())),
                    url,
                }
            })
            .collect();

        let paths = store.fetch_many(&requests, 2).unwrap();
        assert_eq!(paths.len(), 4);
        assert_eq!(observer.calls().len(), 4);
        assert!(observer.max_concurrency() <= 2);
        for (path, request) in paths.iter().zip(&requests) {
            assert_eq!(std::fs::read(path).unwrap(), request.url.as_bytes());
        }
    }

    #[test]
    fn concurrent_fetches_of_one_crate_download_once() {
        let fixture = Fixture::new("concurrent");
        let downloader = FakeDownloader::new()
            .with_payload("https://example/shared", b"shared")
            .with_delay(Duration::from_millis(30));
        let observer = downloader.clone();
        let store = fixture.store(false, downloader);
        let request = make_request("shared", "1.0.0", b"shared", "https://example/shared");

        std::thread::scope(|scope| {
            scope.spawn(|| {
                store.fetch(&request).unwrap();
            });
            scope.spawn(|| {
                store.fetch(&request).unwrap();
            });
        });
        assert_eq!(observer.calls().len(), 1);
    }
}
