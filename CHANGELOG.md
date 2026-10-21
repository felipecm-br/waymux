# Changelog

All notable changes to **Waymux** are documented in this file.

## [Unreleased]

### 🛡️ Cockpit Safety & Triage Signals

- **Ship asks first (`src/cockpit/mod.rs`)**: `S` in the Cockpit merges, pushes to origin and deletes the branch, which cannot be undone. It now passes `--confirm` to `awt-merge.sh`, matching `S` in the awt popup. `split_merge_args` (`src/wt/mod.rs`) is extracted and tested so `--confirm` keeps reaching the script.
- **Guarded kills (`src/cockpit/actions.rs`)**: `waymux window-delete` checks every pane of the target window (or session) against the daemon and `@ai_agent_state_raw`, using the same rule as `close-window.sh` (anything but idle/done counts as in-flight). Idle targets still close on one press (`Prefix+u` restores them); a target with an agent at work arms on the first press and closes on a second press within 2 s. The redundant `D`, `X` and filter-mode `ctrl-x` aliases are removed from the embedded preset; `d`, `x` and `ctrl-d` remain.

### 🐛 Release Pipeline & Cross-Platform Build

- **GitHub Release Asset Uploads (`.github/workflows/release.yml`)**: Split `taiki-e/upload-rust-binary-action@v1` into separate steps for `waymux` and `acpd` so `archive: $bin-$tag-$target` builds and uploads both binary archives instead of failing with `when multiple binary names are specified, default archive name or '$bin' variable cannot be used in 'archive' option`.
- **macOS Targets (`aarch64-apple-darwin`, `x86_64-apple-darwin`)**: Add Apple Silicon and Intel macOS release targets on `macos-latest` alongside the Linux `musl` and `gnu` targets (`x86_64` / `aarch64`), and remove the unused Linux-only `tracing-journald` crate from `Cargo.toml`.
- **Rust 1.99 Clippy Compatibility (`src/adapters.rs`)**: Allow `clippy::double_must_use` on `OutputAdapter` (`#[async_trait]`) so `cargo clippy --all-targets --all-features -- -D warnings` passes cleanly on Rust 1.99+.

## [0.2.3] - 2026-10-18

### 🐳 Isolated Docker Sandbox & Simulator

- **Docker Sandbox (`docker/Dockerfile`)**: Multi-stage build providing `runtime` (instant <3s startup mounting pre-built binaries without Rust build bloat), `lite` (compiles `waymux` from source in Docker), and `full` (compiles `waymux` + `wm v0.3.2` + `lazygitrs v0.1.3-cockpit`).
- **Interactive Simulator (`docker/scenario.sh`)**: Interactive CLI to inspect states (`scenario status`), cycle simulated agents through live states (`scenario cycle`), test permission/question/idle states, and seed/dispatch inline review notes via bracketed paste.
- **Automated Smoke Tests (`docker/entrypoint.sh`)**: 8-step end-to-end integration test suite verifying health, Tmux sessions, agent state tracking, `/proc` scanner & 12-column TSV output, Cockpit preview, live diffs, review notes dispatch, and badge dismissal.
- **Developer Workflows**: Added `just docker-demo`, `just docker-test`, and `just docker-build` recipes, alongside runner script `scripts/docker-sandbox.sh`.

## [0.2.2] - 2026-10-18

### 🧹 Native `waymux` Paths (legacy `acpd` demoted to fallback)

- **Sounds**: `SoundAdapter` now searches `~/.config/waymux/sounds/` before the legacy `~/.config/acpd/sounds/` (mute marker, `<event>.sound` pointers and `<event>.wav` files). Omarchy-managed files keep top priority. Existing `acpd` setups keep working unchanged.
- **systemd unit**: `RuntimeDirectory` is now `waymux` only (`waymux init --systemd`, `systemd/waymux.service`). The legacy token mirror at `$XDG_RUNTIME_DIR/acpd/token` is still written best-effort for old clients and creates its own directory.
- **Reference config**: `config/default.toml` documents `~/.config/waymux/config.toml` and uses `/tmp/waymux.pid`.
- **Tests**: add `test_sound_search_paths_prefer_waymux_over_legacy_acpd`.

### 🧹 Storage & Worktree Hygiene

- **Worktree CLI Help (`waymux wt`)**: Document `awt sweep [-a|--all] [--clean-targets]` for cross-repository worktree sweeping and Rust `target/` pruning.
- **Agent Governance**: Document strict `target/` lifecycle discipline, worktree cleanup (`awt rm`), and read-only storage auditing (`dev-gc --dry-run`) in `AGENTS.md`.

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
