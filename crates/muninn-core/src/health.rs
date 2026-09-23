//! Health gate: nine checks, all arithmetic over typed evidence, no model [G3].
//! Alert semantics: cold ≠ dead; optional ≠ broken; RED is always actionable.

use crate::caps;
use crate::db::{now_ms, Db};
use crate::error::Error;
use crate::heartbeat::{self, Line};
use crate::paths::ProjectPaths;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Green,
    Cold,
    Red,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: u8,
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn reds(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.status == Status::Red)
    }
    pub fn colds(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.status == Status::Cold)
    }
    pub fn is_green(&self) -> bool {
        self.reds().next().is_none()
    }

    /// The one line the agent sees at SessionStart.
    pub fn summary(&self) -> String {
        let total = self.checks.len();
        let reds: Vec<&Check> = self.reds().collect();
        if let Some(r) = reds.first() {
            let fix = r.fix.as_deref().unwrap_or("see `muninn doctor`");
            return format!("MUNINN RED {}: {} — {}", r.name, r.detail, fix);
        }
        let colds: Vec<&Check> = self.colds().collect();
        if colds.is_empty() {
            format!("MUNINN {total}/{total} GREEN")
        } else {
            let names: Vec<&str> = colds.iter().map(|c| c.name).collect();
            format!(
                "MUNINN {}/{total} GREEN · cold: {}",
                total - colds.len(),
                names.join(", ")
            )
        }
    }
}

/// The remedy for a repeated hook failure, read off the error the hook itself recorded.
/// Only one cause is named specifically, because only one is produced by the shape of the
/// install: the plugin copies the binary once, the repository keeps migrating the store,
/// and the writing hooks are then the first to refuse it.
fn heartbeat_fix(err: &str) -> &'static str {
    if err.contains("newer than this binary supports") {
        "the binary the hooks run is older than the store: replace it with the newer build \
         (`scripts/install.sh`, or copy `target/release/muninn` over the one in the plugin's `bin/`)"
    } else {
        "the failing hook's error is above; its full history is in `.muninn/log/heartbeat.jsonl`"
    }
}

/// First `max` characters, on a character boundary, so one long error cannot push the
/// SessionStart summary past what the agent will read.
fn head(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((i, _)) => format!("{}…", &s[..i]),
    }
}

fn check(
    id: u8,
    name: &'static str,
    status: Status,
    detail: impl Into<String>,
    fix: Option<&str>,
) -> Check {
    Check {
        id,
        name,
        status,
        detail: detail.into(),
        fix: fix.map(str::to_string),
    }
}

/// Run all nine checks. `db` is `None` when the database could not be opened;
/// that alone is a RED on check 3.
///
/// `live=false` (hooks): only O(1) evidence is consulted — the recorded
/// `quick_check` outcome and the trigger-maintained FTS counter — so the gate
/// costs well under a millisecond at any store size. `live=true` (`muninn
/// doctor`): the checks are executed for real.
///
/// That contract was written before checks 4 and 8 existed and neither honoured it:
/// both counted the active rows on every hook, and at the schema's cap of 20 000 that
/// was most of why `SessionStart` measured 10.5 ms against a 10 ms limit.
pub fn run(
    paths: &ProjectPaths,
    db: Option<&Db>,
    open_error: Option<&Error>,
    live: bool,
) -> Report {
    let mut checks = Vec::with_capacity(10);
    let now = now_ms();

    // 1. ingest watermark
    checks.push(match db {
        Some(db) => {
            let last_end = db
                .meta_get("last_session_end_ms")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<i64>().ok());
            let watermark = db
                .meta_get("ingest_watermark_ms")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<i64>().ok());
            match (last_end, watermark) {
                (None, _) => check(1, "ingest", Status::Green, "no session ended yet", None),
                (Some(e), Some(w)) if w >= e => {
                    check(1, "ingest", Status::Green, "watermark fresh", None)
                }
                (Some(e), w) => {
                    let age_min = (now - e) / 60_000;
                    if age_min > 24 * 60 {
                        check(
                            1,
                            "ingest",
                            Status::Red,
                            format!("last session not ingested for {age_min} min"),
                            Some(
                                "run `muninn hook SessionEnd` manually or `muninn doctor --ingest`",
                            ),
                        )
                    } else {
                        check(
                            1,
                            "ingest",
                            Status::Cold,
                            format!("ingest pending (watermark {:?})", w),
                            None,
                        )
                    }
                }
            }
        }
        None => check(1, "ingest", Status::Cold, "no database", None),
    });

    // 2. heartbeats
    {
        let tail = heartbeat::read_tail(paths, caps::HEARTBEAT_TAIL_BYTES);
        if tail.is_empty() {
            checks.push(check(
                2,
                "heartbeat",
                Status::Cold,
                "no hook has run yet",
                None,
            ));
        } else {
            // the count alone is not actionable: the last error is what names the cause,
            // so it travels with the count and lands in the RED line itself.
            let mut fails: HashMap<String, (u32, Option<String>)> = HashMap::new();
            let mut open: HashMap<(String, String, i64), i64> = HashMap::new();
            for l in &tail {
                match l {
                    Line::Start {
                        hook, session, at, ..
                    } => {
                        open.insert((hook.clone(), session.clone(), *at), *at);
                    }
                    Line::Finish {
                        hook,
                        session,
                        at,
                        ok,
                        err,
                        ..
                    } => {
                        open.remove(&(hook.clone(), session.clone(), *at));
                        let e = fails.entry(hook.clone()).or_default();
                        if *ok {
                            *e = (0, None)
                        } else {
                            e.0 += 1;
                            e.1 = err.clone();
                        }
                    }
                }
            }
            let stuck = open.values().filter(|at| now - **at > 120_000).count();
            let worst = fails.iter().max_by_key(|(_, (n, _))| *n);
            match worst {
                Some((hook, (n, err))) if *n >= 3 => {
                    let err = err.as_deref().unwrap_or("no error recorded");
                    checks.push(check(
                        2,
                        "heartbeat",
                        Status::Red,
                        format!("{hook} failed {n} times in a row: {}", head(err, 160)),
                        Some(heartbeat_fix(err)),
                    ))
                }
                _ if stuck > 0 => checks.push(check(
                    2,
                    "heartbeat",
                    Status::Red,
                    format!("{stuck} hook run(s) started and never finished"),
                    Some("a hook is exceeding its timeout; check `.muninn/log/heartbeat.jsonl`"),
                )),
                _ => checks.push(check(
                    2,
                    "heartbeat",
                    Status::Green,
                    format!("{} events", tail.len()),
                    None,
                )),
            }
        }
    }

    // 3. integrity
    checks.push(match db {
        Some(db) if live => match db.quick_check() {
            Ok(()) => check(3, "integrity", Status::Green, "quick_check ok (live)", None),
            Err(e) => check(
                3,
                "integrity",
                Status::Red,
                e.to_string(),
                Some("restore from `.muninn/records/` with `muninn import --from-records`"),
            ),
        },
        Some(db) if !db.has_served_view() => check(
            3,
            "integrity",
            Status::Red,
            "the `served_record` view is missing: this store predates F1's serving gate, \
             and read hooks stay silent until it is applied",
            Some("run `muninn maintain`"),
        ),
        Some(db) => {
            // A store that cannot even answer a meta query is broken regardless of what was recorded.
            match db.meta_get("quick_check_result") {
                Err(e) => check(
                    3,
                    "integrity",
                    Status::Red,
                    e.to_string(),
                    Some("run `muninn doctor`"),
                ),
                Ok(None) => check(
                    3,
                    "integrity",
                    Status::Cold,
                    "quick_check not run yet (runs at session end)",
                    None,
                ),
                Ok(Some(r)) if r == "ok" => {
                    let at: i64 = db
                        .meta_get("quick_check_at")
                        .ok()
                        .flatten()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let age_h = (now - at).max(0) / 3_600_000;
                    if age_h > 24 * 7 {
                        check(
                            3,
                            "integrity",
                            Status::Cold,
                            format!("last quick_check {age_h} h ago"),
                            None,
                        )
                    } else {
                        check(
                            3,
                            "integrity",
                            Status::Green,
                            "quick_check ok (recorded)",
                            None,
                        )
                    }
                }
                Ok(Some(r)) => check(
                    3,
                    "integrity",
                    Status::Red,
                    r,
                    Some("restore from `.muninn/records/` with `muninn import --from-records`"),
                ),
            }
        }
        None => check(
            3,
            "integrity",
            Status::Red,
            open_error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "database missing".into()),
            Some("run `muninn init`"),
        ),
    });

    // 4. FTS coherence
    checks.push(match db {
        Some(db) => {
            // `count(*)` on an external-content FTS5 table reads the content table, invalid
            // rows included, so it is not the index; the trigger-maintained counter is
            let f = db
                .meta_get("fts_rows")
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(-2);
            // Comparing the counter against the truth means counting the truth, which is the
            // scan this gate promises hooks it will not do. The counter is what the hook can
            // know for free; `muninn doctor` is where the two are actually compared.
            let a = if live || f < 0 {
                db.count("SELECT count(*) FROM record WHERE invalid=0")
                    .unwrap_or(-1)
            } else {
                f
            };
            if a == f {
                check(
                    4,
                    "fts",
                    Status::Green,
                    format!("{a} active rows indexed"),
                    None,
                )
            } else {
                check(
                    4,
                    "fts",
                    Status::Red,
                    format!("record active={a} fts={f}"),
                    Some("run `muninn doctor --rebuild-fts`"),
                )
            }
        }
        None => check(4, "fts", Status::Cold, "no database", None),
    });

    // 5. pending capture queue
    checks.push(match db {
        Some(db) => {
            let pending = db
                .meta_get("capture_pending")
                .ok()
                .flatten()
                .unwrap_or_default();
            let n = if pending.trim().is_empty() {
                0
            } else {
                pending.split('\n').filter(|l| !l.is_empty()).count()
            };
            match n {
                0 => check(5, "capture", Status::Green, "queue empty", None),
                1..=3 => check(
                    5,
                    "capture",
                    Status::Cold,
                    format!("{n} transcript(s) pending"),
                    None,
                ),
                _ => check(
                    5,
                    "capture",
                    Status::Red,
                    format!("{n} transcripts pending"),
                    Some("run `muninn doctor --ingest`"),
                ),
            }
        }
        None => check(5, "capture", Status::Cold, "no database", None),
    });

    // 6. render not frozen
    checks.push(match db {
        Some(db) => {
            let last = db.meta_get("last_render_hash").ok().flatten();
            let prev = db.meta_get("prev_render_hash").ok().flatten();
            let changed_since = db
                .meta_get("records_changed_since_render")
                .ok()
                .flatten()
                .unwrap_or_default()
                == "1";
            match (last, prev) {
                (Some(a), Some(b)) if a == b && changed_since => check(
                    6,
                    "render",
                    Status::Red,
                    "invariant render identical across sessions despite new records",
                    Some("run `muninn status --render` to inspect"),
                ),
                (Some(_), Some(_)) => check(6, "render", Status::Green, "render changed", None),
                _ => check(6, "render", Status::Cold, "fewer than two renders", None),
            }
        }
        None => check(6, "render", Status::Cold, "no database", None),
    });

    // 7. rules in sync with source hashes
    checks.push(match db {
        Some(db) => {
            let mut stmt = db
                .conn
                .prepare("SELECT DISTINCT source_file, source_hash FROM rule")
                .ok();
            let rows: Vec<(String, String)> = stmt
                .as_mut()
                .and_then(|s| s.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).ok())
                .map(|it| it.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();
            if rows.is_empty() {
                check(7, "rules", Status::Green, "no compiled rules", None)
            } else {
                let stale: Vec<String> = rows
                    .iter()
                    .filter(|(f, h)| {
                        let p = paths.root.join(f);
                        match std::fs::read(&p) {
                            Ok(b) => blake3::hash(&b).to_hex().as_str() != h,
                            Err(_) => true,
                        }
                    })
                    .map(|(f, _)| f.clone())
                    .collect();
                if stale.is_empty() {
                    check(
                        7,
                        "rules",
                        Status::Green,
                        format!("{} source file(s) in sync", rows.len()),
                        None,
                    )
                } else {
                    check(
                        7,
                        "rules",
                        Status::Red,
                        format!("changed since compile: {}", stale.join(", ")),
                        Some("run `muninn compile`"),
                    )
                }
            }
        }
        None => check(7, "rules", Status::Cold, "no database", None),
    });

    // 8. caps
    checks.push(match db {
        Some(db) => {
            let inv = db
                .count("SELECT count(*) FROM record WHERE invalid=0 AND kind='invariant'")
                .unwrap_or(0);
            // the cap is on active records, and the trigger-maintained counter holds exactly
            // that number: a hook reads it instead of scanning the whole index for it
            let act = db
                .meta_get("fts_rows")
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .filter(|_| !live)
                .unwrap_or_else(|| {
                    db.count("SELECT count(*) FROM record WHERE invalid=0")
                        .unwrap_or(0)
                });
            // body length is a CHECK constraint; only the live report re-verifies it.
            let big = if live {
                db.count(&format!(
                    "SELECT count(*) FROM record WHERE length(body) > {}",
                    caps::MAX_BODY_CHARS
                ))
                .unwrap_or(0)
            } else {
                0
            };
            if inv > caps::MAX_INVARIANTS {
                check(
                    8,
                    "caps",
                    Status::Red,
                    format!("{inv} invariants > {}", caps::MAX_INVARIANTS),
                    Some("run `muninn doctor --enforce-caps`"),
                )
            } else if act > caps::MAX_ACTIVE_RECORDS {
                check(
                    8,
                    "caps",
                    Status::Red,
                    format!("{act} active records > {}", caps::MAX_ACTIVE_RECORDS),
                    Some("run `muninn doctor --enforce-caps`"),
                )
            } else if big > 0 {
                check(
                    8,
                    "caps",
                    Status::Red,
                    format!("{big} bodies over {} chars", caps::MAX_BODY_CHARS),
                    Some("run `muninn doctor --enforce-caps`"),
                )
            } else {
                check(
                    8,
                    "caps",
                    Status::Green,
                    format!(
                        "{inv}/{} invariants, {act}/{} active",
                        caps::MAX_INVARIANTS,
                        caps::MAX_ACTIVE_RECORDS
                    ),
                    None,
                )
            }
        }
        None => check(8, "caps", Status::Cold, "no database", None),
    });

    // 9. applied artefacts are still in place
    {
        let applied = paths.compiled_dir().join("applied.json");
        if !applied.exists() {
            checks.push(check(9, "compiled", Status::Green, "nothing applied", None));
        } else {
            let v: serde_json::Value = std::fs::read_to_string(&applied)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::Value::Null);
            let want: Vec<String> = ["deny", "ask"]
                .iter()
                .flat_map(|k| {
                    v.get(*k)
                        .and_then(|a| a.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect();
            let settings: serde_json::Value =
                std::fs::read_to_string(paths.root.join(".claude/settings.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
            let have: Vec<String> = ["deny", "ask"]
                .iter()
                .flat_map(|k| {
                    settings
                        .get("permissions")
                        .and_then(|p| p.get(*k))
                        .and_then(|a| a.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect();
            let missing: Vec<&String> = want.iter().filter(|w| !have.contains(w)).collect();
            let hooks_ok = paths.compiled_dir().join("pretooluse.json").exists();
            if !hooks_ok {
                checks.push(check(
                    9,
                    "compiled",
                    Status::Red,
                    "pretooluse.json missing while hooks are applied",
                    Some("run `muninn compile` then `muninn apply`"),
                ));
            } else if missing.is_empty() {
                checks.push(check(
                    9,
                    "compiled",
                    Status::Green,
                    format!("{} applied rule(s) present", want.len()),
                    None,
                ));
            } else {
                checks.push(check(
                    9,
                    "compiled",
                    Status::Red,
                    format!(
                        "{} applied rule(s) missing from .claude/settings.json",
                        missing.len()
                    ),
                    Some("run `muninn apply` again or `muninn apply --revert`"),
                ));
            }
        }
    }

    // 10. embedding sidecar: fresh, or cold — never RED by itself (plan, Phase 3 §8)
    checks.push(match db {
        Some(db) => {
            let has_table = db
                .count("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='record_vec'")
                .unwrap_or(0)
                > 0;
            let model = db.meta_get("embed_model").ok().flatten();
            if !has_table || model.is_none() {
                check(10, "embed", Status::Cold, "sidecar not enabled (no model)", None)
            } else if !live {
                // Coverage costs a join over every vector — 3.1 ms of a 10 ms hook on a store
                // at the schema's cap — and it says nothing about the read path, which never
                // touches the sidecar. The full report still counts them.
                check(10, "embed", Status::Green, "sidecar enabled", None)
            } else {
                let active = db.count("SELECT count(*) FROM record WHERE invalid=0").unwrap_or(0);
                let vecs = db
                    .count("SELECT count(*) FROM record_vec v JOIN record r ON r.id=v.record_id WHERE r.invalid=0")
                    .unwrap_or(0);
                if vecs >= active {
                    check(10, "embed", Status::Green, format!("{vecs} vector(s), up to date"), None)
                } else {
                    check(
                        10,
                        "embed",
                        Status::Cold,
                        format!("{} record(s) not embedded yet", active - vecs),
                        Some("run `muninn embed` (the next Stop/SessionEnd does it too)"),
                    )
                }
            }
        }
        None => check(10, "embed", Status::Cold, "no database", None),
    });

    // 11. the allow rules Muninn's own commands need
    //
    // The boot summary tells the agent to run `muninn show <id>`, and an install made before
    // that command existed does not permit it — the agent meets a permission prompt on the one
    // command the memory named. The read hooks cannot fix it: they are `query_only` by
    // contract and `.claude/settings.json` is the user's file. Saying so is what is left.
    {
        const NEEDED: [&str; 3] = [
            "Bash(muninn why:*)",
            "Bash(muninn status:*)",
            "Bash(muninn show:*)",
        ];
        let allow: Vec<String> = std::fs::read_to_string(paths.root.join(".claude/settings.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| {
                v.get("permissions")
                    .and_then(|p| p.get("allow"))
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
            })
            .unwrap_or_default();
        // only for an install that wrote some of them: a project that never ran `init` has no
        // opinion about these and is not missing anything
        let ours = NEEDED
            .iter()
            .filter(|n| allow.iter().any(|a| a == *n))
            .count();
        let missing: Vec<&str> = NEEDED
            .iter()
            .copied()
            .filter(|n| !allow.iter().any(|a| a == n))
            .collect();
        checks.push(if ours == 0 || missing.is_empty() {
            check(
                11,
                "permissions",
                Status::Green,
                "allow rules in place",
                None,
            )
        } else {
            check(
                11,
                "permissions",
                Status::Cold,
                format!("{} not allowed", missing.join(", ")),
                Some("run `muninn init` to add it"),
            )
        });
    }
    Report { checks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Mode;

    #[test]
    fn fresh_project_is_green_or_cold_never_red() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        std::fs::create_dir_all(paths.muninn_dir.clone()).unwrap();
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        let r = run(&paths, Some(&db), None, true);
        assert!(r.is_green(), "{:?}", r.checks);
        let r = run(&paths, Some(&db), None, false);
        assert!(r.is_green(), "{:?}", r.checks);
        assert!(r.summary().starts_with("MUNINN "));
    }

    /// A repeated hook failure used to report only its count, and point at the command
    /// that produced the report: the reader was sent back to where they already were.
    /// The error the hook recorded is what names the cause, so it is in the RED line.
    #[test]
    fn repeated_failure_carries_the_error_and_a_remedy() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        std::fs::create_dir_all(paths.muninn_dir.clone()).unwrap();
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        let log = paths.heartbeat_log();
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let err = "schema version 2 is newer than this binary supports (1)";
        let mut lines = String::new();
        for i in 0..4 {
            lines.push_str(&format!(
                r#"{{"ev":"start","hook":"Stop","session":"s","at":{i},"pid":1}}
{{"ev":"finish","hook":"Stop","session":"s","at":{i},"pid":1,"ok":false,"ms":1.0,"err":"{err}"}}
"#
            ));
        }
        std::fs::write(&log, lines).unwrap();

        let r = run(&paths, Some(&db), None, false);
        let c = r.checks.iter().find(|c| c.id == 2).unwrap();
        assert_eq!(c.status, Status::Red);
        assert!(c.detail.contains(err), "{}", c.detail);
        let fix = c.fix.as_deref().unwrap();
        assert!(fix.contains("older than the store"), "{fix}");
        assert!(
            !fix.contains("muninn doctor"),
            "the fix must not name the report itself"
        );
    }

    /// A failure with no recognised cause still says where its history is, and a long
    /// error cannot push the one line the agent reads past what it will read.
    #[test]
    fn unknown_failure_is_still_actionable_and_bounded() {
        assert!(heartbeat_fix("disk full").contains("heartbeat.jsonl"));
        let long = "x".repeat(400);
        assert_eq!(head(&long, 160).chars().count(), 161);
        assert_eq!(head("short", 160), "short");
        assert_eq!(head("áéíóú", 2), "áé…");
    }

    #[test]
    fn missing_db_is_red_on_integrity() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        let r = run(&paths, None, None, false);
        assert!(r.summary().starts_with("MUNINN RED integrity"));
    }
}
