//! The project's `.codex/hooks.json` as a file two parties write: the user's own hooks and
//! Muninn's. `init` adds Muninn's and `clean` takes them out; neither touches a handler it
//! did not write, and a file that cannot be read as a hooks file is left as it is.
//!
//! Whether Codex is on this machine is read from the file system, never by running it: its
//! home directory exists (`$CODEX_HOME`, else `.codex` under the user's home), or the project
//! already has a `.codex` directory.

use anyhow::{bail, Result};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Codex's home directory, whether or not it exists.
pub fn home() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    var("CODEX_HOME").map(PathBuf::from).or_else(|| {
        var("HOME")
            .or_else(|| var("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(".codex"))
    })
}

/// Is Codex in use here: on this machine, or in this project?
pub fn present(root: &Path) -> bool {
    home().is_some_and(|h| h.is_dir()) || root.join(".codex").is_dir()
}

pub fn file(root: &Path) -> PathBuf {
    root.join(".codex").join("hooks.json")
}

/// Where the Codex plugin of this name is installed, if it is: Codex keeps a plugin under
/// `<Codex home>/plugins/cache/<marketplace>/<plugin>/<version>/` with its manifest in
/// `.codex-plugin/plugin.json`. Returns the plugin's directory, whatever the marketplace and
/// the version. Read from the file system; whether Codex has the plugin switched on is in its
/// own configuration, which this does not read.
pub fn plugin_install() -> Option<PathBuf> {
    let cache = home()?.join("plugins").join("cache");
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    for marketplace in dirs(&cache) {
        let plugin = marketplace.join("muninn");
        let installed = dirs(&plugin).iter().any(|version| {
            std::fs::read_to_string(version.join(".codex-plugin").join("plugin.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .is_some_and(|m| m.get("name").and_then(|n| n.as_str()) == Some("muninn"))
        });
        if installed {
            return Some(plugin);
        }
    }
    None
}

/// Which way Muninn's hooks reach a Codex session in this project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Codex is here and nothing runs Muninn's hooks.
    None,
    /// The project's `.codex/hooks.json`.
    Project,
    /// The Codex plugin, installed at this directory.
    Plugin(PathBuf),
    /// Both: every hook runs twice.
    Both(PathBuf),
}

impl Route {
    pub fn name(&self) -> &'static str {
        match self {
            Route::None => "none",
            Route::Project => "project",
            Route::Plugin(_) => "plugin",
            Route::Both(_) => "plugin+project",
        }
    }
}

/// The route in this project, or `None` when Codex is not in use here at all.
pub fn route(root: &Path) -> Option<Route> {
    if !present(root) {
        return None;
    }
    Some(match (plugin_install(), installed(root)) {
        (Some(p), true) => Route::Both(p),
        (Some(p), false) => Route::Plugin(p),
        (None, true) => Route::Project,
        (None, false) => Route::None,
    })
}

/// A handler that runs `muninn hook <Event>`: the program's file name is `muninn` or
/// `muninn.exe`, whatever directory it is in and however it is quoted.
fn is_ours(handler: &Value) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#"(?i)(?:^|[\\/'"])muninn(?:\.exe)?['"]?\s+hook\s+[A-Za-z]+\s*$"#)
            .unwrap()
    });
    ["command", "commandWindows", "command_windows"]
        .iter()
        .filter_map(|k| handler.get(*k).and_then(|c| c.as_str()))
        .any(|c| re.is_match(c))
}

/// The `hooks` object of a hooks file, or why the file is not one.
fn events(v: &mut Value) -> Result<&mut Map<String, Value>> {
    let Some(top) = v.as_object_mut() else {
        bail!("it is not a JSON object")
    };
    let hooks = top
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!("its `hooks` is not an object")
    };
    if let Some((event, _)) = hooks.iter().find(|(_, groups)| !groups.is_array()) {
        bail!("its `hooks.{event}` is not a list")
    }
    Ok(hooks)
}

/// Take Muninn's handlers out of every group, and a group out when nothing is left in it.
/// Returns whether anything was taken.
fn take_ours(hooks: &mut Map<String, Value>) -> bool {
    let mut took = false;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for g in groups.iter_mut() {
            if let Some(handlers) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                let before = handlers.len();
                handlers.retain(|h| !is_ours(h));
                took |= handlers.len() != before;
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .is_none_or(|h| !h.is_empty())
        });
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    took
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default() + "\n"
}

/// The file with Muninn's hooks in it: `ours` when there is none yet; otherwise the user's
/// file with any earlier Muninn handlers replaced by these and everything else kept. `None`
/// when the file already says exactly that. An error when the file is not a hooks file.
pub fn merged(existing: Option<&str>, ours: &Value) -> Result<Option<String>> {
    let Some(text) = existing.filter(|t| !t.trim().is_empty()) else {
        return Ok(Some(pretty(ours)));
    };
    let mut v: Value =
        serde_json::from_str(text).map_err(|e| anyhow::anyhow!("it is not JSON ({e})"))?;
    let hooks = events(&mut v)?;
    take_ours(hooks);
    for (event, groups) in ours["hooks"].as_object().into_iter().flatten() {
        let list = hooks
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let (Some(list), Some(groups)) = (list.as_array_mut(), groups.as_array()) {
            list.extend(groups.iter().cloned());
        }
    }
    if let Some(top) = v.as_object_mut() {
        if !top.contains_key("description") {
            top.insert("description".into(), ours["description"].clone());
        }
    }
    let next = pretty(&v);
    Ok((next != text).then_some(next))
}

/// The file without Muninn's hooks, and whether that leaves nothing of the user's: no
/// handler, and no key but an empty `hooks` once the description `init` writes is gone.
pub fn without_ours(existing: &str, our_description: &Value) -> Result<(String, bool)> {
    let mut v: Value =
        serde_json::from_str(existing).map_err(|e| anyhow::anyhow!("it is not JSON ({e})"))?;
    let hooks = events(&mut v)?;
    take_ours(hooks);
    let no_handler = hooks.is_empty();
    let Some(top) = v.as_object_mut() else {
        bail!("it is not a JSON object")
    };
    if top.get("description") == Some(our_description) {
        top.remove("description");
    }
    let nothing_left = no_handler && top.len() == 1;
    Ok((pretty(&v), nothing_left))
}

/// Does the project's file hold a Muninn hook?
pub fn installed(root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(file(root)) else {
        return false;
    };
    let Ok(mut v) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    events(&mut v).is_ok_and(take_ours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ours(bin: &str) -> Value {
        json!({
            "description": "Muninn's",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": format!("'{bin}' hook Stop"), "timeout": 30 }] }],
                "PreToolUse": [{ "matcher": "^Bash$", "hooks": [{ "type": "command", "command": format!("'{bin}' hook PreToolUse") }] }],
            }
        })
    }

    #[test]
    fn a_handler_is_muninns_by_the_program_it_runs() {
        for cmd in [
            "'/usr/local/bin/muninn' hook Stop",
            r#""C:\Users\A B\.local\bin\muninn.exe" hook SessionStart"#,
            "muninn hook Stop",
            "/opt/x/muninn hook PreToolUse ",
        ] {
            assert!(is_ours(&json!({ "command": cmd })), "{cmd}");
            assert!(
                is_ours(&json!({ "command": "sh x", "commandWindows": cmd })),
                "{cmd}"
            );
        }
        for cmd in [
            "python3 ~/.codex/hooks/stop.py",
            "'/opt/notmuninn' hook Stop",
            "echo muninn hook Stop | tee log",
            "'/usr/local/bin/muninn' status",
        ] {
            assert!(!is_ours(&json!({ "command": cmd })), "{cmd}");
        }
    }

    #[test]
    fn the_users_hooks_stay_and_muninns_are_added_once() {
        let theirs = json!({
            "description": "team hooks",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "python3 notify.py" }] }],
                "PermissionRequest": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "./ask.sh" }] }],
            }
        });
        let first = merged(Some(&pretty(&theirs)), &ours("/a/muninn"))
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(v["description"], "team hooks");
        assert_eq!(
            v["hooks"]["PermissionRequest"],
            theirs["hooks"]["PermissionRequest"]
        );
        assert_eq!(v["hooks"]["Stop"][0], theirs["hooks"]["Stop"][0]);
        assert_eq!(v["hooks"]["Stop"][1], ours("/a/muninn")["hooks"]["Stop"][0]);
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        // again: nothing to write
        assert_eq!(merged(Some(&first), &ours("/a/muninn")).unwrap(), None);
        // from another binary: its handlers replace the earlier ones, never stand beside them
        let moved = merged(Some(&first), &ours("/b/muninn")).unwrap().unwrap();
        assert!(!moved.contains("/a/muninn"), "{moved}");
        assert_eq!(moved.matches("hook Stop").count(), 1);
        // and out again: the user's file, value for value
        let (back, nothing_left) = without_ours(&moved, &ours("/b/muninn")["description"]).unwrap();
        assert!(!nothing_left);
        assert_eq!(serde_json::from_str::<Value>(&back).unwrap(), theirs);
    }

    #[test]
    fn a_handler_of_the_users_in_a_group_with_muninns_is_kept() {
        let mixed = json!({ "hooks": { "Stop": [{ "hooks": [
            { "type": "command", "command": "'/a/muninn' hook Stop" },
            { "type": "command", "command": "python3 notify.py" },
        ] }] } });
        let (out, nothing_left) = without_ours(&pretty(&mixed), &Value::Null).unwrap();
        assert!(!nothing_left);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["hooks"]["Stop"],
            json!([{ "hooks": [{ "type": "command", "command": "python3 notify.py" }] }])
        );
    }

    #[test]
    fn a_file_that_is_only_muninns_leaves_nothing() {
        let description = &ours("/a/muninn")["description"];
        let all = merged(None, &ours("/a/muninn")).unwrap().unwrap();
        assert!(without_ours(&all, description).unwrap().1);
        // a hooks file the user had left empty goes back to what it was
        let empty = merged(Some("{\"hooks\":{}}\n"), &ours("/a/muninn"))
            .unwrap()
            .unwrap();
        let (back, nothing_left) = without_ours(&empty, description).unwrap();
        assert!(nothing_left);
        assert_eq!(
            serde_json::from_str::<Value>(&back).unwrap(),
            json!({ "hooks": {} })
        );
        // a description of the user's is something of theirs
        let described = merged(
            Some("{\"description\":\"mine\",\"hooks\":{}}"),
            &ours("/a/muninn"),
        )
        .unwrap()
        .unwrap();
        assert!(!without_ours(&described, description).unwrap().1);
    }

    #[test]
    fn a_file_that_is_not_a_hooks_file_is_refused() {
        for bad in [
            "{ not json",
            "[1, 2]",
            "{\"hooks\": []}",
            "{\"hooks\": {\"Stop\": {}}}",
        ] {
            assert!(merged(Some(bad), &ours("/a/muninn")).is_err(), "{bad}");
            assert!(without_ours(bad, &Value::Null).is_err(), "{bad}");
        }
    }
}
