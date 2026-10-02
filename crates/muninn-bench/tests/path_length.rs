//! Every tracked path must fit Windows' 260-character MAX_PATH once Claude Code clones the
//! repository under `C:\Users\<name>\.claude\plugins\marketplaces\ilien-dev-muninn..clone\`
//! (about 70 characters, more with a longer user name). Git for Windows does not enable
//! `core.longpaths` by default, so one longer path fails the whole marketplace checkout.

use std::process::Command;

const MAX_TRACKED_PATH: usize = 170;

#[test]
fn tracked_paths_fit_windows_clone() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let Ok(out) = Command::new("git")
        .args(["-C", root, "ls-files", "-z"])
        .output()
    else {
        eprintln!("git not available; skipped");
        return;
    };
    if !out.status.success() {
        eprintln!("not a git checkout; skipped");
        return;
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let long: Vec<&str> = listing
        .split('\0')
        .filter(|p| p.chars().count() > MAX_TRACKED_PATH)
        .collect();
    assert!(
        long.is_empty(),
        "{} tracked paths exceed {MAX_TRACKED_PATH} characters, e.g. {:?}",
        long.len(),
        &long[..long.len().min(5)]
    );
}
