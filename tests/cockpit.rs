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
