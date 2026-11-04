//! git in repositories an agent can write to.
//!
//! A repository's own config can name programs that git runs while it reads the work tree:
//! `core.fsmonitor`, hooks, and filter and diff drivers. An agent that can write the repository
//! (a jailed one included, since `ai-jail` mounts the project read-write, `.git` and the `.git`
//! pointer file of a linked worktree with it) could then have the daemon or the Cockpit run a
//! program outside the agent's sandbox. TUIOS fixed the same problem as GHSA-qcx6-vr32-xr8q.
//!
//! Everything waymux runs on its own (Cockpit rows and previews, review snapshots, turn
//! checkpoints) goes through [`safe`]. Commands you run yourself, such as `waymux wt`, keep the
//! repository's config and hooks, as plain git would.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Config keys that name a program git runs while it reads a work tree or makes a diff.
const DRIVER_KEYS: &str = r"^(filter\..+\.(clean|smudge|process)|diff\..+\.(textconv|command))$";

/// `git -C dir` that runs nothing the repository's own config names.
pub fn safe(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(neutralize(dir));
    cmd
}

/// Resolves `(common_dir, git_dir)` for `dir` without spawning git.
///
/// In a normal checkout both paths are `<root>/.git`. In a linked worktree `.git` is a
/// `gitdir: <path>` pointer file and `<git_dir>/commondir` points back to the shared `.git`.
pub fn git_dirs(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let git_dir = dir
        .ancestors()
        .find_map(|anc| {
            let dot_git = anc.join(".git");
            if dot_git.is_dir() {
                return Some(dot_git);
            }
            let content = fs::read_to_string(&dot_git).ok()?;
            let target = content
                .lines()
                .find_map(|l| l.strip_prefix("gitdir:"))?
                .trim();
            let target = Path::new(target);
            Some(if target.is_absolute() {
                target.to_path_buf()
            } else {
                anc.join(target)
            })
        })
        .or_else(|| {
            (dir.join("HEAD").is_file() && dir.join("config").is_file()).then(|| dir.to_path_buf())
        })?;
    let common_dir = match fs::read_to_string(git_dir.join("commondir")) {
        Ok(raw) => {
            let rel = Path::new(raw.trim());
            if rel.is_absolute() {
                rel.to_path_buf()
            } else {
                git_dir.join(rel)
            }
        }
        Err(_) => git_dir.clone(),
    };
    Some((common_dir, git_dir))
}

fn split_section_header(s: &str) -> Option<(&str, &str)> {
    let mut in_quotes = false;
    let mut escaped = false;
    for (idx, c) in s.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_quotes = false;
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ']' {
            return Some((&s[..idx], &s[idx + 1..]));
        }
    }
    None
}

fn parse_section_header(header: &str) -> Option<(String, Option<String>)> {
    let header = header.trim();
    if header.is_empty() {
        return None;
    }
    if let Some((sec_part, quote_part)) = header.split_once('"') {
        let sec = sec_part.trim().to_ascii_lowercase();
        let inner = quote_part.strip_suffix('"')?;
        let mut sub = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                sub.push(chars.next()?);
            } else {
                sub.push(c);
            }
        }
        return Some((sec, Some(sub)));
    }
    if let Some((sec_part, sub_part)) = header.split_once('.') {
        return Some((
            sec_part.trim().to_ascii_lowercase(),
            Some(sub_part.trim().to_ascii_lowercase()),
        ));
    }
    Some((header.to_ascii_lowercase(), None))
}

/// Parses `filter.<name>.(clean|smudge|process)` and `diff.<name>.(textconv|command)` keys from
/// a git config file. Returns `Err(())` when an `[include]` / `[includeIf]` directive or a
/// malformed section header is present so the caller can fall back to `git config`.
fn parse_config_driver_keys(text: &str) -> Result<Vec<String>, ()> {
    let mut joined = String::with_capacity(text.len());
    let mut continuing = false;
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if !continuing {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
        }
        let backslashes = line.chars().rev().take_while(|&c| c == '\\').count();
        if backslashes % 2 == 1 {
            joined.push_str(&line[..line.len() - 1]);
            continuing = true;
        } else {
            joined.push_str(line);
            joined.push('\n');
            continuing = false;
        }
    }
    if continuing {
        return Err(());
    }

    let mut keys = Vec::new();
    let mut current_section: Option<(String, String)> = None;
    for line in joined.lines() {
        let mut rest = line.trim_start();
        while !rest.is_empty() {
            if rest.starts_with('#') || rest.starts_with(';') {
                break;
            }
            if let Some(after_open) = rest.strip_prefix('[') {
                let (header, after_close) = split_section_header(after_open).ok_or(())?;
                let (sec, subsec) = parse_section_header(header).ok_or(())?;
                if sec == "include" || sec.starts_with("includeif") {
                    return Err(());
                }
                current_section = match (sec.as_str(), subsec) {
                    ("filter" | "diff", Some(sub)) if !sub.is_empty() => Some((sec, sub)),
                    _ => None,
                };
                rest = after_close.trim_start();
                continue;
            }
            if let Some((sec, subsec)) = &current_section {
                let name_len = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .unwrap_or(rest.len());
                if name_len > 0 {
                    let var = rest[..name_len].to_ascii_lowercase();
                    let is_driver = (sec == "filter"
                        && matches!(var.as_str(), "clean" | "smudge" | "process"))
                        || (sec == "diff" && matches!(var.as_str(), "textconv" | "command"));
                    if is_driver {
                        let key = format!("{sec}.{subsec}.{var}");
                        if !keys.contains(&key) {
                            keys.push(key);
                        }
                    }
                }
            }
            break;
        }
    }
    Ok(keys)
}

fn read_optional_config(path: &Path) -> Result<String, ()> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(_) => Err(()),
    }
}

fn native_driver_keys(dir: &Path) -> Result<Vec<String>, ()> {
    let Some((common_dir, git_dir)) = git_dirs(dir) else {
        return Ok(Vec::new());
    };
    let local_cfg = read_optional_config(&common_dir.join("config"))?;
    let wt_cfg = read_optional_config(&git_dir.join("config.worktree"))?;
    let mut keys = parse_config_driver_keys(&local_cfg)?;
    for k in parse_config_driver_keys(&wt_cfg)? {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    Ok(keys)
}

fn subprocess_driver_keys(dir: &Path) -> Vec<String> {
    let mut keys = Vec::new();
    let listed = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "config",
            "--show-scope",
            "--name-only",
            "--get-regexp",
            DRIVER_KEYS,
        ])
        .stderr(Stdio::null())
        .output();
    if let Ok(out) = listed {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(("local" | "worktree", key)) = line.split_once('\t') {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

fn push_driver_overrides(args: &mut Vec<String>, keys: &[String]) {
    for key in keys {
        // An empty filter is no filter; an empty textconv fails the whole diff, so it
        // becomes `cat`. An empty external diff command fails too, which is the point.
        let value = if key.ends_with(".textconv") {
            "cat"
        } else {
            ""
        };
        args.push("-c".into());
        args.push(format!("{key}={value}"));
    }
}

/// The `-c` arguments [`safe`] adds, for callers that point `GIT_DIR` at the repository
/// themselves: no fsmonitor, no hooks, and every filter or diff driver defined in local or
/// worktree config disabled. Drivers from the global config, such as Git LFS, still run.
///
/// Reads `.git/config` and `config.worktree` directly and falls back to `git config` only when
/// an `[include]` / `[includeIf]` section is present.
pub fn neutralize(dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
    ]
    .map(String::from)
    .to_vec();
    let keys = native_driver_keys(dir).unwrap_or_else(|()| subprocess_driver_keys(dir));
    push_driver_overrides(&mut args, &keys);
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A repository whose local config names a program for every hook git has, each of which
    /// leaves a marker file when it runs.
    struct Planted {
        dir: PathBuf,
    }

    impl Planted {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "waymux-git-{tag}-{}-{}",
                std::process::id(),
                crate::sys::unix_ms()
            ));
            let repo = dir.join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            let git = |args: &[&str]| {
                assert!(
                    Command::new("git")
                        .arg("-C")
                        .arg(&repo)
                        .args(args)
                        .stdout(Stdio::null())
                        .status()
                        .unwrap()
                        .success()
                );
            };
            git(&["init", "-q"]);
            std::fs::write(repo.join("f.txt"), "a\n").unwrap();
            git(&["add", "f.txt"]);
            git(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "init",
            ]);
            std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
            std::fs::write(repo.join(".gitattributes"), "*.txt filter=x diff=x\n").unwrap();
            let mark =
                |name: &str| format!("sh -c 'touch {}/{name}; cat \"$@\"' --", dir.display());
            git(&[
                "config",
                "core.fsmonitor",
                &format!("touch {}/fsmonitor; false", dir.display()),
            ]);
            git(&["config", "filter.x.clean", &mark("clean")]);
            git(&["config", "diff.x.textconv", &mark("textconv")]);
            Self { dir }
        }

        fn repo(&self) -> PathBuf {
            self.dir.join("repo")
        }

        fn ran(&self) -> Vec<String> {
            ["fsmonitor", "clean", "textconv"]
                .into_iter()
                .filter(|m| self.dir.join(m).exists())
                .map(String::from)
                .collect()
        }
    }

    impl Drop for Planted {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn test_plain_git_runs_what_the_repository_names() {
        // The premise: without `safe`, a status and a diff run the planted programs.
        let p = Planted::new("plain");
        let _ = Command::new("git")
            .arg("-C")
            .arg(p.repo())
            .args(["diff", "HEAD"])
            .output();
        assert_ne!(p.ran(), Vec::<String>::new());
    }

    #[test]
    fn test_safe_git_runs_nothing_the_repository_names() {
        let p = Planted::new("safe");
        for args in [
            &["--no-optional-locks", "status", "--porcelain", "-b"][..],
            &[
                "--no-optional-locks",
                "diff",
                "--no-ext-diff",
                "--shortstat",
                "HEAD",
            ],
            &["diff", "HEAD"],
            &["add", "-A", "--", "."],
        ] {
            let out = safe(&p.repo()).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        }
        assert_eq!(p.ran(), Vec::<String>::new());
    }

    #[test]
    fn test_global_drivers_are_kept() {
        let p = Planted::new("scope");
        let args = neutralize(&p.repo());
        assert!(args.contains(&"filter.x.clean=".to_string()));
        assert!(args.contains(&"diff.x.textconv=cat".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("filter.lfs.")));
    }

    #[test]
    fn test_parse_config_driver_keys_and_include_fallback() {
        let cfg = "\
# comment
; semicolon comment
[core]
\trepositoryformatversion = 0
[Filter \"a\\\"b\"]
\tClean = evil
\tsmudge = \\\n\tevil2
\tother = ignored
[diff.legacy] command = run
[diff \"x\"]
\ttextconv = tc
";
        assert_eq!(
            parse_config_driver_keys(cfg).unwrap(),
            vec![
                "filter.a\"b.clean",
                "filter.a\"b.smudge",
                "diff.legacy.command",
                "diff.x.textconv",
            ]
        );
        assert!(parse_config_driver_keys("[include]\n\tpath = extra.cfg\n").is_err());
        assert!(parse_config_driver_keys("[includeIf \"gitdir:~/w/\"]\n\tpath = x\n").is_err());
        assert!(parse_config_driver_keys("[unclosed\n").is_err());

        // [include] in .git/config falls back to `git config` and neutralizes the included driver.
        let p = Planted::new("include");
        let extra = p.dir.join("included.cfg");
        fs::write(&extra, "[filter \"inc\"]\n\tprocess = evil\n").unwrap();
        let cfg_path = p.repo().join(".git/config");
        let raw = fs::read_to_string(&cfg_path).unwrap();
        fs::write(
            &cfg_path,
            format!("{raw}\n[include]\n\tpath = {}\n", extra.display()),
        )
        .unwrap();
        assert!(native_driver_keys(&p.repo()).is_err());
        let args = neutralize(&p.repo());
        assert!(args.contains(&"filter.inc.process=".to_string()));
    }
}
