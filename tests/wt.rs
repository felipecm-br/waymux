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
