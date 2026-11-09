#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

./scripts/bootstrap.sh
bin=target/bootstrap/rusted
tmp=target/bench
rm -rf "$tmp"
mkdir -p "$tmp"

now() {
    date +%s%N
}

elapsed_ms() {
    echo $(( ($2 - $1) / 1000000 ))
}

fixture=tests/fixtures/local-workspace/Cargo.toml

cold_start=$(now)
RUSTED_CACHE_DIR="$tmp/cold-cache" "$bin" build --manifest-path "$fixture" --target-dir "$tmp/cold" -q
cold_end=$(now)
store_kb=$(du -sk "$tmp/cold-cache" | cut -f1)
target_kb=$(du -sk "$tmp/cold" | cut -f1)
artifacts=$(find "$tmp/cold-cache/v1/artifacts" -mindepth 1 -maxdepth 1 -type d | wc -l)

warm_start=$(now)
RUSTED_CACHE_DIR="$tmp/cold-cache" "$bin" build --manifest-path "$fixture" --target-dir "$tmp/warm" -q
warm_end=$(now)

workspace_start=$(now)
RUSTED_CACHE_DIR="$tmp/cold-cache" "$bin" build --workspace --manifest-path "$fixture" --target-dir "$tmp/workspace" -q
workspace_end=$(now)

selfhost_start=$(now)
RUSTED_CACHE_DIR="$tmp/selfhost-cache" "$bin" build --manifest-path rusted.toml --target-dir "$tmp/selfhost" -q
selfhost_end=$(now)

chain_fixture=$tmp/deep-chain/Cargo.toml
python3 - "$tmp/deep-chain" <<'PY'
import os
import sys

root = sys.argv[1]
os.makedirs(os.path.join(root, "src"), exist_ok=True)
depth = 10
with open(os.path.join(root, "Cargo.toml"), "w") as handle:
    handle.write(
        '[package]\nname = "deep-app"\nversion = "0.1.0"\nedition = "2021"\n\n'
        '[workspace]\nmembers = []\n\n[dependencies]\ndep0 = { path = "dep0" }\n'
    )
with open(os.path.join(root, "src", "main.rs"), "w") as handle:
    handle.write('fn main() { println!("{}", dep0::value()); }\n')
for index in range(depth):
    directory = os.path.join(root, f"dep{index}")
    os.makedirs(os.path.join(directory, "src"), exist_ok=True)
    with open(os.path.join(directory, "Cargo.toml"), "w") as handle:
        handle.write(
            f'[package]\nname = "dep{index}"\nversion = "0.1.0"\nedition = "2021"\n'
        )
        if index + 1 < depth:
            handle.write(
                f'\n[dependencies]\ndep{index + 1} = {{ path = "../dep{index + 1}" }}\n'
            )
    tail = f"dep{index + 1}::value() + 1" if index + 1 < depth else "0"
    body = f"pub fn value() -> u32 {{ {tail} }}\n"
    body += "\n".join(
        f"pub fn work{n}(x: u64) -> u64 {{ x.wrapping_mul({n + 1}).wrapping_add({n}) }}"
        for n in range(1200)
    )
    body += "\n"
    with open(os.path.join(directory, "src", "lib.rs"), "w") as handle:
        handle.write(body)
PY

chain_start=$(now)
RUSTED_CACHE_DIR="$tmp/chain-pipeline-cache" "$bin" build --manifest-path "$chain_fixture" --target-dir "$tmp/chain-pipeline" -q
chain_end=$(now)
chain_nopipe_start=$(now)
RUSTED_NO_PIPELINE=1 RUSTED_CACHE_DIR="$tmp/chain-nopipe-cache" "$bin" build --manifest-path "$chain_fixture" --target-dir "$tmp/chain-nopipe" -q
chain_nopipe_end=$(now)

registry_result="skipped, set RUSTED_NETWORK_TESTS=1"
if [ "${RUSTED_NETWORK_TESTS:-0}" = "1" ]; then
    registry_start=$(now)
    RUSTED_CACHE_DIR="$tmp/registry-cache" "$bin" build --manifest-path tests/fixtures/registry/Cargo.toml --target-dir "$tmp/registry" -q
    registry_end=$(now)
    registry_result="$(elapsed_ms "$registry_start" "$registry_end") ms"
fi

rustc_version=$("$bin" --version)
machine=$(uname -srm)

echo "| Measurement | Result |"
echo "| --- | --- |"
echo "| Machine | $machine |"
echo "| Tool | $rustc_version |"
echo "| Cold dev build | $(elapsed_ms "$cold_start" "$cold_end") ms |"
echo "| Warm dev build, new target dir | $(elapsed_ms "$warm_start" "$warm_end") ms, 0 rustc calls |"
echo "| Workspace build | $(elapsed_ms "$workspace_start" "$workspace_end") ms |"
echo "| Registry fixture | $registry_result |"
echo "| Self-host stage 1 | $(elapsed_ms "$selfhost_start" "$selfhost_end") ms |"
echo "| Deep chain, pipelined | $(elapsed_ms "$chain_start" "$chain_end") ms |"
echo "| Deep chain, non-pipelined | $(elapsed_ms "$chain_nopipe_start" "$chain_nopipe_end") ms |"
echo "| Store after cold build | $store_kb KiB, $artifacts artifacts |"
echo "| Project target after cold build | $target_kb KiB |"
