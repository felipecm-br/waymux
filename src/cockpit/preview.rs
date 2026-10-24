use crate::cockpit::notes;
use crate::theme::{RESET, ThemePalette, strip_ansi};
use crate::tmux_cmd::{tmux_out, tmux_out_raw};
use std::fmt::Write as _;

pub fn box_line(card_border: &str, w: usize, content: &str) -> String {
    let clean = strip_ansi(content);
    let len = clean.chars().count();
    let max_content = w.saturating_sub(4);
    let pad = max_content.saturating_sub(len);
    format!(
        "{}│{} {}{} {}│{}\n",
        card_border,
        RESET,
        content,
        " ".repeat(pad),
        card_border,
        RESET
    )
}

pub fn run_cli(args: &[String]) -> i32 {
    let session = args.first().map_or("", std::string::String::as_str);
    let idx = args.get(1).map_or("", std::string::String::as_str);
    let mut pane_id = args
        .get(2)
        .map_or("", std::string::String::as_str)
        .to_string();
    let mut ppath = args
        .get(3)
        .map_or("", std::string::String::as_str)
        .to_string();

    if session.is_empty() || idx.is_empty() || idx == "-" {
        println!("  \x1b[38;2;146;131;116m(no active window selected)\x1b[0m");
        return 0;
    }

    let target_win = format!("{session}:{idx}");
    if pane_id.is_empty() || pane_id == "-" {
        pane_id = tmux_out(&["display-message", "-p", "-t", &target_win, "#{pane_id}"])
            .unwrap_or_default();
    }
    if ppath.is_empty() || ppath == "-" {
        ppath = tmux_out(&[
            "display-message",
            "-p",
            "-t",
            &target_win,
            "#{pane_current_path}",
        ])
        .unwrap_or_default();
    }

    let theme = ThemePalette::load();
    let c_gray = &theme.c_muted;

    let raw_terminal = tmux_out_raw(&["capture-pane", "-ep", "-S", "-120", "-t", &pane_id])
        .unwrap_or_default()
        .replace("\x1b[4m", "")
        .replace("\x1b[24m", "")
        .replace("\x1b[21m", "");

    let mut out = String::with_capacity(4096);
    let lines: Vec<&str> = raw_terminal.lines().collect();
    if let Some(last_non_empty) = lines.iter().rposition(|l| !l.trim().is_empty()) {
        for line in &lines[..=last_non_empty] {
            out.push_str(line);
            out.push('\n');
        }
    } else {
        let _ = writeln!(out, "  {c_gray}(pane output empty or detached){RESET}");
    }

    let notes_section = notes::render_pending_notes(&ppath);
    if !notes_section.is_empty() {
        out.push('\n');
        out.push_str(&notes_section);
    }

    print!("{out}");
    0
}
