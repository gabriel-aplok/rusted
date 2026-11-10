#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

mode=${1:-all}
case "$mode" in
    unit|integration|all) ;;
    *)
        echo "usage: scripts/test.sh [unit|integration|all]" >&2
        exit 2
        ;;
esac

if [ "$mode" = unit ] || [ "$mode" = all ]; then
    mkdir -p target/tests
    rustc --edition 2021 --test src/main.rs -o target/tests/rusted-unit
    ./target/tests/rusted-unit
fi

if [ "$mode" = integration ] || [ "$mode" = all ]; then
    RUSTED_BOOTSTRAP_OPT_LEVEL="${RUSTED_BOOTSTRAP_OPT_LEVEL:-0}" ./scripts/bootstrap.sh
    bin=target/bootstrap/rusted

    it=target/it
    rm -rf "$it"
    export RUSTED_CACHE_DIR="$it/cache"
    export RUSTED_RUSTC_LOG="$it/rustc.log"

    "$bin" --version | grep -q '^rusted '
    "$bin" --help | grep -q 'build'
    "$bin" build --help | grep -q 'Usage: rusted build'

    new_package="$it/new-package"
    "$bin" new "$new_package" >/dev/null
    test -f "$new_package/Cargo.toml"
    "$bin" build --manifest-path "$new_package/Cargo.toml" --target-dir "$it/new-build" -q
    "$it/new-build/debug/new-package" | grep -q 'Hello, world!'

    init_package="$it/init-package"
    mkdir -p "$init_package"
    "$bin" init "$init_package" >/dev/null
    test -f "$init_package/Cargo.toml"

    if "$bin" not-a-command >/dev/null 2>&1; then
        echo "unknown command should fail" >&2
        exit 1
    fi

    fixture=tests/fixtures/local-workspace/Cargo.toml

    clear_log() {
        : > "$RUSTED_RUSTC_LOG"
    }

    expect_no_rustc() {
        if [ -s "$RUSTED_RUSTC_LOG" ]; then
            echo "expected a cache hit, but rustc ran:" >&2
            cat "$RUSTED_RUSTC_LOG" >&2
            exit 1
        fi
    }

    expect_rustc() {
        if [ ! -s "$RUSTED_RUSTC_LOG" ]; then
            echo "expected a cache miss, but rustc did not run" >&2
            exit 1
        fi
    }

    "$bin" build --manifest-path "$fixture" --target-dir "$it/local" -q
    test -x "$it/local/debug/app"
    expect_rustc

    clear_log
    "$bin" build --manifest-path "$fixture" --target-dir "$it/local" -q
    expect_no_rustc

    stats=$("$bin" build --stats --manifest-path "$fixture" --target-dir "$it/local" -q)
    echo "$stats" | grep -q 'rustc_calls=0'
    echo "$stats" | grep -q 'cache_hits=5'

    stats=$(RUSTED_CACHE_DIR="$it/cache3" "$bin" build -j 2 --stats --manifest-path "$fixture" --target-dir "$it/jobs" -q)
    echo "$stats" | grep -q 'rustc_calls=5'

    "$bin" build --profile fast --manifest-path "$fixture" --target-dir "$it/profiles" -q
    test -x "$it/profiles/fast/app"

    RUSTED_CACHE_DIR="$it/inc-flag-cache" "$bin" build --incremental --manifest-path "$fixture" --target-dir "$it/incremental" -q
    test -d "$it/incremental/debug/incremental"

    clear_log
    RUSTED_CACHE_DIR="$it/inc-cache" "$bin" build --manifest-path "$fixture" --target-dir "$it/inc-default" -q
    grep -q -- '-C incremental=' "$RUSTED_RUSTC_LOG"
    clear_log
    RUSTED_CACHE_DIR="$it/inc-cache" "$bin" build --release --manifest-path "$fixture" --target-dir "$it/inc-release" -q
    if grep -q -- '-C incremental=' "$RUSTED_RUSTC_LOG"; then
        echo "release build should not use incremental compilation" >&2
        exit 1
    fi

    set +e
    "$bin" build --profile fast --release --manifest-path "$fixture" --target-dir "$it/both" -q >/dev/null 2>&1
    code=$?
    set -e
    if [ "$code" -ne 1 ]; then
        echo "release with profile should exit 1, got $code" >&2
        exit 1
    fi

    tool_output=$("$bin" run -p core --bin tool --manifest-path "$fixture" --target-dir "$it/tool" -q)
    if [ "$tool_output" != "tool=42" ]; then
        echo "unexpected tool output: $tool_output" >&2
        exit 1
    fi

    fx=tests/fixtures/examples-fixture/Cargo.toml

    "$bin" build --manifest-path "$fx" --target-dir "$it/targets" -q
    test ! -e "$it/targets/debug/examples/adder"

    clear_log
    "$bin" build --examples --manifest-path "$fx" --target-dir "$it/targets" -q
    test -x "$it/targets/debug/examples/adder"
    expect_rustc

    vendor_fx=tests/fixtures/vendor-offline/Cargo.toml
    vendor_cache="$it/vendor-cache"
    RUSTED_CACHE_DIR="$vendor_cache" "$bin" build --offline \
        --manifest-path "$vendor_fx" --target-dir "$it/vendor" -q
    vendor_output=$(RUSTED_CACHE_DIR="$vendor_cache" "$bin" run --offline \
        --manifest-path "$vendor_fx" --target-dir "$it/vendor" -q)
    if [ "$vendor_output" != "hello vendor" ]; then
        echo "unexpected vendor fixture output: $vendor_output" >&2
        exit 1
    fi

    clear_log
    "$bin" build --examples --manifest-path "$fx" --target-dir "$it/targets" -q
    expect_no_rustc

    example_output=$("$bin" run --example adder --manifest-path "$fx" --target-dir "$it/targets" -q)
    if [ "$example_output" != "adder=7" ]; then
        echo "unexpected example output: $example_output" >&2
        exit 1
    fi

    clear_log
    "$bin" test --bench simple --manifest-path "$fx" --target-dir "$it/targets" -q > "$it/bench.out"
    grep -q 'test result: ok' "$it/bench.out"
    grep -q 'add_works' "$it/bench.out"

    set +e
    "$bin" build --example missing --manifest-path "$fx" --target-dir "$it/targets" -q >/dev/null 2>&1
    code=$?
    set -e
    if [ "$code" -ne 1 ]; then
        echo "unknown example should exit 1, got $code" >&2
        exit 1
    fi

    clear_log
    "$bin" test -p core --manifest-path "$fixture" --target-dir "$it/tests" -q > "$it/tests.out"
    grep -q 'test result: ok' "$it/tests.out"
    grep -q 'flow_uses_dev_dependency' "$it/tests.out"
    expect_rustc

    clear_log
    "$bin" test -p core answer --test-threads 1 --manifest-path "$fixture" --target-dir "$it/tests" -q > "$it/filter.out"
    grep -q 'answer_is_42' "$it/filter.out"

    clear_log
    "$bin" test -p core --no-run --manifest-path "$fixture" --target-dir "$it/tests" -q
    expect_no_rustc

    set +e
    "$bin" test --manifest-path tests/fixtures/failing-test/Cargo.toml --target-dir "$it/failing" -q >/dev/null 2>&1
    code=$?
    set -e
    if [ "$code" -ne 101 ]; then
        echo "failing test should exit 101, got $code" >&2
        exit 1
    fi

    json=$("$bin" build --message-format=json --manifest-path "$fixture" --target-dir "$it/json" -q 2>/dev/null)
    printf '%s\n' "$json" | python3 -c '
import json, sys
for line in sys.stdin:
    if not line.strip():
        continue
    value = json.loads(line)
    reason = value["reason"]
    assert reason in {"compiler-message", "compiler-artifact", "build-finished", "build-stats"}, reason
    if reason == "compiler-artifact":
        assert "package_id" in value and "target" in value
        assert "filenames" in value and isinstance(value["fresh"], bool)
    if reason == "build-finished":
        assert isinstance(value["success"], bool) and "warnings" in value
'
    printf '%s' "$json" | grep -q '"reason":"compiler-artifact"'
    printf '%s' "$json" | grep -q '"reason":"build-finished"'

    json_stats=$("$bin" build --stats --message-format=json --manifest-path "$fixture" --target-dir "$it/json" -q 2>/dev/null)
    printf '%s' "$json_stats" | grep -q '"reason":"build-stats"'
    printf '%s' "$json_stats" | grep -q '"rustc_calls":0'
    printf '%s' "$json_stats" | grep -q '"cache_hits":5'

    json_first=$("$bin" build --force --message-format=json --manifest-path "$fixture" --target-dir "$it/json-order" -q 2>/dev/null)
    json_second=$("$bin" build --force --message-format=json --manifest-path "$fixture" --target-dir "$it/json-order" -q 2>/dev/null)
    if [ "$json_first" != "$json_second" ]; then
        echo "JSON output is not deterministic" >&2
        exit 1
    fi

    set +e
    quiet_stderr=$("$bin" build -q --manifest-path tests/fixtures/broken/Cargo.toml --target-dir "$it/quiet" 2>&1 >/dev/null)
    quiet_code=$?
    set -e
    if [ "$quiet_code" -ne 101 ]; then
        echo "quiet broken build should exit 101, got $quiet_code" >&2
        exit 1
    fi
    if printf '%s' "$quiet_stderr" | grep -q 'Compiling\|Finished'; then
        echo "--quiet printed progress output" >&2
        exit 1
    fi
    printf '%s' "$quiet_stderr" | grep -q 'error'

    set +e
    json=$("$bin" build --message-format=json --manifest-path tests/fixtures/broken/Cargo.toml --target-dir "$it/json-broken" 2>/dev/null)
    code=$?
    set -e
    if [ "$code" -ne 101 ]; then
        echo "broken JSON build should exit 101, got $code" >&2
        exit 1
    fi
    printf '%s\n' "$json" | python3 -c 'import json,sys; [json.loads(line) for line in sys.stdin if line.strip()]'
    printf '%s' "$json" | grep -q '"reason":"compiler-message"'
    printf '%s' "$json" | grep -q '"success":false'

    json=$("$bin" build --message-format=json --manifest-path tests/fixtures/warnings/Cargo.toml --target-dir "$it/warn" 2>/dev/null)
    printf '%s\n' "$json" | python3 -c 'import json,sys; [json.loads(line) for line in sys.stdin if line.strip()]'
    printf '%s' "$json" | grep -q '"warnings":1'

    human=$("$bin" build --force --manifest-path tests/fixtures/warnings/Cargo.toml --target-dir "$it/warn" 2>&1)
    printf '%s' "$human" | grep -q 'warning: 1 warning emitted'

    short=$("$bin" build --force --message-format=short --manifest-path tests/fixtures/warnings/Cargo.toml --target-dir "$it/warn" 2>&1)
    printf '%s' "$short" | grep -q ': warning:'

    clear_log
    "$bin" build --force --manifest-path "$fixture" --target-dir "$it/local" -q
    expect_rustc

    "$bin" build --release --manifest-path "$fixture" --target-dir "$it/local" -q
    test -x "$it/local/release/app"

    output=$("$bin" run --manifest-path "$fixture" --target-dir "$it/local" -q)
    if [ "$output" != "answer=42 left=11 right=31" ]; then
        echo "unexpected run output: $output" >&2
        exit 1
    fi

    set +e
    "$bin" run --manifest-path "$fixture" --target-dir "$it/local" -q -- fail >/dev/null
    code=$?
    set -e
    if [ "$code" -ne 3 ]; then
        echo "run should forward exit code 3, got $code" >&2
        exit 1
    fi

    "$bin" build -p util --manifest-path "$fixture" --target-dir "$it/verbose" -v 2>&1 \
        | grep -q -- '--crate-name util'

    "$bin" check --manifest-path "$fixture" --target-dir "$it/check" -q
    clear_log
    "$bin" check --manifest-path "$fixture" --target-dir "$it/check" -q
    expect_no_rustc

    cp -R tests/fixtures/local-workspace "$it/copy"
    clear_log
    "$bin" build --manifest-path "$it/copy/Cargo.toml" --target-dir "$it/copied" -q
    expect_no_rustc
    "$it/copied/debug/app" | grep -q 'answer=42 left=11 right=31'

    printf 'pub fn value() -> u32 { 12 }\n' > "$it/copy/left/src/lib.rs"
    clear_log
    "$bin" build --manifest-path "$it/copy/Cargo.toml" --target-dir "$it/copied" -q
    if [ "$(wc -l < "$RUSTED_RUSTC_LOG")" -ne 2 ]; then
        echo "edit rebuild should run rustc twice, ran $(wc -l < "$RUSTED_RUSTC_LOG") times" >&2
        exit 1
    fi
    "$it/copied/debug/app" | grep -q 'left=12'
    clear_log

    link_source=$(find "$RUSTED_CACHE_DIR" -path '*/artifacts/*/app' -type f | head -1)
    target_inode=$(stat -c %i "$it/local/debug/app")
    linked=0
    for candidate in $(find "$RUSTED_CACHE_DIR" -path '*/artifacts/*/app' -type f); do
        if [ "$(stat -c %i "$candidate")" = "$target_inode" ]; then
            linked=1
        fi
    done
    if [ "$linked" -ne 1 ]; then
        echo "target binary is not hardlinked to the store ($link_source)" >&2
        exit 1
    fi

    find "$RUSTED_CACHE_DIR" -name 'libutil.rlib' | while IFS= read -r artifact; do
        rm "$artifact"
    done
    clear_log
    "$bin" build --manifest-path "$fixture" --target-dir "$it/local" -q
    expect_rustc
    clear_log
    "$bin" build --manifest-path "$fixture" --target-dir "$it/local" -q
    expect_no_rustc

    if "$bin" build -p missing --manifest-path "$fixture" --target-dir "$it/missing" -q >/dev/null 2>&1; then
        echo "unknown package should fail" >&2
        exit 1
    fi

    set +e
    "$bin" build --manifest-path tests/fixtures/broken/Cargo.toml --target-dir "$it/broken" -q >/dev/null 2>&1
    code=$?
    set -e
    if [ "$code" -ne 101 ]; then
        echo "compile failure should exit 101, got $code" >&2
        exit 1
    fi

    RUSTED_CACHE_DIR="$it/cache2" "$bin" build --manifest-path "$fixture" --target-dir "$it/race1" -q >/dev/null 2>&1 &
    first=$!
    RUSTED_CACHE_DIR="$it/cache2" "$bin" build --manifest-path "$fixture" --target-dir "$it/race2" -q >/dev/null 2>&1 &
    second=$!
    set +e
    wait "$first"
    first_code=$?
    wait "$second"
    second_code=$?
    set -e
    if [ "$first_code" -ne 0 ] || [ "$second_code" -ne 0 ]; then
        echo "concurrent cold builds failed" >&2
        exit 1
    fi
    "$it/race1/debug/app" | grep -q 'answer=42 left=11 right=31'
    "$it/race2/debug/app" | grep -q 'answer=42 left=11 right=31'

    "$bin" build --manifest-path rusted.toml --target-dir "$it/self" -q
    if [ "$("$it/self/debug/rusted" --version)" != "rusted 0.1.0" ]; then
        echo "self-built rusted reports the wrong version" >&2
        exit 1
    fi

    "$bin" clean --manifest-path "$fixture" --target-dir "$it/local" -q
    if [ -d "$it/local" ]; then
        echo "clean left the target directory behind" >&2
        exit 1
    fi
    if [ ! -d "$RUSTED_CACHE_DIR" ]; then
        echo "clean removed the global cache" >&2
        exit 1
    fi

    gc_cache="$it/gc-cache"
    RUSTED_CACHE_DIR="$gc_cache" "$bin" build --manifest-path "$fixture" --target-dir "$it/gc-build" -q

    cache_info=$(RUSTED_CACHE_DIR="$gc_cache" "$bin" cache info)
    printf '%s' "$cache_info" | grep -q '^index'
    printf '%s' "$cache_info" | grep -q '^crates'
    printf '%s' "$cache_info" | grep -q '^sources'
    printf '%s' "$cache_info" | grep -q '^artifacts'
    printf '%s' "$cache_info" | grep -q '^total'
    cache_json=$(RUSTED_CACHE_DIR="$gc_cache" "$bin" cache info --json)
    printf '%s' "$cache_json" | python3 -c '
import json, sys
value = json.load(sys.stdin)
assert value["reason"] == "cache-info"
for key in ("index", "crates", "sources", "artifacts", "total"):
    assert key in value
assert value["total"] > 0 and value["artifacts"] > 0
'

    gc_dry=$(RUSTED_CACHE_DIR="$gc_cache" "$bin" cache gc --max-size 1 --dry-run)
    printf '%s' "$gc_dry" | grep -q '^would remove'
    if [ -z "$(find "$gc_cache/v1/artifacts" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
        echo "gc dry-run removed artifacts" >&2
        exit 1
    fi
    gc_json=$(RUSTED_CACHE_DIR="$gc_cache" "$bin" cache gc --max-size 1 --dry-run --json)
    printf '%s' "$gc_json" | python3 -c '
import json, sys
value = json.load(sys.stdin)
assert value["reason"] == "cache-gc" and value["dry_run"] is True
assert value["removed"] > 0 and value["bytes"] > 0
assert len(value["artifacts"]) == value["removed"]
'

    RUSTED_CACHE_DIR="$gc_cache" "$bin" cache gc --max-size 1 >/dev/null
    if [ -n "$(find "$gc_cache/v1/artifacts" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
        echo "gc left artifacts behind" >&2
        exit 1
    fi
    RUSTED_CACHE_DIR="$gc_cache" "$bin" build --manifest-path "$fixture" --target-dir "$it/gc-rebuild" -q
    "$it/gc-rebuild/debug/app" | grep -q 'answer=42 left=11 right=31'

    scope_cache="$it/scope-cache"
    RUSTED_CACHE_DIR="$scope_cache" "$bin" build --manifest-path "$fixture" --target-dir "$it/scope-build" -q
    mkdir -p "$scope_cache/v1/index" "$scope_cache/v1/src" "$scope_cache/v1/crates"
    : > "$scope_cache/v1/index/entry"
    : > "$scope_cache/v1/src/source"
    : > "$scope_cache/v1/crates/archive"

    RUSTED_CACHE_DIR="$scope_cache" "$bin" clean --store
    test ! -d "$scope_cache/v1/artifacts"
    test -d "$scope_cache/v1/index"
    test -d "$scope_cache/v1/src"
    RUSTED_CACHE_DIR="$scope_cache" "$bin" clean --index
    test ! -d "$scope_cache/v1/index"
    test -d "$scope_cache/v1/src"
    RUSTED_CACHE_DIR="$scope_cache" "$bin" clean --source
    test ! -d "$scope_cache/v1/src"
    test ! -d "$scope_cache/v1/crates"

    RUSTED_CACHE_DIR="$scope_cache" "$bin" build --manifest-path "$fixture" --target-dir "$it/scope-build2" -q
    test -d "$it/scope-build2"
    RUSTED_CACHE_DIR="$scope_cache" "$bin" clean --all --manifest-path "$fixture" --target-dir "$it/scope-build2" -q
    test ! -d "$it/scope-build2"
    test ! -d "$scope_cache/v1"

    s3_dir="$it/s3"
    mkdir -p "$s3_dir/storage"
    python3 tests/support/s3_mock.py "$s3_dir/storage" "$s3_dir/port" &
    s3_pid=$!
    trap 'kill "$s3_pid" 2>/dev/null || true' EXIT
    tries=0
    while [ ! -s "$s3_dir/port" ]; do
        tries=$((tries + 1))
        if [ "$tries" -gt 100 ]; then
            echo "S3 mock did not start" >&2
            exit 1
        fi
        sleep 0.1
    done
    s3_port=$(cat "$s3_dir/port")
    export RUSTED_REMOTE_CACHE="s3://bucket"
    export RUSTED_REMOTE_ACCESS_KEY=test
    export RUSTED_REMOTE_SECRET_KEY=test
    export RUSTED_REMOTE_ENDPOINT="http://127.0.0.1:$s3_port"
    export RUSTED_REMOTE_REGION=us-east-1

    clear_log
    RUSTED_CACHE_DIR="$it/remote-a" "$bin" build --manifest-path "$fixture" --target-dir "$it/remote-target-a" -q
    expect_rustc
    if [ -z "$(find "$s3_dir/storage" -type f -print -quit)" ]; then
        echo "remote cache received no artifacts" >&2
        exit 1
    fi

    clear_log
    RUSTED_CACHE_DIR="$it/remote-b" "$bin" build --manifest-path "$fixture" --target-dir "$it/remote-target-b" -q
    expect_no_rustc
    "$it/remote-target-b/debug/app" | grep -q 'answer=42 left=11 right=31'

    victim=$(find "$s3_dir/storage" -type f | head -1)
    python3 -c '
import sys
path = sys.argv[1]
data = bytearray(open(path, "rb").read())
data[len(data) // 2] ^= 0xff
open(path, "wb").write(data)
' "$victim"
clear_log
remote_corrupt=$(RUSTED_CACHE_DIR="$it/remote-corrupt" "$bin" build --manifest-path "$fixture" --target-dir "$it/remote-corrupt-target" 2>&1)
printf '%s' "$remote_corrupt" | grep -q 'failed checksum verification'
test -x "$it/remote-corrupt-target/debug/app"

    offline_output=$(RUSTED_OFFLINE=1 RUSTED_REMOTE_ENDPOINT="http://127.0.0.1:1" \
        RUSTED_CACHE_DIR="$it/remote-offline" \
        "$bin" build --manifest-path "$fixture" --target-dir "$it/remote-offline-target" 2>&1)
    if printf '%s' "$offline_output" | grep -q 'remote cache'; then
        echo "offline build contacted the remote cache" >&2
        exit 1
    fi
    test -x "$it/remote-offline-target/debug/app"

    remote_dry=$(RUSTED_CACHE_DIR="$it/remote-a" "$bin" cache gc --remote --max-size 1 --dry-run)
    printf '%s' "$remote_dry" | grep -q '^would remove'
    remote_json=$(RUSTED_CACHE_DIR="$it/remote-a" "$bin" cache gc --remote --max-size 1 --dry-run --json)
    printf '%s' "$remote_json" | python3 -c '
import json, sys
value = json.load(sys.stdin)
assert value["dry_run"] is True
assert value["removed"] > 0 and value["bytes"] > 0
assert len(value["artifacts"]) == value["removed"]
'
    RUSTED_CACHE_DIR="$it/remote-a" "$bin" cache gc --remote --max-size 1 >/dev/null
    if [ -n "$(find "$s3_dir/storage" -type f -print -quit)" ]; then
        echo "remote gc left objects behind" >&2
        exit 1
    fi

    clear_log
    remote_fail=$(RUSTED_REMOTE_ENDPOINT="http://127.0.0.1:1" RUSTED_CACHE_DIR="$it/remote-fail" \
        "$bin" build --manifest-path "$fixture" --target-dir "$it/remote-fail-target" 2>&1)
    printf '%s' "$remote_fail" | grep -q 'warning: remote cache fetch failed'
    test -x "$it/remote-fail-target/debug/app"

    unset RUSTED_REMOTE_CACHE RUSTED_REMOTE_ACCESS_KEY RUSTED_REMOTE_SECRET_KEY
    unset RUSTED_REMOTE_ENDPOINT RUSTED_REMOTE_REGION
    kill "$s3_pid" 2>/dev/null || true

    bs="$it/build-script"
    cp -R tests/fixtures/build-script "$bs"
    bs_manifest="$bs/Cargo.toml"
    clear_log
    bs_build=$("$bin" build --manifest-path "$bs_manifest" --target-dir "$it/bs" 2>&1)
    printf '%s' "$bs_build" | grep -q 'build-script-ran'
    bs_output=$("$bin" run --manifest-path "$bs_manifest" --target-dir "$it/bs" -q)
    if [ "$bs_output" != "7 builddep 7 true" ]; then
        echo "unexpected build script output: $bs_output" >&2
        exit 1
    fi

    clear_log
    "$bin" build --manifest-path "$bs_manifest" --target-dir "$it/bs" -q 2> "$it/bs-warm.err"
    expect_no_rustc
    if grep -q 'build-script-ran' "$it/bs-warm.err"; then
        echo "build script reran on a warm build" >&2
        exit 1
    fi

    bs_special=$("$bin" run --features special --manifest-path "$bs_manifest" --target-dir "$it/bs-special" -q)
    if [ "$bs_special" != "100 builddep 100 true" ]; then
        echo "unexpected special build script output: $bs_special" >&2
        exit 1
    fi

    printf 'trigger-two\n' > "$bs/builddep/trigger.txt"
    clear_log
    "$bin" build --manifest-path "$bs_manifest" --target-dir "$it/bs" -q 2> "$it/bs-trigger.err"
    grep -q 'build-script-ran' "$it/bs-trigger.err"
    expect_rustc

    printf 'aux-two\n' > "$bs/builddep-aux.txt"
    clear_log
    "$bin" build --manifest-path "$bs_manifest" --target-dir "$it/bs" -q 2> "$it/bs-aux.err"
    grep -q 'build-script-ran' "$it/bs-aux.err"

    clear_log
    BUILD_EXTRA=1 "$bin" build --manifest-path "$bs_manifest" --target-dir "$it/bs" -q 2> "$it/bs-env.err"
    grep -q 'build-script-ran' "$it/bs-env.err"

    pm_fixture=tests/fixtures/proc-macro/Cargo.toml
    clear_log
    "$bin" build --manifest-path "$pm_fixture" --target-dir "$it/pm" -q
    pm_output=$("$bin" run --manifest-path "$pm_fixture" --target-dir "$it/pm" -q)
    if [ "$pm_output" != "7 9" ]; then
        echo "unexpected proc macro output: $pm_output" >&2
        exit 1
    fi
    "$bin" check --manifest-path "$pm_fixture" --target-dir "$it/pm-check" -q
    clear_log
    "$bin" build --manifest-path "$pm_fixture" --target-dir "$it/pm" -q
    expect_no_rustc

    inheritance=tests/fixtures/inheritance/workspace/Cargo.toml
    inheritance_output=$("$bin" run --manifest-path "$inheritance" --target-dir "$it/inheritance" -q)
    if [ "$inheritance_output" != "21" ]; then
        echo "unexpected inheritance output: $inheritance_output" >&2
        exit 1
    fi
    "$bin" build --workspace --manifest-path "$inheritance" --target-dir "$it/inheritance-all" -q
    test -f "$it/inheritance-all/debug/app"

    features=tests/fixtures/features/Cargo.toml
    features_output=$("$bin" run --manifest-path "$features" --target-dir "$it/features" -q)
    if [ "$features_output" != "alpha_on=true beta_loud=false unix=false" ]; then
        echo "unexpected default feature output: $features_output" >&2
        exit 1
    fi
    features_output=$("$bin" run --features full --manifest-path "$features" --target-dir "$it/features" -q)
    if [ "$features_output" != "alpha_on=true beta_loud=true unix=false" ]; then
        echo "unexpected full feature output: $features_output" >&2
        exit 1
    fi
    features_output=$("$bin" run --no-default-features --features unix --manifest-path "$features" --target-dir "$it/features-unix" -q)
    if [ "$features_output" != "alpha_on=false beta_loud=false unix=true" ]; then
        echo "unexpected unix feature output: $features_output" >&2
        exit 1
    fi
    "$bin" build --features cycle-a --manifest-path "$features" --target-dir "$it/features-cycle" -q

    features_tree=$("$bin" tree --manifest-path "$features")
    if printf '%s' "$features_tree" | grep -q 'windows-only'; then
        echo "resolver 2 should skip cfg(windows) dependencies" >&2
        exit 1
    fi
    if printf '%s' "$features_tree" | grep -q 'unix-only'; then
        echo "optional unix-only should not be active by default" >&2
        exit 1
    fi

    resolver1=tests/fixtures/features-resolver1/Cargo.toml
    resolver1_tree=$("$bin" tree --manifest-path "$resolver1")
    if ! printf '%s' "$resolver1_tree" | grep -q 'windows-only'; then
        echo "resolver 1 should include cfg(windows) dependencies" >&2
        exit 1
    fi

    dev_lock="$it/dev-lock"
    cp -R tests/fixtures/dev-lock "$dev_lock"
    rm -f "$dev_lock/Cargo.lock"
    RUSTED_CACHE_DIR="$it/dev-lock-cache" "$bin" build --manifest-path "$dev_lock/Cargo.toml" --target-dir "$it/dev-lock-target" -q
    if ! grep -q 'name = "extra"' "$dev_lock/Cargo.lock"; then
        echo "build lockfile should include dev-only optional dependencies" >&2
        exit 1
    fi

    context_fixture=tests/fixtures/feature-context/Cargo.toml
    clear_log
    RUSTED_CACHE_DIR="$it/context-cache" "$bin" build --manifest-path "$context_fixture" --target-dir "$it/context-target" -q
    if [ "$(grep -c 'crate-name shared ' "$RUSTED_RUSTC_LOG")" -ne 2 ]; then
        echo "build and normal contexts should compile separate shared crates" >&2
        exit 1
    fi
    grep 'crate-name shared ' "$RUSTED_RUSTC_LOG" | grep -q 'feature="normal"'
    grep 'crate-name shared ' "$RUSTED_RUSTC_LOG" | grep -q 'feature="build"'
    context_output=$("$bin" run --manifest-path "$context_fixture" --target-dir "$it/context-target" -q)
    if [ "$context_output" != "1" ]; then
        echo "unexpected feature context output: $context_output" >&2
        exit 1
    fi

    split_fixture=tests/fixtures/proc-macro-split/Cargo.toml
    clear_log
    RUSTED_CACHE_DIR="$it/pm-split-cache" "$bin" build --manifest-path "$split_fixture" --target-dir "$it/pm-split" -q
    if [ "$(grep -c 'crate-name worker ' "$RUSTED_RUSTC_LOG")" -ne 2 ]; then
        echo "proc macro and target contexts should compile separate worker crates" >&2
        exit 1
    fi
    grep 'crate-name worker ' "$RUSTED_RUSTC_LOG" | grep -q 'feature="t"'
    grep 'crate-name worker ' "$RUSTED_RUSTC_LOG" | grep -q 'feature="h"'
    split_output=$("$bin" run --manifest-path "$split_fixture" --target-dir "$it/pm-split" -q)
    if [ "$split_output" != "7 700" ]; then
        echo "unexpected proc macro feature split output: $split_output" >&2
        exit 1
    fi
    if [ "$(grep -c '^name = "worker"$' tests/fixtures/proc-macro-split/Cargo.lock)" -ne 1 ]; then
        echo "proc macro feature split must keep one lockfile entry" >&2
        exit 1
    fi

    clear_log
    RUSTED_CACHE_DIR="$it/features-cache" "$bin" build --manifest-path "$features" --target-dir "$it/features-plain" -q
    if grep -q 'crate-name unix_only' "$RUSTED_RUSTC_LOG"; then
        echo "unix-only built without its feature" >&2
        exit 1
    fi
    if grep -q 'crate-name windows_only' "$RUSTED_RUSTC_LOG"; then
        echo "windows-only built on unix" >&2
        exit 1
    fi
    clear_log
    RUSTED_CACHE_DIR="$it/features-cache" "$bin" build --no-default-features --features unix --manifest-path "$features" --target-dir "$it/features-unix-plain" -q
    if ! grep -q 'crate-name unix_only' "$RUSTED_RUSTC_LOG"; then
        echo "unix-only did not build with its feature" >&2
        exit 1
    fi

    profile_fixture=tests/fixtures/profile-overrides/Cargo.toml
    profile_flags=$(RUSTED_CACHE_DIR="$it/profile-cache" "$bin" build -v --manifest-path "$profile_fixture" --target-dir "$it/profile" 2>&1)
    printf '%s\n' "$profile_flags" | grep 'crate-name dep ' | grep -q 'opt-level=3'
    printf '%s\n' "$profile_flags" | grep 'crate-name profile_app ' | grep -q 'opt-level=1'
    profile_output=$("$bin" run --manifest-path "$profile_fixture" --target-dir "$it/profile" -q)
    if [ "$profile_output" != "5" ]; then
        echo "unexpected profile fixture output: $profile_output" >&2
        exit 1
    fi

    linker_dir=$(pwd)/$it/linker
    mkdir -p "$linker_dir/config/rusted" "$linker_dir/bin"
    printf '#!/bin/sh\nexec cc "$@"\n' > "$linker_dir/bin/cc-link"
    chmod +x "$linker_dir/bin/cc-link"
    cat > "$linker_dir/config/rusted/config.toml" <<EOF
linker = "$linker_dir/bin/cc-link"
EOF

    clear_log
    RUSTED_CACHE_DIR="$it/linker-cache" XDG_CONFIG_HOME="$linker_dir/config" \
        "$bin" build --manifest-path "$profile_fixture" --target-dir "$it/linker-profile" -q
    if grep -q -- '-C linker=' "$RUSTED_RUSTC_LOG"; then
        echo "profile linker = system did not override the global linker" >&2
        exit 1
    fi

    clear_log
    RUSTED_CACHE_DIR="$it/linker-cache2" XDG_CONFIG_HOME="$linker_dir/config" \
        "$bin" build --manifest-path "$fixture" --target-dir "$it/linker-build" -q
    grep -q -- "-C linker=$linker_dir/bin/cc-link" "$RUSTED_RUSTC_LOG"
    clear_log
    RUSTED_CACHE_DIR="$it/linker-cache2" XDG_CONFIG_HOME="$linker_dir/config" \
        "$bin" build --manifest-path "$fixture" --target-dir "$it/linker-build" -q
    expect_no_rustc
    clear_log
    RUSTED_CACHE_DIR="$it/linker-cache2" \
        "$bin" build --manifest-path "$fixture" --target-dir "$it/linker-build" -q
    expect_rustc

    clear_log
    RUSTED_CACHE_DIR="$it/pipeline-cache" "$bin" build --manifest-path "$fixture" --target-dir "$it/pipeline" -q
    grep -q -- '--emit metadata=' "$RUSTED_RUSTC_LOG"
    if [ -z "$(find "$it/pipeline-cache/v1/metadata" -name '*.rmeta' -print -quit)" ]; then
        echo "pipelined build published no metadata" >&2
        exit 1
    fi
    clear_log
    RUSTED_NO_PIPELINE=1 RUSTED_CACHE_DIR="$it/nopipe-cache" "$bin" build --manifest-path "$fixture" --target-dir "$it/nopipe" -q
    if grep -q -- '--emit metadata=' "$RUSTED_RUSTC_LOG"; then
        echo "non-pipelined build used split metadata emit" >&2
        exit 1
    fi

    edition_fixture=tests/fixtures/edition-2024/Cargo.toml
    edition_output=$("$bin" run --manifest-path "$edition_fixture" --target-dir "$it/edition" -q)
    if [ "$edition_output" != "edition 2024 6" ]; then
        echo "unexpected edition 2024 output: $edition_output" >&2
        exit 1
    fi

    set +e
    rust_version_output=$("$bin" build --manifest-path tests/fixtures/rust-version/Cargo.toml --target-dir "$it/rust-version" 2>&1)
    rust_version_code=$?
    set -e
    if [ "$rust_version_code" -ne 1 ]; then
        echo "rust-version failure should exit 1, got $rust_version_code" >&2
        exit 1
    fi
    printf '%s' "$rust_version_output" | grep -q 'package `rust-version-app 0.1.0` requires rustc 99.0'
    printf '%s' "$rust_version_output" | grep -q 'rustc 1\.'

    "$bin" build --frozen --manifest-path "$fixture" --target-dir "$it/local" -q
    set +e
    frozen_output=$("$bin" update --frozen --manifest-path "$fixture" 2>&1)
    frozen_code=$?
    set -e
    if [ "$frozen_code" -ne 1 ]; then
        echo "update --frozen should exit 1, got $frozen_code" >&2
        exit 1
    fi
    printf '%s' "$frozen_output" | grep -q 'prevents updating the lockfile'

    tree=$("$bin" tree --manifest-path "$fixture")
    printf '%s\n' "$tree" | grep -q '^app v0.1.0$'
    printf '%s\n' "$tree" | grep -q 'core v0.1.0'
    printf '%s\n' "$tree" | grep -q 'util v0.1.0'

    metadata=$("$bin" metadata --manifest-path "$fixture")
    printf '%s' "$metadata" | python3 -c '
import json, sys
data = json.load(sys.stdin)
assert data["version"] == 1
assert data["workspace_root"].endswith("local-workspace")
names = {package["name"] for package in data["packages"]}
assert {"app", "core", "left", "right", "util"} <= names
assert data["workspace_members"]
app = next(package for package in data["packages"] if package["name"] == "app")
assert app["id"].startswith("app 0.1.0 (path+")
assert any(target["kind"] == ["bin"] for target in app["targets"])
assert {dependency["name"] for dependency in app["dependencies"]} >= {"core", "left", "right"}
assert all(dependency["kind"] in {"normal", "dev", "build"} for dependency in app["dependencies"])
core = next(package for package in data["packages"] if package["name"] == "core")
assert any(target["kind"] == ["lib"] for target in core["targets"])
'

    set +e
    "$bin" tree --release --manifest-path "$fixture" >/dev/null 2>&1
    code=$?
    set -e
    if [ "$code" -ne 1 ]; then
        echo "tree should reject build options, got $code" >&2
        exit 1
    fi

    systemd_dev_default=$(./scripts/dev.sh bin)
    test "$systemd_dev_default" = target/bootstrap/rusted
    dev_dogfood=$(RUSTED_DEV_STAGE=rusted RUSTED_DEV_TARGET="$it/dev-dogfood" ./scripts/dev.sh bin)
    test -x "$dev_dogfood"
    test "$dev_dogfood" = "$it/dev-dogfood/debug/rusted"
    RUSTED_DEV_STAGE=rusted RUSTED_DEV_TARGET="$it/dev-dogfood" ./scripts/dev.sh --version | grep -q '^rusted 0.1.0$'
    if sha256sum "$dev_dogfood" target/bootstrap/rusted | awk '{print $1}' | uniq -d | grep -q .; then
        echo "dogfood binary should not be a byte copy of the bootstrap binary" >&2
        exit 1
    fi

    if [ "${RUSTED_NETWORK_TESTS:-0}" = "1" ]; then
        echo "running network tests"
        export RUSTED_CACHE_DIR="$it/network-cache"
        network_fixture=tests/fixtures/registry/Cargo.toml

        clear_log
        "$bin" build --manifest-path "$network_fixture" --target-dir "$it/network" -q
        expect_rustc
        output=$("$bin" run --manifest-path "$network_fixture" --target-dir "$it/network" -q)
        if [ "$output" != "unix 42 2" ]; then
            echo "unexpected registry fixture output: $output" >&2
            exit 1
        fi

        "$bin" build --force --manifest-path "$network_fixture" --target-dir "$it/network" -v 2>&1 \
            | grep -q -- '--cfg feature='

        clear_log
        "$bin" build --locked --manifest-path "$network_fixture" --target-dir "$it/network2" -q
        expect_no_rustc

        serde_fixture=tests/fixtures/serde-derive/Cargo.toml
        "$bin" build --manifest-path "$serde_fixture" --target-dir "$it/serde" -q
        serde_output=$("$bin" run --manifest-path "$serde_fixture" --target-dir "$it/serde" -q)
        if [ "$serde_output" != '{"value":42}' ]; then
            echo "unexpected serde derive output: $serde_output" >&2
            exit 1
        fi

        multi_fixture=tests/fixtures/multi-version/Cargo.toml
        "$bin" build --manifest-path "$multi_fixture" --target-dir "$it/multi-version" -q
        if [ "$(grep -c 'name = "bitflags"' tests/fixtures/multi-version/Cargo.lock)" -ne 2 ]; then
            echo "multi-version lockfile should carry two bitflags versions" >&2
            exit 1
        fi
        grep -q 'version = "1\.' tests/fixtures/multi-version/Cargo.lock
        grep -q 'version = "2\.' tests/fixtures/multi-version/Cargo.lock
        multi_tree=$("$bin" tree --manifest-path "$multi_fixture")
        printf '%s' "$multi_tree" | grep -q 'bitflags v1\.'
        printf '%s' "$multi_tree" | grep -q 'bitflags v2\.'

        patch_fixture=tests/fixtures/patch-replace/Cargo.toml
        patch_output=$("$bin" run --manifest-path "$patch_fixture" --target-dir "$it/patch-replace" -q)
        if [ "$patch_output" != "77 20" ]; then
            echo "unexpected patch fixture output: $patch_output" >&2
            exit 1
        fi
        patch_tree=$("$bin" tree --manifest-path "$patch_fixture")
        printf '%s' "$patch_tree" | grep -q 'itoa v1\.0\.99'
        printf '%s' "$patch_tree" | grep -q 'libc v0\.2\.170'

        ecosystem_fixture=tests/fixtures/ecosystem/Cargo.toml
        ecosystem_output=$("$bin" run --manifest-path "$ecosystem_fixture" --target-dir "$it/ecosystem" -q)
        if [ "$(printf '%s\n' "$ecosystem_output" | head -1)" != '{"value":42}' ]; then
            echo "unexpected ecosystem output: $ecosystem_output" >&2
            exit 1
        fi
        printf '%s' "$ecosystem_output" | grep -q 'bad value'

        edit="$it/manifest-edit"
        cp -R tests/fixtures/manifest-edit "$edit"
        edit_manifest="$edit/Cargo.toml"

        "$bin" add anyhow --manifest-path "$edit_manifest" -q
        grep -q '^anyhow = "1' "$edit_manifest"
        grep -q '# keep this comment' "$edit_manifest"
        test -f "$edit/Cargo.lock"
        grep -q 'name = "anyhow"' "$edit/Cargo.lock"
        "$bin" build --manifest-path "$edit_manifest" --target-dir "$it/edit" -q

        edit_tree=$("$bin" tree --manifest-path "$edit_manifest")
        printf '%s\n' "$edit_tree" | grep -q 'anyhow v1'
        edit_metadata=$("$bin" metadata --manifest-path "$edit_manifest")
        printf '%s' "$edit_metadata" | python3 -c '
import json, sys
data = json.load(sys.stdin)
names = {package["name"] for package in data["packages"]}
assert "anyhow" in names
'

        "$bin" add serde --features derive --no-default-features --manifest-path "$edit_manifest" -q
        grep -q 'serde = { version = "1.0.' "$edit_manifest"
        grep -q 'default-features = false' "$edit_manifest"
        grep -q 'features = \["derive"\]' "$edit_manifest"
        "$bin" remove serde --manifest-path "$edit_manifest" -q
        if grep -q '^serde = ' "$edit_manifest"; then
            echo "remove left serde in the manifest" >&2
            exit 1
        fi

        "$bin" update --manifest-path "$edit_manifest" -q
        "$bin" remove anyhow --manifest-path "$edit_manifest" -q
        if grep -q '^anyhow = ' "$edit_manifest"; then
            echo "remove left anyhow in the manifest" >&2
            exit 1
        fi
        if grep -q 'name = "anyhow"' "$edit/Cargo.lock"; then
            echo "remove left anyhow in the lockfile" >&2
            exit 1
        fi

        ambiguous="$it/ambiguous"
        mkdir -p "$ambiguous/src"
        printf 'fn main() {}\n' > "$ambiguous/src/main.rs"
        cat > "$ambiguous/Cargo.toml" <<'EOF'
[package]
name = "ambiguous"
version = "0.1.0"
edition = "2021"

[dependencies.anyhow]
version = "1"
EOF
        set +e
        ambiguous_output=$("$bin" add anyhow --manifest-path "$ambiguous/Cargo.toml" 2>&1)
        ambiguous_code=$?
        set -e
        if [ "$ambiguous_code" -ne 1 ]; then
            echo "ambiguous add should exit 1, got $ambiguous_code" >&2
            exit 1
        fi
        printf '%s' "$ambiguous_output" | grep -q 'edit it manually'
        printf '%s' "$ambiguous_output" | grep -q '^+ anyhow = '
    fi

    echo "integration tests passed"
fi

if [ "$mode" = all ]; then
    clear_log
    clippy_output=$("$bin" clippy --target-dir "$it/clippy" 2>&1)
    if [ -n "$(printf '%s' "$clippy_output" | grep -E '^(warning|error):')" ]; then
        echo "`rusted clippy` dogfood must be clean:" >&2
        printf '%s\n' "$clippy_output" >&2
        exit 1
    fi

    clean_cache=$PWD/target/selfhost-clean-cache
    rm -rf "$clean_cache"
    env -u RUSTED_CACHE_DIR -u RUSTED_RUSTC_LOG \
        XDG_CACHE_HOME="$clean_cache" \
        RUSTED_OFFLINE=1 \
        ./scripts/selfhost.sh

    store="$clean_cache/rusted/v1"
    for directory in src index crates build-scripts; do
        if [ -d "$store/$directory" ] && [ -n "$(find "$store/$directory" -mindepth 1 -print -quit)" ]; then
            echo "clean store contains $directory entries" >&2
            exit 1
        fi
    done
    if [ -z "$(find "$store/artifacts" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]; then
        echo "clean store has no artifacts" >&2
        exit 1
    fi

    for entry in target/selfhost/*; do
        case "$entry" in
            target/selfhost/stage1|target/selfhost/stage2) ;;
            *)
                echo "unexpected selfhost output: $entry" >&2
                exit 1
                ;;
        esac
    done

    rm -f target/bootstrap/rusted
    target/selfhost/stage1 --version | grep -q '^rusted '
    target/selfhost/stage2 --version | grep -q '^rusted '

    rm -rf "$clean_cache"
fi
