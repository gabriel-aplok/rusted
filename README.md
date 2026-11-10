# Rusted

A Cargo-compatible Rust build tool that bootstraps with rustc alone. It keeps one content-addressed cache for compiled artifacts and can build itself.

Pre-1.0. It reads `Cargo.toml` and `rusted.toml`, resolves crates.io and git dependencies, writes `Cargo.lock`, and builds workspaces with path, registry, and git dependencies. Build scripts, proc macros, resolver 2 features, workspace inheritance, profiles, editions through 2024, pipelined library builds, linker selection, cross-compilation through `--target`, and an S3-compatible remote cache are supported.

## Performance

Measured on a 2-core Intel Celeron 6305 at 1.8 GHz. The project is `tests/fixtures/local-workspace`, five library crates plus one application.

| Measurement | Rusted | cargo |
| --- | --- | --- |
| Cold build | 0.47 s | 0.60 s |
| Warm build | 0.04 s, 0 rustc calls | 0.04 s |
| Disk after cold build | 508 KiB per project | 12 MiB per project |

Measured on an AMD Ryzen 5 4600H laptop running Windows 10 x64 with rustc 1.99.1 MSVC, same project and method. The cargo build used an identical manifest copy without the `[profile.fast]` block, which cargo rejects.

| Measurement | Rusted | cargo |
| --- | --- | --- |
| Cold build | 0.53 s | 0.56 s |
| Warm build | 0.09 s, 0 rustc calls | 0.06 s |
| Disk after cold build | 0.14 MB per project | 3.86 MB per project |

Artifacts live in the shared store, so ten projects that use the same crates compile those crates once. Warm builds skip rustc entirely through content-addressed hits. Source hashing is cached per machine by size and mtime, and dev builds use incremental compilation by default.

## Requirements

Linux, or Windows 10 and 11 x64 with rustc 1.85 or newer, `curl`, and `tar`. Git dependencies additionally need `git` on every platform. Using Rusted on Windows needs no shell, `curl.exe` and `tar.exe` ship with Windows. The `sh` scripts are developer helpers and need WSL or Git Bash, use `scripts/bootstrap.ps1` and `scripts/test.ps1` instead. macOS (Apple Silicon) runs the same rustc-only build and is covered by CI. Tests use `python3`. Cargo is optional and only used by `scripts/lock-compare.sh`.

## Quick start

    ./scripts/bootstrap.sh
    ./target/bootstrap/rusted new hello
    ./target/bootstrap/rusted run --manifest-path hello/Cargo.toml
    install -m 755 target/bootstrap/rusted ~/.local/bin/rusted

## Commands

    rusted new <PATH>
    rusted init [PATH]
    rusted build
    rusted check
    rusted clippy
    rusted doc
    rusted fmt
    rusted run
    rusted test
    rusted add <NAME>
    rusted remove <NAME>
    rusted update
    rusted tree
    rusted metadata
    rusted clean [--project|--store|--index|--source|--all]
    rusted cache info
    rusted cache gc --max-size <SIZE> [--dry-run] [--remote]

Build flags include `--release`, `--profile`, `--workspace`, `-p`, `--features`, `--all-features`, `--no-default-features`, `--locked`, `--frozen`, `--offline`, `--target <TRIPLE>`, `-j`, `--example <NAME>`, `--examples`, `--bench <NAME>`, `--benches`, `--stats`, and `--message-format=json`. Run `rusted help` for the full list.

## Configuration

Global config lives at `${XDG_CONFIG_HOME:-~/.config}/rusted/config.toml`. Keys are `manifest`, `cache_dir`, `target_dir`, `jobs`, `offline`, `registry.index`, `linker`, and `profile.*`. Environment overrides are `RUSTED_MANIFEST`, `RUSTED_CACHE_DIR`, `RUSTED_TARGET_DIR`, `RUSTED_JOBS`, `RUSTED_OFFLINE`, and `RUSTED_RUSTC_LOG`.

The default linker mode prefers `mold`, then `lld`, then the system linker. A profile or package override wins.

## Cache

The store lives at `${XDG_CACHE_HOME:-~/.cache}/rusted/v1`. `--force` rebuilds even on a hit. `clean` removes the project target by default. `clean --store`, `--index`, `--source`, and `--all` remove cache scopes. `cache info` reports sizes. `cache gc --max-size <SIZE>` removes least recently used artifacts and skips artifacts held by a running build.

Library builds are pipelined. A dependency publishes its rmeta before its rlib finishes, so library dependents start early. Bins wait for complete rlibs.

## Remote cache

Set `RUSTED_REMOTE_CACHE=s3://bucket/prefix` to share artifacts. Credentials come from `RUSTED_REMOTE_ACCESS_KEY` and `RUSTED_REMOTE_SECRET_KEY`. `RUSTED_REMOTE_ENDPOINT` selects a MinIO-style endpoint. `RUSTED_REMOTE_REGION` defaults to `us-east-1`.

Uploads happen after a successful local build. Fetches are verified before install. Remote failures fall back to a local build with a warning. Offline mode never contacts the remote cache. `cache gc --remote` applies the same limit to remote objects.

## Self-host

    ./scripts/selfhost.sh
    ./target/selfhost/stage2 --version

Stage 0 is built by rustc. Stage 1 is built by stage 0, and stage 2 is built by stage 1. Both stages run the unit suite. `scripts/test.sh all` runs this loop with a fresh cache and offline mode, then checks that the second build makes zero rustc calls.

## Dogfooding

Development defaults to the stage 0 binary that rustc builds:

    scripts/dev.sh build --release

To make Rusted compile Rusted itself and use that binary for everything:

    RUSTED_DEV_STAGE=rusted scripts/dev.sh build --release
    RUSTED_DEV_STAGE=rusted scripts/dev.sh test

In that mode every compilation of the tool is driven by Rusted. The dogfood binary lives in `target/dogfood`, so the fast stage 0 loop keeps working. `RUSTED_DEV_TARGET` moves it. `scripts/selfhost.sh` is the stricter version of the same idea: it builds three stages, and `scripts/test.sh all` runs that loop with a fresh cache, offline mode, and zero-rustc assertions.

## Tests

    ./scripts/test.sh unit
    ./scripts/test.sh integration
    ./scripts/test.sh all

Network tests are gated by `RUSTED_NETWORK_TESTS=1`. `scripts/lock-compare.sh` compares Rusted lockfiles with cargo output for six fixtures when cargo is installed.

## Contributing

See `CONTRIBUTING.md`. Report security issues through the private flow in `SECURITY.md`. Changes are tracked in `CHANGELOG.md`.

## Known gaps

- The S3 client is verified against a local Garage S3 server and an in-process mock. A public bucket test is pending.

## License

MIT. See `LICENSE`.
