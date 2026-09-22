//! Embedding sidecar (plan, Phase 3 §10). A static model2vec model (`potion-base-8M`,
//! 256 dims) embeds records **on the asynchronous write path only**; the read hooks
//! never load it (35–106 ms per process [I2]). Vectors live in `record_vec`; search is
//! an exact dot product over the active rows — no HNSW, so the same query always
//! returns the same ids [V4]. Consumers: `muninn why` and the semantic-duplicate check
//! at ingest. If the model is missing the sidecar is cold and everything else works.

use model2vec_rs::model::StaticModel;
use muninn_core::db::now_ms;
use muninn_core::{Db, Error, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const MODEL_NAME: &str = "potion-base-8M";
pub const DIM: usize = 256;
/// sha256 of the three model files, as published on the Hub on 2026-09-13.
pub const MODEL_FILES: [(&str, &str); 3] = [
    (
        "config.json",
        "2a6ac0e9aaa356a68a5688070db78fc3a464fefe85d2f06a1905ce3718687553",
    ),
    (
        "tokenizer.json",
        "e67e803f624fb4d67dea1c730d06e1067e1b14d830e2c2202569e3ef0f70bb50",
    ),
    (
        "model.safetensors",
        "f65d0f325faadc1e121c319e2faa41170d3fa07d8c89abd48ca5358d9a223de2",
    ),
];
/// Cosine at or above which a new record with the same (kind, subject) is a variant.
pub const DUP_COSINE: f32 = 0.95;

/// Where the model lives: `MUNINN_MODEL_DIR`, then `$CLAUDE_PLUGIN_ROOT/models/<name>`,
/// then `~/.local/share/muninn/models/<name>`. The first existing directory wins.
pub fn model_dir() -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(d) = std::env::var("MUNINN_MODEL_DIR") {
        cands.push(PathBuf::from(d));
    }
    if let Ok(r) = std::env::var("CLAUDE_PLUGIN_ROOT") {
        cands.push(PathBuf::from(r).join("models").join(MODEL_NAME));
    }
    if let Ok(h) = std::env::var("HOME") {
        cands.push(
            PathBuf::from(h)
                .join(".local/share/muninn/models")
                .join(MODEL_NAME),
        );
    }
    cands
        .into_iter()
        .find(|p| p.join("model.safetensors").is_file())
}

fn sha256_file(p: &Path) -> Result<String> {
    let bytes = std::fs::read(p).map_err(|e| Error::io(p, e))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Verify the three files against the published hashes. Returns the model id
/// (`name@<12 hex of the weights hash>`) that vectors are tagged with.
pub fn verify(dir: &Path) -> Result<String> {
    let mut weights = String::new();
    for (name, want) in MODEL_FILES {
        let got = sha256_file(&dir.join(name))?;
        if got != want {
            return Err(Error::Other(format!(
                "model file {name} has sha256 {got}, expected {want}; re-download the model"
            )));
        }
        if name == "model.safetensors" {
            weights = got;
        }
    }
    Ok(format!("{MODEL_NAME}@{}", &weights[..12]))
}

pub struct Embedder {
    model: StaticModel,
    pub model_id: String,
    pub load_ms: f64,
}

impl Embedder {
    /// Load from `dir` after verifying checksums. Never call this from a read hook.
    pub fn load(dir: &Path) -> Result<Self> {
        let t0 = Instant::now();
        let model_id = verify(dir)?;
        let model = StaticModel::from_pretrained(dir.to_string_lossy().as_ref(), None, None, None)
            .map_err(|e| Error::Other(format!("loading {}: {e}", dir.display())))?;
        Ok(Self {
            model,
            model_id,
            load_ms: t0.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub fn load_default() -> Result<Self> {
        let dir = model_dir().ok_or_else(|| {
            Error::Other(format!(
                "no model: put {MODEL_NAME} under $MUNINN_MODEL_DIR, $CLAUDE_PLUGIN_ROOT/models/ or ~/.local/share/muninn/models/"
            ))
        })?;
        Self::load(&dir)
    }

    pub fn encode(&self, texts: &[String]) -> Vec<Vec<f32>> {
        self.model.encode_with_args(texts, Some(512), 256)
    }

    pub fn encode_one(&self, text: &str) -> Vec<f32> {
        self.encode(&[text.to_string()]).pop().unwrap_or_default()
    }

    /// Same as `encode`, plus how long the call took, in milliseconds.
    pub fn encode_timed(&self, texts: &[String]) -> (Vec<Vec<f32>>, f64) {
        let t0 = Instant::now();
        let vecs = self.encode(texts);
        (vecs, t0.elapsed().as_secs_f64() * 1000.0)
    }
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(v.len() * 4);
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
    b
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// The text a record is embedded from: subject, object and the body's head.
fn record_text(subject: &str, object: &str, body: &str) -> String {
    let head: String = body.chars().take(1_200).collect();
    format!("{subject}\n{object}\n{head}")
}

#[derive(Debug, Default, Serialize)]
pub struct EmbedStats {
    pub embedded: usize,
    pub pending_before: usize,
    pub load_ms: f64,
    pub encode_ms: f64,
    pub model_id: String,
    /// Newly embedded records retired as semantic variants of an older one.
    pub variants_retired: usize,
    #[serde(skip)]
    pub new_ids: Vec<i64>,
}

/// Embed every active record without a vector for the current model. `rebuild` drops
/// all vectors first (model change). Runs inside the write path, never a read hook.
pub fn embed_pending(db: &Db, emb: &Embedder, rebuild: bool) -> Result<EmbedStats> {
    let mut st = EmbedStats {
        load_ms: emb.load_ms,
        model_id: emb.model_id.clone(),
        ..Default::default()
    };
    if rebuild {
        db.conn.execute("DELETE FROM record_vec", [])?;
    }
    let mut stmt = db.conn.prepare(
        "SELECT r.id, r.subject, r.object, r.body FROM record r \
         LEFT JOIN record_vec v ON v.record_id = r.id AND v.model_id = ?1 \
         WHERE r.invalid = 0 AND v.record_id IS NULL ORDER BY r.id",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([&emb.model_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                record_text(
                    &r.get::<_, String>(1)?,
                    &r.get::<_, String>(2)?,
                    &r.get::<_, String>(3)?,
                ),
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    st.pending_before = rows.len();
    if rows.is_empty() {
        db.meta_set("embed_model", &emb.model_id)?;
        return Ok(st);
    }
    let t0 = Instant::now();
    let texts: Vec<String> = rows.iter().map(|(_, t)| t.clone()).collect();
    let vecs = emb.encode(&texts);
    st.encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let now = now_ms();
    let tx = db.write_tx()?;
    {
        let mut ins = tx.prepare(
            "INSERT OR REPLACE INTO record_vec(record_id, dim, vec, model_id, created_at) VALUES(?1, ?2, ?3, ?4, ?5)",
        )?;
        for ((id, _), v) in rows.iter().zip(vecs.iter()) {
            ins.execute(rusqlite::params![
                id,
                v.len() as i64,
                to_blob(v),
                emb.model_id,
                now
            ])?;
            st.embedded += 1;
            st.new_ids.push(*id);
        }
    }
    tx.commit()?;
    st.variants_retired = retire_variants(db, &st.new_ids, &emb.model_id)?;
    db.meta_set("embed_model", &emb.model_id)?;
    db.meta_set("embed_at_ms", &now.to_string())?;
    Ok(st)
}

/// A newly embedded record whose vector is within `DUP_COSINE` of an **older** active
/// record of the same kind is a variant of it (a compaction summary restated, a chunk
/// repeated across sessions): it is retired as `superseded` by the older one and never
/// competes with it for the budget. Typed records keep their own subject rule
/// same (kind, subject); this pass covers records across subjects.
pub fn retire_variants(db: &Db, new_ids: &[i64], model_id: &str) -> Result<usize> {
    let mut retired = 0usize;
    let mut sel = db.conn.prepare("SELECT v.vec, r.kind FROM record_vec v JOIN record r ON r.id = v.record_id WHERE v.record_id = ?1 AND v.model_id = ?2")?;
    let mut cand = db.conn.prepare(
        "SELECT v.record_id, v.vec FROM record_vec v JOIN record r ON r.id = v.record_id \
         WHERE r.invalid = 0 AND r.kind = ?1 AND v.model_id = ?2 AND v.record_id < ?3 ORDER BY v.record_id",
    )?;
    for id in new_ids {
        let Ok((blob, kind)) = sel.query_row(rusqlite::params![id, model_id], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
        }) else {
            continue;
        };
        let v = from_blob(&blob);
        let best = cand
            .query_map(rusqlite::params![kind, model_id, id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?
            .filter_map(|r| r.ok())
            .map(|(oid, ob)| (oid, dot(&v, &from_blob(&ob))))
            .filter(|(_, s)| *s >= DUP_COSINE)
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
        if let Some((older, _)) = best {
            let n = db.conn.execute(
                "UPDATE record SET invalid = 1, invalid_reason = 'superseded', invalidated_by = ?1 WHERE id = ?2 AND invalid = 0",
                rusqlite::params![older, id],
            )?;
            retired += n;
        }
    }
    Ok(retired)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Neighbour {
    pub id: i64,
    pub score: f32,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Exact k nearest neighbours of `query` among active records (vectors are unit
/// length, so the dot product is the cosine). Ties break on id; the result is a pure
/// function of the store and the query.
pub fn knn(db: &Db, query: &[f32], k: usize, model_id: &str) -> Result<Vec<Neighbour>> {
    let mut stmt = db.conn.prepare(
        "SELECT v.record_id, v.vec FROM record_vec v JOIN served_record r ON r.id = v.record_id \
         WHERE v.model_id = ?1 ORDER BY v.record_id",
    )?;
    let mut out: Vec<Neighbour> = stmt
        .query_map([model_id], |r| {
            let id: i64 = r.get(0)?;
            let blob: Vec<u8> = r.get(1)?;
            Ok(Neighbour {
                id,
                score: dot(query, &from_blob(&blob)),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    out.truncate(k);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip_and_dot() {
        let v = vec![0.5f32, -0.25, 1.0];
        assert_eq!(from_blob(&to_blob(&v)), v);
        assert_eq!(dot(&v, &v), 0.25 + 0.0625 + 1.0);
    }

    /// Runs only where the model is installed; CI without the model skips it.
    #[test]
    fn end_to_end_when_model_present() {
        let Some(dir) = model_dir() else { return };
        let emb = Embedder::load(&dir).unwrap();
        assert!(emb.model_id.starts_with(MODEL_NAME));
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        for (i, body) in [
            "retry with exponential backoff",
            "the webhook retries with backoff",
            "grammar for python",
        ]
        .iter()
        .enumerate()
        {
            db.conn.execute(
                "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) VALUES('decision','retry.policy','is',?1,?1,'user_said',3,'s',?2,1)",
                rusqlite::params![body, format!("h{i}")],
            ).unwrap();
        }
        let st = embed_pending(&db, &emb, false).unwrap();
        assert_eq!(st.embedded, 3);
        assert_eq!(st.variants_retired, 0, "three different sentences");
        // a near-identical restatement of #1, inserted later, is retired as its variant
        db.conn.execute(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) VALUES('decision','retry.policy','is','retry with exponential backoff.','retry with exponential backoff.','user_said',3,'s','h9',2)",
            [],
        ).unwrap();
        let st2 = embed_pending(&db, &emb, false).unwrap();
        assert_eq!(st2.embedded, 1);
        assert_eq!(st2.variants_retired, 1);
        let q = emb.encode_one("backoff retries");
        let a = knn(&db, &q, 2, &emb.model_id).unwrap();
        let b = knn(&db, &q, 2, &emb.model_id).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 2);
        assert!(
            a.iter().all(|n| n.id != 3),
            "python grammar is not a backoff neighbour: {a:?}"
        );
    }
}
