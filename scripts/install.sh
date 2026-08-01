#!/bin/sh
set -eu

version=${1:-v0.1.0-alpha.1}
case "$version" in
    v|*[!0-9A-Za-z.v-]*) echo "invalid Akeep version: $version" >&2; exit 2 ;;
esac

case "$(uname -s):$(uname -m)" in
    Linux:x86_64) target=x86_64-unknown-linux-gnu ;;
    Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-gnu ;;
    Darwin:x86_64) target=x86_64-apple-darwin ;;
    Darwin:arm64) target=aarch64-apple-darwin ;;
    *) echo "unsupported platform: $(uname -s) $(uname -m)" >&2; exit 2 ;;
esac

repository=https://github.com/eunomia-bpf/akeep
archive="akeep-${version}-${target}.tar.gz"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

curl -fsSL "$repository/releases/download/$version/$archive" -o "$temporary/$archive"
curl -fsSL "$repository/releases/download/$version/SHA256SUMS" -o "$temporary/SHA256SUMS"
expected=$(awk -v archive="$archive" '$2 == archive { print $1 }' "$temporary/SHA256SUMS")
test -n "$expected" || { echo "release checksum is missing for $archive" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/$archive" | awk '{print $1}')
else
    actual=$(shasum -a 256 "$temporary/$archive" | awk '{print $1}')
fi
test "$actual" = "$expected" || { echo "checksum mismatch for $archive" >&2; exit 1; }

tar -C "$temporary" -xzf "$temporary/$archive"
destination=${AKEEP_INSTALL_DIR:-"$HOME/.local/bin"}
mkdir -p "$destination"
install -m 0755 "$temporary/akeep" "$destination/akeep"
"$destination/akeep" --version
printf 'Installed Akeep in %s\n' "$destination"
