#!/usr/bin/env sh
# Install the muninn binary into a Claude Code plugin root (or any directory).
# Verifies the published checksum before placing the binary. Not meant for `curl | sh`:
# download it, read it, run it.
set -eu
VERSION="${MUNINN_VERSION:-0.1.0}"
DEST="${1:-${CLAUDE_PLUGIN_ROOT:-$HOME/.local/share/muninn}/bin}"
REPO="https://github.com/muninn-dev/muninn/releases/download/v${VERSION}"
os=$(uname -s | tr '[:upper:]' '[:lower:]'); arch=$(uname -m)
case "$os-$arch" in
  linux-x86_64)  target=x86_64-unknown-linux-gnu ;;
  linux-aarch64) target=aarch64-unknown-linux-gnu ;;
  darwin-arm64)  target=aarch64-apple-darwin ;;
  *) echo "unsupported platform $os-$arch" >&2; exit 1 ;;
esac
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$REPO/muninn-$target" -o "$tmp/muninn"
curl -fsSL "$REPO/checksums.txt" -o "$tmp/checksums.txt"
expected=$(grep " muninn-$target\$" "$tmp/checksums.txt" | cut -d' ' -f1)
actual=$(sha256sum "$tmp/muninn" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$tmp/muninn" | cut -d' ' -f1)
[ "$expected" = "$actual" ] || { echo "checksum mismatch for muninn-$target" >&2; exit 1; }
mkdir -p "$DEST"; install -m 0755 "$tmp/muninn" "$DEST/muninn"
echo "installed $DEST/muninn ($VERSION, $target)"
