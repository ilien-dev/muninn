//! `muninn init` and Codex, seen from outside the binary. Every test gives the binary a home
//! and a Codex home of its own under a temporary directory: the real ones are never read.
//!
//! What it holds: where Codex is present `init` leaves Muninn's hooks in the project's
//! `.codex/hooks.json` without being asked, once, beside whatever hooks the user has there;
//! `clean` takes out exactly those; and the commands that file registers, run the way Codex
//! runs them, ingest a rollout and deliver from it.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");
const ROLLOUT: &str = "rollout-2026-11-14T10-00-00-019a7c00-0000-7000-8000-000000000001.jsonl";

/// A project, a home and a Codex home, all temporary.
struct Machine {
    tmp: tempfile::TempDir,
}

impl Machine {
    /// `codex`: whether Codex has been run on this machine, which is what leaves its home.
    fn new(codex: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("project/.git")).unwrap();
        std::fs::create_dir_all(tmp.path().join("home")).unwrap();
        if codex {
            std::fs::create_dir_all(tmp.path().join("home/.codex")).unwrap();
        }
        Machine { tmp }
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("project")
    }

    fn codex_home(&self) -> PathBuf {
        self.tmp.path().join("home/.codex")
    }

    fn hooks_file(&self) -> PathBuf {
        self.root().join(".codex/hooks.json")
    }

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env("HOME", self.tmp.path().join("home"))
            .env("USERPROFILE", self.tmp.path().join("home"))
            .env_remove("CODEX_HOME")
            .env("MUNINN_NO_PROJECT", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Run `muninn <args>` in the project; returns (exit code, stdout, stderr).
    fn muninn(&self, args: &[&str]) -> (i32, String, String) {
        let mut cmd = self.command(BIN);
        cmd.arg("--cwd").arg(self.root()).args(args);
        finish(cmd, "")
    }

    fn hooks(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.hooks_file()).unwrap()).unwrap()
    }

    /// Run the command the project's hooks file registers for `event`, the way Codex does:
    /// through the session's shell, in the project, with the event's input on stdin. On
    /// Windows that shell is PowerShell (codex-cli 0.166.0, read from a real session).
    fn codex_fires(&self, event: &str, input: Value) -> (i32, String, String) {
        self.codex_fires_in(Shell::Session, event, input)
    }

    fn codex_fires_in(&self, shell: Shell, event: &str, input: Value) -> (i32, String, String) {
        let handler = &self.hooks()["hooks"][event][0]["hooks"][0];
        let mut input = input;
        input["hook_event_name"] = json!(event);
        input["cwd"] = json!(self.root());
        input["model"] = json!("gpt-fixture");
        #[cfg(windows)]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            let line = handler["commandWindows"]
                .as_str()
                .expect("a Windows command");
            match shell {
                Shell::Session => {
                    let mut cmd = self.command("powershell.exe");
                    cmd.args(["-NoProfile", "-Command", line]);
                    cmd
                }
                // what Codex runs when the session has no shell: `%COMSPEC% /C "<line>"`
                Shell::Fallback => {
                    let mut cmd = self.command("cmd.exe");
                    cmd.raw_arg(format!("/C \"{line}\""));
                    cmd
                }
            }
        };
        #[cfg(not(windows))]
        let mut cmd = {
            // one shell family here: Codex runs the user's shell with the line as one
            // argument, and the line is a single-quoted path with two words after it
            let _ = shell;
            let line = handler["command"].as_str().expect("a command");
            let mut cmd = self.command("sh");
            cmd.arg("-c").arg(line);
            cmd
        };
        cmd.current_dir(self.root());
        finish(cmd, &input.to_string())
    }

    /// `init` run from a copy of the binary under `dir`, a directory of this machine.
    fn init_from(&self, dir: &str) -> (i32, String, String) {
        let at = self.tmp.path().join(dir);
        std::fs::create_dir_all(&at).unwrap();
        let copy = at.join(Path::new(BIN).file_name().unwrap());
        std::fs::copy(BIN, &copy).unwrap();
        let mut cmd = self.command(copy.to_str().unwrap());
        cmd.arg("--cwd")
            .arg(self.root())
            .args(["init", "--keep-native"]);
        finish(cmd, "")
    }
}

/// The two shells Codex may hand a hook's command line to.
#[derive(Clone, Copy, Debug)]
enum Shell {
    Session,
    Fallback,
}

fn finish(mut cmd: Command, stdin: &str) -> (i32, String, String) {
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn muninn_handlers(hooks: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (event, groups) in hooks["hooks"].as_object().unwrap() {
        for g in groups.as_array().unwrap() {
            for h in g["hooks"].as_array().unwrap() {
                let c = h["command"].as_str().unwrap_or("");
                if c.contains("muninn") && c.contains(" hook ") {
                    out.push((event.clone(), c.to_string()));
                }
            }
        }
    }
    out
}

/// Codex has been run on this machine: `init` leaves its hooks in the project, saying which
/// file it wrote and that Codex wants them reviewed. They are the hooks the repository's
/// template documents, and the ones `--codex` writes.
#[test]
fn init_installs_the_codex_hooks_where_codex_is_present() {
    let m = Machine::new(true);
    let (code, out, err) = m.muninn(&["init", "--keep-native"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(".codex/hooks.json"), "{out}");
    assert!(
        out.contains("/hooks"),
        "init says Codex wants the hooks reviewed: {out}"
    );
    // I2. The file names this machine's binary by its full path, and it is in the project
    // tree where nothing ignores it: committed, it carries that path to every teammate.
    assert!(
        out.contains("this machine's muninn binary") && out.contains("do not commit"),
        "init says the file is for this machine only: {out}"
    );
    let written = m.hooks();
    let template: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../codex/hooks.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let events = |v: &Value| {
        v["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(events(&written), events(&template));
    for event in events(&template) {
        let (w, t) = (&written["hooks"][&event][0], &template["hooks"][&event][0]);
        assert_eq!(w["matcher"], t["matcher"], "{event}");
        assert_eq!(
            w["hooks"][0]["timeout"], t["hooks"][0]["timeout"],
            "{event}"
        );
        assert_eq!(w["hooks"][0]["async"], t["hooks"][0]["async"], "{event}");
        let cmd = w["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.ends_with(&format!(" hook {event}")), "{cmd}");
    }
    // asked for outright on a machine without Codex, the file is the same
    let asked = Machine::new(false);
    assert_eq!(asked.muninn(&["init", "--keep-native", "--codex"]).0, 0);
    assert_eq!(asked.hooks(), written);
}

/// No Codex home and no `.codex` in the project: nothing is written, and nothing is said.
#[test]
fn init_writes_no_codex_file_where_codex_is_absent() {
    let m = Machine::new(false);
    let (code, out, err) = m.muninn(&["init", "--keep-native"]);
    assert_eq!(code, 0, "{err}");
    assert!(!m.root().join(".codex").exists());
    assert!(!out.to_lowercase().contains("codex"), "{out}");
    // `$CODEX_HOME` names the home when it is set, and a directory that is not there is no Codex
    let mut cmd = m.command(BIN);
    cmd.arg("--cwd")
        .arg(m.root())
        .args(["init", "--keep-native"]);
    cmd.env("CODEX_HOME", m.tmp.path().join("nowhere"));
    assert_eq!(finish(cmd, "").0, 0);
    assert!(!m.root().join(".codex").exists());
    // a project that has a `.codex` directory of its own is a Codex project
    std::fs::create_dir_all(m.root().join(".codex")).unwrap();
    assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
    assert!(m.hooks_file().is_file());
}

/// `init` runs again at every upgrade. The second run changes nothing and says nothing of the
/// file; a run from a build at another path moves Muninn's handlers there and adds none.
#[test]
fn init_twice_leaves_one_set_of_hooks() {
    let m = Machine::new(true);
    assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
    let first = std::fs::read_to_string(m.hooks_file()).unwrap();
    let (code, out, _) = m.muninn(&["init", "--keep-native"]);
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(m.hooks_file()).unwrap(), first);
    assert!(
        !out.contains(".codex/hooks.json"),
        "nothing was touched: {out}"
    );
    // the file an earlier build left, pointing at a binary that has since moved
    let elsewhere = if cfg!(windows) {
        r"C:\earlier build\muninn.exe"
    } else {
        "/earlier build/muninn"
    };
    let mut moved = m.hooks();
    for groups in moved["hooks"].as_object_mut().unwrap().values_mut() {
        for key in ["command", "commandWindows"] {
            if let Some(c) = groups[0]["hooks"][0][key].as_str() {
                groups[0]["hooks"][0][key] = json!(c.replace(BIN, elsewhere));
            }
        }
    }
    assert_eq!(
        muninn_handlers(&moved)
            .iter()
            .filter(|(_, c)| c.contains("earlier build"))
            .count(),
        7
    );
    std::fs::write(
        m.hooks_file(),
        serde_json::to_string_pretty(&moved).unwrap(),
    )
    .unwrap();
    assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
    let ours = muninn_handlers(&m.hooks());
    assert_eq!(ours.len(), 7, "{ours:?}");
    assert!(
        ours.iter().all(|(_, c)| !c.contains("earlier build")),
        "{ours:?}"
    );
    assert_eq!(std::fs::read_to_string(m.hooks_file()).unwrap(), first);
}

/// The user has hooks of their own in the project. `init` adds Muninn's beside them and
/// changes none of theirs; `clean` takes Muninn's out and leaves the file as it was.
#[test]
fn the_users_own_codex_hooks_are_kept_by_init_and_by_clean() {
    let m = Machine::new(true);
    let theirs = json!({
        "description": "team hooks",
        "hooks": {
            "Stop": [{ "hooks": [{ "type": "command", "command": "python3 notify.py", "timeout": 3 }] }],
            "PermissionRequest": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "./ask.sh" }] }],
        }
    });
    std::fs::create_dir_all(m.root().join(".codex")).unwrap();
    std::fs::write(
        m.hooks_file(),
        serde_json::to_string_pretty(&theirs).unwrap(),
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
        let v = m.hooks();
        assert_eq!(v["description"], "team hooks");
        assert_eq!(
            v["hooks"]["PermissionRequest"],
            theirs["hooks"]["PermissionRequest"]
        );
        assert_eq!(v["hooks"]["Stop"][0], theirs["hooks"]["Stop"][0]);
        assert_eq!(muninn_handlers(&v).len(), 7);
    }
    let (code, out, err) = m.muninn(&["clean", "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(".codex/hooks.json"), "{out}");
    assert_eq!(m.hooks(), theirs);
}

/// A project whose only Codex hooks were Muninn's has no hooks file after `clean`; one whose
/// file the user had left empty gets that file back.
#[test]
fn clean_removes_the_file_only_when_init_made_it() {
    let m = Machine::new(true);
    assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
    assert!(m.hooks_file().is_file());
    assert_eq!(m.muninn(&["clean", "--yes"]).0, 0);
    assert!(!m.hooks_file().exists());

    let m = Machine::new(true);
    std::fs::create_dir_all(m.root().join(".codex")).unwrap();
    std::fs::write(m.hooks_file(), "{\"hooks\":{}}").unwrap();
    assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
    assert_eq!(muninn_handlers(&m.hooks()).len(), 7);
    assert_eq!(m.muninn(&["clean", "--yes"]).0, 0);
    assert_eq!(m.hooks(), json!({ "hooks": {} }));
}

/// A `.codex/hooks.json` that is not a hooks file is the user's to repair. Unasked, `init`
/// leaves it byte for byte, says so and goes on; asked with `--codex`, it refuses.
#[test]
fn a_hooks_file_init_cannot_read_is_left_alone() {
    for broken in [
        "{ \"hooks\": { \"Stop\": [",
        "[]",
        "{\"hooks\": {\"Stop\": {}}}",
    ] {
        let m = Machine::new(true);
        std::fs::create_dir_all(m.root().join(".codex")).unwrap();
        std::fs::write(m.hooks_file(), broken).unwrap();
        let (code, out, err) = m.muninn(&["init", "--keep-native"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(std::fs::read_to_string(m.hooks_file()).unwrap(), broken);
        assert!(
            out.contains("left") && out.contains(".codex/hooks.json"),
            "{out}"
        );
        let (code, _, err) = m.muninn(&["init", "--keep-native", "--codex"]);
        assert_eq!(code, 1, "{broken}");
        assert!(err.contains(".codex/hooks.json"), "{err}");
        assert_eq!(std::fs::read_to_string(m.hooks_file()).unwrap(), broken);
        // and `clean` does not delete what `init` never wrote
        assert_eq!(m.muninn(&["clean", "--yes"]).0, 0);
        assert_eq!(std::fs::read_to_string(m.hooks_file()).unwrap(), broken);
    }
}

/// A file that is there and cannot be read as text is not "no file". PowerShell 5.1 writes
/// UTF-16 when a command's output is redirected into a file; a directory of that name cannot
/// be read at all. `init` writes over neither, and `clean` deletes neither.
#[test]
fn a_hooks_file_that_is_not_text_is_never_taken_for_no_file() {
    // UTF-16 LE with its byte order mark, as `'{"hooks":{}}' > hooks.json` leaves it
    let mut utf16: Vec<u8> = vec![0xFF, 0xFE];
    for unit in "{\"hooks\":{}}\r\n".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    let m = Machine::new(true);
    std::fs::create_dir_all(m.root().join(".codex")).unwrap();
    std::fs::write(m.hooks_file(), &utf16).unwrap();
    let (code, out, err) = m.muninn(&["init", "--keep-native"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        std::fs::read(m.hooks_file()).unwrap(),
        utf16,
        "written over"
    );
    assert!(
        out.contains("left") && out.contains(".codex/hooks.json"),
        "{out}"
    );
    let (code, _, err) = m.muninn(&["init", "--keep-native", "--codex"]);
    assert_eq!(code, 1);
    assert!(err.contains(".codex/hooks.json"), "{err}");
    assert_eq!(m.muninn(&["clean", "--yes"]).0, 0);
    assert_eq!(
        std::fs::read(m.hooks_file()).unwrap(),
        utf16,
        "deleted by clean"
    );

    // something of that name that cannot be opened as a file
    let m = Machine::new(true);
    std::fs::create_dir_all(m.hooks_file().join("kept")).unwrap();
    let (code, out, err) = m.muninn(&["init", "--keep-native"]);
    assert_eq!(code, 0, "{err}");
    assert!(m.hooks_file().join("kept").is_dir());
    assert!(
        out.contains("left") && out.contains(".codex/hooks.json"),
        "{out}"
    );
    assert_eq!(m.muninn(&["init", "--keep-native", "--codex"]).0, 1);
    assert_eq!(m.muninn(&["clean", "--yes"]).0, 0);
    assert!(m.hooks_file().join("kept").is_dir(), "deleted by clean");
}

/// `status` says which route Codex's hooks take in this project, or that there is none.
#[test]
fn status_says_which_route_the_codex_hooks_take() {
    let m = Machine::new(true);
    assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
    let (_, out, _) = m.muninn(&["status"]);
    assert!(
        out.contains("codex: hooks from this project's .codex/hooks.json"),
        "{out}"
    );
    let (_, json, _) = m.muninn(&["--json", "status"]);
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["codex_hooks"], "project");
    std::fs::remove_file(m.hooks_file()).unwrap();
    let (_, out, _) = m.muninn(&["status"]);
    assert!(
        out.contains("codex:") && out.contains("no Muninn hooks") && out.contains("muninn init"),
        "{out}"
    );
    let none = Machine::new(false);
    assert_eq!(none.muninn(&["init", "--keep-native"]).0, 0);
    let (_, out, _) = none.muninn(&["status"]);
    assert!(!out.contains("codex:"), "{out}");
    let (_, json, _) = none.muninn(&["--json", "status"]);
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["codex_hooks"], Value::Null);
}

impl Machine {
    /// A Codex plugin as `codex plugin add` leaves it: under the Codex home's plugin cache,
    /// by marketplace, plugin and version, with its manifest.
    fn install_codex_plugin(&self, marketplace: &str, name: &str) -> PathBuf {
        let root = self
            .codex_home()
            .join("plugins/cache")
            .join(marketplace)
            .join(name)
            .join("1.2.0");
        std::fs::create_dir_all(root.join(".codex-plugin")).unwrap();
        std::fs::write(
            root.join(".codex-plugin/plugin.json"),
            json!({"name": name, "version": "1.2.0", "hooks": "./hooks/codex.json"}).to_string(),
        )
        .unwrap();
        root
    }
}

/// With the Codex plugin installed its hooks already run in every project. Unasked, `init`
/// adds none of its own beside them, where each hook would run twice, and says which
/// directory it found; `status` names the plugin as the route. A plugin of another name is
/// not Muninn's, and `--codex` still writes the file for whoever asks.
#[test]
fn init_leaves_the_hooks_to_an_installed_codex_plugin() {
    let m = Machine::new(true);
    let plugin = m.install_codex_plugin("muninn", "muninn");
    let (code, out, err) = m.muninn(&["init", "--keep-native"]);
    assert_eq!(code, 0, "{err}");
    assert!(!m.hooks_file().exists(), "{out}");
    assert!(
        out.contains("Codex plugin") && out.contains("--codex"),
        "{out}"
    );
    let slashed = |s: &str| s.replace('\\', "/");
    let where_ = slashed(&plugin.parent().unwrap().display().to_string());
    assert!(
        slashed(&out).contains(&where_),
        "init names {where_}: {out}"
    );
    let (_, out, _) = m.muninn(&["status"]);
    assert!(out.contains("codex: hooks from the Codex plugin"), "{out}");
    let (_, json, _) = m.muninn(&["--json", "status"]);
    assert_eq!(
        serde_json::from_str::<Value>(&json).unwrap()["codex_hooks"],
        "plugin"
    );

    // asked for outright, the file is written, and status says every hook now has two routes
    assert_eq!(m.muninn(&["init", "--keep-native", "--codex"]).0, 0);
    assert_eq!(muninn_handlers(&m.hooks()).len(), 7);
    let (_, out, _) = m.muninn(&["status"]);
    assert!(out.contains("codex:") && out.contains("twice"), "{out}");
    // and a later plain init neither adds to that file nor takes from it
    let before = std::fs::read_to_string(m.hooks_file()).unwrap();
    assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
    assert_eq!(std::fs::read_to_string(m.hooks_file()).unwrap(), before);

    // another plugin in the cache is not Muninn's
    let other = Machine::new(true);
    other.install_codex_plugin("openai-curated-remote", "sites");
    assert_eq!(other.muninn(&["init", "--keep-native"]).0, 0);
    assert_eq!(muninn_handlers(&other.hooks()).len(), 7);
}

/// Whichever shell Codex hands the command to, it runs: installed from a path with nothing to
/// quote and from one with a space in it, a session start answers with its context in both.
#[test]
fn the_installed_command_runs_in_either_shell_codex_uses() {
    for dir in ["bin", "program files"] {
        let m = Machine::new(true);
        let (code, _, err) = m.init_from(dir);
        assert_eq!(code, 0, "{err}");
        for shell in [Shell::Session, Shell::Fallback] {
            let (code, out, err) = m.codex_fires_in(
                shell,
                "SessionStart",
                json!({"session_id": "s1", "transcript_path": Value::Null, "source": "startup"}),
            );
            assert_eq!(code, 0, "{dir} {shell:?}: {err}");
            let v: Value = serde_json::from_str(&out)
                .unwrap_or_else(|e| panic!("{dir} {shell:?}: no answer ({e}): {out:?} {err:?}"));
            assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        }
    }
}

/// The route a Codex session takes, end to end without Codex: the commands `init` registered
/// are run through the shell Codex uses, with the input Codex documents. A session's rollout
/// is ingested at its Stop, and the next session is given what the person said in it, at
/// session start and when a prompt asks.
#[test]
fn the_hooks_init_installs_ingest_a_rollout_and_deliver_from_it() {
    let m = Machine::new(true);
    assert_eq!(m.muninn(&["init", "--keep-native"]).0, 0);
    let sessions = m.codex_home().join("sessions/2026/11/14");
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join(ROLLOUT);
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../muninn-capture/tests/fixtures/codex")
            .join(ROLLOUT),
        &rollout,
    )
    .unwrap();
    let first = "019a7c00-0000-7000-8000-000000000001";

    let (code, out, err) = m.codex_fires(
        "SessionStart",
        json!({"session_id": first, "transcript_path": rollout, "source": "startup", "permission_mode": "default"}),
    );
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).expect("SessionStart answers in JSON");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let (code, out, err) = m.codex_fires(
        "Stop",
        json!({"session_id": first, "transcript_path": rollout, "turn_id": "t", "stop_hook_active": false,
               "last_assistant_message": "Moving the tide cache to Postgres.", "permission_mode": "default"}),
    );
    assert_eq!(
        (code, out.trim()),
        (0, ""),
        "Stop answers nothing on stdout: {err}"
    );
    let (code, _, err) = m.codex_fires(
        "SessionEnd",
        json!({"session_id": first, "transcript_path": rollout, "reason": "other"}),
    );
    assert_eq!(code, 0, "{err}");

    let second = "019a7c00-0000-7000-8000-000000000002";
    let (code, out, err) = m.codex_fires(
        "SessionStart",
        json!({"session_id": second, "transcript_path": Value::Null, "source": "startup", "permission_mode": "default"}),
    );
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let context = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        context.contains("Never commit the generated tables under out/"),
        "{context}"
    );
    // the queued message is nobody's rule: where it shows at all it is a thing that was said
    for line in context
        .lines()
        .filter(|l| l.contains("push straight to main"))
    {
        assert!(line.contains(" episode "), "{line}");
    }
    assert!(!context.contains("user: Always push"), "{context}");
    let (code, out, err) = m.codex_fires(
        "UserPromptSubmit",
        json!({"session_id": second, "transcript_path": Value::Null, "turn_id": "t2", "permission_mode": "default",
               "prompt": "Which database does the tide cache use, and why did we choose it?"}),
    );
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).expect("a block for the prompt");
    let block = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(block.contains("Postgres"), "{block}");
}
