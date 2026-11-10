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
curl -fsSL -o "$tmp/rusted.sha256" "$base/checksums.sha256"

expected=$(awk -v name="rusted-$target" '$2 == name { print $1 }' "$tmp/rusted.sha256")
if [ -z "$expected" ]; then
    echo "rusted install: checksum file has no entry for $target" >&2
    exit 1
fi
actual=$(sha256sum "$tmp/rusted" | cut -d" " -f1)
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/rusted" | cut -d" " -f1)
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp/rusted" | cut -d" " -f1)
else
    echo "rusted install: no sha256 tool found, skipping verification" >&2
    actual=$expected
fi
if [ "$actual" != "$expected" ]; then
    echo "rusted install: checksum mismatch for rusted-$target" >&2
    exit 1
fi

mkdir -p "$install_dir"
mv "$tmp/rusted" "$install_dir/rusted"
chmod +x "$install_dir/rusted"
echo "rusted install: $tag installed in $install_dir/rusted"

case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "rusted install: add $install_dir to PATH" >&2 ;;
esac
