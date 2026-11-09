#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

./scripts/bootstrap.sh
bin=target/bootstrap/rusted
tmp=target/bench-vs
rm -rf "$tmp"
mkdir -p "$tmp"

now() {
    date +%s%N
}

elapsed_ms() {
    echo $(( ($2 - $1) / 1000000 ))
}

fixture=tests/fixtures/local-workspace/Cargo.toml

echo "Tool availability:"
for tool in cargo sccache mold ld.lld lld; do
    if command -v "$tool" >/dev/null 2>&1; then
        echo "  $tool: $(command -v "$tool")"
    else
        echo "  $tool: not installed"
    fi
done
echo

rusted_start=$(now)
RUSTED_CACHE_DIR="$tmp/rusted-cache" "$bin" build --manifest-path "$fixture" --target-dir "$tmp/rusted" -q
rusted_end=$(now)
echo "rusted cold build: $(elapsed_ms "$rusted_start" "$rusted_end") ms"

if command -v cargo >/dev/null 2>&1; then
    cargo_start=$(now)
    CARGO_TARGET_DIR="$tmp/cargo" cargo build --manifest-path "$fixture" -q
    cargo_end=$(now)
    echo "cargo cold build: $(elapsed_ms "$cargo_start" "$cargo_end") ms"

    cargo_warm_start=$(now)
    CARGO_TARGET_DIR="$tmp/cargo" cargo build --manifest-path "$fixture" -q
    cargo_warm_end=$(now)
    echo "cargo warm build: $(elapsed_ms "$cargo_warm_start" "$cargo_warm_end") ms"

    if command -v sccache >/dev/null 2>&1; then
        sccache_start=$(now)
        CARGO_TARGET_DIR="$tmp/cargo-sccache" RUSTC_WRAPPER=sccache \
            cargo build --manifest-path "$fixture" -q
        sccache_end=$(now)
        echo "cargo with sccache cold build: $(elapsed_ms "$sccache_start" "$sccache_end") ms"
    else
        echo "sccache comparison skipped: sccache is not installed"
    fi
else
    echo "cargo comparison skipped: cargo is not installed"
fi

if command -v mold >/dev/null 2>&1; then
    echo "mold detected, measuring mold linker"
elif command -v ld.lld >/dev/null 2>&1 || command -v lld >/dev/null 2>&1; then
    echo "lld detected, measuring lld linker"
else
    echo "mold and lld are not installed, measuring the system linker only"
fi

linker_config() {
    name=$1
    value=$2
    dir=$tmp/config-$name
    mkdir -p "$dir/rusted"
    printf 'linker = "%s"\n' "$value" > "$dir/rusted/config.toml"
}

measure_linker() {
    name=$1
    start=$(now)
    XDG_CONFIG_HOME="$tmp/config-$name" RUSTED_CACHE_DIR="$tmp/link-$name-cache" \
        "$bin" build --manifest-path "$fixture" --target-dir "$tmp/link-$name" -q
    end=$(now)
    echo "$name linker build: $(elapsed_ms "$start" "$end") ms"
}

echo
linker_config system system
measure_linker system
if command -v mold >/dev/null 2>&1; then
    linker_config mold mold
    measure_linker mold
fi
if command -v ld.lld >/dev/null 2>&1 || command -v lld >/dev/null 2>&1; then
    linker_config lld lld
    measure_linker lld
fi
