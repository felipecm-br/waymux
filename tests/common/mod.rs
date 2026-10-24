//! Shared harness for the end-to-end tests: a temp directory with its own `HOME`, runtime
//! dir and tmux dir, a `PATH` holding only `git` (no `delta`/`bat`, no tmux), and helpers to
//! build git repositories.
#![allow(dead_code)] // each test crate uses a different subset

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const BIN: &str = env!("CARGO_BIN_EXE_waymux");

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

pub struct Env {
    pub dir: PathBuf,
    repo: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        Self::with_repo_at("repo")
    }

    /// `rel` is the repository's path under the temp dir, e.g. `proj/main` so that the
    /// repository's parent folder (which `wt` names sessions after) is `proj`.
    pub fn with_repo_at(rel: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "waymux-e2e-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["bin", "home", "run", "tmux", rel] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        // The resolved path, as git and current_dir report it (macOS: /var -> /private/var).
        let dir = dir.canonicalize().unwrap();
        let git = which("git");
        std::os::unix::fs::symlink(git, dir.join("bin/git")).unwrap();
        // An identity for commits that waymux itself makes (`wt -W` stashes).
        std::fs::write(
            dir.join("home/.gitconfig"),
            "[user]\n\tname = t\n\temail = t@example.com\n",
        )
        .unwrap();
        let repo = dir.join(rel);
        Env { dir, repo }
    }

    pub fn repo(&self) -> PathBuf {
        self.repo.clone()
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env_clear()
            .env("PATH", self.dir.join("bin"))
            .env("HOME", self.dir.join("home"))
            .env("XDG_RUNTIME_DIR", self.dir.join("run"))
            .env("TMUX_TMPDIR", self.dir.join("tmux"))
            .env("COLUMNS", "100")
            // Never the developer's daemon on 4040: a port nothing listens on.
            .env("WAYMUX_PORT", unused_port().to_string());
        // Keep `cargo llvm-cov` counting the spawned binary.
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    }

    pub fn run(&self, args: &[&str]) -> String {
        let out = self.command(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        strip_ansi(&String::from_utf8_lossy(&out.stdout))
    }

    pub fn git(&self, args: &[&str]) {
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

    pub fn write(&self, rel: &str, content: &str) {
        let path = self.repo().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A repository on `main` with one commit of `a.txt`.
    pub fn init_repo(&self) {
        self.git(&["init", "-q", "-b", "main"]);
        self.write("a.txt", "one\n");
        self.git(&["add", "."]);
        self.git(&["commit", "-q", "-m", "first"]);
    }

    pub fn add_review_note(&self, comment: &str) {
        let notes = format!(
            r#"{{"notes":[{{"file":"a.txt","line":1,"comment":"{comment}","status":"New"}},{{"file":"a.txt","line":2,"comment":"done","status":"Sent"}}]}}"#
        );
        std::fs::write(self.repo().join(".git/info/lines.json"), notes).unwrap();
    }

    pub fn diff(&self, path: &Path, agent: &str) -> String {
        let path = path.to_string_lossy();
        self.run(&["cockpit-diff", "s", "-", "-", &path, "-", agent, "-", "-"])
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap()
}

/// A private tmux server for one test, under the test's own `TMUX_TMPDIR`. Started only when
/// tmux is installed; killed on drop.
pub struct Tmux<'a> {
    env: &'a Env,
}

impl Env {
    /// Puts tmux on the test `PATH` and starts a server with one detached session `main`
    /// whose first window runs `cmd`. None when tmux is not installed.
    pub fn tmux(&self, cmd: &str) -> Option<Tmux<'_>> {
        let tmux = std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join("tmux"))
            .find(|p| p.is_file())?;
        let link = self.dir.join("bin/tmux");
        if !link.exists() {
            std::os::unix::fs::symlink(tmux, link).unwrap();
        }
        let t = Tmux { env: self };
        let out = t.raw(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "main",
            "-x",
            "120",
            "-y",
            "30",
            cmd,
        ]);
        assert!(out.status.success(), "tmux new-session: {out:?}");
        Some(t)
    }
}

impl Tmux<'_> {
    fn raw(&self, args: &[&str]) -> std::process::Output {
        Command::new(self.env.dir.join("bin/tmux"))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.env.dir.join("bin").display()),
            )
            .env("HOME", self.env.dir.join("home"))
            .env("TMUX_TMPDIR", self.env.dir.join("tmux"))
            .env("TERM", "xterm-256color")
            .output()
            .unwrap()
    }

    /// Runs a tmux command and returns its trimmed stdout.
    pub fn run(&self, args: &[&str]) -> String {
        let out = self.raw(args);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn window_count(&self, session: &str) -> usize {
        self.run(&["list-windows", "-t", session]).lines().count()
    }
}

impl Drop for Tmux<'_> {
    fn drop(&mut self) {
        let _ = self.raw(&["kill-server"]);
    }
}

/// Polls `f` for up to two seconds.
pub fn eventually(f: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    false
}

pub fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not on PATH"))
}

pub fn strip_ansi(s: &str) -> String {
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
