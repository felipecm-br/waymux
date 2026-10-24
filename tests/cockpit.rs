//! End-to-end tests of the Cockpit render commands (`cockpit-diff`, `cockpit-preview`,
//! `cockpit-help`, `review-notes`) against real git repositories. `PATH` holds only `git`,
//! so diffs print as plain text (no `delta`/`bat`) and tmux is never reached.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_waymux");

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "waymux-cockpit-it-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["bin", "home", "run", "tmux", "repo"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let git = which("git");
        std::os::unix::fs::symlink(git, dir.join("bin/git")).unwrap();
        Env { dir }
    }

    fn repo(&self) -> PathBuf {
        self.dir.join("repo")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env_clear()
            .env("PATH", self.dir.join("bin"))
            .env("HOME", self.dir.join("home"))
            .env("XDG_RUNTIME_DIR", self.dir.join("run"))
            .env("TMUX_TMPDIR", self.dir.join("tmux"))
            .env("COLUMNS", "100");
        // Keep `cargo llvm-cov` counting the spawned binary.
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> String {
        let out = self.command(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        strip_ansi(&String::from_utf8_lossy(&out.stdout))
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.repo())
            .env("HOME", self.dir.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.repo().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A repository on `main` with one commit of `a.txt`.
    fn init_repo(&self) {
        self.git(&["init", "-q", "-b", "main"]);
        self.write("a.txt", "one\n");
        self.git(&["add", "."]);
        self.git(&["commit", "-q", "-m", "first"]);
    }

    fn add_review_note(&self, comment: &str) {
        let notes = format!(
            r#"{{"notes":[{{"file":"a.txt","line":1,"comment":"{comment}","status":"New"}},{{"file":"a.txt","line":2,"comment":"done","status":"Sent"}}]}}"#
        );
        std::fs::write(self.repo().join(".git/info/lines.json"), notes).unwrap();
    }

    fn diff(&self, path: &Path, agent: &str) -> String {
        let path = path.to_string_lossy();
        self.run(&["cockpit-diff", "s", "-", "-", &path, "-", agent, "-", "-"])
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not on PATH"))
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn diff_outside_a_repository_says_so() {
    let env = Env::new();
    let out = env.diff(&env.dir.join("home"), "-");
    assert!(out.contains("Not a Git Repository"), "{out}");
}

#[test]
fn diff_of_a_dirty_tree_shows_changes_untracked_files_and_notes() {
    let env = Env::new();
    env.init_repo();
    env.write("a.txt", "one\nchanged\n");
    for i in 0..8 {
        env.write(&format!("new{i}.txt"), "x\n");
    }
    env.add_review_note("check this line");

    let out = env.diff(&env.repo(), "claude");
    assert!(out.contains("Agent:  claude"), "{out}");
    assert!(out.contains("Working Tree (Staged & Unstaged)"), "{out}");
    assert!(out.contains("Branch: main"), "{out}");
    assert!(out.contains("1 file changed"), "{out}");
    assert!(out.contains("8 untracked"), "{out}");
    assert!(out.contains("Untracked Files (8)"), "{out}");
    assert!(out.contains("and 2 more untracked files"), "{out}");
    assert!(out.contains("+changed"), "{out}");
    // Only the note still marked New is pending.
    assert!(out.contains("Pending AI Review Notes (1)"), "{out}");
    assert!(out.contains("a.txt:1"), "{out}");
}

#[test]
fn diff_of_only_untracked_files_lists_them_without_a_patch() {
    let env = Env::new();
    env.init_repo();
    env.write("fresh.txt", "x\n");
    let out = env.diff(&env.repo(), "-");
    assert!(out.contains("1 untracked file(s)"), "{out}");
    assert!(
        out.contains("All changes are newly created untracked files"),
        "{out}"
    );
}

#[test]
fn diff_of_a_clean_main_shows_the_latest_commit() {
    let env = Env::new();
    env.init_repo();
    let out = env.diff(&env.repo(), "-");
    assert!(out.contains("Latest Commit ("), "{out}");
    assert!(out.contains("first"), "{out}");
    assert!(out.contains("+one"), "{out}");
}

#[test]
fn diff_of_a_feature_branch_shows_commits_ahead_of_main() {
    let env = Env::new();
    env.init_repo();
    env.git(&["switch", "-q", "-c", "feature"]);
    env.write("b.txt", "two\n");
    env.git(&["add", "."]);
    env.git(&["commit", "-q", "-m", "second"]);
    env.write("c.txt", "three\n");
    env.git(&["add", "."]);
    env.git(&["commit", "-q", "-m", "third"]);

    let out = env.diff(&env.repo(), "-");
    assert!(out.contains("Branch Commits (2 ahead of main)"), "{out}");
    assert!(out.contains("Branch: feature"), "{out}");
    assert!(out.contains("+two") && out.contains("+three"), "{out}");
}

#[test]
fn review_notes_count_and_list_pending_notes_only() {
    let env = Env::new();
    env.init_repo();
    let repo = env.repo();
    let repo = repo.to_string_lossy();
    assert_eq!(env.run(&["review-notes", "count", &repo]).trim(), "0");

    // A JSON-escaped newline, which the list must flatten to a space.
    env.add_review_note(r"multi\nline comment");
    assert_eq!(env.run(&["review-notes", "count", &repo]).trim(), "1");
    let list = env.run(&["review-notes", "list", &repo]);
    assert!(list.contains("a.txt:1"), "{list}");
    assert!(list.contains("\"multi line comment\""), "{list}");
    assert!(!list.contains("done"), "sent notes are not pending: {list}");
}

#[test]
fn review_notes_send_without_notes_exits_cleanly() {
    let env = Env::new();
    env.init_repo();
    let repo = env.repo();
    let out = env
        .command(&[
            "review-notes",
            "send",
            &repo.to_string_lossy(),
            "s",
            "1",
            "%1",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn preview_without_a_window_or_pane_output_says_so() {
    let env = Env::new();
    let out = env.run(&["cockpit-preview", "", ""]);
    assert!(out.contains("(no active window selected)"), "{out}");

    env.init_repo();
    env.add_review_note("look here");
    let repo = env.repo();
    let out = env.run(&["cockpit-preview", "s", "1", "%1", &repo.to_string_lossy()]);
    assert!(out.contains("(pane output empty or detached)"), "{out}");
    assert!(out.contains("Pending AI Review Notes (1)"), "{out}");
}

#[test]
fn help_lists_the_keybindings() {
    let env = Env::new();
    let out = env.run(&["cockpit-help"]);
    assert!(out.contains("Keybindings & Shortcuts"), "{out}");
    assert!(out.contains("Worktree Lifecycle"), "{out}");
}
