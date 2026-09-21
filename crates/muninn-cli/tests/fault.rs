//! Fault injection: 17 scenarios × N repetitions (MUNINN_FAULT_REPS, default 3; CI 200).
//! Modelled on the reliability study in [Y1]. Each scenario states the property it
//! holds: the hook exits 0 and never blocks the agent; data is never lost; a broken
//! store shows as RED in the gate, never as silence.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

fn reps() -> usize {
    std::env::var("MUNINN_FAULT_REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
}

fn init_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let out = Command::new(BIN)
        .args([
            "--cwd",
            tmp.path().to_str().unwrap(),
            "init",
            "--keep-native",
            "--no-boot-block",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    tmp
}

fn payload(root: &Path, extra: serde_json::Value) -> String {
    let mut v = serde_json::json!({ "session_id": "fault", "cwd": root });
    if let (Some(a), Some(b)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in b {
            a.insert(k.clone(), val.clone());
        }
    }
    v.to_string()
}

struct Run {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    elapsed: Duration,
}

/// Run a hook with a wall-clock limit. A hook that outlives the limit is a failure.
fn hook_with(root: &Path, event: &str, input: &str, env: &[(&str, &str)], stdout: Stdio) -> Run {
    let mut cmd = Command::new(BIN);
    cmd.args(["hook", event])
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let _ = root;
    let t = Instant::now();
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).ok();
    let limit = Duration::from_secs(3);
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            let out: Output = child.wait_with_output().unwrap();
            return Run {
                status: st.code(),
                stdout: String::from_utf8_lossy(&out.stdout).into(),
                stderr: String::from_utf8_lossy(&out.stderr).into(),
                elapsed: t.elapsed(),
            };
        }
        if t.elapsed() > limit {
            child.kill().ok();
            return Run {
                status: None,
                stdout: String::new(),
                stderr: "TIMEOUT".into(),
                elapsed: t.elapsed(),
            };
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn hook(root: &Path, event: &str, extra: serde_json::Value) -> Run {
    hook_with(root, event, &payload(root, extra), &[], Stdio::piped())
}

fn doctor(root: &Path) -> serde_json::Value {
    let out = Command::new(BIN)
        .args(["--cwd", root.to_str().unwrap(), "--json", "doctor"])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or(
        serde_json::json!({"summary": format!("NOJSON {}", String::from_utf8_lossy(&out.stderr))}),
    )
}

fn summary(root: &Path) -> String {
    doctor(root)["summary"].as_str().unwrap_or("").to_string()
}

fn assert_clean_exit(r: &Run, what: &str) {
    assert_eq!(
        r.status,
        Some(0),
        "{what}: exit {:?} stderr={} ({:?})",
        r.status,
        r.stderr,
        r.elapsed
    );
    assert!(
        r.elapsed < Duration::from_secs(2),
        "{what}: took {:?}",
        r.elapsed
    );
    assert!(
        !r.stderr.contains("panicked"),
        "{what}: panic: {}",
        r.stderr
    );
}

fn db_path(root: &Path) -> PathBuf {
    root.join(".muninn/muninn.db")
}

// 1. Another process holds an exclusive lock.
#[test]
fn s01_db_locked_by_other_process() {
    for _ in 0..reps() {
        let p = init_project();
        let conn = rusqlite::Connection::open(db_path(p.path())).unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let r = hook(p.path(), "SessionStart", serde_json::json!({}));
        assert_clean_exit(&r, "locked SessionStart");
        let w = hook(p.path(), "SessionEnd", serde_json::json!({}));
        assert_clean_exit(&w, "locked SessionEnd");
        conn.execute_batch("COMMIT").unwrap();
        drop(conn);
        let s = summary(p.path());
        assert!(!s.contains("RED integrity"), "{s}");
    }
}

// 2. The database file is garbage.
#[test]
fn s02_corrupt_db_file() {
    for _ in 0..reps() {
        let p = init_project();
        std::fs::write(
            db_path(p.path()),
            b"this is not a database at all, but it is long enough to look like one\0\0\0",
        )
        .unwrap();
        for ev in ["SessionStart", "UserPromptSubmit", "SessionEnd"] {
            let r = hook(p.path(), ev, serde_json::json!({}));
            assert_clean_exit(&r, &format!("corrupt {ev}"));
        }
        let s = summary(p.path());
        assert!(
            s.starts_with("MUNINN RED integrity"),
            "corruption must be RED, got: {s}"
        );
    }
}

// 3. No space / no write: heartbeat log is /dev/full, store directory read-only.
#[cfg(unix)]
#[test]
fn s03_disk_full_and_readonly_store() {
    use std::os::unix::fs::PermissionsExt;
    for _ in 0..reps() {
        let p = init_project();
        let log = p.path().join(".muninn/log/heartbeat.jsonl");
        let _ = std::fs::remove_file(&log);
        std::os::unix::fs::symlink("/dev/full", &log).unwrap();
        let r = hook(p.path(), "SessionStart", serde_json::json!({}));
        assert_clean_exit(&r, "disk-full SessionStart");
        assert!(
            r.stdout.contains("MUNINN"),
            "health line must still be delivered: {}",
            r.stdout
        );
        // the detached write path may already have folded (and removed) the log
        let _ = std::fs::remove_file(&log);
        let dir = p.path().join(".muninn");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let w = hook(p.path(), "SessionEnd", serde_json::json!({}));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_clean_exit(&w, "readonly SessionEnd");
    }
}

// 4. stdout is dead while the hook writes (13 such failures in [Q3]).
#[cfg(unix)]
#[test]
fn s04_stdout_dead() {
    for _ in 0..reps() {
        let p = init_project();
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let r = hook_with(
            p.path(),
            "SessionStart",
            &payload(p.path(), serde_json::json!({})),
            &[],
            Stdio::from(full),
        );
        assert_eq!(
            r.status,
            Some(0),
            "stdout dead: {:?} {}",
            r.status,
            r.stderr
        );
        assert!(!r.stderr.contains("panicked"), "{}", r.stderr);
        let w = hook_with(
            p.path(),
            "SessionEnd",
            &payload(p.path(), serde_json::json!({})),
            &[],
            Stdio::null(),
        );
        assert_clean_exit(&w, "SessionEnd with null stdout");
    }
}

// 5. Transcript full of control characters and invalid UTF-8.
#[test]
fn s05_transcript_control_chars() {
    for _ in 0..reps() {
        let p = init_project();
        let t = p.path().join("t.jsonl");
        let mut bytes = br#"{"type":"user","message":{"content":"hi"#.to_vec();
        bytes.extend_from_slice(&[0x00, 0x1b, 0x5b, 0xff, 0xfe, 0x07]);
        bytes.extend_from_slice(
            b"\"}}\n{\"type\":\"assistant\",\"message\":{\"content\":\"ok\"}}\n",
        );
        std::fs::write(&t, bytes).unwrap();
        let r = hook(
            p.path(),
            "SessionEnd",
            serde_json::json!({ "transcript_path": t }),
        );
        assert_clean_exit(&r, "control chars");
        let s = summary(p.path());
        assert!(!s.starts_with("MUNINN RED"), "{s}");
    }
}

// 6. Transcript truncated mid-line.
#[test]
fn s06_transcript_truncated() {
    for _ in 0..reps() {
        let p = init_project();
        let t = p.path().join("t.jsonl");
        std::fs::write(
            &t,
            b"{\"type\":\"user\",\"message\":{\"content\":\"start\"}}\n{\"type\":\"assis",
        )
        .unwrap();
        let r = hook(
            p.path(),
            "SessionEnd",
            serde_json::json!({ "transcript_path": t }),
        );
        assert_clean_exit(&r, "truncated");
    }
}

// 7. The hook is killed half-way; the store must stay sound (WAL).
#[cfg(unix)]
#[test]
fn s07_killed_midway() {
    for _ in 0..reps() {
        let p = init_project();
        let mut child = Command::new(BIN)
            .args(["hook", "SessionEnd"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload(p.path(), serde_json::json!({})).as_bytes())
            .ok();
        child.kill().ok();
        child.wait().ok();
        let s = summary(p.path());
        assert!(!s.contains("RED integrity"), "store damaged by kill: {s}");
        let r = hook(p.path(), "SessionEnd", serde_json::json!({}));
        assert_clean_exit(&r, "SessionEnd after kill");
    }
}

// 8. Clock goes backwards between sessions.
#[test]
fn s08_clock_backwards() {
    for _ in 0..reps() {
        let p = init_project();
        let w = hook(p.path(), "SessionEnd", serde_json::json!({}));
        assert_clean_exit(&w, "SessionEnd");
        let r = hook_with(
            p.path(),
            "SessionStart",
            &payload(p.path(), serde_json::json!({})),
            &[("MUNINN_FAKE_NOW_MS", "1000")],
            Stdio::piped(),
        );
        assert_clean_exit(&r, "clock backwards");
        assert!(
            r.stdout.contains("MUNINN") && !r.stdout.contains("RED"),
            "{}",
            r.stdout
        );
    }
}

// 9. Several sessions write at once.
#[test]
fn s09_concurrent_writers() {
    for _ in 0..reps() {
        let p = init_project();
        let root = p.path().to_path_buf();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let root = root.clone();
                std::thread::spawn(move || {
                    let input = serde_json::json!({ "session_id": format!("c{i}"), "cwd": root })
                        .to_string();
                    hook_with(&root, "SessionEnd", &input, &[], Stdio::piped())
                })
            })
            .collect();
        for h in handles {
            let r = h.join().unwrap();
            assert_clean_exit(&r, "concurrent SessionEnd");
        }
        let s = summary(&root);
        assert!(!s.starts_with("MUNINN RED"), "{s}");
        // property: no heartbeat is lost — folded into the store, or past the fold
        // watermark for the next fold (the live log is never renamed under appenders)
        let conn = rusqlite::Connection::open(db_path(&root)).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM heartbeat", [], |r| r.get(0))
            .unwrap();
        let pending =
            muninn_core::logfold::pending(&conn, &root.join(".muninn/log/heartbeat.jsonl"))
                .iter()
                .filter(|l| l.contains("\"ev\":\"start\""))
                .count() as i64;
        assert!(
            n + pending >= 4,
            "heartbeats folded {n} + pending {pending}"
        );
    }
}

// 10. Older and newer schema versions on disk.
#[test]
fn s10_schema_versions() {
    for _ in 0..reps() {
        // older: an empty file where the store should be
        let p = init_project();
        std::fs::remove_file(db_path(p.path())).unwrap();
        std::fs::write(db_path(p.path()), b"").unwrap();
        let r = hook(p.path(), "SessionStart", serde_json::json!({}));
        assert_clean_exit(&r, "empty db SessionStart");
        let w = hook(p.path(), "SessionEnd", serde_json::json!({}));
        assert_clean_exit(&w, "empty db SessionEnd migrates");
        let s = summary(p.path());
        assert!(!s.starts_with("MUNINN RED"), "after migration: {s}");
        // newer: refuse to touch, report RED
        let conn = rusqlite::Connection::open(db_path(p.path())).unwrap();
        conn.execute("UPDATE meta SET value='99' WHERE key='schema_version'", [])
            .unwrap();
        drop(conn);
        let r = hook(p.path(), "SessionStart", serde_json::json!({}));
        assert_clean_exit(&r, "newer schema SessionStart");
        assert!(
            r.stdout.contains("RED"),
            "newer schema must be RED: {}",
            r.stdout
        );
        let w = hook(p.path(), "SessionEnd", serde_json::json!({}));
        assert_clean_exit(&w, "newer schema SessionEnd");
    }
}

// 11. .muninn/ deleted while a session is running.
#[test]
fn s11_store_dir_deleted_midsession() {
    for _ in 0..reps() {
        let p = init_project();
        let _ = hook(p.path(), "SessionStart", serde_json::json!({}));
        // the detached write path may still be touching the directory: retry briefly
        for i in 0..50 {
            match std::fs::remove_dir_all(p.path().join(".muninn")) {
                Ok(()) => break,
                Err(e) if i < 49 => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("remove .muninn: {e}"),
            }
        }
        for ev in ["UserPromptSubmit", "PostToolUse", "Stop", "SessionEnd"] {
            let r = hook(p.path(), ev, serde_json::json!({ "prompt": "implement x" }));
            assert_clean_exit(&r, &format!("deleted store {ev}"));
            assert!(
                r.stdout.trim().is_empty(),
                "must stay silent when not initialised: {}",
                r.stdout
            );
        }
        assert!(
            !p.path().join(".muninn").exists(),
            "hooks must not recreate the store"
        );
    }
}

// ---- Phase 3: the real write path ------------------------------------------------

fn transcript_lines(n_turns: usize, prompt_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n_turns {
        let prompt = format!("turn {i} {}", "word ".repeat(prompt_len / 5));
        out.extend_from_slice(
            serde_json::json!({"type":"user","sessionId":"f","timestamp":"2026-09-12T14:03:11.084Z","message":{"role":"user","content":prompt}})
                .to_string()
                .as_bytes(),
        );
        out.push(b'\n');
        out.extend_from_slice(
            serde_json::json!({"type":"assistant","sessionId":"f","message":{"role":"assistant","content":[{"type":"text","text":format!("done {i}")}]}})
                .to_string()
                .as_bytes(),
        );
        out.push(b'\n');
    }
    out
}

fn records(root: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path(root)).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

// 12. The model on disk does not match the published checksums: the sidecar stays
//     cold, nothing else changes, and no hook slows down.
#[test]
fn s12_model_checksum_mismatch() {
    for _ in 0..reps() {
        let p = init_project();
        let model = p.path().join("badmodel");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("config.json"), "{}").unwrap();
        std::fs::write(model.join("tokenizer.json"), "{}").unwrap();
        std::fs::write(model.join("model.safetensors"), vec![0u8; 4096]).unwrap();
        let t = p.path().join("t.jsonl");
        std::fs::write(&t, transcript_lines(3, 100)).unwrap();
        let r = hook_with(
            p.path(),
            "SessionEnd",
            &payload(p.path(), serde_json::json!({ "transcript_path": t })),
            &[("MUNINN_MODEL_DIR", model.to_str().unwrap())],
            Stdio::piped(),
        );
        assert_clean_exit(&r, "bad model");
        assert!(records(p.path(), "SELECT count(*) FROM record") >= 3);
        assert_eq!(records(p.path(), "SELECT count(*) FROM record_vec"), 0);
        let d = doctor(p.path());
        let s = d["summary"].as_str().unwrap_or("");
        assert!(!s.starts_with("MUNINN RED"), "{s}");
    }
}

// 13. No git on PATH and no repository history: capture is skipped, nothing fails.
#[test]
fn s13_git_unavailable() {
    for _ in 0..reps() {
        let p = init_project();
        let t = p.path().join("t.jsonl");
        std::fs::write(&t, transcript_lines(2, 80)).unwrap();
        let r = hook_with(
            p.path(),
            "SessionEnd",
            &payload(p.path(), serde_json::json!({ "transcript_path": t })),
            &[("PATH", "/nonexistent")],
            Stdio::piped(),
        );
        assert_clean_exit(&r, "no git");
        let out = Command::new(BIN)
            .args(["--cwd", p.path().to_str().unwrap(), "maintain"])
            .env("PATH", "/nonexistent")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(records(p.path(), "SELECT count(*) FROM record") >= 2);
    }
}

// 14. `maintain` and `Stop` ingest the same transcript at the same time: every
//     process exits clean and no record is lost or duplicated.
#[test]
fn s14_maintain_and_stop_concurrent() {
    for _ in 0..reps() {
        let p = init_project();
        let t = p.path().join("t.jsonl");
        std::fs::write(&t, transcript_lines(20, 300)).unwrap();
        // one clean ingest tells us the expected count
        let expect = {
            let q = init_project();
            let r = hook(
                q.path(),
                "Stop",
                serde_json::json!({ "transcript_path": t }),
            );
            assert_clean_exit(&r, "reference ingest");
            records(q.path(), "SELECT count(*) FROM record")
        };
        let root = p.path().to_path_buf();
        let tt = t.clone();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let root = root.clone();
                let tt = tt.clone();
                std::thread::spawn(move || {
                    if i % 2 == 0 {
                        let input = serde_json::json!({ "session_id": format!("c{i}"), "cwd": root, "transcript_path": tt }).to_string();
                        hook_with(&root, "Stop", &input, &[], Stdio::piped())
                    } else {
                        let t0 = Instant::now();
                        let out = Command::new(BIN)
                            .args(["--cwd", root.to_str().unwrap(), "maintain"])
                            .output()
                            .unwrap();
                        Run {
                            status: out.status.code(),
                            stdout: String::from_utf8_lossy(&out.stdout).into(),
                            stderr: String::from_utf8_lossy(&out.stderr).into(),
                            elapsed: t0.elapsed(),
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            let r = h.join().unwrap();
            assert_clean_exit(&r, "maintain/Stop concurrent");
        }
        assert_eq!(records(p.path(), "SELECT count(*) FROM record"), expect);
        let s = summary(p.path());
        assert!(!s.starts_with("MUNINN RED"), "{s}");
    }
}

// 15. One 200 KB turn: stored whole as chunks, every body under the cap, fast.
#[test]
fn s15_huge_single_turn() {
    for _ in 0..reps() {
        let p = init_project();
        let t = p.path().join("t.jsonl");
        std::fs::write(&t, transcript_lines(1, 200_000)).unwrap();
        let r = hook(
            p.path(),
            "SessionEnd",
            serde_json::json!({ "transcript_path": t }),
        );
        assert_clean_exit(&r, "huge turn");
        assert!(records(p.path(), "SELECT count(*) FROM record WHERE kind='episode'") >= 12);
        assert_eq!(
            records(
                p.path(),
                "SELECT count(*) FROM record WHERE length(body) > 2000"
            ),
            0
        );
    }
}

// ---- F1: the serving gate, asserted on real hook stdout ----

/// A distinctive value that only ever appears in the retired record. If it turns up in a
/// hook's stdout, F1 leaked: no accidental match is possible.
const OLD_VALUE: &str = "ZQXOLDCODEC";
const NEW_VALUE: &str = "ZQXNEWCODEC";

/// Import one superseded pair on a shared subject: the old record arrives already retired
/// (`muninn import` honours `invalid`), the new one active. Both carry an `event` cue on
/// `session_start`, so the cue path and the lexical path both have something to find.
fn seed_replaced_pair(root: &Path) {
    let f = root.join("pair.jsonl");
    let old = serde_json::json!({
        "kind": "decision", "subject": "policy.faultcodec", "relation": "is",
        "object": OLD_VALUE, "origin": "user_said", "session_id": "seed-f1",
        "body": format!("user: the faultcodec transport codec is {OLD_VALUE}.\n"),
        "created_at": 1789100000000i64, "invalid": true, "invalid_reason": "superseded",
        "cues": [{"kind": "event", "key": "session_start", "grp": 0}],
    });
    let new = serde_json::json!({
        "kind": "decision", "subject": "policy.faultcodec", "relation": "is",
        "object": NEW_VALUE, "origin": "user_said", "session_id": "seed-f1",
        "body": format!("user: change of plan — the faultcodec transport codec is {NEW_VALUE}.\n"),
        "created_at": 1789101800000i64, "invalid": false,
        "cues": [{"kind": "event", "key": "session_start", "grp": 0}],
    });
    std::fs::write(&f, format!("{old}\n{new}\n")).unwrap();
    let out = Command::new(BIN)
        .args([
            "--cwd",
            root.to_str().unwrap(),
            "import",
            f.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        records(root, "SELECT count(*) FROM record WHERE invalid=1"),
        1,
        "the seed must arrive with exactly one retired record"
    );
}

/// Every hook event that can carry record text, driven with a prompt that names the
/// retired record's own words. Returns the concatenated stdout.
fn drive_serving_hooks(root: &Path) -> String {
    let prompt = "what is the faultcodec transport codec we decided on?";
    let mut all = String::new();
    for (ev, extra) in [
        ("SessionStart", serde_json::json!({})),
        ("UserPromptSubmit", serde_json::json!({ "prompt": prompt })),
        (
            "PostToolUse",
            serde_json::json!({ "tool_name": "Read", "tool_input": {"file_path": "src/faultcodec.rs"} }),
        ),
        (
            "PostCompact",
            serde_json::json!({ "compact_summary": prompt }),
        ),
    ] {
        let r = hook(root, ev, extra);
        assert_clean_exit(&r, &format!("f1 {ev}"));
        all.push_str(&r.stdout);
    }
    all
}

// 16. A retired record's text never reaches a hook's stdout, and the record that replaced
//     it does. The second half matters: without it the assertion would pass on silence.
#[test]
fn s16_retired_never_reaches_stdout() {
    for _ in 0..reps() {
        let p = init_project();
        seed_replaced_pair(p.path());
        let out = drive_serving_hooks(p.path());
        assert!(
            !out.contains(OLD_VALUE),
            "retired value served: {}",
            out.replace('\n', " ")
        );
        assert!(
            out.contains(NEW_VALUE),
            "the replacement was never delivered, so the test proves nothing: {}",
            out.replace('\n', " ")
        );
    }
}

// 17. The same property under the faults of scenarios 2, 3, 8 and 10: whatever breaks, the
//     retired value stays out. A broken store may serve nothing; it may not serve that.
#[test]
fn s17_retired_never_reaches_stdout_under_fault() {
    for _ in 0..reps() {
        // unknown schema version (s10): the binary must not read a store it cannot vouch for
        let p = init_project();
        seed_replaced_pair(p.path());
        {
            let c = rusqlite::Connection::open(db_path(p.path())).unwrap();
            c.execute("UPDATE meta SET value='99' WHERE key='schema_version'", [])
                .unwrap();
        }
        let out = drive_serving_hooks(p.path());
        assert!(!out.contains(OLD_VALUE), "schema-too-new: {out}");

        // clock moved backwards (s8)
        let p = init_project();
        seed_replaced_pair(p.path());
        let r = hook_with(
            p.path(),
            "UserPromptSubmit",
            &payload(
                p.path(),
                serde_json::json!({ "prompt": "which faultcodec transport codec?" }),
            ),
            &[("MUNINN_FAKE_NOW_MS", "1000")],
            Stdio::piped(),
        );
        assert_clean_exit(&r, "clock-back UserPromptSubmit");
        assert!(!r.stdout.contains(OLD_VALUE), "clock back: {}", r.stdout);

        // corrupt store (s2): silence is fine, the retired value is not
        let p = init_project();
        seed_replaced_pair(p.path());
        std::fs::write(db_path(p.path()), b"not a database, but long enough\0\0\0").unwrap();
        let out = drive_serving_hooks(p.path());
        assert!(!out.contains(OLD_VALUE), "corrupt store: {out}");
    }
}
