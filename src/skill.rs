//! `waymux skill`: install, check or remove the Waymux agent skill for each harness.
//!
//! The skill (`.agents/skills/waymux/SKILL.md`, embedded at build time) teaches an agent the
//! CLI it can call from its pane. Claude Code reads it from `~/.claude/skills/waymux/`; Codex,
//! opencode and Pi from `~/.agents/skills/waymux/`. Each installed copy carries a marker line,
//! so `status` can tell a copy this binary wrote (`current`, or `outdated` after an upgrade)
//! from one somebody else wrote (`foreign`: a hand-written skill, or a symlink a dotfiles
//! repository manages), which `setup` never overwrites without `--force`. Hooks are not
//! written here: `waymux init --hooks` prints them and `waymux doctor` checks them. Idea from
//! Rex's `rex skill setup | status | remove`.

use std::path::{Path, PathBuf};

const SKILL: &str = include_str!("../.agents/skills/waymux/SKILL.md");
const MARKER: &str = "<!-- installed by `waymux skill setup`; `waymux skill status` checks it -->";

const USAGE: &str = "\
usage: waymux skill <setup | status | remove | show> [--agent <claude|codex|opencode|pi|all>]
                    [--dry-run] [--force] [--json]

  setup    Write the skill for each agent (default: every agent whose CLI is installed)
  status   current | outdated | missing | foreign | unreadable, per agent; exit 1 unless all current
  remove   Delete copies this binary wrote (a foreign one is kept)
  show     Print the skill, for an agent with its own skill folder
  --dry-run  Say what setup or remove would do, change nothing
  --force    Let setup replace a foreign copy (a symlink is replaced by a file)";

pub const AGENTS: [&str; 4] = ["claude", "codex", "opencode", "pi"];

/// The skill as written to disk: the marker goes after the front matter, which must stay first.
pub fn rendered() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let marker = MARKER.replace("-->", &format!("(waymux {version}) -->"));
    match SKILL
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
    {
        Some((front, body)) => format!("---\n{front}\n---\n{marker}\n{body}"),
        None => format!("{marker}\n{SKILL}"),
    }
}

/// Where `agent` reads its skills from, under `home`.
pub fn skill_path(home: &Path, agent: &str) -> PathBuf {
    let root = if agent == "claude" {
        home.join(".claude/skills")
    } else {
        home.join(".agents/skills")
    };
    root.join("waymux/SKILL.md")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Current,
    Outdated,
    Missing,
    Foreign,
    Unreadable,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Current => "current",
            Status::Outdated => "outdated",
            Status::Missing => "missing",
            Status::Foreign => "foreign",
            Status::Unreadable => "unreadable",
        }
    }
}

/// What is at `path` compared with `want`. A symlink is foreign: someone else manages it.
pub fn status_of(path: &Path, want: &str) -> Status {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Status::Missing;
    };
    if meta.file_type().is_symlink() {
        return Status::Foreign;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Status::Unreadable;
    };
    if text == want {
        Status::Current
    } else if text.contains(MARKER.trim_end_matches(" -->")) {
        Status::Outdated
    } else {
        Status::Foreign
    }
}

struct Opts {
    action: String,
    agents: Vec<String>,
    dry_run: bool,
    force: bool,
    json: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut it = args.iter();
    let action = it.next().cloned().ok_or("missing action")?;
    if !matches!(action.as_str(), "setup" | "status" | "remove" | "show") {
        return Err(format!("unknown action '{action}'"));
    }
    let mut o = Opts {
        action,
        agents: Vec::new(),
        dry_run: false,
        force: false,
        json: false,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agent" => {
                let v = it.next().ok_or("--agent needs a name")?;
                if v == "all" {
                    o.agents = AGENTS.map(String::from).to_vec();
                } else if AGENTS.contains(&v.as_str()) {
                    o.agents.push(v.clone());
                } else {
                    return Err(format!(
                        "unknown agent '{v}' (claude, codex, opencode, pi, all)"
                    ));
                }
            }
            "--dry-run" | "-n" => o.dry_run = true,
            "--force" | "-f" => o.force = true,
            "--json" => o.json = true,
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    Ok(o)
}

/// What `setup` does for one agent, given what is there.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Write,
    Keep,
    Refuse,
}

pub fn setup_plan(status: Status, force: bool) -> Plan {
    match status {
        Status::Current => Plan::Keep,
        Status::Missing | Status::Outdated => Plan::Write,
        Status::Foreign | Status::Unreadable if force => Plan::Write,
        Status::Foreign | Status::Unreadable => Plan::Refuse,
    }
}

fn write_skill(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(path)?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Runs `args` against `home`; `installed` says whether an agent's CLI is on `PATH`.
pub fn run_in(home: &Path, args: &[String], installed: &dyn Fn(&str) -> bool) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let mut o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("waymux skill: {e} (see --help)");
            return 2;
        }
    };
    let want = rendered();
    if o.action == "show" {
        print!("{SKILL}");
        return 0;
    }
    if o.agents.is_empty() {
        o.agents = match o.action.as_str() {
            "setup" => AGENTS
                .iter()
                .filter(|a| installed(a))
                .map(|a| (*a).to_string())
                .collect(),
            _ => AGENTS.map(String::from).to_vec(),
        };
    }
    let mut code = 0;
    let mut rows = Vec::new();
    for agent in &o.agents {
        let path = skill_path(home, agent);
        let before = status_of(&path, &want);
        let (after, note) = match o.action.as_str() {
            "status" => {
                if before != Status::Current {
                    code = 1;
                }
                (before, String::new())
            }
            "setup" => match setup_plan(before, o.force) {
                Plan::Keep => (before, "already current".to_string()),
                Plan::Refuse => {
                    code = 1;
                    (
                        before,
                        "kept: not written by waymux (--force replaces it)".into(),
                    )
                }
                Plan::Write if o.dry_run => (before, "would write".into()),
                Plan::Write => match write_skill(&path, &want) {
                    Ok(()) => (Status::Current, "written".into()),
                    Err(e) => {
                        code = 1;
                        (before, format!("not written: {e}"))
                    }
                },
            },
            _ => match before {
                Status::Current | Status::Outdated if o.dry_run => (before, "would remove".into()),
                Status::Current | Status::Outdated => {
                    let removed = std::fs::remove_file(&path);
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::remove_dir(dir);
                    }
                    match removed {
                        Ok(()) => (Status::Missing, "removed".into()),
                        Err(e) => {
                            code = 1;
                            (before, format!("not removed: {e}"))
                        }
                    }
                }
                Status::Missing => (before, String::new()),
                _ => (before, "kept: not written by waymux".into()),
            },
        };
        rows.push((agent.clone(), path, after, note));
    }
    if o.json {
        let out: Vec<_> = rows
            .iter()
            .map(|(agent, path, st, note)| {
                serde_json::json!({
                    "agent": agent,
                    "path": path.display().to_string(),
                    "status": st.word(),
                    "note": note,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(out));
    } else {
        for (agent, path, st, note) in &rows {
            let note = if note.is_empty() {
                String::new()
            } else {
                format!("  ({note})")
            };
            println!("{agent:<9} {:<10} {}{note}", st.word(), path.display());
        }
        if o.action == "setup" && !o.dry_run && rows.iter().any(|r| r.0 == "claude") {
            println!("hooks: `waymux init --hooks` prints them; `waymux doctor` checks them");
        }
    }
    code
}

pub fn run_cli(args: &[String]) -> i32 {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("waymux skill: HOME is not set");
        return 2;
    };
    run_in(&home, args, &|a| crate::sys::find_in_path(a).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    fn home() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "waymux-skill-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_marker_follows_the_front_matter() {
        let r = rendered();
        assert!(r.starts_with("---\nname: waymux\n"));
        let after_front = r.splitn(3, "---\n").nth(2).unwrap();
        assert!(after_front.starts_with("<!-- installed by `waymux skill setup`"));
    }

    #[test]
    fn setup_status_and_remove_round_trip_and_keep_foreign_copies() {
        let h = home();
        let only_claude = |a: &str| a == "claude";
        assert_eq!(
            run_in(&h, &s(&["status", "--agent", "claude"]), &only_claude),
            1
        );

        assert_eq!(run_in(&h, &s(&["setup", "--dry-run"]), &only_claude), 0);
        assert_eq!(
            status_of(&skill_path(&h, "claude"), &rendered()),
            Status::Missing
        );

        assert_eq!(run_in(&h, &s(&["setup"]), &only_claude), 0);
        assert_eq!(
            status_of(&skill_path(&h, "claude"), &rendered()),
            Status::Current
        );
        assert_eq!(
            status_of(&skill_path(&h, "codex"), &rendered()),
            Status::Missing
        );
        assert_eq!(
            run_in(&h, &s(&["status", "--agent", "claude"]), &only_claude),
            0
        );

        // An older copy this binary wrote is outdated and setup refreshes it.
        let path = skill_path(&h, "claude");
        std::fs::write(&path, rendered().replace("Waymux", "Old")).unwrap();
        assert_eq!(status_of(&path, &rendered()), Status::Outdated);
        assert_eq!(
            run_in(&h, &s(&["setup", "--agent", "claude"]), &only_claude),
            0
        );
        assert_eq!(status_of(&path, &rendered()), Status::Current);

        // A hand-written skill for codex is foreign: kept without --force, replaced with it.
        let codex = skill_path(&h, "codex");
        std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
        std::fs::write(&codex, "my own notes\n").unwrap();
        assert_eq!(
            run_in(&h, &s(&["setup", "--agent", "codex"]), &only_claude),
            1
        );
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), "my own notes\n");
        assert_eq!(
            run_in(&h, &s(&["remove", "--agent", "codex"]), &only_claude),
            0
        );
        assert!(codex.exists());
        assert_eq!(
            run_in(
                &h,
                &s(&["setup", "--agent", "codex", "--force"]),
                &only_claude
            ),
            0
        );
        assert_eq!(status_of(&codex, &rendered()), Status::Current);

        assert_eq!(
            run_in(&h, &s(&["remove", "--agent", "all"]), &only_claude),
            0
        );
        assert_eq!(status_of(&path, &rendered()), Status::Missing);
        assert_eq!(status_of(&codex, &rendered()), Status::Missing);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_symlinked_skill_is_foreign() {
        let h = home();
        let path = skill_path(&h, "claude");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = h.join("managed.md");
        std::fs::write(&target, rendered()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(status_of(&path, &rendered()), Status::Foreign);
        assert_eq!(setup_plan(Status::Foreign, false), Plan::Refuse);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn bad_arguments_exit_2() {
        let h = home();
        let none = |_: &str| false;
        assert_eq!(run_in(&h, &s(&[]), &none), 2);
        assert_eq!(run_in(&h, &s(&["install"]), &none), 2);
        assert_eq!(run_in(&h, &s(&["setup", "--agent", "vim"]), &none), 2);
        let _ = std::fs::remove_dir_all(&h);
    }
}
