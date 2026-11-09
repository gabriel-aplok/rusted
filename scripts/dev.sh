#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

# Builds rusted for local development and prints the binary to use.
#
# RUSTED_DEV_STAGE=rustc   default, stage 0 from rustc (fast loop)
# RUSTED_DEV_STAGE=rusted  dogfooding, the compiler is rusted itself
# RUSTED_DEV_TARGET=target/dogfood   where the dogfood binary goes

stage=${RUSTED_DEV_STAGE:-rustc}
case "$stage" in
    rustc | rusted) ;;
    *)
        echo "unknown RUSTED_DEV_STAGE \`$stage\`, use rustc or rusted" >&2
        exit 2
        ;;
esac

RUSTED_BOOTSTRAP_OPT_LEVEL="${RUSTED_BOOTSTRAP_OPT_LEVEL:-0}" ./scripts/bootstrap.sh

if [ "$stage" = rustc ]; then
    bin=target/bootstrap/rusted
else
    target_dir=${RUSTED_DEV_TARGET:-target/dogfood}
    force=${RUSTED_FORCE:+--force}
    # shellcheck disable=SC2086
    target/bootstrap/rusted build \
        --manifest-path rusted.toml \
        --target-dir "$target_dir" \
        $force \
        -q
    bin=$target_dir/debug/rusted
fi

if [ "${1:-}" = bin ]; then
    echo "$bin"
    exit 0
fi

exec "$bin" "$@"
