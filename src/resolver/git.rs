use std::path::{Path, PathBuf};
use std::process::Command;

use crate::manifest::select::CARGO_MANIFEST_FILE;

const GIT: &str = "git";
const LS_REMOTE: &str = "ls-remote";
const HEAD_REF: &str = "HEAD";
const PEELED_SUFFIX: &str = "^{}";
const REPOS_DIR: &str = "repos";
const TEMP_PREFIX: &str = "tmp-";
const STDERR_TAIL_LINES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum GitRefSelection {
    Branch(String),
    Tag(String),
    Rev(String),
}

pub(crate) fn resolve_ref(
    url: &str,
    reference: Option<&GitRefSelection>,
) -> Result<String, String> {
    match reference {
        Some(GitRefSelection::Branch(branch)) => {
            let pattern = format!("refs/heads/{branch}");
            let output = ls_remote(url, Some(&pattern))?;
            find_ref(&output, &pattern)
                .map(str::to_string)
                .ok_or_else(|| format!("`{url}` has no branch `{branch}`"))
        }
        Some(GitRefSelection::Tag(tag)) => {
            let pattern = format!("refs/tags/{tag}");
            let output = ls_remote(url, None)?;
            let peeled = format!("{pattern}{PEELED_SUFFIX}");
            find_ref(&output, &peeled)
                .or_else(|| find_ref(&output, &pattern))
                .map(str::to_string)
                .ok_or_else(|| format!("`{url}` has no tag `{tag}`"))
        }
        Some(GitRefSelection::Rev(rev)) => {
            let output = ls_remote(url, None)?;
            let mut matches: Vec<&str> = Vec::new();
            for (sha, _) in ls_remote_refs(&output) {
                let matched = sha == rev.as_str() || sha.starts_with(rev.as_str());
                if matched && !matches.contains(&sha) {
                    matches.push(sha);
                }
            }
            match matches.as_slice() {
                [sha] => Ok((*sha).to_string()),
                [] => Err(format!("`{url}` has no revision matching `{rev}`")),
                _ => Err(format!(
                    "`{url}` revision `{rev}` is ambiguous, {} revisions match",
                    matches.len()
                )),
            }
        }
        None => {
            let output = ls_remote(url, None)?;
            let head = ls_remote_refs(&output)
                .find_map(|(sha, name)| (name == HEAD_REF).then_some(sha))
                .map(str::to_string);
            head.ok_or_else(|| format!("`{url}` has no `{HEAD_REF}` ref"))
        }
    }
}

pub(crate) fn checkout_dir(root: &Path, rev: &str) -> PathBuf {
    root.join(REPOS_DIR).join(rev)
}

pub(crate) fn checkout(root: &Path, url: &str, rev: &str) -> Result<PathBuf, String> {
    let url = canonical_url(url);
    let destination = checkout_dir(root, rev);
    if destination.join(CARGO_MANIFEST_FILE).is_file() {
        return Ok(destination);
    }
    std::fs::create_dir_all(root).map_err(|error| format!("{}: {error}", root.display()))?;
    let repos = root.join(REPOS_DIR);
    std::fs::create_dir_all(&repos).map_err(|error| format!("{}: {error}", repos.display()))?;
    let temp = root.join(format!("{TEMP_PREFIX}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    let result = clone_into(&temp, &url, rev, &destination);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    result.map(|()| destination)
}

pub(crate) fn source_string(url: &str, reference: Option<&GitRefSelection>, rev: &str) -> String {
    let mut source = format!("git+{url}");
    match reference {
        Some(GitRefSelection::Branch(branch)) => source.push_str(&format!("?branch={branch}")),
        Some(GitRefSelection::Tag(tag)) => source.push_str(&format!("?tag={tag}")),
        Some(GitRefSelection::Rev(rev)) => source.push_str(&format!("?rev={rev}")),
        None => {}
    }
    source.push('#');
    source.push_str(rev);
    source
}

fn clone_into(temp: &Path, url: &str, rev: &str, destination: &Path) -> Result<(), String> {
    let temp_text = temp
        .to_str()
        .ok_or_else(|| format!("{} is not valid utf-8", temp.display()))?;
    run_git(
        &[
            "clone".to_string(),
            "--filter=blob:none".to_string(),
            "--no-checkout".to_string(),
            url.to_string(),
            temp_text.to_string(),
        ],
        url,
    )?;
    run_git(
        &[
            "-C".to_string(),
            temp_text.to_string(),
            "checkout".to_string(),
            "--detach".to_string(),
            rev.to_string(),
        ],
        url,
    )?;
    let git_dir = temp.join(".git");
    std::fs::remove_dir_all(&git_dir).map_err(|error| format!("{}: {error}", git_dir.display()))?;
    if destination.exists() {
        std::fs::remove_dir_all(destination)
            .map_err(|error| format!("{}: {error}", destination.display()))?;
    }
    std::fs::rename(temp, destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    Ok(())
}

fn ls_remote(url: &str, pattern: Option<&str>) -> Result<String, String> {
    let url = &canonical_url(url);
    let mut args = vec![LS_REMOTE.to_string()];
    args.push(url.to_string());
    if let Some(pattern) = pattern {
        args.push(pattern.to_string());
    }
    run_git(&args, url)
}

pub(crate) fn canonical_url(url: &str) -> String {
    let Some(rest) = url.strip_prefix("file://") else {
        return url.to_string();
    };
    let normalized = rest.replace('\\', "/");
    let trimmed = normalized.trim_start_matches('/');
    let bytes = trimmed.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    match drive {
        true => format!("file:///{trimmed}"),
        false => url.to_string(),
    }
}

fn ls_remote_refs(output: &str) -> impl Iterator<Item = (&str, &str)> {
    output.lines().filter_map(|line| {
        let (sha, reference) = line.split_once('\t')?;
        Some((sha, reference.trim()))
    })
}

fn find_ref<'a>(output: &'a str, reference: &str) -> Option<&'a str> {
    ls_remote_refs(output)
        .find_map(|(sha, name)| (name == reference).then_some(sha))
}

fn run_git(args: &[String], url: &str) -> Result<String, String> {
    // a credential prompt would hang a non-interactive build forever
    let output = Command::new(GIT)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .map_err(|error| format!("failed to run git for `{url}`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` for `{url}` failed: {}",
            args.join(" "),
            stderr_tail(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(STDERR_TAIL_LINES);
    lines[start..].join(", ")
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::process::Command;

    use super::GIT;

    const CI_ENV: &str = "CI";

    pub(crate) struct TestRepo {
        dir: PathBuf,
    }

    impl TestRepo {
        pub(crate) fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-git-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let repo = Self { dir };
            repo.git(&["init"]);
            repo
        }

        pub(crate) fn git(&self, args: &[&str]) -> String {
            let output = Command::new(GIT)
                .args(args)
                .current_dir(&self.dir)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        }

        pub(crate) fn write(&self, relative: &str, contents: &str) {
            let path = self.dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
        }

        pub(crate) fn commit(&self, message: &str) -> String {
            self.git(&["add", "-A"]);
            self.git(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-m",
                message,
            ]);
            self.git(&["rev-parse", "HEAD"]).trim().to_string()
        }

        pub(crate) fn tag(&self, name: &str) {
            self.git(&["tag", "-a", name, "-m", name]);
        }

        pub(crate) fn url(&self) -> String {
            let path = self.dir.display().to_string().replace('\\', "/");
            format!("file:///{path}")
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    pub(crate) fn git_available() -> bool {
        Command::new(GIT)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    // without git the git tests cannot run, CI runners must provide it
    pub(crate) fn git_or_skip() -> bool {
        if git_available() {
            return true;
        }
        if std::env::var_os(CI_ENV).is_some() {
            panic!("git is missing, but CI runners must provide it");
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::test_support::{git_or_skip, TestRepo};
    use super::{
        canonical_url, checkout, checkout_dir, resolve_ref, source_string, GitRefSelection,
    };

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rusted-git-fixture-{}-{name}", std::process::id()));
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

    fn repo_with_two_commits(name: &str) -> TestRepo {
        let repo = TestRepo::new(name);
        repo.write("Cargo.toml", "[package]\nname = \"foo\"\nversion = \"1\"\n");
        repo.write("src/lib.rs", "pub fn value() {}\n");
        repo.commit("first");
        repo.write("src/lib.rs", "// second\n");
        repo.commit("second");
        repo
    }

    #[test]
    fn resolve_ref_finds_a_branch() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("ref-branch");
        repo.write("Cargo.toml", "[package]\nname = \"foo\"\n");
        let head = repo.commit("init");
        repo.git(&["branch", "dev"]);
        let sha = resolve_ref(&repo.url(), Some(&GitRefSelection::Branch("dev".to_string())))
            .unwrap();
        assert_eq!(sha, head);

        let error = resolve_ref(&repo.url(), Some(&GitRefSelection::Branch("missing".to_string())))
            .unwrap_err();
        assert!(error.contains(&repo.url()), "{error}");
    }

    #[test]
    fn resolve_ref_prefers_the_peeled_annotated_tag_sha() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("ref-tag");
        repo.write("Cargo.toml", "[package]\nname = \"foo\"\n");
        let head = repo.commit("init");
        repo.tag("v1.0.0");
        let tag_sha = repo
            .git(&["rev-parse", "v1.0.0"])
            .trim()
            .to_string();
        assert_ne!(tag_sha, head);
        let sha =
            resolve_ref(&repo.url(), Some(&GitRefSelection::Tag("v1.0.0".to_string()))).unwrap();
        assert_eq!(sha, head);
    }

    #[test]
    fn resolve_ref_matches_full_and_prefix_revs() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("ref-rev");
        repo.write("Cargo.toml", "[package]\nname = \"foo\"\n");
        let head = repo.commit("init");
        let full = resolve_ref(&repo.url(), Some(&GitRefSelection::Rev(head.clone()))).unwrap();
        assert_eq!(full, head);
        let prefix =
            resolve_ref(&repo.url(), Some(&GitRefSelection::Rev(head[..7].to_string()))).unwrap();
        assert_eq!(prefix, head);
        assert!(resolve_ref(&repo.url(), Some(&GitRefSelection::Rev("missing".to_string())))
            .unwrap_err()
            .contains(&repo.url()));
    }

    #[test]
    fn resolve_ref_defaults_to_head() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("ref-head");
        repo.write("Cargo.toml", "[package]\nname = \"foo\"\n");
        let head = repo.commit("init");
        let sha = resolve_ref(&repo.url(), None).unwrap();
        assert_eq!(sha, head);
    }

    #[test]
    fn checkout_creates_a_clean_directory_once() {
        if !git_or_skip() {
            return;
        }
        let repo = TestRepo::new("checkout");
        repo.write("Cargo.toml", "[package]\nname = \"foo\"\nversion = \"1\"\n");
        repo.write("src/lib.rs", "pub fn value() {}\n");
        let head = repo.commit("init");
        let cache = Fixture::new("checkout-cache");
        let dir = checkout(&cache.dir, &repo.url(), &head).unwrap();
        assert_eq!(dir, checkout_dir(&cache.dir, &head));
        assert!(!dir.join(".git").exists());
        assert!(dir.join("Cargo.toml").is_file());

        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"mutated\"\n").unwrap();
        let again = checkout(&cache.dir, &repo.url(), &head).unwrap();
        let text = std::fs::read_to_string(again.join("Cargo.toml")).unwrap();
        assert!(text.contains("mutated"));
    }

    #[test]
    fn canonical_url_normalizes_local_paths() {
        assert_eq!(
            canonical_url("file://C:\\repo\\dep"),
            "file:///C:/repo/dep"
        );
        assert_eq!(canonical_url("file://C:/repo/dep"), "file:///C:/repo/dep");
        assert_eq!(
            canonical_url("file:///C:\\repo\\dep"),
            "file:///C:/repo/dep"
        );
        assert_eq!(canonical_url("file:///srv/repo"), "file:///srv/repo");
        assert_eq!(
            canonical_url("https://example.com/repo"),
            "https://example.com/repo"
        );
    }

    #[test]
    fn source_string_covers_every_reference_form() {
        let url = "https://example.com/foo";
        assert_eq!(
            source_string(url, None, "abc"),
            "git+https://example.com/foo#abc"
        );
        assert_eq!(
            source_string(
                url,
                Some(&GitRefSelection::Branch("main".to_string())),
                "abc"
            ),
            "git+https://example.com/foo?branch=main#abc"
        );
        assert_eq!(
            source_string(url, Some(&GitRefSelection::Tag("v1".to_string())), "abc"),
            "git+https://example.com/foo?tag=v1#abc"
        );
        assert_eq!(
            source_string(url, Some(&GitRefSelection::Rev("def456".to_string())), "abc"),
            "git+https://example.com/foo?rev=def456#abc"
        );
    }

    #[test]
    fn two_commits_have_two_checkout_directories() {
        if !git_or_skip() {
            return;
        }
        let repo = repo_with_two_commits("two-dirs");
        let shas = repo.git(&["rev-list", "HEAD"]).trim().to_string();
        let mut revs = shas.lines();
        let head = revs.next().unwrap().to_string();
        let parent = revs.next().unwrap().to_string();
        let cache = Fixture::new("two-dirs-cache");
        let head_dir = checkout(&cache.dir, &repo.url(), &head).unwrap();
        let parent_dir = checkout(&cache.dir, &repo.url(), &parent).unwrap();
        assert_ne!(head_dir, parent_dir);
    }
}

