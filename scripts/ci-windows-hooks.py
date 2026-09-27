"""Windows hook check, run by CI on a Windows runner after ci-install-smoke.sh.

Runs the hook commands the way each harness spawns them on Windows, against a local mirror
of the release assets:

- Codex runs `commandWindows` as `cmd.exe /C ""<line>""` (codex-rs/hooks, command_runner.rs).
  A plugin copy without bin/ runs the SessionStart line, which downloads the binary with
  PowerShell and then serves the hook; every other event's line then runs the binary.
- Claude Code spawns exec-form hooks without a shell, naming `bin/muninn` with no
  extension. Node and Bun spawn that path directly here, to show it resolves to
  bin/muninn.exe.

    python scripts/ci-windows-hooks.py <path to muninn.exe>
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

built = Path(sys.argv[1]).resolve()
repo = Path(__file__).resolve().parent.parent
work = Path(tempfile.mkdtemp())
mirror = work / "mirror"
mirror.mkdir()
asset = "muninn-x86_64-pc-windows-msvc.exe"
shutil.copy(built, mirror / asset)
digest = hashlib.sha256((mirror / asset).read_bytes()).hexdigest()
(mirror / "checksums.txt").write_text(f"{digest}  {asset}\n")
plugin = work / "plugin"
shutil.copytree(repo / "plugin", plugin, ignore=shutil.ignore_patterns("bin"))
proj = work / "proj"
proj.mkdir()
subprocess.run(["git", "init", "-q"], cwd=proj, check=True)

env = dict(os.environ)
env.update(
    PLUGIN_ROOT=str(plugin),
    CLAUDE_PLUGIN_ROOT=str(plugin),
    MUNINN_RELEASE_URL=mirror.as_uri(),
    MUNINN_BIN_DIR=str(work / "localbin"),
)
hooks = json.loads((repo / "plugin/hooks/codex.json").read_text())["hooks"]


def codex(event: str, payload: dict) -> str:
    line = hooks[event][0]["hooks"][0]["commandWindows"]
    out = subprocess.run(
        f'cmd.exe /C ""{line}""',
        input=json.dumps(payload).encode(),
        capture_output=True,
        cwd=proj,
        env=env,
        timeout=120,
    )
    print(f"{event}: exit {out.returncode}; stderr: {out.stderr.decode(errors='replace').strip()}")
    assert out.returncode == 0, event
    return out.stdout.decode(errors="replace")


base = {"session_id": "w1", "cwd": str(proj)}
codex("SessionStart", {**base, "hook_event_name": "SessionStart", "source": "startup"})
exe = plugin / "bin" / "muninn.exe"
assert exe.is_file(), "SessionStart did not install bin/muninn.exe"
assert (work / "localbin" / "muninn.exe").is_file(), "no terminal copy"
assert (work / "localbin" / "muninn.plugin").is_file(), "terminal copy not marked"

subprocess.run([str(exe), "init"], cwd=proj, check=True, env=env)
status = subprocess.run([str(exe), "status"], cwd=proj, env=env, capture_output=True, text=True)
print(status.stdout)
assert status.stdout.startswith("MUNINN"), status.stdout

out = codex("SessionStart", {**base, "hook_event_name": "SessionStart", "source": "startup"})
assert "Muninn memory" in out, out
codex("UserPromptSubmit", {**base, "hook_event_name": "UserPromptSubmit", "prompt": "hello"})
codex(
    "PreToolUse",
    {**base, "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "dir"}},
)
codex("Stop", {**base, "hook_event_name": "Stop"})

# Claude Code exec form: the command names bin/muninn, no extension
extless = (plugin / "bin" / "muninn").as_posix()
for runtime, code in [
    ("node", "const r=require('child_process').spawnSync(process.argv[1],['--version']);"
             "console.log(String(r.stdout).trim(), r.error||'');process.exit(r.status===0?0:1)"),
    ("bun", "const r=Bun.spawnSync([process.argv[process.argv.length-1],'--version']);"
            "console.log(r.stdout.toString().trim());process.exit(r.exitCode===0?0:1)"),
]:
    if shutil.which(runtime) is None:
        print(f"{runtime}: not installed, skipped")
        continue
    r = subprocess.run([runtime, "-e", code, extless], capture_output=True, text=True)
    print(f"{runtime} spawns {extless}: exit {r.returncode} {r.stdout.strip()} {r.stderr.strip()}")
    assert r.returncode == 0, runtime

print("windows hooks passed")
