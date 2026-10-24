//! End-to-end tests of the Cockpit render commands (`cockpit-diff`, `cockpit-preview`,
//! `cockpit-help`, `review-notes`) against real git repositories. `PATH` holds only `git`,
//! so diffs print as plain text (no `delta`/`bat`) and tmux is never reached.

mod common;

use common::Env;

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

#[test]
fn keylog_is_off_by_default_and_respects_config_and_env_override() {
    let env = Env::new();
    let keys_tsv = env.dir.join("home/.local/state/flow/keys.tsv");

    // Default config: nothing is written.
    env.run(&["keylog", "cockpit", "g", "review", "s:1", "s:0"]);
    assert!(!keys_tsv.exists(), "keys.tsv created without opt-in");

    // WAYMUX_KEYLOG=1 turns it on without a config file.
    let out = env
        .command(&["keylog", "cockpit", "y", "permission", "s:2"])
        .env("WAYMUX_KEYLOG", "1")
        .env("WAYMUX_KEYLOG_ORIGIN", "s:0")
        .output()
        .unwrap();
    assert!(out.status.success());
    let content = std::fs::read_to_string(&keys_tsv).unwrap();
    assert!(
        content.contains("\tcockpit\ty\tpermission\ts:2\ts:0\n"),
        "{content}"
    );

    // keylog = true in ~/.config/waymux/config.toml turns it on, and WAYMUX_KEYLOG=0 overrides it.
    let cfg_dir = env.dir.join("home/.config/waymux");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("config.toml"), "keylog = true\n").unwrap();
    env.run(&["keylog", "cockpit", "g", "review", "s:1", "s:0"]);
    let content = std::fs::read_to_string(&keys_tsv).unwrap();
    assert!(
        content.contains("\tcockpit\tg\treview\ts:1\ts:0\n"),
        "{content}"
    );

    let out = env
        .command(&["keylog", "cockpit", "n", "permission", "s:3", "s:0"])
        .env("WAYMUX_KEYLOG", "0")
        .output()
        .unwrap();
    assert!(out.status.success());
    let after = std::fs::read_to_string(&keys_tsv).unwrap();
    assert_eq!(after, content, "WAYMUX_KEYLOG=0 must override config");
}

#[test]
fn cockpit_popup_loop_runs_wm_reopens_and_exits_on_cancel() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let local_bin = env.dir.join("home/.local/bin");
    std::fs::create_dir_all(&local_bin).unwrap();
    let state_file = env.dir.join("wm_calls");
    let wm_script = local_bin.join("wm");
    // Call 1 prints __OPEN_PR__ (which reopens the loop saving session:idx); call 2 prints nothing (Esc).
    std::fs::write(
        &wm_script,
        format!(
            "#!/bin/sh\ncat >/dev/null\nif [ ! -f '{0}' ]; then\n  echo \"$*\" > '{0}'\n  printf '__OPEN_PR__t\\t2\\tname\\tapi\\t%%1\\t{1}\\treview\\t-\\t-\\t-\\t-\\t-\\n'\nelse\n  echo \"$*\" >> '{0}'\nfi\n",
            state_file.display(),
            env.repo().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wm_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = env
        .command(&["cockpit"])
        .env("TMUX_POPUP", "1")
        .env("TMUX_ORIGIN_SESSION", "orig")
        .env("TMUX_ORIGIN_WINDOW", "0")
        .env("WAYMUX_KEYLOG", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        env.dir.join("run/waymux/cockpit-picker.toml").is_file(),
        "cockpit preset materialized in runtime dir"
    );
    let calls = std::fs::read_to_string(&state_file).unwrap();
    assert_eq!(
        calls.lines().count(),
        2,
        "expected 2 wm invocations: {calls}"
    );
}
