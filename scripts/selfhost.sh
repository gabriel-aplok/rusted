#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

manifest=rusted.toml
stage=target/selfhost
log=$stage/rustc.log
rm -rf "$stage"
mkdir -p "$stage"
export RUSTED_RUSTC_LOG="$log"

RUSTED_BOOTSTRAP_OPT_LEVEL="${RUSTED_BOOTSTRAP_OPT_LEVEL:-0}" ./scripts/bootstrap.sh
stage0=target/bootstrap/rusted

"$stage0" build --manifest-path "$manifest" --target-dir "$stage/stage1-build" -q
cp "$stage/stage1-build/debug/rusted" "$stage/stage1"

"$stage/stage1" build --manifest-path "$manifest" --target-dir "$stage/stage2-build" -q
cp "$stage/stage2-build/debug/rusted" "$stage/stage2"

stage1_version=$("$stage/stage1" --version)
stage2_version=$("$stage/stage2" --version)
if [ "$stage1_version" != "$stage2_version" ]; then
    echo "stage versions differ: $stage1_version vs $stage2_version" >&2
    exit 1
fi

"$stage/stage1" test --manifest-path "$manifest" --target-dir "$stage/stage1-tests" -q
"$stage/stage2" test --manifest-path "$manifest" --target-dir "$stage/stage2-tests" -q

: > "$log"
"$stage/stage2" build --manifest-path "$manifest" --target-dir "$stage/stage2-build" -q
if [ -s "$log" ]; then
    echo "second build ran rustc:" >&2
    cat "$log" >&2
    exit 1
fi

rm -rf "$stage/stage1-build" "$stage/stage2-build" "$stage/stage1-tests" "$stage/stage2-tests" "$log"

echo "selfhost passed: $stage1_version"
