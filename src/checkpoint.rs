//! Turn checkpoints: at the end of each agent turn the daemon saves the pane's work tree, so
//! you can read what one turn changed and put the files back as an earlier turn left them.
//!
//! Checkpoints never touch your repository. They live in a bare store per repository,
//! `$XDG_STATE_HOME/waymux/checkpoints/<name>-<hash>.git`, that reads your objects through
//! `objects/info/alternates` and writes only its own: no refs in your repository (so
//! `git log --all` and lazygitrs's graph stay as they are), no loose objects, and your index,
//! `HEAD`, branch and stash are left alone. The snapshot is built in a temporary index.
//!
//! A checkpoint holds tracked files as they are on disk and untracked files git does not
//! ignore (an untracked file over [`MAX_UNTRACKED_BYTES`] is left out). Its parent is `HEAD`
//! at the time. A turn that changed nothing since the pane's last checkpoint gets none. Each
//! pane keeps the newest `keep` (default 50).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const MAX_UNTRACKED_BYTES: u64 = 50 * 1024 * 1024;
pub const DEFAULT_KEEP: usize = 50;

pub struct Repo {
    pub top: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    index: PathBuf,
    pub store: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub n: u64,
    pub commit: String,
    pub time: u64,
    pub subject: String,
}

fn state_root() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .map(|s| s.join("waymux").join("checkpoints"))
}

/// FNV-1a: a stable short id for a repository path (no crypto needed).
fn path_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn output(cmd: &mut Command) -> Option<String> {
    let out = cmd.stderr(Stdio::null()).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn ok(cmd: &mut Command) -> bool {
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Ref segment for a pane: `%12` → `12`.
pub fn pane_key(pane: &str) -> String {
    pane.chars().filter(char::is_ascii_digit).collect()
}

impl Repo {
    /// The repository holding `dir`, with its checkpoint store under `root` (default
    /// `$XDG_STATE_HOME/waymux/checkpoints`).
    pub fn open(dir: &Path, root: Option<&Path>) -> Option<Self> {
        let out = output(Command::new("git").arg("-C").arg(dir).args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
            "--git-path",
            "index",
        ]))?;
        let mut lines = out.lines().map(PathBuf::from);
        let (top, git_dir, common_dir, index) =
            (lines.next()?, lines.next()?, lines.next()?, lines.next()?);
        let root = root.map(Path::to_path_buf).or_else(state_root)?;
        let name = top
            .file_name()
            .map_or_else(|| "repo".into(), |n| n.to_string_lossy().to_string());
        let store = root.join(format!("{name}-{}.git", path_hash(&top.to_string_lossy())));
        Some(Self {
            top,
            git_dir,
            common_dir,
            index,
            store,
        })
    }

    fn ensure_store(&self) -> bool {
        let alternates = self.store.join("objects/info/alternates");
        if !alternates.exists() {
            if let Some(parent) = self.store.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if !ok(Command::new("git")
                .args(["init", "--bare", "-q"])
                .arg(&self.store))
            {
                return false;
            }
            let objects = self.common_dir.join("objects");
            if std::fs::write(&alternates, format!("{}\n", objects.display())).is_err() {
                return false;
            }
        }
        true
    }

    /// git against your repository, writing new objects to the store only.
    fn main_git(&self, index: &Path) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(crate::git::neutralize(&self.top))
            .current_dir(&self.top)
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.top)
            .env("GIT_INDEX_FILE", index)
            .env("GIT_OBJECT_DIRECTORY", self.store.join("objects"))
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                self.common_dir.join("objects"),
            )
            .env("GIT_AUTHOR_NAME", "waymux")
            .env("GIT_AUTHOR_EMAIL", "waymux@localhost")
            .env("GIT_COMMITTER_NAME", "waymux")
            .env("GIT_COMMITTER_EMAIL", "waymux@localhost");
        cmd
    }

    /// git against the store. Your repository's config is left out on purpose: filters it
    /// defines would run outside the agent's sandbox (see [`crate::git`]), so checkpoints hold
    /// files as they are on disk. Global filters, such as Git LFS, still apply.
    fn store_git(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(["-c", "core.hooksPath=/dev/null", "--git-dir"])
            .arg(&self.store);
        cmd
    }

    fn temp_index(&self) -> PathBuf {
        self.store.join(format!(
            "index-{}-{}",
            std::process::id(),
            crate::sys::unix_ms()
        ))
    }

    /// The work tree as a tree object in the store.
    fn snapshot(&self) -> Option<String> {
        let tmp = self.temp_index();
        if self.index.exists() {
            std::fs::copy(&self.index, &tmp).ok()?;
            // Keep the index's mtime: git re-reads a file whose stat matches its entry only when
            // the entry is as new as the index ("racy git"). A fresh mtime on the copy hid a
            // same-size edit made in the second the index was written, and the snapshot kept
            // the old contents.
            let mtime = std::fs::metadata(&self.index)
                .and_then(|m| m.modified())
                .ok()?;
            std::fs::File::options()
                .write(true)
                .open(&tmp)
                .and_then(|f| f.set_modified(mtime))
                .ok()?;
        }
        let tree = (|| {
            let untracked =
                output(
                    self.main_git(&tmp)
                        .args(["ls-files", "-o", "--exclude-standard", "-z"]),
                )?;
            let mut add = self.main_git(&tmp);
            add.args(["add", "-A", "--", "."]);
            for path in untracked.split('\0').filter(|p| !p.is_empty()) {
                let big = std::fs::metadata(self.top.join(path))
                    .is_ok_and(|m| m.len() > MAX_UNTRACKED_BYTES);
                if big {
                    add.arg(format!(":(top,exclude,literal){path}"));
                }
            }
            if !ok(&mut add) {
                return None;
            }
            output(self.main_git(&tmp).arg("write-tree")).map(|t| t.trim().to_string())
        })();
        let _ = std::fs::remove_file(&tmp);
        tree
    }

    pub fn list(&self, pane: &str) -> Vec<Checkpoint> {
        if !self.store.exists() {
            return Vec::new();
        }
        let prefix = format!("refs/checkpoints/{}/", pane_key(pane));
        let Some(out) = output(self.store_git().args([
            "for-each-ref",
            "--format=%(refname)%09%(objectname)%09%(committerdate:unix)%09%(subject)",
            &prefix,
        ])) else {
            return Vec::new();
        };
        let mut list: Vec<Checkpoint> = out
            .lines()
            .filter_map(|l| {
                let mut f = l.splitn(4, '\t');
                let n = f.next()?.strip_prefix(&prefix)?.parse().ok()?;
                Some(Checkpoint {
                    n,
                    commit: f.next()?.to_string(),
                    time: f.next()?.parse().unwrap_or(0),
                    subject: f.next().unwrap_or("").to_string(),
                })
            })
            .collect();
        list.sort_by_key(|c| c.n);
        list
    }

    fn tree_of(&self, commit: &str) -> Option<String> {
        output(
            self.store_git()
                .args(["rev-parse", &format!("{commit}^{{tree}}")]),
        )
        .map(|t| t.trim().to_string())
    }

    /// Saves the work tree for `pane`. `Ok(None)` when nothing changed since its last one.
    pub fn create(
        &self,
        pane: &str,
        label: &str,
        keep: usize,
    ) -> Result<Option<Checkpoint>, String> {
        if !self.ensure_store() {
            return Err("cannot create the checkpoint store".into());
        }
        let tree = self.snapshot().ok_or("cannot snapshot the work tree")?;
        let existing = self.list(pane);
        if let Some(last) = existing.last()
            && self.tree_of(&last.commit).as_deref() == Some(tree.as_str())
        {
            return Ok(None);
        }
        let n = existing.last().map_or(1, |c| c.n + 1);
        let subject = format!("turn {n} · {label}");
        let tmp = self.temp_index();
        let head = output(
            self.main_git(&tmp)
                .args(["rev-parse", "-q", "--verify", "HEAD"]),
        )
        .map(|h| h.trim().to_string());
        let mut commit = self.main_git(&tmp);
        commit.args(["commit-tree", "--no-gpg-sign", &tree, "-m", &subject]);
        if let Some(h) = &head {
            commit.args(["-p", h]);
        }
        let sha = output(&mut commit)
            .map(|s| s.trim().to_string())
            .ok_or("cannot write the checkpoint commit")?;
        let refname = format!("refs/checkpoints/{}/{n}", pane_key(pane));
        if !ok(self.store_git().args(["update-ref", &refname, &sha])) {
            return Err("cannot record the checkpoint".into());
        }
        self.prune(pane, keep);
        Ok(Some(Checkpoint {
            n,
            commit: sha,
            time: crate::sys::unix_ms() / 1000,
            subject,
        }))
    }

    fn prune(&self, pane: &str, keep: usize) {
        let list = self.list(pane);
        if list.len() <= keep {
            return;
        }
        for old in &list[..list.len() - keep] {
            let refname = format!("refs/checkpoints/{}/{}", pane_key(pane), old.n);
            let _ = ok(self.store_git().args(["update-ref", "-d", &refname]));
        }
        let _ = ok(self.store_git().args(["gc", "--auto", "--quiet"]));
    }

    fn find(&self, pane: &str, n: u64) -> Option<Checkpoint> {
        self.list(pane).into_iter().find(|c| c.n == n)
    }

    /// What checkpoint `n` changed: against the pane's checkpoint before it, else `HEAD` at
    /// the time it was taken.
    pub fn diff(&self, pane: &str, n: u64, color: bool) -> Result<String, String> {
        let list = self.list(pane);
        let pos = list
            .iter()
            .position(|c| c.n == n)
            .ok_or_else(|| format!("no checkpoint {n}"))?;
        let target = &list[pos].commit;
        let base = if pos > 0 {
            list[pos - 1].commit.clone()
        } else {
            format!("{target}^")
        };
        let mut cmd = self.store_git();
        cmd.args([
            "diff",
            if color {
                "--color=always"
            } else {
                "--no-color"
            },
            "--stat",
            "-p",
            &base,
            target,
        ]);
        let out = output(&mut cmd).or_else(|| {
            // A first checkpoint in a repository with no commit yet: diff from nothing.
            output(
                self.store_git()
                    .args(["show", "--format=", "--stat", "-p", target]),
            )
        });
        out.ok_or_else(|| "cannot diff the checkpoint".into())
    }

    /// Puts the files back as checkpoint `n` left them, after saving the work tree as a safety
    /// checkpoint. Returns `(written, removed, safety)`. Ignored files, the index and `HEAD`
    /// are not touched; `git status` then shows the restored files as changes.
    pub fn restore(&self, pane: &str, n: u64, keep: usize) -> Result<(usize, usize, u64), String> {
        let target = self
            .find(pane, n)
            .ok_or_else(|| format!("no checkpoint {n}"))?;
        let safety = match self.create(pane, &format!("safety before restoring {n}"), keep + 1)? {
            Some(c) => c,
            None => self
                .list(pane)
                .pop()
                .ok_or("no checkpoint to restore from")?,
        };
        let names = |filter: &str| -> Result<Vec<String>, String> {
            let out = output(self.store_git().args([
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                filter,
                &safety.commit,
                &target.commit,
            ]))
            .ok_or("cannot compare the checkpoints")?;
            Ok(out
                .split('\0')
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect())
        };
        let write = names("--diff-filter=d")?;
        let remove = names("--diff-filter=D")?;

        if !write.is_empty() {
            let tmp = self.temp_index();
            let read = ok(self
                .store_git()
                .env("GIT_INDEX_FILE", &tmp)
                .args(["read-tree", &target.commit]));
            let mut child = self
                .store_git()
                .current_dir(&self.top)
                .env("GIT_INDEX_FILE", &tmp)
                .env("GIT_WORK_TREE", &self.top)
                .args(["checkout-index", "-f", "-z", "--stdin"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?;
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(write.join("\0").as_bytes());
            }
            let checked_out = child.wait().is_ok_and(|s| s.success());
            let _ = std::fs::remove_file(&tmp);
            if !(read && checked_out) {
                return Err(format!(
                    "could not write the files; restore checkpoint {} to undo",
                    safety.n
                ));
            }
        }
        for path in &remove {
            let _ = std::fs::remove_file(self.top.join(path));
        }
        Ok((write.len(), remove.len(), safety.n))
    }
}

/// The directory a pane's shell is in.
pub fn pane_dir(pane: &str) -> Option<PathBuf> {
    crate::tmux_cmd::tmux_out(&["display-message", "-p", "-t", pane, "#{pane_current_path}"])
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

fn current_pane() -> String {
    std::env::var("TMUX_PANE").unwrap_or_default()
}

/// `waymux checkpoint <list|diff N|restore N [--force]> [-w <pane>]`
pub fn run_cli(args: &[String]) -> i32 {
    let mut pane = current_pane();
    let mut force = false;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-w" | "--pane" => pane = it.next().cloned().unwrap_or_default(),
            "--force" | "-f" => force = true,
            _ => rest.push(a.as_str()),
        }
    }
    let usage = "Usage: waymux checkpoint <list | diff <n> | restore <n> [--force]> [-w <pane>]";
    if pane.is_empty() {
        eprintln!("{usage}\n(no pane: pass -w <pane> outside tmux)");
        return 1;
    }
    let Some(repo) = pane_dir(&pane).and_then(|d| Repo::open(&d, None)) else {
        eprintln!("pane {pane} is not in a git work tree");
        return 1;
    };
    let n = rest.get(1).and_then(|s| s.parse::<u64>().ok());
    match (rest.first().copied().unwrap_or("list"), n) {
        ("list" | "ls", _) => {
            let list = repo.list(&pane);
            if list.is_empty() {
                println!("No checkpoints for {pane} in {}", repo.top.display());
            }
            for c in list {
                println!(
                    "{}\t{}\t{}",
                    c.n,
                    &c.commit[..c.commit.len().min(10)],
                    c.subject
                );
            }
            0
        }
        ("diff", Some(n)) => {
            use std::io::IsTerminal;
            match repo.diff(&pane, n, std::io::stdout().is_terminal()) {
                Ok(d) => {
                    print!("{d}");
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        ("restore", Some(n)) => {
            let state = crate::rpc_client::fetch_agent_states()
                .get(&pane)
                .map(|e| e.state.clone())
                .unwrap_or_default();
            if !force
                && matches!(
                    state.as_str(),
                    "working" | "permission" | "question" | "awaiting_input"
                )
            {
                eprintln!(
                    "The agent in {pane} is {state}: it would write over the files. Wait for the turn to end, or pass --force."
                );
                return 1;
            }
            match repo.restore(&pane, n, DEFAULT_KEEP) {
                Ok((w, r, safety)) => {
                    println!(
                        "Restored checkpoint {n}: {w} file(s) written, {r} removed. To undo: waymux checkpoint restore {safety} -w {pane}"
                    );
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        _ => {
            eprintln!("{usage}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "waymux-ckpt-{tag}-{}-{}",
                std::process::id(),
                crate::sys::unix_ms()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("repo")).unwrap();
            let s = Self { dir };
            s.git(&["init", "-q"]);
            s.git(&["config", "user.name", "t"]);
            s.git(&["config", "user.email", "t@example.com"]);
            s.git(&["config", "commit.gpgsign", "false"]);
            s
        }
        fn repo(&self) -> PathBuf {
            self.dir.join("repo")
        }
        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(self.repo())
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).to_string()
        }
        fn write(&self, path: &str, body: &str) {
            let p = self.repo().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        fn read(&self, path: &str) -> Option<String> {
            std::fs::read_to_string(self.repo().join(path)).ok()
        }
        fn open(&self) -> Repo {
            Repo::open(&self.repo(), Some(&self.dir.join("store"))).unwrap()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn test_snapshot_sees_a_same_size_edit_from_the_index_second() {
        // An edit that keeps the size, made in the second the index was written: git must
        // re-read the file, which it only does when the index copy keeps the index's mtime.
        let sb = Sandbox::new("racy");
        sb.git(&["config", "core.trustctime", "false"]);
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
        let touch = |path: &std::path::Path| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(then)
                .unwrap();
        };
        sb.write("a.txt", "one\n");
        touch(&sb.repo().join("a.txt"));
        sb.git(&["add", "-A"]);
        sb.git(&["commit", "-qm", "init"]);
        touch(&sb.repo().join(".git/index"));
        sb.write("a.txt", "two\n");
        touch(&sb.repo().join("a.txt"));

        let repo = sb.open();
        let c = repo.create("%1", "idle", 50).unwrap().unwrap();
        let blob = Command::new("git")
            .arg("--git-dir")
            .arg(&repo.store)
            .args(["show", &format!("{}:a.txt", c.commit)])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&blob.stdout),
            "two\n",
            "{}",
            String::from_utf8_lossy(&blob.stderr)
        );
    }

    #[test]
    fn test_checkpoints_restore_and_leave_the_repository_alone() {
        let sb = Sandbox::new("restore");
        sb.write("a.txt", "one\n");
        sb.write(".gitignore", "ignored.log\n");
        sb.git(&["add", "-A"]);
        sb.git(&["commit", "-qm", "init"]);
        let refs_before = sb.git(&["for-each-ref"]);
        let objects_before = sb.git(&["count-objects"]);

        let repo = sb.open();
        // Turn 1: edit a tracked file and add an untracked one.
        sb.write("a.txt", "two\n");
        sb.write("new.txt", "fresh\n");
        sb.write("ignored.log", "noise\n");
        let c1 = repo.create("%7", "idle", 50).unwrap().unwrap();
        assert_eq!(c1.n, 1);
        // Nothing changed: no second checkpoint.
        assert!(repo.create("%7", "idle", 50).unwrap().is_none());

        // Turn 2: more edits, a deletion.
        sb.write("a.txt", "three\n");
        std::fs::remove_file(sb.repo().join("new.txt")).unwrap();
        sb.write("later.txt", "x\n");
        let c2 = repo.create("%7", "idle", 50).unwrap().unwrap();
        assert_eq!(c2.n, 2);

        let diff = repo.diff("%7", 2, false).unwrap();
        assert!(diff.contains("-two") && diff.contains("+three"), "{diff}");
        assert!(diff.contains("new.txt") && diff.contains("later.txt"));

        // An edit no checkpoint holds yet: the safety checkpoint must keep it.
        sb.write("a.txt", "four\n");
        let status_before_restore = sb.git(&["status", "--porcelain"]);
        let (written, removed, safety) = repo.restore("%7", 1, 50).unwrap();
        assert_eq!(sb.read("a.txt").as_deref(), Some("two\n"));
        assert_eq!(sb.read("new.txt").as_deref(), Some("fresh\n"));
        assert_eq!(
            sb.read("later.txt"),
            None,
            "file not in checkpoint 1 removed"
        );
        assert_eq!(
            sb.read("ignored.log").as_deref(),
            Some("noise\n"),
            "ignored untouched"
        );
        assert_eq!((written, removed), (2, 1));
        assert_eq!(safety, 3, "the work tree was saved before restoring");

        // Undo the restore with the safety checkpoint.
        repo.restore("%7", safety, 50).unwrap();
        assert_eq!(sb.read("a.txt").as_deref(), Some("four\n"));
        assert_eq!(sb.read("later.txt").as_deref(), Some("x\n"));
        assert_eq!(sb.git(&["status", "--porcelain"]), status_before_restore);

        // The repository itself: no new refs, no new objects, HEAD and index as they were.
        assert_eq!(sb.git(&["for-each-ref"]), refs_before);
        assert_eq!(sb.git(&["count-objects"]), objects_before);
        assert_eq!(sb.git(&["log", "--all", "--oneline"]).lines().count(), 1);
    }

    #[test]
    fn test_keep_prunes_oldest_per_pane() {
        let sb = Sandbox::new("prune");
        sb.write("f", "0");
        sb.git(&["add", "-A"]);
        sb.git(&["commit", "-qm", "init"]);
        let repo = sb.open();
        for i in 1..=4 {
            sb.write("f", &i.to_string());
            repo.create("%1", "idle", 2).unwrap().unwrap();
        }
        let ns: Vec<u64> = repo.list("%1").iter().map(|c| c.n).collect();
        assert_eq!(ns, [3, 4]);
        assert!(repo.list("%2").is_empty(), "panes are separate");
    }

    #[test]
    fn test_repository_without_commits() {
        let sb = Sandbox::new("empty");
        sb.write("a", "x");
        let repo = sb.open();
        let c = repo.create("%3", "idle", 50).unwrap().unwrap();
        assert_eq!(c.n, 1);
        assert!(repo.diff("%3", 1, false).unwrap().contains("+x"));
    }

    #[test]
    fn test_pane_key_and_hash_are_stable() {
        assert_eq!(pane_key("%12"), "12");
        assert_eq!(path_hash("/a"), path_hash("/a"));
        assert_ne!(path_hash("/a"), path_hash("/b"));
    }
}
