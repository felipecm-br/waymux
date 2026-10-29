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

use std::path::Path;
use std::process::{Command, Stdio};

/// Config keys that name a program git runs while it reads a work tree or makes a diff.
const DRIVER_KEYS: &str = r"^(filter\..+\.(clean|smudge|process)|diff\..+\.(textconv|command))$";

/// `git -C dir` that runs nothing the repository's own config names.
pub fn safe(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(neutralize(dir));
    cmd
}

/// The `-c` arguments [`safe`] adds, for callers that point `GIT_DIR` at the repository
/// themselves: no fsmonitor, no hooks, and every filter or diff driver defined in local or
/// worktree config disabled. Drivers from the global config, such as Git LFS, still run.
pub fn neutralize(dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
    ]
    .map(String::from)
    .to_vec();
    // Reading config runs nothing.
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
    }
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
}
