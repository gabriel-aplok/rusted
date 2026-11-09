#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

rustc=${RUSTC:-rustc}
opt_level=${RUSTED_BOOTSTRAP_OPT_LEVEL:-2}
out=target/bootstrap
marker=$out/opt-level
mkdir -p "$out"

if [ "${RUSTED_BOOTSTRAP_FORCE:-0}" != "1" ] && [ -x "$out/rusted" ]; then
    if [ "$(cat "$marker" 2>/dev/null)" = "$opt_level" ] \
        && [ -z "$(find src -newer "$out/rusted" -print -quit)" ]; then
        exit 0
    fi
fi

"$rustc" --edition 2021 --crate-name rusted -C "opt-level=$opt_level" \
    -C debuginfo=0 -C strip=debuginfo -C codegen-units=16 \
    src/main.rs -o "$out/rusted"
printf '%s' "$opt_level" > "$marker"
