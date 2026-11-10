# Changelog

All notable changes to this project are recorded here. The format follows Keep a Changelog. Versions follow semantic versioning once the tool reaches 1.0.

## Unreleased

### Added

- Tag release pipeline with musl Linux, aarch64 Linux, macOS arm64, and Windows binaries, sha256 checksums, and an install script.
- Windows installer in PowerShell: sha256 check, install in LOCALAPPDATA, and user Path update.

### Added

- Git dependencies: `foo = { git = "<url>", branch | tag | rev }` with pinned full revisions in `Cargo.lock`, per-revision source caching, `--locked`, `--offline`, and `rusted update` re-pinning.
- `rusted doc` builds rustdoc for the workspace lib, bin, and proc-macro targets into `<target>/doc`, reusing compiled dependency artifacts, with `--open`, `--document-private-items`, and `--` pass-through.
- `--target <TRIPLE>` on `build`, `check`, `run`, `test`, `doc`, and `clippy`: cargo-compatible `<target>/<triple>/<profile>` layout, host-rooted build scripts and proc macros, per-triple cfg caches and artifact keys, and cross-run refusals.
- `rusted clippy` lints the workspace with clippy-driver, reusing the check pipeline with a separate artifact cache namespace. Extra arguments after `--` reach clippy-driver.
- `rusted fmt` formats workspace code with rustfmt, grouping one rustfmt invocation per edition, with `--check` support and pass-through arguments after `--`.
- `build`, `check`, `run`, and `test` accept `--example <NAME>`, `--examples`, `--bench <NAME>`, and `--benches`. Examples build into `<target>/<profile>/examples`, benches compile through the test harness and run with `test`. Benches that declare `harness = false` report an error for now.

### Fixed

- Lockfiles now include dev-only optional dependencies in every build mode.
- Build-dependencies now resolve and compile in a separate feature context.
- Concurrent units now pass unique rustc metadata to avoid object file collisions.
- Registry resolution now backtracks to one version when compatible requirements disagree.
- Dev builds default to incremental compilation, matching Cargo.
- Proc-macro dependency subtrees now compile in a host feature context. A crate used by both a proc macro and the target compiles twice with independent features, while the lockfile keeps one entry.
- The S3 remote cache was driven end to end against a real Garage S3 server: cold fetch, push, second cache reuse, and remote gc with SigV4 signing.
- Registry resolution matches the semver crate on a 168-case comparator matrix, including pre-release and wildcard comparators.
- Warm builds cache per-file source hashes by size and mtime.
- The host cfg table is cached, so warm builds spawn rustc once instead of three times.
- `scripts/dev.sh` makes dogfooding configurable: stage 0 from rustc, or rusted compiled and driven by rusted itself.

## 0.1.0 - 2026-11-09

### Added

- Cargo-compatible manifests through `Cargo.toml` and `rusted.toml`.
- Workspace loading with members, globs, excludes, default members, inheritance, and outside members.
- Sparse index resolution, checksum-verified downloads, safe extraction, and Cargo.lock v4 output.
- Rustc-only builds with a shared content-addressed artifact cache and hardlinked outputs.
- Parallel scheduling with pipelined library metadata.
- Build scripts, proc macros, resolver 2 features, target dependencies, and profile overrides.
- Commands for `new`, `init`, `build`, `check`, `run`, `test`, `clean`, `add`, `remove`, `update`, `tree`, and `metadata`.
- Cache inspection, garbage collection, clean scopes, and JSON cache output.
- Stable JSON build lines and workspace metadata.
- Linker detection for mold and lld.
- S3-compatible remote artifact cache with checksum verification and offline fallback.
- Self-host stages 0, 1, and 2 with unit test runs through each stage.
- Integration tests, network tests, lockfile comparison against cargo, and benchmarks.
