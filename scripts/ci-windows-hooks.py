"""Windows hook check, run by CI on a Windows runner after ci-install-smoke.sh.

Runs the hook commands the way each harness spawns them on Windows, against a local mirror
of the release assets:

- Codex hands `commandWindows` to the session's shell with the line as one argument, which
  is Windows PowerShell unless the session has none, and then runs `cmd.exe /C "<line>"`
  (codex-rs/hooks, command_runner.rs; codex-cli 0.166.0 ran a probe hook in PowerShell). The
  whole sequence below runs once through each, on a plugin copy of its own whose path holds a
  space. A plugin copy without bin/ runs the SessionStart line, which downloads the binary
  with PowerShell and then serves the hook; every other event's line then runs the binary.
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
hooks = json.loads((repo / "plugin/hooks/codex.json").read_text())["hooks"]


def spawn(shell: str, line: str):
    """The command Codex starts for one hook line."""
    if shell == "cmd":
        # command_runner.rs: raw_arg(format!(r#""{command_line}""#)), one pair of quotes
        # around the line; cmd.exe strips exactly that pair
        return f'cmd.exe /C "{line}"'
    return ["powershell.exe", "-NoProfile", "-Command", line]


def run_as(shell: str) -> Path:
    plugin = work / f"plug in {shell}"
    shutil.copytree(repo / "plugin", plugin, ignore=shutil.ignore_patterns("bin"))
    proj = work / f"proj-{shell}"
    proj.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=proj, check=True)
    env = dict(os.environ)
    env.update(
        PLUGIN_ROOT=str(plugin),
        CLAUDE_PLUGIN_ROOT=str(plugin),
        MUNINN_RELEASE_URL=mirror.as_uri(),
        MUNINN_BIN_DIR=str(work / f"localbin-{shell}"),
    )

    def codex(event: str, payload: dict) -> str:
        line = hooks[event][0]["hooks"][0]["commandWindows"]
        out = subprocess.run(
            spawn(shell, line),
            input=json.dumps(payload).encode(),
            capture_output=True,
            cwd=proj,
            env=env,
            timeout=120,
        )
        print(f"[{shell}] {event}: exit {out.returncode}; stderr: {out.stderr.decode(errors='replace').strip()}")
        assert out.returncode == 0, f"{shell} {event}"
        return out.stdout.decode(errors="replace")

    base = {"session_id": "w1", "cwd": str(proj)}
    codex("SessionStart", {**base, "hook_event_name": "SessionStart", "source": "startup"})
    exe = plugin / "bin" / "muninn.exe"
    assert exe.is_file(), f"[{shell}] SessionStart did not install bin/muninn.exe"
    assert (work / f"localbin-{shell}" / "muninn.exe").is_file(), "no terminal copy"
    assert (work / f"localbin-{shell}" / "muninn.plugin").is_file(), "terminal copy not marked"

    subprocess.run([str(exe), "init"], cwd=proj, check=True, env=env)
    status = subprocess.run([str(exe), "status"], cwd=proj, env=env, capture_output=True, text=True)
    print(status.stdout)
    assert status.stdout.startswith("MUNINN"), status.stdout

    out = codex("SessionStart", {**base, "hook_event_name": "SessionStart", "source": "startup"})
    assert "Muninn memory" in out, f"[{shell}] {out}"
    if shell == "cmd":
        # With the binary there, the SessionStart line runs it and nothing else: starting
        # PowerShell first cost 145.9 ms against 16.4 ms without it (p50, 12 runs each; the
        # line as it stands now, inside `cmd --% /c`, reads 32.0 ms here). Shown
        # by taking PowerShell off the PATH: the line still serves the hook. Where PowerShell
        # is the shell Codex starts, there is nothing to take away.
        with_ps = env["PATH"]
        env["PATH"] = os.pathsep.join(p for p in with_ps.split(os.pathsep) if "powershell" not in p.lower())
        assert shutil.which("powershell", path=env["PATH"]) is None, "PowerShell is still on the PATH"
        out = codex("SessionStart", {**base, "hook_event_name": "SessionStart", "source": "startup"})
        assert "Muninn memory" in out, "with the binary present the line needs no PowerShell: " + out
        env["PATH"] = with_ps
    codex("UserPromptSubmit", {**base, "hook_event_name": "UserPromptSubmit", "prompt": "hello"})
    codex(
        "PreToolUse",
        {**base, "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "dir"}},
    )
    codex("Stop", {**base, "hook_event_name": "Stop"})
    codex("SessionEnd", {**base, "hook_event_name": "SessionEnd", "reason": "other"})
    beats = (proj / ".muninn" / "log" / "heartbeat.jsonl").read_text(errors="replace")
    for event in ("SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"):
        assert f'"hook":"{event}"' in beats, f"[{shell}] {event} left no heartbeat: the line ran nothing"
    # a download that fails still exits 0 and prints nothing a harness would read as context
    broken = work / f"plug in {shell} offline"
    shutil.copytree(repo / "plugin", broken, ignore=shutil.ignore_patterns("bin"))
    env["PLUGIN_ROOT"] = str(broken)
    env["MUNINN_RELEASE_URL"] = (work / "nowhere").as_uri()
    out = codex("SessionStart", {**base, "hook_event_name": "SessionStart", "source": "startup"})
    assert out.strip() == "", f"[{shell}] a failed download printed: {out}"
    assert not (broken / "bin" / "muninn.exe").exists()
    return plugin


plugin = None
for shell in ("powershell", "cmd"):
    plugin = run_as(shell)

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
