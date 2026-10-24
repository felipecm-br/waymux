use crate::cockpit::notes;
use crate::cockpit::preview::box_line;
use crate::theme::{BOLD, DIM, RESET, ThemePalette};
use crate::tmux_cmd::tmux_out;
use std::io::Write;
use std::process::{Command, Stdio};

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else if max_chars <= 1 {
        "…".to_string()
    } else {
        let prefix: String = s.chars().take(max_chars - 1).collect();
        format!("{prefix}…")
    }
}

fn git_out(ppath: &str, args: &[&str]) -> String {
    let mut full = vec!["-C", ppath];
    full.extend_from_slice(args);
    Command::new("git")
        .args(&full)
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn render_diff_stream(raw_diff: &str, preview_w: usize) {
    let _ = std::io::stdout().flush();
    let width_s = preview_w.to_string();
    let delta_args = if preview_w >= 60 {
        vec![
            "--side-by-side",
            "--line-numbers",
            "--paging=never",
            "--width",
            &width_s,
            "--hunk-header-style",
            "omit",
        ]
    } else {
        vec![
            "--line-numbers",
            "--paging=never",
            "--width",
            &width_s,
            "--hunk-header-style",
            "omit",
        ]
    };

    if let Ok(mut child) = Command::new("delta")
        .args(&delta_args)
        .stdin(Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(raw_diff.as_bytes());
        }
        let _ = child.wait();
        return;
    }

    if let Ok(mut child) = Command::new("bat")
        .args([
            "--paging=never",
            "--color=always",
            "--language=diff",
            "--style=plain",
        ])
        .stdin(Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(raw_diff.as_bytes());
        }
        let _ = child.wait();
        return;
    }

    print!("{raw_diff}");
}

pub fn run_cli(args: &[String]) -> i32 {
    let mut session = args
        .first()
        .map_or("", std::string::String::as_str)
        .to_string();
    let idx = args.get(1).map_or("", std::string::String::as_str);
    let mut ppath = args
        .get(3)
        .map_or("", std::string::String::as_str)
        .to_string();
    let agent_name = args.get(5).map_or("", std::string::String::as_str);
    let mut branch = args
        .get(7)
        .map_or("", std::string::String::as_str)
        .to_string();

    if (session.is_empty() || idx.is_empty() || idx == "-")
        && let Some(stripped) = session.strip_prefix('#')
    {
        session = stripped.trim().to_string();
    }

    if ppath.is_empty() || ppath == "-" {
        if !session.is_empty() && !idx.is_empty() && idx != "-" {
            let target = format!("{session}:{idx}");
            ppath = tmux_out(&[
                "display-message",
                "-p",
                "-t",
                &target,
                "#{pane_current_path}",
            ])
            .unwrap_or_default();
        } else if !session.is_empty() {
            let target = format!("{session}:");
            ppath = tmux_out(&[
                "display-message",
                "-p",
                "-t",
                &target,
                "#{pane_current_path}",
            ])
            .unwrap_or_default();
        }
    }
    if ppath.is_empty() {
        ppath = std::env::current_dir()
            .ok()
            .map_or_else(|| ".".to_string(), |p| p.to_string_lossy().to_string());
    }

    let theme = ThemePalette::load();
    let c_orange = &theme.c_perm;
    let c_yellow = &theme.c_busy;
    let c_red = &theme.c_error;
    let c_magenta = &theme.c_question;
    let c_cyan = &theme.c_cyan;
    let c_blue = &theme.c_branch;
    let c_fg = &theme.c_name;
    let c_gray = &theme.c_muted;

    let preview_w: usize = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80)
        .max(50);
    let w = (preview_w - 2).clamp(48, 76);
    let border_dashes = "─".repeat(w - 2);

    let is_git = Command::new("git")
        .args(["-C", &ppath, "rev-parse", "--is-inside-work-tree"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if !is_git {
        println!("{c_gray}╭{border_dashes}╮{RESET}");
        print!(
            "{}",
            box_line(
                c_gray,
                w,
                &format!(
                    "{BOLD}󰈈 Mode:{RESET}   {c_cyan}{BOLD}󰊢 LIVE GIT DIFF (Side-by-Side){RESET} {DIM}[v toggles preview]{RESET}"
                )
            )
        );
        print!(
            "{}",
            box_line(
                c_gray,
                w,
                &format!("{BOLD}󰅚 Target:{RESET} {c_red}Not a Git Repository{RESET}")
            )
        );
        let clean_path = truncate_str(&ppath, w.saturating_sub(14));
        print!(
            "{}",
            box_line(
                c_gray,
                w,
                &format!("{BOLD}󰉋 Path:{RESET}   {c_gray}{clean_path}{RESET}")
            )
        );
        print!("{c_gray}╰{border_dashes}╯{RESET}\n\n");
        println!(
            "  {c_gray}(Press {c_blue}{BOLD}?{c_gray} for help, {c_cyan}{BOLD}v{c_gray} to return to terminal snapshot){RESET}"
        );
        return 0;
    }

    if branch.is_empty() || branch == "-" || branch == "non-git" {
        branch = git_out(&ppath, &["rev-parse", "--abbrev-ref", "HEAD"])
            .trim()
            .to_string();
        if branch.is_empty() {
            branch = "detached".to_string();
        }
    }

    let porcelain = git_out(&ppath, &["status", "--porcelain"]);
    let mut untracked_files: Vec<String> = Vec::new();
    let mut modified_count = 0usize;

    for line in porcelain.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("??") {
            untracked_files.push(rest.trim().to_string());
        } else {
            modified_count += 1;
        }
    }
    let untracked_count = untracked_files.len();

    let (diff_type, scope_desc, stat_summary, diff_stat, base_commit, card_border) =
        if porcelain.trim().is_empty() {
            let def_branch = ["main", "master"]
                .iter()
                .find(|&&b| {
                    Command::new("git")
                        .args(["-C", &ppath, "rev-parse", "--verify", b])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_ok_and(|s| s.success())
                })
                .copied()
                .unwrap_or("main");

            let base_c = if branch != def_branch && branch != "detached" {
                git_out(&ppath, &["merge-base", def_branch, "HEAD"])
                    .trim()
                    .to_string()
            } else {
                String::new()
            };
            let head_c = git_out(&ppath, &["rev-parse", "HEAD"]).trim().to_string();

            if !base_c.is_empty() && base_c != head_c {
                let range_dots = format!("{base_c}..HEAD");
                let range_triple = format!("{base_c}...HEAD");
                let c_count = git_out(&ppath, &["rev-list", "--count", &range_dots])
                    .trim()
                    .to_string();
                let summary = git_out(&ppath, &["diff", &range_triple, "--shortstat"])
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let dstat = git_out(&ppath, &["diff", &range_triple, "--stat", "--color=always"]);
                (
                    "branch_commits",
                    format!("Branch Commits ({c_count} ahead of {def_branch})"),
                    summary,
                    dstat,
                    base_c,
                    c_cyan,
                )
            } else {
                let clog = git_out(&ppath, &["log", "-1", "--format=%h · %s (%cr)"])
                    .trim()
                    .to_string();
                let summary = git_out(&ppath, &["show", "--shortstat", "--format=", "HEAD"])
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let dstat = git_out(
                    &ppath,
                    &["show", "--stat", "--color=always", "--format=", "HEAD"],
                );
                (
                    "latest_commit",
                    format!("Latest Commit ({clog})"),
                    summary,
                    dstat,
                    String::new(),
                    c_cyan,
                )
            }
        } else {
            let mut summary = if modified_count > 0 {
                let s = git_out(&ppath, &["diff", "HEAD", "--shortstat"]);
                s.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                String::new()
            };
            let dstat = if modified_count > 0 {
                git_out(&ppath, &["diff", "HEAD", "--stat", "--color=always"])
            } else {
                String::new()
            };
            if untracked_count > 0 {
                if summary.is_empty() {
                    summary = format!("{untracked_count} untracked file(s)");
                } else {
                    summary = format!("{summary}, {untracked_count} untracked");
                }
            }
            (
                "uncommitted",
                "Working Tree (Staged & Unstaged)".to_string(),
                summary,
                dstat,
                String::new(),
                c_orange,
            )
        };

    println!("{card_border}╭{border_dashes}╮{RESET}");
    print!(
        "{}",
        box_line(
            card_border,
            w,
            &format!(
                "{BOLD}󰈈 Mode:{RESET}   {c_cyan}{BOLD}󰊢 LIVE GIT DIFF (Side-by-Side){RESET} {DIM}[v toggles preview]{RESET}"
            )
        )
    );
    if !agent_name.is_empty() && agent_name != "-" {
        print!(
            "{}",
            box_line(
                card_border,
                w,
                &format!("{BOLD}󰑮 Agent:{RESET}  {c_yellow}{agent_name}{RESET}")
            )
        );
    }
    let clean_scope = truncate_str(&scope_desc, w.saturating_sub(14));
    print!(
        "{}",
        box_line(
            card_border,
            w,
            &format!("{BOLD}󰜘 Scope:{RESET}  {c_fg}{clean_scope}{RESET}")
        )
    );
    let clean_branch = truncate_str(&branch, w.saturating_sub(14));
    print!(
        "{}",
        box_line(
            card_border,
            w,
            &format!("{BOLD}󰊢 Branch:{RESET} {c_blue}{clean_branch}{RESET}")
        )
    );

    if stat_summary.is_empty() {
        print!(
            "{}",
            box_line(
                card_border,
                w,
                &format!("{BOLD}󰄧 Churn:{RESET}  {c_cyan}Working tree clean{RESET}")
            )
        );
    } else {
        let raw_stat = truncate_str(&stat_summary, w.saturating_sub(14));
        print!(
            "{}",
            box_line(
                card_border,
                w,
                &format!("{BOLD}󰄧 Churn:{RESET}  {raw_stat}")
            )
        );
    }
    print!("{card_border}╰{border_dashes}╯{RESET}\n\n");

    let notes_section = notes::render_pending_notes(&ppath);
    if !notes_section.is_empty() {
        print!("{notes_section}");
    }

    if !untracked_files.is_empty() {
        println!("  {c_yellow}{BOLD}󰄱 Untracked Files ({untracked_count}):{RESET}");
        for ufile in untracked_files.iter().take(6) {
            println!("    {c_gray}?{RESET} {c_yellow}{ufile}{RESET}");
        }
        if untracked_count > 6 {
            println!(
                "    {}... and {} more untracked files{}",
                c_gray,
                untracked_count - 6,
                RESET
            );
        }
        println!();
    }

    if !diff_stat.trim().is_empty() {
        println!("  {c_blue}{BOLD}󰄧 File Breakdown:{RESET}");
        let dlines: Vec<&str> = diff_stat.lines().collect();
        for dl in dlines.iter().take(12) {
            println!("  {dl}");
        }
        if dlines.len() > 12 {
            println!(
                "    {}... and {} more files in diff{}",
                c_gray,
                dlines.len() - 12,
                RESET
            );
        }
        println!();
    }

    let sep_len = preview_w.saturating_sub(20).clamp(20, 80);
    println!(
        "{}─── 󰜘 Side-by-Side Diff {}{}\n",
        c_gray,
        "─".repeat(sep_len.saturating_sub(24)),
        RESET
    );

    let mut has_diff_content = false;
    match diff_type {
        "uncommitted" => {
            if modified_count > 0 {
                let raw_diff = git_out(&ppath, &["diff", "HEAD"]);
                if !raw_diff.is_empty() {
                    has_diff_content = true;
                    render_diff_stream(&raw_diff, preview_w);
                }
            } else if untracked_count > 0 {
                has_diff_content = true;
                println!(
                    "  {c_gray}All changes are newly created untracked files (listed above).{RESET}"
                );
                println!(
                    "  {c_gray}Stage files ({c_fg}git add{c_gray}) or open Lazygitrs ({c_magenta}{BOLD}g{RESET}) to inspect contents.{RESET}"
                );
            }
        }
        "branch_commits" => {
            let range_triple = format!("{base_commit}...HEAD");
            let raw_diff = git_out(&ppath, &["diff", &range_triple]);
            if !raw_diff.is_empty() {
                has_diff_content = true;
                render_diff_stream(&raw_diff, preview_w);
            }
        }
        "latest_commit" => {
            let raw_diff = git_out(&ppath, &["show", "HEAD", "--format="]);
            if !raw_diff.is_empty() {
                has_diff_content = true;
                render_diff_stream(&raw_diff, preview_w);
            }
        }
        _ => {}
    }

    if !has_diff_content {
        println!("  {c_cyan}󰄬 No diff changes to display (working tree clean).{RESET}\n");
        println!(
            "  {c_gray}• Press {c_magenta}{BOLD}g{RESET}{c_gray} to launch Lazygitrs (Dual-Diff){RESET}"
        );
        println!(
            "  {c_gray}• Press {c_cyan}{BOLD}v{RESET}{c_gray} to toggle back to live terminal snapshot{RESET}"
        );
    }

    0
}
