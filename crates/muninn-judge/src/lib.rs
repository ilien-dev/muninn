//! Write-path judge (PREREGISTRATION.md, judge-v3 and v42). A local CPU model reads, for each
//! new conversational record, which active record it replaces — every candidate at once, one
//! letter back — then confirms that one pair with a yes/no. A confirmed pair is written to
//! `judged_conflict` and served as a conflict for the agent to ask about; **nothing is retired
//! on the model's word**. judge-v3 measured the retire tier at one false retirement on a fresh
//! set and the ask tier at 17/20; the ask tier is what this builds.
//!
//! Runs in `maintain` only, never in a read hook: a call is seconds on a CPU, not
//! milliseconds. If the model file is missing or its hash does not match, the judge is cold
//! and Muninn behaves as it did without it.

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::token::LlamaToken;
use muninn_core::db::now_ms;
use muninn_core::{Db, Error, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const MODEL_FILE: &str = "qwen35-9b-q4km.gguf";
/// bartowski/Qwen_Qwen3.5-9B-GGUF, Qwen_Qwen3.5-9B-Q4_K_M.gguf — the file judge-v3's C2 ran.
pub const MODEL_SHA256: &str = "d784ce9eda1a5a7b51e8f705a9e6310844bf4f173654d115823c775fdea56d43";
/// judge-v3's ask tier for C2, fitted on dev: pick probability and confirm P(yes).
pub const PICK_MIN: f32 = 0.28;
pub const CONFIRM_MIN: f32 = 0.08;
/// Candidates shown per call: the most recent active conversational records, oldest first.
pub const MAX_CANDIDATES: usize = 20;
/// New messages judged per `maintain`; older pending ones are skipped, not queued forever.
pub const MAX_PER_RUN: usize = 8;
const CONTEXT: u32 = 8192;
const TEXT_CAP: usize = 400;

// The prompts are judge-v3's (`experiment/judge/v3.py`), word for word.
const PICK_HEAD: &str = "You keep the memory of a software project. Below are records the project holds, \
oldest first, each with the assistant's reply when there was one. Records can be in any language.\n\nRecords:\n";
const PICK_TAIL: &str = "\nA new message was written later in the same project:\n{b}\n\n\
Question: which record does the new message replace, reverse or withdraw, so that it is no longer current? \
A message that asks something, adds detail, is about something else, or leaves every record true replaces none of them.\n\n\
Answer with the letter only.";
const CONFIRM: &str = "You keep the memory of a software project. Each record states something the project \
decided or holds. Records can be in any language.\n\n\
Question: does the new message replace, reverse or withdraw what the earlier record states, so that the earlier record is no longer current?\n\
A) Yes: the earlier record is no longer current.\n\
B) No: the new message is about something else, adds detail, asks something, is hypothetical, or leaves the earlier record true.\n\n\
Earlier record:\n{a}\n\nNew message, written later in the same project:\n{b}\n\n\
Answer with the letter only.";

/// Where the model lives: `MUNINN_JUDGE_MODEL` (a file), then `$CLAUDE_PLUGIN_ROOT/models/`,
/// then `~/.local/share/muninn/models/`. The first existing file wins.
pub fn model_path() -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(p) = std::env::var("MUNINN_JUDGE_MODEL") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(r) = std::env::var("CLAUDE_PLUGIN_ROOT") {
        cands.push(PathBuf::from(r).join("models").join(MODEL_FILE));
    }
    if let Ok(h) = std::env::var("HOME") {
        cands.push(
            PathBuf::from(h)
                .join(".local/share/muninn/models")
                .join(MODEL_FILE),
        );
    }
    cands.into_iter().find(|p| p.is_file())
}

/// The file's hash, checked once per (size, mtime): hashing 6 GB on every `maintain` would cost
/// seconds for nothing. The stamp sits beside the model.
fn verify(path: &Path) -> Result<()> {
    let md = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tag = format!("{} {} {}", md.len(), mtime, MODEL_SHA256);
    let stamp = path.with_extension("gguf.verified");
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(tag.as_str()) {
        return Ok(());
    }
    let mut f = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| Error::io(path, e))?;
    let got = format!("{:x}", h.finalize());
    if got != MODEL_SHA256 {
        return Err(Error::Other(format!(
            "judge model hash {got} != {MODEL_SHA256}"
        )));
    }
    let _ = std::fs::write(&stamp, tag);
    Ok(())
}

pub struct Judge {
    backend: LlamaBackend,
    model: LlamaModel,
    threads: i32,
    letters: Vec<LlamaToken>,
}

fn other(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("judge: {e}"))
}

impl Judge {
    pub fn load_default() -> Result<Judge> {
        let path = model_path().ok_or_else(|| Error::Other("judge: no model".into()))?;
        Judge::load(&path)
    }

    pub fn load(path: &Path) -> Result<Judge> {
        verify(path)?;
        let mut backend = LlamaBackend::init().map_err(other)?;
        backend.void_logs();
        let model = LlamaModel::load_from_file(&backend, path, &LlamaModelParams::default())
            .map_err(other)?;
        let threads = std::env::var("MUNINN_JUDGE_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get().min(16) as i32)
                    .unwrap_or(4)
            });
        let mut letters = Vec::new();
        for c in 'A'..='Z' {
            let t = model
                .str_to_token(&c.to_string(), AddBos::Never)
                .map_err(other)?;
            if t.len() != 1 {
                return Err(Error::Other(format!(
                    "judge: letter {c} is {} tokens",
                    t.len()
                )));
            }
            letters.push(t[0]);
        }
        Ok(Judge {
            backend,
            model,
            threads,
            letters,
        })
    }

    /// Qwen3.5's chat template with thinking off, as judge-v3 rendered it through Jinja.
    fn render(content: &str) -> String {
        format!(
            "<|im_start|>user\n{content}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        )
    }

    /// Softmax over the first `n` letters' logits after the prompt.
    fn letters(&self, content: &str, n: usize) -> Result<Vec<f32>> {
        let tokens = self
            .model
            .str_to_token(&Self::render(content), AddBos::Never)
            .map_err(other)?;
        if tokens.len() as u32 >= CONTEXT {
            return Err(Error::Other(format!(
                "judge: prompt of {} tokens",
                tokens.len()
            )));
        }
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(CONTEXT))
            .with_n_batch(CONTEXT)
            .with_n_ubatch(512)
            .with_n_threads(self.threads)
            .with_n_threads_batch(self.threads);
        let mut ctx = self
            .model
            .new_context(&self.backend, params)
            .map_err(other)?;
        let mut batch = LlamaBatch::new(tokens.len(), 1);
        let last = tokens.len() - 1;
        for (i, t) in tokens.iter().enumerate() {
            batch.add(*t, i as i32, &[0], i == last).map_err(other)?;
        }
        ctx.decode(&mut batch).map_err(other)?;
        let lg = ctx.get_logits_ith(last as i32);
        let v: Vec<f32> = self.letters[..n].iter().map(|t| lg[t.0 as usize]).collect();
        let m = v.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let e: Vec<f32> = v.iter().map(|x| (x - m).exp()).collect();
        let s: f32 = e.iter().sum();
        Ok(e.iter().map(|x| x / s).collect())
    }

    /// judge-v3's pick: index into `cands` and its probability, or None for "none of them".
    pub fn pick(&self, cands: &[Said], b: &Said) -> Result<Option<(usize, f32)>> {
        let p = self.pick_probs(cands, b)?;
        let k = (0..p.len())
            .max_by(|&i, &j| p[i].total_cmp(&p[j]))
            .unwrap_or(0);
        Ok(if k == 0 { None } else { Some((k - 1, p[k])) })
    }

    /// The whole letter distribution: [none, candidate 0, candidate 1, …].
    pub fn pick_probs(&self, cands: &[Said], b: &Said) -> Result<Vec<f32>> {
        let n = cands.len().min(25);
        let mut recs = String::from("A) none of them\n");
        for (i, c) in cands[..n].iter().enumerate() {
            recs.push_str(&format!("{}) {}\n", (b'B' + i as u8) as char, c.show()));
        }
        let content = format!("{PICK_HEAD}{recs}{}", PICK_TAIL.replace("{b}", &b.show()));
        self.letters(&content, n + 1)
    }

    /// judge-v3's confirm: P(yes, the new message makes the earlier record no longer current).
    pub fn confirm(&self, a: &Said, b: &Said) -> Result<f32> {
        let content = CONFIRM.replace("{a}", &a.show()).replace("{b}", &b.show());
        Ok(self.letters(&content, 2)?[0])
    }
}

/// A message as judge-v3 showed it: what the user said, and the assistant's reply if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    pub text: String,
    pub reply: String,
}

fn cap(s: &str) -> String {
    s.trim().chars().take(TEXT_CAP).collect()
}

impl Said {
    /// From a record body: episodes read `user: …\nassistant: …`, decisions `user: …`.
    pub fn from_body(body: &str) -> Said {
        let body = body.trim();
        let (u, a) = match body.find("\nassistant: ") {
            Some(i) => (&body[..i], &body[i + "\nassistant: ".len()..]),
            None => (body, ""),
        };
        let u = u.strip_prefix("user: ").unwrap_or(u);
        Said {
            text: cap(u),
            reply: cap(a),
        }
    }

    fn show(&self) -> String {
        if self.reply.is_empty() {
            format!("<<<{}>>>", self.text)
        } else {
            format!(
                "<<<{}>>>\n   (assistant replied: <<<{}>>>)",
                self.text, self.reply
            )
        }
    }
}

/// One message in the store: the records one user turn produced (its decision and its
/// episode share session and time), shown by its episode when there is one.
#[derive(Debug, Clone)]
struct Unit {
    ids: Vec<i64>,
    said: Said,
}

fn units(db: &Db, where_: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<Unit>> {
    let sql = format!(
        "SELECT id, kind, body, session_id, created_at FROM record \
         WHERE invalid = 0 AND kind IN ('decision','invariant','episode') \
           AND origin IN ('user_said','tool_observed') AND {where_} ORDER BY created_at, id"
    );
    let mut stmt = db.conn.prepare(&sql)?;
    let rows: Vec<(i64, String, String, String, i64)> = stmt
        .query_map(params, |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    let mut out: Vec<(String, i64, Unit, bool)> = Vec::new();
    for (id, kind, body, sid, at) in rows {
        let said = Said::from_body(&body);
        match out.iter_mut().find(|u| u.0 == sid && u.1 == at) {
            Some(u) => {
                u.2.ids.push(id);
                if kind == "episode" && !u.3 {
                    u.2.said = said;
                    u.3 = true;
                }
            }
            None => out.push((
                sid,
                at,
                Unit {
                    ids: vec![id],
                    said,
                },
                kind == "episode",
            )),
        }
    }
    Ok(out.into_iter().map(|u| u.2).collect())
}

#[derive(Debug, Default, Serialize)]
pub struct JudgeStats {
    pub judged: usize,
    pub conflicts: usize,
    pub skipped: usize,
    pub ms: u128,
}

/// Whether anything was written since the last run, so `maintain` loads the model (seconds,
/// and 6 GB mapped) only when there is something to judge.
pub fn has_pending(db: &Db) -> bool {
    let wm: i64 = db
        .meta_get("judge_watermark")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    db.conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM record WHERE id > ?1)",
            [wm],
            |r| r.get::<_, i64>(0),
        )
        .map(|x| x != 0)
        .unwrap_or(false)
}

/// Judge the conversational records written since the last run. Each new message is asked
/// against the most recent active messages before it; a confirmed pair marks every record of
/// the old message as in conflict with every record of the new one.
pub fn judge_pending(db: &Db, judge: &Judge) -> Result<JudgeStats> {
    let t0 = Instant::now();
    let mut st = JudgeStats::default();
    let wm: i64 = db
        .meta_get("judge_watermark")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let max_id: i64 = db
        .conn
        .query_row("SELECT COALESCE(MAX(id), 0) FROM record", [], |r| r.get(0))?;
    let fresh = units(db, "id > ?1", &[&wm])?;
    let skip = fresh.len().saturating_sub(MAX_PER_RUN);
    st.skipped = skip;
    for u in fresh.into_iter().skip(skip) {
        let first = *u.ids.iter().min().unwrap_or(&0);
        let at: i64 = db.conn.query_row(
            "SELECT created_at FROM record WHERE id = ?1",
            [first],
            |r| r.get(0),
        )?;
        let mut before = units(db, "created_at < ?1", &[&at])?;
        let keep = before.len().saturating_sub(MAX_CANDIDATES);
        let before: Vec<Unit> = before.drain(keep..).collect();
        st.judged += 1;
        if before.is_empty() {
            continue;
        }
        let cands: Vec<Said> = before.iter().map(|c| c.said.clone()).collect();
        let Some((k, pk)) = judge.pick(&cands, &u.said)? else {
            continue;
        };
        if pk < PICK_MIN {
            continue;
        }
        let pc = judge.confirm(&cands[k], &u.said)?;
        if pc < CONFIRM_MIN {
            continue;
        }
        let tx = db.conn.unchecked_transaction()?;
        for old in &before[k].ids {
            for new in &u.ids {
                tx.execute(
                    "INSERT OR IGNORE INTO judged_conflict (old_id, new_id, p_pick, p_confirm, model, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![old, new, pk as f64, pc as f64, &MODEL_SHA256[..16], now_ms()],
                )?;
            }
        }
        tx.commit()?;
        st.conflicts += 1;
    }
    db.meta_set("judge_watermark", &max_id.to_string())?;
    st.ms = t0.elapsed().as_millis();
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_splits_into_what_was_said_and_the_reply() {
        let s = Said::from_body(
            "user: We're using gzip for compression\nassistant: Noted: gzip it is.\n",
        );
        assert_eq!(s.text, "We're using gzip for compression");
        assert_eq!(s.reply, "Noted: gzip it is.");
        let d = Said::from_body("user: We're using gzip for compression\n");
        assert_eq!(d.reply, "");
    }

    #[test]
    fn the_shown_record_matches_judge_v3() {
        let s = Said {
            text: "a".into(),
            reply: "b".into(),
        };
        assert_eq!(s.show(), "<<<a>>>\n   (assistant replied: <<<b>>>)");
    }
}
