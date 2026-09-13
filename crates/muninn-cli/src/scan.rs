//! Configuration scanner (plan, Phase 6 §1): the three defect classes measured in
//! [2609.07360] over published agent configurations — an MCP server without a pinned
//! version, an over-broad `Bash(x:*)` allow rule, and a skill or command that
//! pre-approves a shell — found in the project's own configuration and in what Muninn
//! itself emits. Findings are reported; nothing is changed.

use muninn_core::ProjectPaths;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub class: &'static str,
    pub file: String,
    pub detail: String,
    pub fix: &'static str,
}

fn read_json(p: &Path) -> Option<serde_json::Value> {
    let t = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&t).ok()
}

/// A `Bash(...)` allow pattern that grants more than a rule can justify: everything,
/// a shell or interpreter, an escalation, a network fetch piped to a shell, or a
/// destructive command with a free suffix.
pub fn overbroad_bash(pattern: &str) -> Option<&'static str> {
    let p = pattern.trim();
    let Some(inner) = p.strip_prefix("Bash(").and_then(|s| s.strip_suffix(')')) else {
        return None;
    };
    let inner = inner.trim();
    if inner.is_empty() || inner == "*" || inner == ":*" || inner == "*:*" {
        return Some("grants every shell command");
    }
    let head = inner.split([':', ' ']).next().unwrap_or("").trim();
    const SHELLS: [&str; 12] = [
        "sh", "bash", "zsh", "fish", "dash", "python", "python3", "node", "perl", "ruby", "eval",
        "exec",
    ];
    const ESCALATE: [&str; 4] = ["sudo", "doas", "su", "pkexec"];
    const DESTRUCTIVE: [&str; 6] = ["rm", "dd", "mkfs", "shred", "chmod", "chown"];
    const FETCH: [&str; 2] = ["curl", "wget"];
    if SHELLS.contains(&head) && inner.ends_with('*') {
        return Some("pre-approves an interpreter with any argument");
    }
    if ESCALATE.contains(&head) {
        return Some("pre-approves privilege escalation");
    }
    if DESTRUCTIVE.contains(&head) && inner.ends_with('*') {
        return Some("pre-approves a destructive command with a free suffix");
    }
    if FETCH.contains(&head)
        && (inner.contains("| sh") || inner.contains("| bash") || inner.ends_with('*'))
    {
        return Some("pre-approves a network fetch (curl | sh shape)");
    }
    if inner == "git:*" || inner == "git *" {
        return Some("pre-approves every git command, force push included");
    }
    None
}

/// An MCP server whose launch command does not pin a version: `npx pkg` / `npx -y pkg`
/// (no `@x.y.z`), `pkg@latest`, `uvx pkg` without `==`, or a bare `docker run image`
/// without a tag or digest.
pub fn unpinned_mcp(command: &str, args: &[String]) -> Option<String> {
    let all: Vec<String> = std::iter::once(command.to_string())
        .chain(args.iter().cloned())
        .collect();
    let joined = all.join(" ");
    let pkg = all.iter().skip(1).find(|a| {
        !a.starts_with('-') && !a.starts_with('@') || a.starts_with('@') && a.contains('/')
    });
    match command.rsplit('/').next().unwrap_or(command) {
        "npx" | "pnpx" | "bunx" => {
            let p = pkg?;
            let name_at = p.rfind('@').filter(|&i| i > 0);
            match name_at {
                None => Some(format!("{joined}: no version (use pkg@x.y.z)")),
                Some(i) if p[i + 1..] == *"latest" => {
                    Some(format!("{joined}: @latest is not a pin"))
                }
                _ => None,
            }
        }
        "uvx" | "pipx" => {
            let p = pkg?;
            if p.contains("==") || p.contains('@') {
                None
            } else {
                Some(format!("{joined}: no version (use pkg==x.y.z)"))
            }
        }
        "docker" | "podman" => {
            let img = all
                .iter()
                .skip(1)
                .find(|a| a.contains('/') && !a.starts_with('-'))?;
            if img.contains('@') || img.rsplit('/').next().unwrap_or("").contains(':') {
                None
            } else {
                Some(format!("{joined}: image without tag or digest"))
            }
        }
        _ => None,
    }
}

fn scan_settings(root: &Path, rel: &str, out: &mut Vec<Finding>) {
    let p = root.join(rel);
    let Some(v) = read_json(&p) else { return };
    for list in ["allow"] {
        if let Some(arr) = v
            .pointer(&format!("/permissions/{list}"))
            .and_then(|a| a.as_array())
        {
            for x in arr.iter().filter_map(|x| x.as_str()) {
                if let Some(why) = overbroad_bash(x) {
                    out.push(Finding { class: "overbroad_bash_allow", file: rel.into(), detail: format!("{x}: {why}"), fix: "narrow the pattern to the exact command and arguments the rule needs, or use `ask`" });
                }
            }
        }
    }
    scan_mcp_servers(&v, rel, out);
}

fn scan_mcp_servers(v: &serde_json::Value, rel: &str, out: &mut Vec<Finding>) {
    if let Some(servers) = v.get("mcpServers").and_then(|s| s.as_object()) {
        for (name, cfg) in servers {
            let command = cfg.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let args: Vec<String> = cfg
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if command.is_empty() {
                continue;
            }
            if let Some(d) = unpinned_mcp(command, &args) {
                out.push(Finding { class: "mcp_unpinned", file: rel.into(), detail: format!("{name}: {d}"), fix: "pin the server package to an exact version (and prefer a checksummed install)" });
            }
        }
    }
}

fn scan_markdown_frontmatter(root: &Path, rel_dir: &str, out: &mut Vec<Finding>) {
    let dir = root.join(rel_dir);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).collect();
    // skills live one directory down (skills/<name>/SKILL.md)
    for f in files.clone() {
        if f.is_dir() {
            if let Ok(rd2) = std::fs::read_dir(&f) {
                files.extend(rd2.flatten().map(|e| e.path()));
            }
        }
    }
    for f in files
        .into_iter()
        .filter(|f| f.extension().and_then(|x| x.to_str()) == Some("md"))
    {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let Some(rest) = text.strip_prefix("---\n") else {
            continue;
        };
        let Some(end) = rest.find("\n---") else {
            continue;
        };
        let fm = &rest[..end];
        for line in fm.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            if k.trim() == "allowed-tools" {
                let v = v.trim();
                let tokens: Vec<&str> = v
                    .trim_matches(['[', ']'])
                    .split(',')
                    .map(str::trim)
                    .collect();
                for t in tokens {
                    let t = t.trim_matches(['"', '\'']);
                    let shellish =
                        t == "Bash" || t.starts_with("Bash(") && overbroad_bash(t).is_some();
                    if shellish {
                        let relf = f
                            .strip_prefix(root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| f.to_string_lossy().to_string());
                        out.push(Finding { class: "skill_preapproves_shell", file: relf, detail: format!("allowed-tools: {t}"), fix: "list the exact Bash commands the skill needs, e.g. Bash(cargo test:*)" });
                    }
                }
            }
        }
    }
}

/// Scan the project's own configuration and what Muninn compiled.
pub fn scan(paths: &ProjectPaths) -> Vec<Finding> {
    let root = paths.source_root();
    let mut out = Vec::new();
    for rel in [".claude/settings.json", ".claude/settings.local.json"] {
        scan_settings(&root, rel, &mut out);
    }
    if let Some(v) = read_json(&root.join(".mcp.json")) {
        scan_mcp_servers(&v, ".mcp.json", &mut out);
    }
    scan_markdown_frontmatter(&root, ".claude/commands", &mut out);
    scan_markdown_frontmatter(&root, ".claude/skills", &mut out);
    // what F2 emitted, before it is applied
    let compiled = paths.compiled_dir().join("permissions.json");
    if let Some(v) = read_json(&compiled) {
        for list in ["allow", "deny", "ask"] {
            if let Some(arr) = v
                .pointer(&format!("/permissions/{list}"))
                .or_else(|| v.get(list))
                .and_then(|a| a.as_array())
            {
                if list == "allow" {
                    for x in arr.iter().filter_map(|x| x.as_str()) {
                        if let Some(why) = overbroad_bash(x) {
                            out.push(Finding {
                                class: "overbroad_bash_allow",
                                file: ".muninn/compiled/permissions.json".into(),
                                detail: format!("{x}: {why}"),
                                fix: "Muninn never emits allow rules; this file was edited by hand",
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

pub fn render(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "config scan: no findings (MCP pins, Bash allow rules, skill shells)".into();
    }
    let mut s = format!("config scan: {} finding(s)\n", findings.len());
    for f in findings {
        s.push_str(&format!(
            "  [{}] {} — {}\n      fix: {}\n",
            f.class, f.file, f.detail, f.fix
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert!(overbroad_bash("Bash(*)").is_some());
        assert!(overbroad_bash("Bash(sudo:*)").is_some());
        assert!(overbroad_bash("Bash(rm -rf:*)").is_some());
        assert!(overbroad_bash("Bash(bash:*)").is_some());
        assert!(overbroad_bash("Bash(curl:*)").is_some());
        assert!(overbroad_bash("Bash(git:*)").is_some());
        assert!(overbroad_bash("Bash(cargo test:*)").is_none());
        assert!(overbroad_bash("Bash(git diff *)").is_none());
        assert!(overbroad_bash("Read").is_none());
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(unpinned_mcp(
            "npx",
            &s(&["-y", "@modelcontextprotocol/server-filesystem"])
        )
        .is_some());
        assert!(unpinned_mcp(
            "npx",
            &s(&["-y", "@modelcontextprotocol/server-filesystem@latest"])
        )
        .is_some());
        assert!(unpinned_mcp(
            "npx",
            &s(&["-y", "@modelcontextprotocol/server-filesystem@2025.1.14"])
        )
        .is_none());
        assert!(unpinned_mcp("uvx", &s(&["mcp-server-git"])).is_some());
        assert!(unpinned_mcp("uvx", &s(&["mcp-server-git==0.6.2"])).is_none());
        assert!(unpinned_mcp("docker", &s(&["run", "-i", "ghcr.io/x/y"])).is_some());
        assert!(unpinned_mcp("docker", &s(&["run", "-i", "ghcr.io/x/y:1.2"])).is_none());
        assert!(unpinned_mcp("/usr/local/bin/muninn", &s(&["serve"])).is_none());
    }

    #[test]
    fn scans_a_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        std::fs::create_dir_all(root.join(".claude/skills/deploy")).unwrap();
        std::fs::write(root.join(".claude/settings.json"), r#"{"permissions":{"allow":["Bash(*)","Read"]},"mcpServers":{"fs":{"command":"npx","args":["-y","@modelcontextprotocol/server-filesystem"]}}}"#).unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"git":{"command":"uvx","args":["mcp-server-git==0.6.2"]}}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/commands/ship.md"),
            "---\nallowed-tools: Bash, Read\n---\nship it\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/skills/deploy/SKILL.md"),
            "---\nname: deploy\nallowed-tools: [Bash(cargo test:*)]\n---\n",
        )
        .unwrap();
        let paths = ProjectPaths::from_root(root);
        let f = scan(&paths);
        let classes: Vec<&str> = f.iter().map(|x| x.class).collect();
        assert_eq!(
            classes
                .iter()
                .filter(|c| **c == "overbroad_bash_allow")
                .count(),
            1,
            "{f:?}"
        );
        assert_eq!(
            classes.iter().filter(|c| **c == "mcp_unpinned").count(),
            1,
            "{f:?}"
        );
        assert_eq!(
            classes
                .iter()
                .filter(|c| **c == "skill_preapproves_shell")
                .count(),
            1,
            "{f:?}"
        );
    }
}
