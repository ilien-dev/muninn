#!/usr/bin/env sh
# Install the muninn binary where both callers look for it:
#   - $DEST            the plugin's own bin/, which plugin/hooks/hooks.json names as
#                      ${CLAUDE_PLUGIN_ROOT}/bin/muninn
#   - $MUNINN_BIN_DIR  a directory on PATH, which `muninn init`, the slash commands and
#                      the boot summary need
# Verifies the published checksum, and the Sigstore signature when cosign is installed,
# before placing anything. Not meant for `curl | sh`: download it, read it, run it.
set -eu
VERSION="${MUNINN_VERSION:-1.0.0}"
DEST="${1:-${CLAUDE_PLUGIN_ROOT:-$HOME/.local/share/muninn}/bin}"
BIN_DIR="${MUNINN_BIN_DIR:-$HOME/.local/bin}"
OWNER="${MUNINN_REPO:-ilien-dev/muninn}"
# MUNINN_RELEASE_URL points at a mirror holding the same assets (and serves the install test)
REPO="${MUNINN_RELEASE_URL:-https://github.com/${OWNER}/releases/download/v${VERSION}}"
os=$(uname -s | tr '[:upper:]' '[:lower:]'); arch=$(uname -m)
ext=""
case "$os-$arch" in
  linux-x86_64)          target=x86_64-unknown-linux-gnu ;;
  linux-aarch64|linux-arm64) target=aarch64-unknown-linux-gnu ;;
  darwin-arm64)          target=aarch64-apple-darwin ;;
  darwin-x86_64)         echo "only the arm64 macOS build is published, and an Intel Mac cannot run it — build from source: cargo build --release -p muninn-cli" >&2; exit 1 ;;
  mingw*-x86_64|msys*-x86_64|cygwin*-x86_64)
                         target=x86_64-pc-windows-msvc; ext=".exe" ;;
  *) echo "unsupported platform $os-$arch — build from source: cargo build --release -p muninn-cli" >&2; exit 1 ;;
esac
# sha256sum (Linux, Git Bash) or shasum (macOS). A pipeline's status is its last
# command's, so `sha256sum f | cut || shasum f | cut` never reaches shasum.
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}
asset="muninn-$target$ext"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$REPO/$asset" -o "$tmp/muninn$ext"
curl -fsSL "$REPO/checksums.txt" -o "$tmp/checksums.txt"
expected=$(grep " $asset\$" "$tmp/checksums.txt" | cut -d' ' -f1)
actual=$(sha256 "$tmp/muninn$ext")
[ -n "$expected" ] || { echo "no checksum published for $asset" >&2; exit 1; }
[ "$expected" = "$actual" ] || { echo "checksum mismatch for $asset" >&2; exit 1; }
# Sigstore bundle: verified when cosign is installed (keyless, GitHub Actions identity)
if command -v cosign >/dev/null 2>&1; then
  curl -fsSL "$REPO/$asset.sigstore.json" -o "$tmp/bundle.json" && \
  cosign verify-blob --bundle "$tmp/bundle.json" \
    --certificate-identity-regexp "^https://github.com/${OWNER}/" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com "$tmp/muninn$ext" \
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
    got=$(sha256 "$mdir/$f")
    [ "$want" = "$got" ] || { echo "model file $f checksum mismatch" >&2; rm -f "$mdir/$f"; exit 1; }
  done
  echo "model installed in $mdir"
fi
# Copy next to the target, then rename over it: a hook that starts while this runs sees
# either the old binary or the new one, never a half-written file.
mkdir -p "$DEST"; install -m 0755 "$tmp/muninn$ext" "$DEST/.muninn$ext.$$"
mv -f "$DEST/.muninn$ext.$$" "$DEST/muninn$ext"
echo "installed $DEST/muninn$ext ($VERSION, $target)"
# The hooks call the copy above by absolute path. Everything a person types needs PATH.
if [ "$BIN_DIR" != "$DEST" ]; then
  mkdir -p "$BIN_DIR"; install -m 0755 "$tmp/muninn$ext" "$BIN_DIR/.muninn$ext.$$"
  mv -f "$BIN_DIR/.muninn$ext.$$" "$BIN_DIR/muninn$ext"
  echo "installed $BIN_DIR/muninn$ext"
  case ":${PATH}:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: $BIN_DIR is not on PATH. Add it, or 'muninn init' and the /muninn commands will not run:" >&2
       echo "  export PATH=\"$BIN_DIR:\$PATH\"" >&2 ;;
  esac
fi
