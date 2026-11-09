#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is not installed, lock comparison skipped"
    exit 0
fi

RUSTED_BOOTSTRAP_OPT_LEVEL="${RUSTED_BOOTSTRAP_OPT_LEVEL:-0}" ./scripts/bootstrap.sh
bin=target/bootstrap/rusted
tmp=target/lock-compare
rm -rf "$tmp"
mkdir -p "$tmp"

status=0
for name in local-workspace registry multi-version ecosystem feature-context proc-macro build-script; do
    source_dir=tests/fixtures/$name
    work=$tmp/$name
    cp -R "$source_dir" "$work"
    rm -f "$work/Cargo.lock"

    cargo generate-lockfile --manifest-path "$work/Cargo.toml" -q
    cp "$work/Cargo.lock" "$tmp/$name.cargo"
    rm -f "$work/Cargo.lock"

    RUSTED_CACHE_DIR="$tmp/cache" "$bin" update --manifest-path "$work/Cargo.toml" -q
    cp "$work/Cargo.lock" "$tmp/$name.rusted"

    grep -v '^# This file' "$tmp/$name.cargo" > "$tmp/$name.cargo.norm"
    grep -v '^# This file' "$tmp/$name.rusted" > "$tmp/$name.rusted.norm"

    if diff -q "$tmp/$name.cargo.norm" "$tmp/$name.rusted.norm" >/dev/null; then
        echo "lockfile match: $name"
    else
        echo "lockfile mismatch: $name" >&2
        diff -u "$tmp/$name.cargo.norm" "$tmp/$name.rusted.norm" >&2 || true
        status=1
    fi
done

exit "$status"
