//! Fault injection: 11 scenarios × N repetitions (MUNINN_FAULT_REPS, default 3; CI 200).
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
        std::fs::remove_file(&log).unwrap();
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
        let conn = rusqlite::Connection::open(db_path(&root)).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM heartbeat", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 4, "heartbeats folded: {n}");
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
        std::fs::remove_dir_all(p.path().join(".muninn")).unwrap();
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
