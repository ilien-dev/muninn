//! Resolution of the per-project `.muninn/` directory.
//!
//! Every git worktree of the same repository shares one store, like the native
//! memory does [L1]. The common git directory is found by reading `.git` files
//! directly, without spawning `git` (no subprocess in the hot path).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    /// Repository root (directory that owns `.git`, or the cwd when not a repo).
    pub root: PathBuf,
    pub muninn_dir: PathBuf,
}

impl ProjectPaths {
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let muninn_dir = root.join(".muninn");
        Self { root, muninn_dir }
    }

    /// Resolve from an arbitrary working directory. `MUNINN_ROOT` overrides the
    /// search (used by the experiment runner to give each worktree its own store).
    pub fn resolve(cwd: &Path) -> Self {
        if let Some(r) = std::env::var_os("MUNINN_ROOT") {
            return Self::from_root(PathBuf::from(r));
        }
        let mut dir: Option<&Path> = Some(cwd);
        while let Some(d) = dir {
            // An explicit `.muninn` directory wins: it is where `muninn init` ran.
            if d.join(".muninn").is_dir() {
                return Self::from_root(d);
            }
            let git = d.join(".git");
            if git.is_dir() {
                return Self::from_root(d);
            }
            if git.is_file() {
                if let Some(common_root) = common_root_from_gitfile(&git) {
                    return Self::from_root(common_root);
                }
                return Self::from_root(d);
            }
            dir = d.parent();
        }
        Self::from_root(cwd)
    }

    /// Where the code is. Normally the project root; `MUNINN_SOURCE_ROOT` overrides it
    /// when the store lives outside the checkout (experiment cells).
    pub fn source_root(&self) -> PathBuf {
        std::env::var_os("MUNINN_SOURCE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.root.clone())
    }

    pub fn db_path(&self) -> PathBuf {
        self.muninn_dir.join("muninn.db")
    }
    pub fn records_dir(&self) -> PathBuf {
        self.muninn_dir.join("records")
    }
    pub fn compiled_dir(&self) -> PathBuf {
        self.muninn_dir.join("compiled")
    }
    pub fn log_dir(&self) -> PathBuf {
        self.muninn_dir.join("log")
    }
    pub fn compact_dir(&self) -> PathBuf {
        self.muninn_dir.join("compact")
    }
    pub fn heartbeat_log(&self) -> PathBuf {
        self.log_dir().join("heartbeat.jsonl")
    }
    pub fn trace_log(&self) -> PathBuf {
        self.log_dir().join("muninn.jsonl")
    }
    pub fn index_md(&self) -> PathBuf {
        self.muninn_dir.join("index.md")
    }

    pub fn is_initialised(&self) -> bool {
        self.db_path().is_file()
    }

    /// All directories `muninn init` must create.
    pub fn all_dirs(&self) -> [PathBuf; 5] {
        [
            self.muninn_dir.clone(),
            self.records_dir(),
            self.compiled_dir(),
            self.log_dir(),
            self.compact_dir(),
        ]
    }
}

/// `.git` as a file means a worktree: `gitdir: <common>/.git/worktrees/<name>`.
/// Returns the root that owns the common `.git` directory.
fn common_root_from_gitfile(gitfile: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(gitfile).ok()?;
    let line = content.lines().find_map(|l| l.strip_prefix("gitdir:"))?;
    let gitdir = PathBuf::from(line.trim());
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        gitfile.parent()?.join(gitdir)
    };
    // <common>/.git/worktrees/<name> -> <common>
    let mut anc = gitdir.ancestors();
    let _name = anc.next()?;
    let worktrees = anc.next()?;
    if worktrees.file_name()?.to_str()? != "worktrees" {
        return None;
    }
    let dot_git = anc.next()?;
    dot_git.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_repo_root_from_nested_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let nested = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let p = ProjectPaths::resolve(&nested);
        assert_eq!(p.root, tmp.path());
    }

    #[test]
    fn worktree_shares_common_store() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(main.join(".git/worktrees/feat")).unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/feat").display()),
        )
        .unwrap();
        let p = ProjectPaths::resolve(&wt);
        assert_eq!(p.root, main);
    }

    #[test]
    fn falls_back_to_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let p = ProjectPaths::resolve(tmp.path());
        assert_eq!(p.root, tmp.path());
    }
}
