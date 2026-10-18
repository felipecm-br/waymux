# Changelog

All notable changes to **Waymux** are documented in this file.

## [0.2.1] - 2026-10-17

### 🚀 Improvements & Ergonomics

- **Streamline Cockpit Terminal Preview**: Remove redundant top context card in terminal snapshot preview (`cockpit-preview`), anchoring scrollback to bottom tail (`tmux capture-pane -ep -S -120`) and bottom-anchoring pending review notes.
- **Eliminate Redundant Diff View (`v`)**: Remove `v` / `cockpit-diff` layout from Cockpit picker to delegate full Git inspection and interactive staging to Lazygitrs (`g`).
- **Update Help & Footer Reference**: Update Cockpit footer and help cheat-sheet to reflect `Esc` returning from Lazygitrs and removal of `v` diff.

## [0.2.0] - 2026-10-17

### 🚀 Features

- **Unified Single-Binary Rust Orchestrator (`waymux`)**: Consolidate the `acpd` JSON-RPC 2.0 daemon, Cockpit HUD (`waymux cockpit`, `cockpit-items`, `cockpit-preview`, `cockpit-diff`, `cockpit-help`), guarded agent actions (`agent-action`), native review notes (`review-notes`), alert ring triage (`waymux triage --popup|--jump`), zero-Node lifecycle hooks (`waymux hook tmux|lazygit`), bracketed-paste prompt injection (`waymux inject`), Git worktree orchestration (`waymux wt`), pane badge dismissal (`waymux dismiss`), status inspection (`waymux status`, `waymux list`), and zero-config bootstrapper (`waymux init`) into `waymux` (~5 MB).
- **Sub-Millisecond Synchronous CLI Fast-Path**: Execute all CLI subcommands synchronously before initializing the Tokio multi-threaded runtime or tracing subscriber, cutting hook latency from ~45ms to ~8ms and Cockpit HUD item generation from ~140ms to ~14ms.
- **Embedded Waymaker Presets**: Embed `cockpit-picker.toml` via `include_str!` and materialize dynamically to `$XDG_RUNTIME_DIR/waymux/`.
- **Native `/proc` Process & Git Scanner**: Replace `ps`, `jq`, and `python3` subprocesses with native Rust `/proc/<pid>/stat` inspection and `serde_json` parsing.
- **Proactive Agent Supervision**: Include 20s hung-agent stall detection (`stalled` badge), auto-dismiss on pane focus (`agentState/dismiss`), and synchronous state waiting (`agentState/wait`).

### 🐛 Bug Fixes & Improvements

- Normalize epoch timestamp units (seconds vs milliseconds) when computing elapsed agent age in `cockpit-items`.
- Flush Rust stdout buffers before spawning child `delta` / `bat` streams in `cockpit-diff`.
- Provide built-in `Config::default()` fallback so `waymux daemon` starts with zero configuration files required.

## [0.1.0] - 2026-09-24

- Initial release of the Waymux terminal AI multiplexer stack and ACPD broker daemon.
