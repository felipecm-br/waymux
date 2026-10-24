<div align="center">

![waymux banner](assets/waymux-banner.jpg)

# Waymux

### Autonomous Terminal AI Cockpit & Control Plane Daemon
**Sub-1.5ms CLI Fast-Path · Zero Status-Bar Polling · Biomechanical Keyboard Ergonomics ($H=0$)**

[![Release](https://img.shields.io/github/v/release/felipecm-br/waymux?color=00f5d4&style=flat-square)](https://github.com/felipecm-br/waymux/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-00b4d8?style=flat-square)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS%20(partial)-7209b7?style=flat-square)](#platform-support)

</div>

---

## ⚡ Why Waymux? (The Problem & The Solution)

Running multiple autonomous AI coding agents (`agy`, `claude`, `codex`, `opencode`, `crush`, `devin`) inside Tmux typically degrades terminal ergonomics:
1. **Status Bar Flicker & Polling Drain**: Rapid tool chaining causes 10+ state flips per second, while `status-interval 1` polling scripts drain CPU and battery when idle.
2. **Cold-Start Hook & Shell Latency**: Spawning Node.js or Python scripts on every `PreToolUse` / `PostToolUse` hook adds 40–60ms of latency per tool call, and `/proc` shell loops (`ps` + `awk` + `jq`) take 140ms+ to render window pickers.
3. **Disjointed Code Review & Worktree Contexts**: Sending inline diff review notes from Git TUIs (`lazygitrs`) into an active agent pane without tearing multi-line markdown, or managing 1-worktree-per-session sandboxes, usually requires fragile shell glue.

**Waymux solves all of this in a single ~3 MB Rust binary:**
- **Zero status-bar polling (`status-interval 0`)**: An asynchronous Axum/Tokio daemon (`waymux daemon`) tracks agent states with consolidated 300ms idle debouncing, proactive stall detection (20s hung-agent heuristic), and multi-pane urgency aggregation.
- **<1.5ms Synchronous CLI Fast-Path**: All client subcommands (`cockpit`, `triage`, `hook`, `inject`, `wt`, `dismiss`, `status`, `list`) execute synchronously **before** initializing the Tokio runtime.
- **~17ms Native `/proc` Cockpit HUD**: Scans `/proc/<pid>/stat` and `/proc/<pid>/comm` directly in Rust to render a 12-column TUI fleet dashboard via **Waymaker (`wm`)** with embedded presets and live `delta` side-by-side diffs.
- **Zero-Node Agent Hooks & Bracketed-Paste Injection**: Native `waymux hook {tmux|lazygit}` (~8ms) and `waymux inject` deliver multi-line review notes atomically via `tmux load-buffer` + `paste-buffer -p`.
- **Git Worktree Orchestrator (`waymux wt` / `awt`)**: Provisions sibling `.bare` worktrees, copies untracked `.env*` secrets, and binds 1 worktree to 1 Tmux session, using the bundled `awt-*.sh` helpers.

---

## 🚀 Quick Installation

### 1-Command Zero-Compile Install (Linux & macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/felipecm-br/waymux/main/install.sh | bash
```

The installer downloads the pre-built binary to `~/.local/bin/waymux` (and symlinks `~/.local/bin/acpd`), verifies its SHA-256 checksum (the install stops if the checksum is missing or wrong; `WAYMUX_SKIP_CHECKSUM=1` overrides; `WAYMUX_VERIFY_ATTESTATION=1` also checks the signed build provenance with `gh attestation verify`), installs the tmux plugin and the `awt-*.sh` worktree helpers under `~/.local/share/waymux/`, and enables the user-level systemd service (`waymux.service`).

| Flag | Effect |
| :--- | :--- |
| `--from-source` | Build with cargo instead of downloading a release |
| `--with-stack` | Also build the companion TUIs `wm` ([Waymaker](https://github.com/felipecm-br/waymaker)) and [`lazygitrs`](https://github.com/felipecm-br/lazygitrs) with cargo |
| `--no-systemd` | Skip the systemd unit (start the daemon yourself with `waymux daemon`) |
| `--prefix <dir>` | Install the binary somewhere other than `~/.local/bin` |

### Build from Source (Rust 1.88+)

```bash
git clone https://github.com/felipecm-br/waymux.git
cd waymux
./install.sh --from-source
```

### Check the Setup

```bash
waymux doctor
```

`doctor` checks the daemon, its token, tmux, git, `wm`, `lazygitrs`, `delta`, `gh`, a sound player, the worktree helpers, your Claude Code hooks, the theme file and (on Linux) whether your installed `waymux.service` lacks settings the shipped unit has, and prints a fix for each gap. It exits 1 only when something required (daemon, tmux, git) is missing.

### Platform Support

| Feature | Linux | macOS |
| :--- | :--- | :--- |
| Daemon, hooks, status bar, triage, `inject` | Yes | Yes |
| Sounds | `pw-play`, `paplay` or `aplay` | `afplay` |
| Desktop notifications, Waybar | Yes | No |
| Cockpit CPU/RAM columns and stall detection | Yes (`/proc`) | No |
| systemd service | Yes | No: run `waymux daemon` (or a launchd agent) |

---

## 🛠️ CLI Command Reference

Client commands bypass the async runtime (`--version`, `health`, `list` and `status` run in under 1.5 ms):

| Command | Description |
| :--- | :--- |
| `waymux daemon [--config <file>]` | Start the async JSON-RPC 2.0 & HTTP control plane daemon on `127.0.0.1:4040` |
| `waymux health` | Probe daemon health (`GET /health`) and exit `0` if healthy |
| `waymux status` | Print daemon health and all active AI agent pane states |
| `waymux list` | List active AI agent pane states in TSV format (`pane_id`, `state`, `last_timestamp`) |
| `waymux dismiss [pane_id]` | Dismiss attention/question/permission badge for a pane (used on `pane-focus-in`) |
| `waymux cockpit [--agents]` | Launch the `85% × 75%` AI Fleet & Window Cockpit HUD popup via Waymaker (`wm`) |
| `waymux cockpit-items [--agents]` | Emit prioritized 12-column TSV rows with native `/proc` CPU/RAM telemetry |
| `waymux cockpit-preview <s> <i> <p> [path]` | Render contextual agent metadata card, PR status, review notes, and pane snapshot |
| `waymux cockpit-diff <path> [pane_id]` | Render side-by-side Git diff (`delta` / `bat`) and pending review notes |
| `waymux cockpit-help` | Render telemetry HUD and ergonomic keybinding cheat-sheet |
| `waymux agent-action <act> <st> <pid> [pane]` | Execute guarded agent action (`approve`, `deny`, `prompt`) |
| `waymux review-notes <count\|list\|send> ...` | Count, list, or atomically send `.git/info/lines.json` review notes to an agent |
| `waymux triage --popup` | Cycle notifying AI agent panes inside an isolated `_popups` overlay |
| `waymux triage --jump` | Jump to the next agent waiting on you (permission → question → error/stalled, oldest first), then to finished work nobody has reviewed, with origin-return memory |
| `waymux hook <tmux\|lazygit> <Event>` | Zero-Node AI agent lifecycle hook (`PreInvocation`, `PreToolUse`, `PostToolUse`, `Stop`, `SessionEnd`) |
| `waymux inject <workspace_dir> <prompt>` | Inject multi-line prompt into active workspace agent pane via bracketed paste |
| `waymux wt <subcommand>` | Git worktree orchestrator (`-c`/`new`, `<branch>`, `rm`, `rebase`, `ship`, `pr`, `clone`, `sweep`, `reap`, `popup`; `wt --help`) |
| `waymux init <--tmux\|--systemd\|--hooks>` | Print the tmux plugin line, the systemd unit, or a Claude Code hooks block |
| `waymux doctor` | Check the setup and print a fix for each gap |
| `waymux icon <name>` / `waymux icon --state <state>` | Print one glyph from the icon registry (exit 1 when unknown) |
| `waymux icons [--format tsv\|sh\|tmux]` | Export the icon registry: TSV, `ICON_*` assignments to `eval`, or the `icons:` section for tmux-nerd-font-window-name |

Every command accepts `--help`. Unknown commands exit `2` instead of starting the daemon. `cockpit-items`, `cockpit-preview`, `cockpit-diff`, `cockpit-help`, `agent-action`, `window-create` and `window-delete` are plumbing called by the Cockpit preset and tmux bindings.

---

## ⌨️ Tmux & AI Hook Integration

### 1. Load Plugin in `~/.tmux.conf`

```tmux
run-shell "~/.local/share/waymux/tmux/waymux.tmux"
```

Or bind directly:

```tmux
set -g status-interval 0
set-hook -g pane-focus-in 'run-shell -b "waymux dismiss #{pane_id} >/dev/null 2>&1 || true"'

bind-key Space if-shell -F '#{==:#{session_name},_popups}' 'detach-client' 'run-shell "waymux cockpit"'
bind-key "i"   if-shell -F '#{==:#{session_name},_popups}' 'detach-client' 'run-shell "waymux triage --popup"'
bind-key "I"   run-shell "waymux triage --jump"
bind-key -n 'C-S-i' run-shell "waymux triage --jump"
```

### 2. Agent Hooks

Print the Claude Code hook block and merge it into `~/.claude/settings.json`:

```bash
waymux init --hooks
```

It wires `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Notification` (permission prompts and questions), `Stop` and `SessionEnd` to `waymux hook tmux <Event>`, plus `waymux hook lazygit <Event>` to link review notes to the session. Other agents call the same commands from their own hook config (Antigravity: `~/.gemini/antigravity-cli/hooks.json`, Devin: `~/.config/devin/config.json`). `waymux doctor` confirms the Claude Code wiring.

`SessionStart` leaves the pane `idle` (a fresh session waits at the prompt; `compact` is left untouched) and `UserPromptSubmit` starts the spinner. The window icon follows the agent binary running in the pane, so the agent named in the hook command only applies to unrecognized commands. Devin imports `~/.claude/settings.json` and runs Claude's hooks next to its own; state updates are idempotent and sounds fire only on transitions, so the duplicates are harmless.

---

## 🏛️ Architecture & Ecosystem

```mermaid
flowchart TD
    subgraph UI ["Terminal & Tmux Layer"]
        Tmux["Tmux 3.8+ Native Popups & Panes"]
        Cockpit["waymux cockpit\n(85% × 75% Fleet HUD via wm)"]
        Bar["Event-Driven Tmux Status Bar\n(status-interval 0)"]
    end

    subgraph Agents ["Autonomous AI Coding Agents"]
        Agent["Antigravity / Claude Code / Codex / OpenCode"]
        Hook["waymux hook tmux|lazygit\n(<8ms Native Rust Hook)"]
    end

    subgraph Core ["Waymux Unified Rust Engine"]
        Daemon["waymux daemon\n(Axum JSON-RPC @ 127.0.0.1:4040)"]
        Scanner["waymux cockpit-items\n(~17ms Native /proc & Git Scanner)"]
        Injector["waymux inject & review-notes\n(Atomic Bracketed Paste)"]
        WT["waymux wt\n(Git Worktree Orchestrator)"]
    end

    subgraph Companion ["Companion TUI Engines"]
        WM["waymaker (wm)\n(ACID redb Frecency & Sessions)"]
        LG["lazygitrs\n(.git/info/lines.json Review Notes)"]
    end

    Agent --> Hook
    Hook -->|HTTP /api/status| Daemon
    Daemon -->|Debounced State & Spinners| Bar
    Tmux -->|Prefix+Space| Cockpit
    Cockpit --> WM
    WM --> Scanner
    Tmux -->|Ctrl+G| LG
    LG -->|Press 'S' on diff note| Injector
    Injector -->|tmux load-buffer + paste-buffer -p| Agent
```

---

## 🔌 JSON-RPC 2.0 Control Plane (`127.0.0.1:4040`)

`waymux daemon` runs out-of-the-box with built-in defaults (`127.0.0.1:4040`, 300ms idle debounce) and optionally loads `~/.config/waymux/config.toml`. Client subcommands (`hook`, `cockpit-items`, `triage`, `status`, ...) talk to port `4040` unless `WAYMUX_PORT` is set, e.g. to drive a second, sandboxed daemon in tests.

Protected endpoints authenticate via local Bearer token stored at `$XDG_RUNTIME_DIR/waymux/token` (`0600` permissions):

- **`POST /rpc`**: JSON-RPC 2.0 endpoint supporting:
  - `agentState/update`, `agentState/list`, `agentState/dismiss`, `agentState/wait`
  - `tmux.new_window`, `tmux.select_window`, `tmux.kill_window`, `tmux.list_windows`
  - `tmux.split_pane`, `tmux.select_pane`, `tmux.kill_pane`, `tmux.list_panes`, `tmux.capture_pane`, `tmux.send_keys`, `tmux.select_layout`
  - `tmux.new_session`, `tmux.kill_session`, `tmux.list_sessions`
  - `tmux.display_message`, `tmux.ring_bell`

  Requests follow JSON-RPC 2.0: numeric or string `id`, notifications (no `id`, HTTP 204), batches, and spec error codes (`-32700` parse error, `-32600` invalid request, `-32601` unknown method, `-32602` invalid params, `-32000` tmux failure).
- **`POST /api/status`**: REST endpoint for direct agent state updates (`working`, `idle`, `awaiting_input`, `question`, `permission`, `stalled`, `error`, `closed`).
- **`GET /health` & `GET /ready`**: Liveness and readiness probes.

---

## ⚙️ Configuration & Environment

The daemon reads `~/.config/waymux/config.toml` when present (see [`config/default.toml`](config/default.toml)); everything has a built-in default.

**Icon registry.** `[icons]` maps program/agent names to Nerd Font glyphs (`devin = ""`, `agent` is the generic fallback) and `[theme.states.<state>].icon` sets the state glyphs. Both merge over built-in defaults. waymux uses them for the window rename, the Cockpit, state badges and alert bells. `waymux icon` and `waymux icons` hand the same registry to shell scripts and tmux plugins, so an icon is defined once.

**Reloading.** `systemctl --user reload waymux` (or `kill -HUP`) re-reads the config without dropping agent state: theme, spinner, sounds, `idle_debounce_ms` and `flow_log_hook` change at once (running spinners keep their frames until the window next starts one). A config that fails to parse is logged and the running one kept. `listen_addr`, `port`, `pid_file` and `log_level` need a restart.

**Logging.** `log_level` sets the daemon's log filter: a level (`debug`) or a full `tracing` directive (`waymux=debug,axum=warn`). `RUST_LOG` overrides it.

**Flow events.** `flow_log_hook = "~/bin/flow-log.sh"` runs that program as `<hook> <event> <pane> <extra>` whenever an agent asks a question (`agent_question`), you reply (`human_reply`) or a working agent finishes (`agent_finished`), e.g. to keep a focus or time-tracking log. Unset, nothing runs.

| Variable | Default | Purpose |
| :--- | :--- | :--- |
| `WAYMUX_PORT` | `4040` | Port the client commands talk to (e.g. a sandboxed second daemon) |
| `WAYMUX_COLORS` | unset | Theme `colors.toml`; else `~/.config/waymux/colors.toml`, then Omarchy's active theme, then built-in colors |
| `WAYMUX_REVIEW_MIN_SECS` | `20` | Seconds in lazygitrs (Cockpit `g`) that clear the done-not-reviewed state when nothing was staged, committed or discarded |
| `WAYMUX_SHARE_DIR` | `~/.local/share/waymux` | Where `install.sh` puts the tmux plugin and `scripts/`, and where `wt` looks for bundled helpers |
| `AWT_SWEEP_ROOTS` | `~/.dotfiles:~/dev/github` | Directories `awt sweep --all` scans (each a repository or a folder of repositories) |
| `AWT_SWEEP_KEEP_BRANCHES` | your user name | Branches whose `target/` `awt sweep --clean-targets` never removes |
| `@border_gradient` (tmux option) | unset | Waymaker gradient palette (preset such as `aurora` or `#rrggbb,#rrggbb,...`) for the Cockpit popup: it opens borderless and Waymaker draws a gradient frame |
| `WAYMUX_SKIP_CHECKSUM` | `0` | `1` lets `install.sh` proceed without a published checksum |
| `WAYMUX_VERIFY_ATTESTATION` | `0` | `1` makes `install.sh` also verify each archive's build provenance (needs `gh`; releases from v0.2.4 on) |

Helpers in `~/.config/waymaker/scripts/` override the bundled `awt-*.sh` scripts, so you can customize the worktree flow without forking.

Three optional extension scripts are not bundled and are looked up in the same helper directories, then in `~/.config/tmux/`: `awt-popup.sh` (`waymux wt popup` and the Cockpit's worktree switcher; `wt popup` exits 127 without it), `session-picker.sh` (the Cockpit's session switcher) and `flow-telemetry.py` (run with `--hud` above the Cockpit help).

## 🩺 Troubleshooting

| Symptom | Check |
| :--- | :--- |
| Status bar never changes | `waymux doctor` (daemon and hooks rows); `waymux list` should show your pane while the agent works |
| `waymux cockpit` does nothing | `wm` must be on `PATH` (`./install.sh --with-stack`) |
| `waymux wt ...` prints "awt-*.sh not found" | Rerun `install.sh` to restore `~/.local/share/waymux/scripts` |
| No sound | `doctor`'s sound row; set `[sound] player = "..."` in the config to force one |
| `daemon already running` | Another daemon holds `$XDG_RUNTIME_DIR/waymux/waymux.pid`: `systemctl --user restart waymux` |

## 🧪 Development & Verification

```bash
# Format, lint (zero warnings), and run the unit and integration tests
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test

# Rebuild release binary and restart user systemd service
./scripts/update-daemon.sh
```

### 🐳 Isolated Docker Sandbox

Test the complete Waymux stack (`waymux` daemon, Tmux 3.8 status badges, `wm` Cockpit HUD, `lazygitrs`, inline review notes, and simulated `claude`/`codex`/`opencode` agents) inside a clean container without touching your host Tmux server:

```bash
# Run the automated 8-step end-to-end integration test suite (also runs in CI)
just docker-test

# Launch an interactive Tmux session pre-populated with simulated AI agents & Git diffs
just docker-demo

# Inside the sandbox Tmux session:
#   • Prefix + Space   Open Waymux Cockpit HUD
#   • Prefix + i       Open Alert Triage Popup
#   • Prefix + I       Jump directly to next alerting AI agent
#   • Ctrl + G         Open Lazygitrs side-by-side diff & review notes
#   • scenario cycle   Transition simulated agents across permission/question/idle states
#   • scenario status  Inspect daemon health and Tmux window badges
```

See [AGENTS.md](AGENTS.md) for architecture and module conventions, [CONTRIBUTING.md](CONTRIBUTING.md) for pull request guidelines, and [CHANGELOG.md](CHANGELOG.md) for release history.

## 📜 License

MIT © [Felipe Miranda](LICENSE)
