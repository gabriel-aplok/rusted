# Contributing

## Setup

You need rustc 1.85 or newer, a POSIX shell, `curl`, and `tar`. Python 3 is used by the test scripts. Cargo is optional.

Build the bootstrap binary:

    ./scripts/bootstrap.sh

Set `RUSTED_BOOTSTRAP_OPT_LEVEL=0` for faster local rebuilds. The default is 2. The script skips the rustc call when the binary is newer than every file under `src/` and the opt level has not changed.

## Dogfooding

The fast loop uses the stage 0 binary from rustc:

    scripts/dev.sh build

To let rusted compile rusted itself:

    RUSTED_DEV_STAGE=rusted scripts/dev.sh build
    RUSTED_DEV_STAGE=rusted scripts/dev.sh test

The dogfood binary is separate, so bad dogfood builds never break the stage 0 loop.

## Tests

    ./scripts/test.sh unit
    ./scripts/test.sh integration
    ./scripts/test.sh all

Network tests run only with `RUSTED_NETWORK_TESTS=1`. The full suite also runs the self-host loop.

If cargo is installed, compare lockfiles with:

    ./scripts/lock-compare.sh

## Style

- Run `rustfmt --edition 2021 src/main.rs` before sending a change.
- Keep comments rare. Use a comment only when the code cannot show the reason.
- Do not add new external Rust dependencies. The tool builds with rustc alone.
- Prefer standard library code. Shell out only for established system tools such as `curl` and `tar`.
- Keep user-facing text in plain technical English.

## Commits

Use Conventional Commits, one concern per commit:

    feat: add cache warm flag
    fix: reject a broken lockfile
    docs: explain linker selection

Keep the subject imperative, lowercase, and 72 characters or less.

## Fixtures

Add a fixture under `tests/fixtures/` when a change needs a real manifest or workspace. Keep fixtures small. Generated benchmark projects belong in `target/`, not in the repository.
