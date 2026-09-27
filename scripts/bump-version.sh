#!/bin/sh
# Set one version everywhere a release reads it: Cargo.toml (the binary), both plugin
# manifests and the marketplace. A marketplace install takes the version from these files
# on the default branch and downloads the release tagged with it, so the order is:
#   scripts/bump-version.sh 1.0.1 && git commit -am "Release 1.0.1" && git tag v1.0.1
#   git push && git push --tags
# `every_manifest_carries_the_binary_version` fails if any of them is left behind.
set -eu
new="${1:?usage: scripts/bump-version.sh X.Y.Z}"
root=$(cd "$(dirname "$0")/.." && pwd)
old=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
sed -i.bak "s/^version = \"$old\"/version = \"$new\"/" "$root/Cargo.toml"
rm -f "$root/Cargo.toml.bak"
for f in plugin/.claude-plugin/plugin.json plugin/.codex-plugin/plugin.json .claude-plugin/marketplace.json; do
  sed -i.bak "s/\"version\": \"$old\"/\"version\": \"$new\"/g" "$root/$f"
  rm -f "$root/$f.bak"
done
cargo update --workspace --manifest-path "$root/Cargo.toml" >/dev/null 2>&1 || true
echo "$old -> $new"
