use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

use crate::build::store::Store;
use crate::util::hash::{sha256, to_hex, Sha256};
use crate::util::lock::Lock;

const REMOTE_ENV: &str = "RUSTED_REMOTE_CACHE";
const ACCESS_ENV: &str = "RUSTED_REMOTE_ACCESS_KEY";
const SECRET_ENV: &str = "RUSTED_REMOTE_SECRET_KEY";
const ENDPOINT_ENV: &str = "RUSTED_REMOTE_ENDPOINT";
const REGION_ENV: &str = "RUSTED_REMOTE_REGION";
const DEFAULT_REGION: &str = "us-east-1";
const OBJECT_SUFFIX: &str = ".tar.gz";
const ARTIFACT_PREFIX: &str = "v1/artifacts/";

pub(crate) struct Remote {
    endpoint: String,
    bucket: String,
    prefix: String,
    region: String,
    access_key: String,
    secret_key: String,
}

pub(crate) struct RemoteGc {
    pub(crate) removed: Vec<(String, u64)>,
    pub(crate) remaining: u64,
}

impl Remote {
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        let Some(url) = std::env::var_os(REMOTE_ENV) else {
            return Ok(None);
        };
        let url = url.to_string_lossy();
        let Some(rest) = url.strip_prefix("s3://") else {
            return Err(format!("`{REMOTE_ENV}` must start with `s3://`"));
        };
        let (bucket, prefix) = match rest.split_once('/') {
            Some((bucket, prefix)) => (bucket, prefix.trim_matches('/')),
            None => (rest, ""),
        };
        if bucket.is_empty() {
            return Err(format!("`{REMOTE_ENV}` is missing a bucket name"));
        }
        let access_key =
            std::env::var(ACCESS_ENV).map_err(|_| format!("`{ACCESS_ENV}` is not set"))?;
        let secret_key =
            std::env::var(SECRET_ENV).map_err(|_| format!("`{SECRET_ENV}` is not set"))?;
        let region = std::env::var(REGION_ENV).unwrap_or_else(|_| DEFAULT_REGION.to_string());
        let endpoint = match std::env::var(ENDPOINT_ENV) {
            Ok(endpoint) => endpoint.trim_end_matches('/').to_string(),
            Err(_) => format!("https://s3.{region}.amazonaws.com"),
        };
        Ok(Some(Self {
            endpoint,
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            region,
            access_key,
            secret_key,
        }))
    }

    pub(crate) fn fetch(&self, store: &Store, key: &str) -> Result<bool, String> {
        let object = self.object_key(key);
        let temp = store
            .root()
            .join("tmp")
            .join(format!("remote-fetch-{}-{key}", std::process::id()));
        remove_dir(&temp)?;
        fs::create_dir_all(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;

        let body = temp.join("body");
        self.get(&object, &body)?;

        let digest = read_digest(&body)?;
        let archive = temp.join("artifact.tar.gz");
        copy_after(&body, &archive, digest.len() as u64 + 1)?;
        let actual = to_hex(&sha256(
            &fs::read(&archive).map_err(|error| format!("{}: {error}", archive.display()))?,
        ));
        if actual != digest {
            remove_dir(&temp)?;
            return Err(format!(
                "remote artifact `{key}` failed checksum verification"
            ));
        }

        let extract = temp.join("extract");
        fs::create_dir_all(&extract).map_err(|error| format!("{}: {error}", extract.display()))?;
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&extract)
            .status()
            .map_err(|error| format!("failed to run tar: {error}"))?;
        if !status.success() {
            remove_dir(&temp)?;
            return Err(format!("remote artifact `{key}` could not be extracted"));
        }

        let source = extract.join(key);
        if !source.is_dir() {
            remove_dir(&temp)?;
            return Err(format!("remote artifact `{key}` has no artifact directory"));
        }
        let destination = store.root().join("artifacts").join(key);
        if destination.exists() {
            remove_dir(&temp)?;
            return Ok(false);
        }
        let lock_path = store.root().join("locks").join(format!("{key}.lock"));
        let _lock = Lock::acquire(&lock_path)
            .map_err(|error| format!("{}: {error}", lock_path.display()))?;
        if destination.exists() {
            remove_dir(&temp)?;
            return Ok(false);
        }
        fs::rename(&source, &destination)
            .map_err(|error| format!("{}: {error}", destination.display()))?;
        remove_dir(&temp)?;
        Ok(true)
    }

    pub(crate) fn push(&self, store: &Store, key: &str) -> Result<(), String> {
        let artifact = store.root().join("artifacts").join(key);
        if !artifact.is_dir() {
            return Err(format!("artifact `{key}` is not in the local store"));
        }
        let temp = store
            .root()
            .join("tmp")
            .join(format!("remote-push-{}-{key}", std::process::id()));
        remove_dir(&temp)?;
        fs::create_dir_all(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;

        let archive = temp.join("artifact.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(store.root().join("artifacts"))
            .arg(key)
            .status()
            .map_err(|error| format!("failed to run tar: {error}"))?;
        if !status.success() {
            remove_dir(&temp)?;
            return Err(format!("could not archive artifact `{key}`"));
        }

        let digest = to_hex(&sha256(
            &fs::read(&archive).map_err(|error| format!("{}: {error}", archive.display()))?,
        ));
        let body = temp.join("body");
        let mut output =
            fs::File::create(&body).map_err(|error| format!("{}: {error}", body.display()))?;
        output
            .write_all(digest.as_bytes())
            .and_then(|_| output.write_all(b"\n"))
            .map_err(|error| format!("{}: {error}", body.display()))?;
        let mut input =
            fs::File::open(&archive).map_err(|error| format!("{}: {error}", archive.display()))?;
        std::io::copy(&mut input, &mut output)
            .map_err(|error| format!("{}: {error}", body.display()))?;
        drop(output);

        let result = self.put(&self.object_key(key), &body);
        remove_dir(&temp)?;
        result
    }

    pub(crate) fn gc(&self, max_size: u64, dry_run: bool) -> Result<RemoteGc, String> {
        let mut objects = self.list(&self.artifact_prefix())?;
        objects.sort_by(|left, right| left.2.cmp(&right.2));
        let mut total: u64 = objects.iter().map(|object| object.1).sum();
        let mut removed = Vec::new();
        for (key, size, _) in objects {
            if total <= max_size {
                break;
            }
            if !dry_run {
                self.delete(&key)?;
            }
            total = total.saturating_sub(size);
            removed.push((key, size));
        }
        Ok(RemoteGc {
            removed,
            remaining: total,
        })
    }

    fn object_key(&self, key: &str) -> String {
        format!("{}{}{}", self.artifact_prefix(), key, OBJECT_SUFFIX)
    }

    fn artifact_prefix(&self) -> String {
        if self.prefix.is_empty() {
            ARTIFACT_PREFIX.to_string()
        } else {
            format!("{}/{ARTIFACT_PREFIX}", self.prefix)
        }
    }

    fn get(&self, object: &str, destination: &Path) -> Result<(), String> {
        let request = self.signed("GET", object, &[], &empty_hash());
        let status = Command::new("curl")
            .arg("-s")
            .arg("-f")
            .arg("--connect-timeout")
            .arg("2")
            .arg("--max-time")
            .arg("60")
            .arg("-o")
            .arg(destination)
            .args(headers(&request))
            .arg(&request.url)
            .status()
            .map_err(|error| format!("failed to run curl: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("S3 GET `{object}` failed"))
        }
    }

    fn put(&self, object: &str, body: &Path) -> Result<(), String> {
        let bytes = fs::read(body).map_err(|error| format!("{}: {error}", body.display()))?;
        let payload = to_hex(&sha256(&bytes));
        let request = self.signed("PUT", object, &[], &payload);
        let status = Command::new("curl")
            .arg("-s")
            .arg("-f")
            .arg("--connect-timeout")
            .arg("2")
            .arg("--max-time")
            .arg("60")
            .arg("-X")
            .arg("PUT")
            .arg("--data-binary")
            .arg(format!("@{}", body.display()))
            .args(headers(&request))
            .arg(&request.url)
            .status()
            .map_err(|error| format!("failed to run curl: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("S3 PUT `{object}` failed"))
        }
    }

    fn delete(&self, object: &str) -> Result<(), String> {
        let request = self.signed("DELETE", object, &[], &empty_hash());
        let status = Command::new("curl")
            .arg("-s")
            .arg("-f")
            .arg("--connect-timeout")
            .arg("2")
            .arg("--max-time")
            .arg("60")
            .arg("-X")
            .arg("DELETE")
            .args(headers(&request))
            .arg(&request.url)
            .status()
            .map_err(|error| format!("failed to run curl: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("S3 DELETE `{object}` failed"))
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<(String, u64, String)>, String> {
        let mut objects = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![
                ("list-type".to_string(), "2".to_string()),
                ("prefix".to_string(), prefix.to_string()),
            ];
            if let Some(token) = &token {
                query.push(("continuation-token".to_string(), token.clone()));
            }
            let request = self.signed("GET", "", &query, &empty_hash());
            let output = Command::new("curl")
                .arg("-s")
                .arg("-f")
                .arg("--connect-timeout")
                .arg("2")
                .arg("--max-time")
                .arg("60")
                .args(headers(&request))
                .arg(&request.url)
                .output()
                .map_err(|error| format!("failed to run curl: {error}"))?;
            if !output.status.success() {
                return Err("S3 list failed".to_string());
            }
            let text = String::from_utf8(output.stdout)
                .map_err(|error| format!("S3 list response is not UTF-8: {error}"))?;
            for contents in elements(&text, "Contents") {
                let key = element(contents, "Key")
                    .ok_or_else(|| "S3 list entry has no Key".to_string())?
                    .to_string();
                let size = element(contents, "Size")
                    .and_then(|size| size.parse().ok())
                    .unwrap_or(0);
                let modified = element(contents, "LastModified")
                    .unwrap_or_default()
                    .to_string();
                objects.push((key, size, modified));
            }
            if element(&text, "IsTruncated") == Some("true") {
                token = element(&text, "NextContinuationToken").map(str::to_string);
                if token.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(objects)
    }

    fn signed(
        &self,
        method: &str,
        object: &str,
        query: &[(String, String)],
        payload: &str,
    ) -> Request {
        let (date, timestamp) = amz_date();
        let host = host_of(&self.endpoint);
        let canonical_uri = if object.is_empty() {
            format!("/{}", uri_encode(&self.bucket, true))
        } else {
            format!(
                "/{}/{}",
                uri_encode(&self.bucket, true),
                uri_encode(object, false)
            )
        };
        let mut query = query.to_vec();
        query.sort();
        let canonical_query = query
            .iter()
            .map(|(key, value)| format!("{}={}", uri_encode(key, true), uri_encode(value, true)))
            .collect::<Vec<_>>()
            .join("&");
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{timestamp}\n\n{signed_headers}\n{payload}"
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
            to_hex(&sha256(canonical.as_bytes()))
        );
        let key = signing_key(&self.secret_key, &date, &self.region);
        let signature = to_hex(&hmac(&key, to_sign.as_bytes()));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key
        );
        let mut url = format!("{}{}", self.endpoint, canonical_uri);
        if !canonical_query.is_empty() {
            url.push('?');
            url.push_str(&canonical_query);
        }
        Request {
            url,
            timestamp,
            payload: payload.to_string(),
            authorization,
        }
    }
}

struct Request {
    url: String,
    timestamp: String,
    payload: String,
    authorization: String,
}

fn headers(request: &Request) -> Vec<String> {
    vec![
        "-H".to_string(),
        format!("x-amz-content-sha256: {}", request.payload),
        "-H".to_string(),
        format!("x-amz-date: {}", request.timestamp),
        "-H".to_string(),
        format!("authorization: {}", request.authorization),
    ]
}

fn empty_hash() -> String {
    to_hex(&sha256(b""))
}

fn read_digest(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut digest = [0u8; 64];
    file.read_exact(&mut digest)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut newline = [0u8; 1];
    file.read_exact(&mut newline)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if newline != *b"\n" {
        return Err("remote artifact header is invalid".to_string());
    }
    String::from_utf8(digest.to_vec()).map_err(|_| "remote artifact header is not hex".to_string())
}

fn copy_after(source: &Path, destination: &Path, offset: u64) -> Result<(), String> {
    let mut input =
        fs::File::open(source).map_err(|error| format!("{}: {error}", source.display()))?;
    input
        .seek(SeekFrom::Start(offset))
        .map_err(|error| format!("{}: {error}", source.display()))?;
    let mut output = fs::File::create(destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    Ok(())
}

fn remove_dir(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn host_of(endpoint: &str) -> String {
    endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint)
        .split('/')
        .next()
        .unwrap_or(endpoint)
        .to_string()
}

fn uri_encode(text: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (!encode_slash && byte == b'/');
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn amz_date() -> (String, String) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = (remainder % 3_600) / 60;
    let second = remainder % 60;
    let date = format!("{year:04}{month:02}{day:02}");
    let timestamp = format!("{date}T{hour:02}{minute:02}{second:02}Z");
    (date, timestamp)
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

fn signing_key(secret: &str, date: &str, region: &str) -> [u8; 32] {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    hmac(&k_service, b"aws4_request")
}

pub(crate) fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&sha256(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for index in 0..64 {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }
    let mut inner = Sha256::new();
    inner.update(&inner_pad);
    inner.update(data);
    let inner = inner.finish();
    let mut outer = Sha256::new();
    outer.update(&outer_pad);
    outer.update(&inner);
    outer.finish()
}

fn element<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

fn elements<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut items = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        items.push(&after[..end]);
        rest = &after[end + close.len()..];
    }
    items
}

#[cfg(test)]
mod tests {
    use super::{amz_date, hmac, host_of, signing_key, to_hex, uri_encode};

    #[test]
    fn encodes_uri_segments() {
        assert_eq!(uri_encode("a b/c", false), "a%20b/c");
        assert_eq!(uri_encode("a b/c", true), "a%20b%2Fc");
    }

    #[test]
    fn extracts_the_host() {
        assert_eq!(host_of("http://127.0.0.1:9000"), "127.0.0.1:9000");
        assert_eq!(
            host_of("https://s3.us-east-1.amazonaws.com"),
            "s3.us-east-1.amazonaws.com"
        );
    }

    #[test]
    fn hmac_matches_the_known_vector() {
        assert_eq!(
            to_hex(&hmac(
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn signing_key_is_deterministic() {
        assert_eq!(
            to_hex(&signing_key("secret", "20240101", "us-east-1")),
            to_hex(&signing_key("secret", "20240101", "us-east-1"))
        );
    }

    #[test]
    fn date_has_the_amz_shape() {
        let (date, timestamp) = amz_date();
        assert_eq!(date.len(), 8);
        assert_eq!(timestamp.len(), 16);
        assert!(timestamp.ends_with('Z'));
    }
}
