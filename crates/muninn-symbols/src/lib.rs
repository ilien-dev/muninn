//! Symbol graph (plan, Phase 5 §1-bis / §1-ter). Five grammars compiled into the
//! binary; one extractor for all of them, driven by per-language `.scm` queries with
//! normalised captures (`@name` plus one `@def.<kind>` or `@ref.<kind>` capture per
//! pattern). Files are re-indexed only when their content hash changes, on the write
//! path; the read path never parses — it looks symbols up by name in an index.

use muninn_core::db::now_ms;
use muninn_core::{Db, Error, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

pub const MAX_FILE_BYTES: u64 = 1 << 20;
const EXCLUDED_DIRS: [&str; 8] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".git",
    ".muninn",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
}

impl Lang {
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Python => "python",
            Lang::Go => "go",
        }
    }

    /// Language by extension; `.d.ts` is excluded (declarations, not code).
    pub fn of_path(p: &Path) -> Option<Lang> {
        let name = p.file_name()?.to_str()?;
        if name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts") {
            return None;
        }
        Some(match p.extension()?.to_str()? {
            "rs" => Lang::Rust,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "py" | "pyi" => Lang::Python,
            "go" => Lang::Go,
            _ => return None,
        })
    }

    fn language(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }

    fn scm(self) -> (&'static str, &'static str) {
        match self {
            Lang::Rust => (
                include_str!("../queries/rust/defs.scm"),
                include_str!("../queries/rust/refs.scm"),
            ),
            Lang::TypeScript | Lang::Tsx => (
                include_str!("../queries/typescript/defs.scm"),
                include_str!("../queries/typescript/refs.scm"),
            ),
            Lang::JavaScript => (
                include_str!("../queries/javascript/defs.scm"),
                include_str!("../queries/javascript/refs.scm"),
            ),
            Lang::Python => (
                include_str!("../queries/python/defs.scm"),
                include_str!("../queries/python/refs.scm"),
            ),
            Lang::Go => (
                include_str!("../queries/go/defs.scm"),
                include_str!("../queries/go/refs.scm"),
            ),
        }
    }
}

struct Compiled {
    language: Language,
    defs: Query,
    refs: Query,
}

/// Queries compile once per process and per language.
fn compiled(lang: Lang) -> Result<&'static Compiled> {
    static CACHE: [OnceLock<Compiled>; 6] = [const { OnceLock::new() }; 6];
    let slot = &CACHE[lang as usize];
    if let Some(c) = slot.get() {
        return Ok(c);
    }
    let language = lang.language();
    let (d, r) = lang.scm();
    let defs = Query::new(&language, d)
        .map_err(|e| Error::Other(format!("{} defs.scm: {e}", lang.name())))?;
    let refs = Query::new(&language, r)
        .map_err(|e| Error::Other(format!("{} refs.scm: {e}", lang.name())))?;
    let _ = slot.set(Compiled {
        language,
        defs,
        refs,
    });
    Ok(slot.get().unwrap())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    pub qualified: String,
    pub kind: String,
    pub line: usize,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Ref {
    pub name: String,
    pub kind: String,
    pub line: usize,
    /// The enclosing definition's short name, when inside one.
    pub from: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct FileSymbols {
    pub lang: Option<&'static str>,
    pub defs: Vec<Def>,
    pub refs: Vec<Ref>,
    pub partial: bool,
    pub parse_ms: f64,
}

/// Module path of a file: `src/auth/jwt.rs` → `src::auth::jwt`.
fn module_of(rel: &str) -> String {
    let no_ext = rel.rsplit_once('.').map(|(a, _)| a).unwrap_or(rel);
    no_ext.replace(['/', '\\'], "::")
}

/// Names of the enclosing definition nodes of `node`, outermost first, from the set of
/// node kinds a language uses for scopes.
fn scope_chain(mut node: tree_sitter::Node, src: &[u8]) -> Vec<String> {
    const SCOPES: [&str; 12] = [
        "impl_item",
        "mod_item",
        "trait_item",
        "struct_item",
        "enum_item",
        "function_item",
        "class_declaration",
        "abstract_class_declaration",
        "function_declaration",
        "method_definition",
        "class_definition",
        "function_definition",
    ];
    let mut out = Vec::new();
    while let Some(p) = node.parent() {
        if SCOPES.contains(&p.kind()) || p.kind() == "method_declaration" {
            let name = p
                .child_by_field_name("name")
                .or_else(|| p.child_by_field_name("type"))
                .and_then(|n| n.utf8_text(src).ok())
                .map(str::to_string);
            if let Some(n) = name {
                out.push(n);
            }
        }
        node = p;
    }
    out.reverse();
    out
}

/// Parse one file and run its queries. A file with syntax errors yields a partial
/// tree; what is there is extracted and the result is marked `partial`.
pub fn extract(rel: &str, src: &str) -> Result<FileSymbols> {
    let Some(lang) = Lang::of_path(Path::new(rel)) else {
        return Ok(FileSymbols::default());
    };
    let t0 = std::time::Instant::now();
    let c = compiled(lang)?;
    let mut parser = Parser::new();
    parser
        .set_language(&c.language)
        .map_err(|e| Error::Other(format!("{}: {e}", lang.name())))?;
    let Some(tree) = parser.parse(src, None) else {
        return Ok(FileSymbols {
            lang: Some(lang.name()),
            partial: true,
            ..Default::default()
        });
    };
    let bytes = src.as_bytes();
    let module = module_of(rel);
    let mut out = FileSymbols {
        lang: Some(lang.name()),
        partial: tree.root_node().has_error(),
        ..Default::default()
    };
    let names_d = c.defs.capture_names();
    let mut cur = QueryCursor::new();
    let mut it = cur.matches(&c.defs, tree.root_node(), bytes);
    while let Some(m) = it.next() {
        let mut name: Option<String> = None;
        let mut kind: Option<String> = None;
        let mut node: Option<tree_sitter::Node> = None;
        let mut defnode: Option<tree_sitter::Node> = None;
        for cap in m.captures() {
            let cn = names_d[cap.index as usize];
            if cn == "name" {
                name = cap.node.utf8_text(bytes).ok().map(str::to_string);
                node = Some(cap.node);
            } else if let Some(k) = cn.strip_prefix("def.") {
                kind = Some(k.to_string());
                defnode = Some(cap.node);
            }
        }
        let (Some(name), Some(kind), Some(node)) = (name, kind, node) else {
            continue;
        };
        // scopes enclosing the definition itself, not the definition
        let chain = scope_chain(defnode.unwrap_or(node), bytes);
        let mut q = module.clone();
        for s in &chain {
            q.push_str("::");
            q.push_str(s);
        }
        q.push_str("::");
        q.push_str(&name);
        out.defs.push(Def {
            qualified: q,
            kind,
            line: node.start_position().row + 1,
            parent: chain.last().cloned(),
            name,
        });
    }
    let names_r = c.refs.capture_names();
    let mut cur = QueryCursor::new();
    let mut it = cur.matches(&c.refs, tree.root_node(), bytes);
    while let Some(m) = it.next() {
        let mut name: Option<String> = None;
        let mut kind: Option<String> = None;
        let mut node: Option<tree_sitter::Node> = None;
        for cap in m.captures() {
            let cn = names_r[cap.index as usize];
            if cn == "name" {
                let t = cap
                    .node
                    .utf8_text(bytes)
                    .unwrap_or("")
                    .trim_matches(|ch| ch == '"' || ch == '\'' || ch == '`');
                name = Some(t.to_string());
                node = Some(cap.node);
            } else if let Some(k) = cn.strip_prefix("ref.") {
                kind = Some(k.to_string());
            }
        }
        let (Some(name), Some(kind), Some(node)) = (name, kind, node) else {
            continue;
        };
        if name.is_empty() || name.len() > 200 {
            continue;
        }
        let from = scope_chain(node, bytes).last().cloned();
        out.refs.push(Ref {
            name,
            kind,
            line: node.start_position().row + 1,
            from,
        });
    }
    out.parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(out)
}

#[derive(Debug, Default, Serialize)]
pub struct IndexStats {
    pub files_seen: usize,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub defs: usize,
    pub refs: usize,
    pub partial: usize,
    pub ms: f64,
}

fn file_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Index one file relative to `root` if its hash changed (or `force`). Returns whether
/// it was (re)indexed.
pub fn index_file(
    db: &Db,
    root: &Path,
    rel: &str,
    force: bool,
    st: &mut IndexStats,
) -> Result<bool> {
    st.files_seen += 1;
    let p = root.join(rel);
    let md = std::fs::metadata(&p).map_err(|e| Error::io(&p, e))?;
    if !md.is_file() || md.len() > MAX_FILE_BYTES || Lang::of_path(Path::new(rel)).is_none() {
        st.files_skipped += 1;
        return Ok(false);
    }
    let bytes = std::fs::read(&p).map_err(|e| Error::io(&p, e))?;
    let hash = file_hash(&bytes);
    if !force {
        let known: Option<String> = db
            .conn
            .query_row(
                "SELECT file_hash FROM symbol_file WHERE path = ?1",
                [rel],
                |r| r.get(0),
            )
            .ok();
        if known.as_deref() == Some(hash.as_str()) {
            st.files_skipped += 1;
            return Ok(false);
        }
    }
    let src = String::from_utf8_lossy(&bytes);
    let fs = extract(rel, &src)?;
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM symbol WHERE path = ?1", [rel])?;
    tx.execute("DELETE FROM symbol_ref WHERE path = ?1", [rel])?;
    {
        let mut ins = tx.prepare("INSERT INTO symbol(qualified_name, short_name, kind, path, line, file_hash, partial) VALUES(?1,?2,?3,?4,?5,?6,?7)")?;
        for d in &fs.defs {
            ins.execute(rusqlite::params![
                d.qualified,
                d.name,
                d.kind,
                rel,
                d.line as i64,
                hash,
                fs.partial as i64
            ])?;
        }
        let mut insr = tx.prepare("INSERT INTO symbol_ref(path, line, from_name, to_name, ref_kind) VALUES(?1,?2,?3,?4,?5)")?;
        for r in &fs.refs {
            insr.execute(rusqlite::params![
                rel,
                r.line as i64,
                r.from,
                r.name,
                r.kind
            ])?;
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO symbol_file(path, file_hash, lang, defs, refs, partial, indexed_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![rel, hash, fs.lang.unwrap_or(""), fs.defs.len() as i64, fs.refs.len() as i64, fs.partial as i64, now_ms()],
    )?;
    tx.commit()?;
    st.files_indexed += 1;
    st.defs += fs.defs.len();
    st.refs += fs.refs.len();
    st.partial += fs.partial as usize;
    Ok(true)
}

/// Files worth indexing: tracked and untracked-not-ignored (git honours .gitignore),
/// or a plain walk when git is unavailable; excluded directories skipped either way.
pub fn candidate_files(root: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output();
    let mut files: Vec<String> = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        _ => walk(root, root),
    };
    files.retain(|f| {
        Lang::of_path(Path::new(f)).is_some()
            && !f.split('/').any(|seg| EXCLUDED_DIRS.contains(&seg))
    });
    files.sort();
    files
}

fn walk(root: &Path, dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !EXCLUDED_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                out.extend(walk(root, &p));
            }
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    out
}

/// Index every candidate file whose content changed (`force` re-indexes all) and drop
/// rows of files that no longer exist.
pub fn rebuild(db: &Db, root: &Path, force: bool) -> Result<IndexStats> {
    let t0 = std::time::Instant::now();
    let mut st = IndexStats::default();
    let files = candidate_files(root);
    for f in &files {
        if let Err(e) = index_file(db, root, f, force, &mut st) {
            eprintln!("muninn symbols: {f}: {e}");
        }
    }
    // gone files
    let mut stmt = db.conn.prepare("SELECT path FROM symbol_file")?;
    let known: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    let set: std::collections::HashSet<&String> = files.iter().collect();
    for k in known {
        if !set.contains(&k) {
            db.conn
                .execute("DELETE FROM symbol WHERE path = ?1", [&k])?;
            db.conn
                .execute("DELETE FROM symbol_ref WHERE path = ?1", [&k])?;
            db.conn
                .execute("DELETE FROM symbol_file WHERE path = ?1", [&k])?;
        }
    }
    db.meta_set("symbols_indexed_ms", &now_ms().to_string())?;
    st.ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(st)
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolRow {
    pub id: i64,
    pub qualified_name: String,
    pub short_name: String,
    pub kind: String,
    pub path: String,
    pub line: i64,
}

/// Definitions of a short name (all of them: collisions are kept as rows).
pub fn lookup(db: &Db, short: &str) -> Result<Vec<SymbolRow>> {
    let mut stmt = db.conn.prepare("SELECT id, qualified_name, short_name, kind, path, line FROM symbol WHERE short_name = ?1 ORDER BY path, line")?;
    let rows = stmt
        .query_map([short], |r| {
            Ok(SymbolRow {
                id: r.get(0)?,
                qualified_name: r.get(1)?,
                short_name: r.get(2)?,
                kind: r.get(3)?,
                path: r.get(4)?,
                line: r.get(5)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Who references `short` (calls, imports, type uses): `(path, line, from_name)`,
/// exact by name — the one hop the write path precomputes into cues.
pub fn referrers(db: &Db, short: &str, limit: usize) -> Result<Vec<(String, i64, Option<String>)>> {
    let mut stmt = db.conn.prepare("SELECT path, line, from_name FROM symbol_ref WHERE to_name = ?1 ORDER BY path, line LIMIT ?2")?;
    let rows = stmt
        .query_map(rusqlite::params![short, limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Definitions in a file, for the cue derivation of a record anchored to it.
pub fn defs_in(db: &Db, path: &str) -> Result<Vec<String>> {
    let mut stmt = db
        .conn
        .prepare("SELECT short_name FROM symbol WHERE path = ?1 ORDER BY line")?;
    let rows = stmt
        .query_map([path], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

pub fn root_of(p: &Path) -> PathBuf {
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/").to_string() + name,
        )
        .unwrap()
    }

    #[test]
    fn every_grammar_parses_its_fixture() {
        for (f, min_defs, min_refs) in [
            ("recall.rs", 10, 40),
            ("util.ts", 40, 80),
            ("response.js", 20, 60),
            ("context.go", 60, 150),
            ("sessions.py", 20, 60),
        ] {
            let src = fixture(f);
            let _warm = extract(f, &src).unwrap(); // query compilation is once per process
            let fs = extract(f, &src).unwrap();
            let lines = src.lines().count();
            assert!(lines >= 200, "{f}: fixture too small ({lines} lines)");
            assert!(
                fs.defs.len() >= min_defs,
                "{f}: {} defs: {:?}",
                fs.defs.len(),
                fs.defs.iter().map(|d| &d.name).take(20).collect::<Vec<_>>()
            );
            assert!(fs.refs.len() >= min_refs, "{f}: {} refs", fs.refs.len());
            assert!(!fs.partial, "{f}: unexpected syntax errors");
            // < 5 ms per 1 000 lines is the contract; assert a generous bound in tests
            assert!(fs.parse_ms < 50.0, "{f}: {:.1} ms", fs.parse_ms);
            eprintln!(
                "{f}: {lines} lines · {} defs · {} refs · {:.2} ms",
                fs.defs.len(),
                fs.refs.len(),
                fs.parse_ms
            );
        }
    }

    #[test]
    fn qualified_names_and_scopes() {
        let src = "mod a { pub struct S; impl S { pub fn go(&self) { help(); } } }\nfn help() {}\n";
        let fs = extract("src/x.rs", src).unwrap();
        let go = fs.defs.iter().find(|d| d.name == "go").unwrap();
        assert_eq!(go.qualified, "src::x::a::S::go");
        assert_eq!(go.parent.as_deref(), Some("S"));
        let call = fs
            .refs
            .iter()
            .find(|r| r.name == "help" && r.kind == "call")
            .unwrap();
        assert_eq!(call.from.as_deref(), Some("go"));
        // syntax error → partial, still extracts what parses
        let bad = extract("y.py", "def ok():\n    return 1\ndef broken(:\n").unwrap();
        assert!(bad.partial);
        assert!(bad.defs.iter().any(|d| d.name == "ok"));
    }

    #[test]
    fn index_is_incremental() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/a.rs"),
            "pub fn one() {}\npub fn two() { one(); }\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("b.py"), "def three():\n    pass\n").unwrap();
        let db = Db::open(&tmp.path().join("s.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        let st = rebuild(&db, tmp.path(), false).unwrap();
        assert_eq!(st.files_indexed, 2);
        assert_eq!(st.defs, 3);
        let st2 = rebuild(&db, tmp.path(), false).unwrap();
        assert_eq!(st2.files_indexed, 0, "unchanged files are skipped");
        std::fs::write(tmp.path().join("src/a.rs"), "pub fn one() {}\n").unwrap();
        let st3 = rebuild(&db, tmp.path(), false).unwrap();
        assert_eq!(st3.files_indexed, 1);
        assert_eq!(lookup(&db, "two").unwrap().len(), 0);
        assert_eq!(lookup(&db, "one").unwrap()[0].qualified_name, "src::a::one");
        std::fs::remove_file(tmp.path().join("b.py")).unwrap();
        rebuild(&db, tmp.path(), false).unwrap();
        assert!(lookup(&db, "three").unwrap().is_empty());
    }
}
