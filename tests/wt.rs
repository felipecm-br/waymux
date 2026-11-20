//! End-to-end tests of `waymux wt`: helper lookup and argument forwarding with fake
//! `awt-*.sh` scripts, and worktree creation with real git.

mod common;

use common::Env;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Output;

/// A fake helper that records its name and arguments in `$HOME/calls` and exits 7, so the
/// tests can check both what `wt` passed and that the exit code comes back.
fn fake_helper(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(
        &path,
        "#!/bin/sh\n{ echo \"${0##*/}\"; for a in \"$@\"; do echo \"[$a]\"; done; } > \"$HOME/calls\"\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn waymaker_scripts(env: &Env) -> std::path::PathBuf {
    env.dir.join("home/.config/waymaker/scripts")
}

fn wt(env: &Env, args: &[&str]) -> Output {
    let mut full = vec!["wt"];
    full.extend_from_slice(args);
    env.command(&full).current_dir(env.repo()).output().unwrap()
}

fn calls(env: &Env) -> Vec<String> {
    std::fs::read_to_string(env.dir.join("home/calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn help_prints_usage() {
    let env = Env::new();
    let out = wt(&env, &["--help"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("USAGE:"), "{}", stdout(&out));
}

#[test]
fn a_missing_helper_exits_127_and_names_every_searched_dir() {
    let env = Env::new();
    let out = wt(&env, &["sweep"]);
    assert_eq!(out.status.code(), Some(127));
    let err = stderr(&out);
    assert!(err.contains("awt-sweep.sh not found"), "{err}");
    for dir in [
        ".config/waymaker/scripts",
        ".config/matchmaker/scripts",
        ".local/share/waymux/scripts",
    ] {
        assert!(err.contains(dir), "{dir} missing from: {err}");
    }
}

#[test]
fn helpers_are_found_in_priority_order_and_share_dir_is_configurable() {
    let env = Env::new();
    let share = env.dir.join("share");
    fake_helper(&share.join("scripts"), "awt-reap.sh");
    let out = env
        .command(&["wt", "reap", "--dry-run"])
        .env("WAYMUX_SHARE_DIR", &share)
        .current_dir(env.repo())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(7),
        "the helper's exit code comes back"
    );
    assert_eq!(calls(&env), ["awt-reap.sh", "[--dry-run]"]);

    // The user's own Waymaker copy wins over the bundled one.
    std::fs::write(
        share.join("scripts/awt-reap.sh"),
        "#!/bin/sh\necho bundled > \"$HOME/calls\"\n",
    )
    .unwrap();
    fake_helper(&waymaker_scripts(&env), "awt-reap.sh");
    env.command(&["wt", "reap"])
        .env("WAYMUX_SHARE_DIR", &share)
        .current_dir(env.repo())
        .output()
        .unwrap();
    assert_eq!(calls(&env), ["awt-reap.sh"]);
}

#[test]
fn rm_needs_a_branch_and_passes_session_dir_branch_and_flags() {
    let env = Env::with_repo_at("proj/main");
    env.init_repo();
    let out = wt(&env, &["rm"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Usage: awt rm"), "{}", stderr(&out));

    fake_helper(&waymaker_scripts(&env), "awt-delete.sh");
    let out = wt(&env, &["rm", "feat/x", "-f", "--no-tmux"]);
    assert_eq!(out.status.code(), Some(7));
    let repo = env.repo();
    assert_eq!(
        calls(&env),
        [
            "awt-delete.sh".to_string(),
            "[proj/feat-x]".to_string(),
            format!("[{}/../feat-x]", repo.display()),
            "[feat/x]".to_string(),
            "[-f]".to_string(),
            "[--no-tmux]".to_string(),
        ]
    );
}

#[test]
fn ship_forwards_the_target_flags_and_push() {
    let env = Env::with_repo_at("proj/main");
    env.init_repo();
    fake_helper(&waymaker_scripts(&env), "awt-merge.sh");
    wt(&env, &["ship", "feat-y", "--into", "dev", "--squash"]);
    let repo = env.repo();
    assert_eq!(
        calls(&env),
        [
            "awt-merge.sh".to_string(),
            "[feat-y]".to_string(),
            format!("[{}]", repo.display()),
            "[proj/main]".to_string(),
            "[]".to_string(),
            "[--into]".to_string(),
            "[dev]".to_string(),
            "[--squash]".to_string(),
            "[--push]".to_string(),
        ]
    );
}

#[test]
fn create_without_a_branch_runs_the_wizard_with_the_current_branch() {
    let env = Env::new();
    env.init_repo();
    fake_helper(&waymaker_scripts(&env), "awt-new.sh");
    let out = wt(&env, &["-c", "--no-tmux"]);
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(calls(&env), ["awt-new.sh", "[main]", "[--no-tmux]"]);
}

#[test]
fn outside_a_repository_creation_is_refused() {
    let env = Env::new();
    let out = wt(&env, &["-c", "feat"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("Not inside a Git repository"));
}

#[test]
fn create_adds_a_worktree_records_its_base_and_runs_the_post_create_hook() {
    let env = Env::with_repo_at("proj/main");
    env.init_repo();
    let hooks = env.dir.join("home/.config/waymaker/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("post-create.sh"),
        "#!/bin/sh\necho \"$2 $3\" > \"$HOME/post-create\"\n",
    )
    .unwrap();
    std::fs::set_permissions(
        hooks.join("post-create.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let out = wt(&env, &["-c", "feat/one", "--no-tmux"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let target = env.dir.join("proj/feat-one");
    assert!(
        stdout(&out).contains("Worktree ready at"),
        "{}",
        stdout(&out)
    );
    assert!(target.join("a.txt").is_file());

    let base = std::process::Command::new("git")
        .args([
            "-C",
            &target.to_string_lossy(),
            "config",
            "branch.feat/one.base",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&base.stdout).trim(), "HEAD");
    assert_eq!(
        std::fs::read_to_string(env.dir.join("home/post-create"))
            .unwrap()
            .trim(),
        "feat/one HEAD"
    );

    // Asking again for an existing worktree just succeeds.
    assert!(wt(&env, &["feat/one", "--no-tmux"]).status.success());
}

#[test]
fn with_changes_moves_uncommitted_work_into_the_new_worktree() {
    let env = Env::with_repo_at("proj/main");
    env.init_repo();
    env.write("a.txt", "one\nedited\n");
    env.write("new.txt", "fresh\n");

    let out = wt(&env, &["-W", "carry", "--no-tmux"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let target = env.dir.join("proj/carry");
    assert_eq!(
        std::fs::read_to_string(target.join("a.txt")).unwrap(),
        "one\nedited\n"
    );
    assert!(target.join("new.txt").is_file());
    assert_eq!(
        std::fs::read_to_string(env.repo().join("a.txt")).unwrap(),
        "one\n",
        "the original tree is left clean"
    );
}

#[test]
fn popup_uses_the_helper_dirs_then_the_legacy_tmux_dir_and_reports_a_missing_script() {
    let env = Env::new();
    env.init_repo();
    let out = wt(&env, &["popup"]);
    assert_eq!(out.status.code(), Some(127));
    assert!(
        stderr(&out).contains("awt-popup.sh not found"),
        "{}",
        stderr(&out)
    );

    fake_helper(&env.dir.join("home/.config/tmux"), "awt-popup.sh");
    assert_eq!(wt(&env, &["popup"]).status.code(), Some(7));
    let repo = env.repo();
    assert_eq!(
        calls(&env),
        ["awt-popup.sh".to_string(), format!("[{}]", repo.display())]
    );

    std::fs::remove_file(env.dir.join("home/calls")).unwrap();
    std::fs::write(
        env.dir.join("home/.config/tmux/awt-popup.sh"),
        "#!/bin/sh\necho legacy > \"$HOME/calls\"\n",
    )
    .unwrap();
    fake_helper(&waymaker_scripts(&env), "awt-popup.sh");
    wt(&env, &["popup"]);
    assert_eq!(
        calls(&env)[0],
        "awt-popup.sh",
        "the helper dir wins over the legacy one"
    );
}

/// Runs the bundled `scripts/awt/awt-merge.sh` (not a fake) from `cwd`, with coreutils on
/// `PATH` but no tmux session, sounds or hooks of the developer's.
fn real_merge(env: &Env, cwd: &Path, args: &[&str]) -> Output {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/awt/awt-merge.sh");
    std::process::Command::new("bash")
        .arg(script)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", env.dir.join("bin").display()),
        )
        .env("HOME", env.dir.join("home"))
        .env("TMUX_TMPDIR", env.dir.join("tmux"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn git_in(env: &Env, dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", env.dir.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A plain checkout with the branch checked out in it has no worktree for `main` to merge
/// in. The helper used to guess `../main`: with an unrelated repository there it merged
/// into that one, and then moved the checkout itself to the trash and deleted it.
#[test]
fn ship_in_a_plain_checkout_refuses_and_keeps_the_checkout() {
    let env = Env::with_repo_at("proj/app");
    env.init_repo();
    env.git(&["switch", "-q", "-c", "feat"]);
    env.write("b.txt", "two\n");
    env.git(&["add", "."]);
    env.git(&["commit", "-q", "-m", "feat"]);
    // A clone, so a different repository whose `main` can fast-forward to `feat`: where the
    // old guess looked.
    let decoy = env.dir.join("proj/main");
    git_in(
        &env,
        &env.dir,
        &[
            "clone",
            "-q",
            "-b",
            "main",
            &env.repo().to_string_lossy(),
            &decoy.to_string_lossy(),
        ],
    );
    git_in(&env, &decoy, &["fetch", "-q", "origin", "feat:feat"]);
    let decoy_head = git_in(&env, &decoy, &["rev-parse", "HEAD"]);

    let repo = env.repo();
    let out = real_merge(
        &env,
        &repo,
        &[
            "feat",
            &repo.to_string_lossy(),
            "s",
            "main",
            "--no-tmux",
            "-y",
        ],
    );
    assert!(!out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains("no worktree has 'main' checked out"),
        "{}",
        stderr(&out)
    );
    assert!(repo.join(".git").exists(), "the checkout was removed");
    assert_eq!(git_in(&env, &repo, &["branch", "--show-current"]), "feat");
    assert_eq!(
        git_in(&env, &decoy, &["rev-parse", "HEAD"]),
        decoy_head,
        "merged into another repository"
    );
}

/// From a linked worktree, the merge lands in whichever worktree has the base checked out,
/// here the main checkout (not a `.bare` sibling), and only the linked worktree is removed.
#[test]
fn ship_from_a_linked_worktree_merges_into_the_checkout_that_has_the_base() {
    let env = Env::with_repo_at("proj/app");
    env.init_repo();
    let feat = env.dir.join("proj/feat-x");
    env.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "feat/x",
        &feat.to_string_lossy(),
    ]);
    std::fs::write(feat.join("b.txt"), "two\n").unwrap();
    git_in(&env, &feat, &["add", "."]);
    git_in(
        &env,
        &feat,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "feat x",
        ],
    );

    let out = real_merge(
        &env,
        &feat,
        &[
            "feat/x",
            &feat.to_string_lossy(),
            "s",
            "main",
            "--no-tmux",
            "-y",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let repo = env.repo();
    assert_eq!(
        git_in(&env, &repo, &["log", "-1", "--format=%s", "main"]),
        "feat x"
    );
    assert!(
        repo.join("b.txt").is_file(),
        "the main checkout's tree was not updated"
    );
    assert!(eventually_gone(&feat), "the merged worktree is still there");
    assert_eq!(git_in(&env, &repo, &["branch", "--list", "feat/x"]), "");
}

fn eventually_gone(path: &Path) -> bool {
    common::eventually(|| !path.exists())
}

/// The `.bare` layout `awt clone` makes: every branch is a linked worktree next to the
/// others, so shipping a feature merges into `../main` and removes the feature worktree.
#[test]
fn ship_in_the_bare_layout_merges_into_the_main_worktree_and_removes_the_feature() {
    let env = Env::with_repo_at("seed");
    env.init_repo();
    let proj = env.dir.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    git_in(
        &env,
        &proj,
        &[
            "clone",
            "-q",
            "--bare",
            &env.repo().to_string_lossy(),
            ".bare",
        ],
    );
    std::fs::write(proj.join(".git"), "gitdir: ./.bare\n").unwrap();
    git_in(&env, &proj, &["worktree", "add", "-q", "main", "main"]);
    git_in(
        &env,
        &proj,
        &["worktree", "add", "-q", "-b", "feat-y", "feat-y"],
    );
    let feat = proj.join("feat-y");
    std::fs::write(feat.join("c.txt"), "three\n").unwrap();
    git_in(&env, &feat, &["add", "."]);
    git_in(
        &env,
        &feat,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "feat y",
        ],
    );

    let out = real_merge(
        &env,
        &feat,
        &[
            "feat-y",
            &feat.to_string_lossy(),
            "s",
            "main",
            "--no-tmux",
            "-y",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let main = proj.join("main");
    assert_eq!(git_in(&env, &main, &["log", "-1", "--format=%s"]), "feat y");
    assert!(main.join("c.txt").is_file());
    assert!(eventually_gone(&feat), "the merged worktree is still there");
}

/// Runs the bundled `scripts/awt/awt-delete.sh` (not a fake), with coreutils on `PATH` but no
/// tmux session of the developer's.
fn real_delete(env: &Env, args: &[&str]) -> Output {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/awt/awt-delete.sh");
    std::process::Command::new("bash")
        .arg(script)
        .args(args)
        .current_dir(&env.dir)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", env.dir.join("bin").display()),
        )
        .env("HOME", env.dir.join("home"))
        .env("XDG_STATE_HOME", env.dir.join("home/.local/state"))
        .env("TMUX_TMPDIR", env.dir.join("tmux"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

/// A repository with a linked worktree `feat` on branch `feat`, which has one commit of its own.
fn repo_with_feature(env: &Env) -> std::path::PathBuf {
    env.init_repo();
    let feat = env.dir.join("feat");
    env.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "feat",
        &feat.to_string_lossy(),
    ]);
    std::fs::write(feat.join("b.txt"), "two\n").unwrap();
    git_in(env, &feat, &["add", "."]);
    git_in(env, &feat, &["commit", "-q", "-m", "feat"]);
    feat
}

/// `awt rm` deletes the branch with -D and trashes the directory, so uncommitted, untracked and
/// unmerged work goes to `refs/awt-backup/<branch>` first, and the printed command brings it back.
#[test]
fn rm_backs_up_uncommitted_and_untracked_work_and_the_recovery_command_restores_it() {
    let env = Env::new();
    let feat = repo_with_feature(&env);
    std::fs::write(feat.join("a.txt"), "edited\n").unwrap();
    std::fs::write(feat.join("new.txt"), "untracked\n").unwrap();
    let feat_s = feat.to_string_lossy().to_string();

    let out = real_delete(&env, &["s", &feat_s, "feat", "--no-tmux", "--no-hooks"]);
    assert!(out.status.success(), "{out:?}");
    assert!(eventually_gone(&feat));
    let repo = env.repo();
    assert_eq!(
        git_in(&env, &repo, &["branch", "--list", "feat"]),
        "",
        "the branch is still deleted"
    );
    let files = git_in(
        &env,
        &repo,
        &["ls-tree", "-r", "--name-only", "refs/awt-backup/feat"],
    );
    assert!(
        files.contains("new.txt") && files.contains("b.txt"),
        "{files}"
    );
    assert_eq!(
        git_in(&env, &repo, &["show", "refs/awt-backup/feat:a.txt"]),
        "edited"
    );

    let err = String::from_utf8_lossy(&out.stderr);
    let recover = err
        .lines()
        .find(|l| l.trim_start().starts_with("git "))
        .expect("a recovery command")
        .trim()
        .to_string();
    let log = std::fs::read_to_string(env.dir.join("home/.local/state/awt/backups.log")).unwrap();
    assert!(log.contains("refs/awt-backup/feat"), "{log}");

    let back = std::process::Command::new("sh")
        .args(["-c", &recover])
        .env("HOME", env.dir.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(back.status.success(), "{back:?}");
    assert_eq!(git_in(&env, &feat, &["log", "-1", "--format=%s"]), "feat");
    assert_eq!(
        std::fs::read_to_string(feat.join("a.txt")).unwrap(),
        "edited\n"
    );
    assert_eq!(
        std::fs::read_to_string(feat.join("new.txt")).unwrap(),
        "untracked\n"
    );
    assert!(
        git_in(&env, &feat, &["status", "--porcelain"]).contains("new.txt"),
        "the work is uncommitted again"
    );
}

/// A clean worktree whose branch is merged loses nothing, so no backup ref is written.
#[test]
fn rm_of_a_clean_merged_branch_writes_no_backup() {
    let env = Env::new();
    let feat = repo_with_feature(&env);
    env.git(&["merge", "-q", "--ff-only", "feat"]);
    let out = real_delete(
        &env,
        &[
            "s",
            &feat.to_string_lossy(),
            "feat",
            "--no-tmux",
            "--no-hooks",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let refs = git_in(&env, &env.repo(), &["for-each-ref", "refs/awt-backup"]);
    assert_eq!(refs, "");
}

/// A clean branch with commits no other ref has is backed up at its own HEAD.
#[test]
fn rm_of_an_unmerged_clean_branch_backs_up_its_head() {
    let env = Env::new();
    let feat = repo_with_feature(&env);
    let head = git_in(&env, &feat, &["rev-parse", "HEAD"]);
    let out = real_delete(
        &env,
        &[
            "s",
            &feat.to_string_lossy(),
            "feat",
            "--no-tmux",
            "--no-hooks",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        git_in(&env, &env.repo(), &["rev-parse", "refs/awt-backup/feat"]),
        head
    );
}

/// Runs the bundled `scripts/awt/awt-sweep.sh` from `cwd` (no tmux, no `gh` on `PATH`).
fn real_sweep(env: &Env, cwd: &Path, args: &[&str]) -> Output {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/awt/awt-sweep.sh");
    std::process::Command::new("bash")
        .arg(script)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", env.dir.join("bin").display()),
        )
        .env("HOME", env.dir.join("home"))
        .env("TMUX_TMPDIR", env.dir.join("tmux"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

/// A worktree `name` on its own branch whose only commit and index are `days` old.
fn old_worktree(env: &Env, name: &str, days: u64) -> std::path::PathBuf {
    let wt = env.dir.join(name);
    env.git(&["worktree", "add", "-q", "-b", name, &wt.to_string_lossy()]);
    std::fs::write(wt.join(format!("{name}.txt")), "x\n").unwrap();
    git_in(env, &wt, &["add", "."]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let then = now - days * 86_400;
    let date = format!("@{then} +0000");
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["commit", "-q", "-m", name])
        .current_dir(&wt)
        .env("HOME", env.dir.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let index = git_in(env, &wt, &["rev-parse", "--absolute-git-dir"]) + "/index";
    // Not `touch -d @…`: BSD touch (macOS) has no `@epoch` form.
    std::fs::File::options()
        .write(true)
        .open(&index)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(then))
        .unwrap();
    wt
}

/// `--idle` (or `awt.sweepIdleDays`) also lists clean worktrees nobody touched for that long;
/// `awt.sweepKeep` globs keep a branch out of every sweep.
#[test]
fn sweep_lists_idle_clean_worktrees_and_honours_the_keep_rule() {
    let env = Env::new();
    env.init_repo();
    old_worktree(&env, "idle-one", 40);
    old_worktree(&env, "spike-a", 40);
    let dirty = old_worktree(&env, "dirty", 40);
    std::fs::write(dirty.join("wip.txt"), "wip\n").unwrap();
    env.git(&["config", "awt.sweepKeep", "spike-*"]);
    let repo = env.repo();

    let merged_only = real_sweep(&env, &repo, &["--dry-run"]);
    let text = String::from_utf8_lossy(&merged_only.stdout).to_string();
    assert!(merged_only.status.success(), "{merged_only:?}");
    assert!(!text.contains("idle-one"), "idle is off by default: {text}");

    let idle = real_sweep(&env, &repo, &["--dry-run", "--idle", "30"]);
    let text = common::strip_ansi(&String::from_utf8_lossy(&idle.stdout));
    assert!(idle.status.success(), "{idle:?}");
    assert!(
        text.contains("idle-one") && text.contains("Idle 30+ days, clean"),
        "{text}"
    );
    assert!(
        text.contains("spike-a") && text.contains("kept by awt.sweepKeep"),
        "{text}"
    );
    assert!(
        !text.contains("dirty "),
        "a dirty worktree is never idle: {text}"
    );

    env.git(&["config", "awt.sweepIdleDays", "60"]);
    let text = String::from_utf8_lossy(&real_sweep(&env, &repo, &["--dry-run"]).stdout).to_string();
    assert!(!text.contains("idle-one"), "40 days is not 60: {text}");
}
