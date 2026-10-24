use crate::theme::{BOLD, DIM, RESET, ThemePalette};
use std::process::Command;

pub fn run_cli() -> i32 {
    let theme = ThemePalette::load();
    let c_cyan = &theme.c_cyan;
    let c_yellow = &theme.c_busy;
    let c_red = &theme.c_error;
    let c_magenta = &theme.c_question;
    let c_blue = &theme.c_branch;

    // Optional user HUD (e.g. stats from the flow_log_hook's log), printed above the keys.
    if let Some(telemetry_py) = crate::wt::find_user_script("flow-telemetry.py") {
        let _ = Command::new("python3")
            .arg(&telemetry_py)
            .arg("--hud")
            .status();
    }

    println!("  {c_cyan}{BOLD}󱂬 Cockpit HUD — Keybindings & Shortcuts{RESET}\n");

    println!("  {c_yellow}{BOLD}󰊢 Worktree Lifecycle{RESET}");
    println!("    {c_yellow}{BOLD}[w / c]{RESET}  New Worktree      Launch 5-step AWT wizard");
    println!(
        "    {c_cyan}{BOLD}[S]{RESET}      Ship Worktree     Merge, push & clean worktree (asks first)"
    );
    println!(
        "    {c_yellow}{BOLD}[W]{RESET}      Sweep Worktrees   Batch-prune merged worktrees\n"
    );

    println!("  {c_magenta}{BOLD}󱚥 AI Fleet & Agent Triage{RESET}");
    println!(
        "    {c_yellow}{BOLD}[y / n]{RESET}  Approve / Deny    Guarded prompt reply (permission/question only)"
    );
    println!(
        "    {c_cyan}{BOLD}[r]{RESET}      Review Notes      Dispatch Lazygitrs review notes to agent"
    );
    println!(
        "    {c_cyan}{BOLD}[i]{RESET}      Live Passthrough  Stream keystrokes to agent ([Esc] exit)"
    );
    println!(
        "    {c_magenta}{BOLD}[I]{RESET}      Prompt Agent      Send sanitized inline prompt to agent"
    );
    println!(
        "    {c_yellow}{BOLD}[a]{RESET}      Toggle Fleet      Fleet View (all windows) ↔ Agent Triage"
    );
    println!(
        "    {c_cyan}{BOLD}[Bksp]{RESET}   Last Agent        Jump back to previously focused agent"
    );
    println!("    {c_red}{BOLD}[R]{RESET}      Reap Agents       Garbage-collect stale agents\n");

    println!("  {c_blue}{BOLD}󰓩 Git & Pull Requests{RESET}");
    println!(
        "    {c_blue}{BOLD}[o]{RESET}      Open PR           Open GitHub pull request in browser"
    );
    println!(
        "    {c_blue}{BOLD}[O]{RESET}      Open PR Checks    Open PR CI status checks in browser"
    );
    println!(
        "    {c_magenta}{BOLD}[g]{RESET}      Git Cockpit       Toggle Lazygitrs (Dual-Diff, Esc returns; clears 󰷋 review)"
    );
    println!(
        "    {c_magenta}{BOLD}[p]{RESET}      Patch Mode        Turbo Hunk Staging in Lazygitrs\n"
    );

    println!("  {c_cyan}{BOLD}󰌌 Navigation & Windows{RESET}");
    println!("    {c_cyan}{BOLD}[Enter]{RESET}  Jump / Connect    Focus selected window/session");
    println!("    {c_yellow}{BOLD}[Tab]{RESET}    AWT Worktrees     Switch to AWT Worktree popup");
    println!("    {c_yellow}{BOLD}[s]{RESET}      Sessions          Open Session Picker (Sesh)");
    println!("    {c_cyan}{BOLD}[t]{RESET}      New Window        Create window in current path");
    println!(
        "    {c_red}{BOLD}[d / x]{RESET}  Kill / Delete     Kill window or session (agent at work: press twice)"
    );
    println!("    {c_blue}{BOLD}[J / K]{RESET}  Scroll Preview    Scroll preview pane up/down");
    println!("    {c_blue}{BOLD}[+ / -]{RESET}  Resize Preview    Expand/shrink preview split");
    println!("    {c_blue}{BOLD}[/]{RESET}      Filter            Enter fuzzy query filter mode");
    println!("    {c_blue}{BOLD}[Esc]{RESET}    Exit              Dismiss Cockpit modal\n");

    println!("  {DIM}────────────────────────────────────────────────────────────{RESET}");
    println!(
        "  {c_cyan}{BOLD}?{RESET} {DIM}HUD help{RESET}   {DIM}│{RESET}   {c_cyan}{BOLD}g{RESET} {DIM}Git{RESET}   {DIM}│{RESET}   {c_cyan}{BOLD}p{RESET} {DIM}Patch{RESET}"
    );

    0
}
