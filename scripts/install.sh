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
# Sigstore bundle: verified when cosign is installed (keyless, GitHub Actions identity)
if command -v cosign >/dev/null 2>&1; then
  curl -fsSL "$REPO/muninn-$target.sigstore.json" -o "$tmp/bundle.json" && \
  cosign verify-blob --bundle "$tmp/bundle.json" \
    --certificate-identity-regexp "^https://github.com/muninn-dev/muninn/" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com "$tmp/muninn" \
    || { echo "signature verification failed" >&2; exit 1; }
else
  echo "cosign not found: checksum verified, signature not checked" >&2
fi
# the embedding model is optional; fetch it only when asked, and verify every file
if [ "${MUNINN_WITH_MODEL:-0}" = "1" ]; then
  mdir="${MUNINN_MODEL_DIR:-$(dirname "$DEST")/models/potion-base-8M}"; mkdir -p "$mdir"
  for f in config.json tokenizer.json model.safetensors; do
    curl -fsSL "https://huggingface.co/minishlab/potion-base-8M/resolve/main/$f" -o "$mdir/$f"
  done
  curl -fsSL "$REPO/models.txt" -o "$tmp/models.txt"
  for f in config.json tokenizer.json model.safetensors; do
    want=$(grep " $f " "$tmp/models.txt" | awk '{print $3}')
    got=$(sha256sum "$mdir/$f" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$mdir/$f" | cut -d' ' -f1)
    [ "$want" = "$got" ] || { echo "model file $f checksum mismatch" >&2; rm -f "$mdir/$f"; exit 1; }
  done
  echo "model installed in $mdir"
fi
mkdir -p "$DEST"; install -m 0755 "$tmp/muninn" "$DEST/muninn"
echo "installed $DEST/muninn ($VERSION, $target)"
