#!/usr/bin/env bash
# End-to-end install check, run by CI on Linux, macOS and Windows (Git Bash): the path a
# marketplace install takes, against a local mirror of the release assets instead of
# GitHub. A plugin copy without bin/ runs scripts/session-start, which downloads the
# binary through install.sh, verifies its checksum and serves the hook; then `muninn init`
# and `muninn status` run in a fresh repository and the next SessionStart delivers the
# memory header.
#
#   scripts/ci-install-smoke.sh <path to a release build of muninn>
set -euo pipefail
built="$1"
repo=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
target=$(rustc -vV | sed -n 's/^host: //p')
ext=""; case "$built" in *.exe) ext=".exe";; esac

mkdir -p "$work/mirror" "$work/proj" "$work/home"
cp "$built" "$work/mirror/muninn-$target$ext"
(cd "$work/mirror" && if command -v sha256sum >/dev/null; then sha256sum muninn-*; else shasum -a 256 muninn-*; fi > checksums.txt)
cp -r "$repo/plugin" "$work/plugin"
rm -rf "$work/plugin/bin"

# file:// wants an absolute path; Git Bash's `pwd -W` gives the C:/... form curl needs
mirror=$(cd "$work/mirror" && { pwd -W 2>/dev/null || pwd; })
export MUNINN_RELEASE_URL="file:///${mirror#/}"
export HOME="$work/home" CLAUDE_PLUGIN_ROOT="$work/plugin"
proj=$(cd "$work/proj" && { pwd -W 2>/dev/null || pwd; })
git -C "$work/proj" init -q

event() { printf '{"session_id":"%s","cwd":"%s","hook_event_name":"SessionStart","source":"startup"}' "$1" "$proj"; }

echo "== first session: download, verify, serve"
event s1 | "$work/plugin/scripts/session-start" > "$work/out1.json"
test -x "$work/plugin/bin/muninn$ext"
case "$ext" in
  .exe) test -f "$HOME/.local/bin/muninn.exe" && test -f "$HOME/.local/bin/muninn.plugin" ;;
  *)    test -L "$HOME/.local/bin/muninn" ;;
esac

echo "== init and status in a fresh repository"
cli="$work/plugin/bin/muninn$ext"
(cd "$work/proj" && "$cli" init)
(cd "$work/proj" && "$cli" status) | tee "$work/status.txt"
grep -q '^MUNINN' "$work/status.txt"
grep -q '^/\?\.muninn/\?$' "$work/proj/.gitignore"

echo "== next session: memory header delivered"
event s2 | "$work/plugin/scripts/session-start" > "$work/out2.json"
grep -q 'Muninn memory' "$work/out2.json"

echo "install smoke passed on $target"
