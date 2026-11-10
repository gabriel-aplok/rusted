#!/bin/sh
set -eu

repo=gabriel-aplok/rusted
install_dir=${RUSTED_INSTALL_DIR:-$HOME/.local/bin}

case "$(uname -s),$(uname -m)" in
    Linux,x86_64) target=x86_64-unknown-linux-musl ;;
    Linux,aarch64 | Linux,arm64) target=aarch64-unknown-linux-musl ;;
    Darwin,arm64) target=aarch64-apple-darwin ;;
    *)
        echo "rusted install: unsupported system $(uname -s),$(uname -m)" >&2
        if uname -s | grep -qi msys; then
            echo "on Windows download rusted-x86_64-pc-windows-msvc.exe from https://github.com/$repo/releases/latest" >&2
        fi
        exit 1
        ;;
esac

api=https://api.github.com/repos/$repo/releases/latest
tag=$(curl -fsSL "$api" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
if [ -z "$tag" ]; then
    echo "rusted install: could not read the latest release" >&2
    exit 1
fi

base=https://github.com/$repo/releases/download/$tag
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL -o "$tmp/rusted" "$base/rusted-$target"
curl -fsSL -o "$tmp/rusted.sha256" "$base/rusted-$target.sha256"

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c rusted.sha256 >/dev/null)
elif command -v shasum >/dev/null 2>&1; then
    (cd "$tmp" && shasum -a 256 -c rusted.sha256 >/dev/null)
else
    echo "rusted install: no sha256 tool found, skipping verification" >&2
fi

mkdir -p "$install_dir"
mv "$tmp/rusted" "$install_dir/rusted"
chmod +x "$install_dir/rusted"
echo "rusted install: $tag installed in $install_dir/rusted"

case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "rusted install: add $install_dir to PATH" >&2 ;;
esac
