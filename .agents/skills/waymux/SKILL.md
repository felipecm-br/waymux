---
name: waymux
description: "Use, set up, and troubleshoot Waymux, the tmux AI-agent cockpit and JSON-RPC daemon on 127.0.0.1:4040. Use when an agent runs inside a Waymux-managed tmux and needs to inspect or control panes/windows/sessions, wait on or dismiss agent states, send review notes or prompts to another agent, or manage worktrees with `waymux wt`; and when a user wants to install Waymux, wire agent hooks (Claude Code, Antigravity, Devin), or asks why the status bar, Cockpit, sounds or `wt` are not working."
---

# Waymux: Use, Set Up, Troubleshoot

Waymux is one binary (`waymux`, legacy alias `acpd`): a synchronous CLI for interactive hot paths and a Tokio daemon (`waymux daemon`) that tracks every agent pane's state and paints tmux, Waybar, notifications and sounds. Every command accepts `--help`; unknown commands exit 2.

## 1. First, check the setup

```bash
waymux doctor
```

It reports `[ok]`, `[warn]` or `[FAIL]` for the daemon, token, config file, tmux, git, `wm`, `lazygitrs`, `delta`, `gh`, `notify-send`, a sound player, the `awt-*.sh` helpers, Claude Code hooks and the theme, with a `fix:` line per gap. Exit 1 means the daemon, config, tmux or git is missing or broken. Run it from a tmux pane, and run it again after each fix.

## 2. Install and wire it up

```bash
curl -fsSL https://raw.githubusercontent.com/felipecm-br/waymux/main/install.sh | bash
# or from a checkout: ./install.sh --from-source   (add --with-stack to build wm and lazygitrs)
```

Then:

1. tmux: add `run-shell "~/.local/share/waymux/tmux/waymux.tmux"` to `~/.tmux.conf` (`waymux init --tmux` prints it with the bindings). Keys: `prefix+Space` Cockpit, `prefix+i` triage popup, `prefix+I` or `C-S-i` jump to the next waiting agent, `C-g` lazygitrs. Override with `@waymux_key_*` options.
2. Hooks: `waymux init --hooks` prints a Claude Code `hooks` block. Merge it into `~/.claude/settings.json`; never overwrite the user's existing hooks, append the `waymux hook ...` commands to the matching events.
3. Daemon: `systemctl --user enable --now waymux` (installed by `install.sh`). On macOS run `waymux daemon` yourself.

## 3. Agent states

| State | Meaning | Who sets it |
| --- | --- | --- |
| `working` | Agent is running tools or thinking | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse` hooks |
| `permission` | Waiting for the user to approve a tool | `Notification` / `PermissionRequest` hook |
| `awaiting_input` (alias `question`) | Agent asked the user something | `PreToolUse` of a question tool |
| `stalled` | No output and no CPU for 20 s while working (Linux only) | Daemon stall detector |
| `error` | Agent reported an error | RPC or hook |
| `idle` | Finished; the Cockpit shows it as "done, not reviewed" until the worktree is opened in lazygitrs | `Stop` hook |
| `closed` | Session ended | `SessionEnd` hook |

The triage order everywhere (status bar, `triage --jump`, Cockpit) is permission, then question, then error/stalled, oldest first.

## 4. CLI for agents

- `waymux list`: TSV `pane_id  state  last_timestamp_ms`, one line per tracked pane.
- `waymux status`: health plus the same states, human-readable.
- `waymux dismiss <pane_id>`: clear a permission/question/stalled/error badge back to idle.
- `waymux inject <workspace_dir> <prompt>`: paste a multi-line prompt into the agent pane working in that directory (bracketed paste, so newlines do not submit early).
- `waymux review-notes <count|list|send> <repo_path> [session] [idx] [pane_id]`: lazygitrs review notes from `<git-dir>/info/lines.json`; `send` pastes them to the agent.
- `waymux quota --recommend` / `--json`: the agent a new session should launch and every agent's quota level (`green`, `yellow`, `red`, `unknown`), Claude Code built in, other agents from `[quota] providers`. Check it before starting a long fan-out of agents.
- `waymux launch --peek --auto`: the agent and model a new session would start (`--pick` shows the menu and prints the pick; never run `launch` without `--peek` or `--pick` from an agent, since it replaces the process).
- `waymux wt`: one worktree per tmux session. `wt -c <branch> [base] [-- cmd]` creates one and runs a command in it; `wt <branch>` switches; `wt rm`, `rebase`, `ship`, `pr [n]`, `clone`, `sweep`, `reap`; bare `wt` opens the picker. `wt --help` lists flags. `ship` merges, pushes and deletes the branch, and `rm` deletes one: confirm with the user first.

## 5. JSON-RPC (`POST http://127.0.0.1:4040/rpc`)

Authenticate with the Bearer token in `$XDG_RUNTIME_DIR/waymux/token` (`0600`). Requests without it get 401. Clients honor `WAYMUX_PORT`.

The endpoint follows JSON-RPC 2.0: `id` may be a number or a string, a request without `id` is a notification (runs, HTTP 204, no body), an array is a batch, and malformed JSON or a request without `"jsonrpc":"2.0"` and `method` gets error `-32700` / `-32600` in the body instead of an HTTP error.

```bash
TOKEN=$(cat "$XDG_RUNTIME_DIR/waymux/token")
rpc() { curl -s -X POST "http://127.0.0.1:${WAYMUX_PORT:-4040}/rpc" \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$1"; }

# Block until another agent's pane finishes (or 600 s pass)
rpc '{"jsonrpc":"2.0","method":"agentState/wait","params":{"pane_id":"%12","target_state":"idle","timeout_secs":600},"id":1}'
```

| Method | Params |
| --- | --- |
| `agentState/list` | none |
| `agentState/update` | `pane_id`, `state`, optional `timestamp` |
| `agentState/dismiss` | `pane_id` |
| `agentState/wait` | `pane_id`, `target_state`, `timeout_secs` |
| `tmux.list_sessions` / `list_windows` / `list_panes` | `target` where it applies |
| `tmux.new_session` | `name`, optional `directory`, `command` |
| `tmux.new_window` | optional `name`, `target`, `command` |
| `tmux.split_pane` | optional `command`, `target_pane`, `vertical` |
| `tmux.send_keys` | `target`, `keys` (array), optional `literal` |
| `tmux.capture_pane` | `target`, optional `start_line`, `end_line`, `escape_sequences` |
| `tmux.select_window` / `select_pane` / `select_layout` / `kill_*` / `display_message` / `ring_bell` | `target` (+ method-specific fields) |

`tmux.send_keys` and `kill_*` act on the user's live tmux: target only panes you created or were asked to touch.

## 6. Troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| Status bar never changes | Hooks not wired, or daemon down | `waymux doctor`; while the agent works, `waymux list` should show its pane |
| `waymux cockpit` does nothing | `wm` (Waymaker) not on PATH | `./install.sh --with-stack` |
| `awt-*.sh not found` (exit 127) | Bundled helpers missing | Rerun `install.sh`; helpers in `~/.config/waymaker/scripts` take precedence |
| Daemon restarted every 30s, `Failed with result 'watchdog'` in the journal | A newer unit (`WatchdogSec=`) runs an older binary without the heartbeat | Install the binary and unit together: `./install.sh`, or `waymux init --systemd` from the same version |
| `doctor` warns the systemd unit is older | Installed unit lacks the watchdog or sandboxing settings | Run the fix it prints; customized values are kept as long as no setting is missing |
| No sound | No player found, or sounds muted | `doctor` sound row; set `[sound] player` in `~/.config/waymux/config.toml` |
| `daemon already running` | Another daemon holds the PID file | `systemctl --user restart waymux` |
| PR badges empty | `gh` not authenticated, or a `gh` wrapper printing to stdout | `gh auth status`; wrappers must keep stdout clean |

Config: `~/.config/waymux/config.toml` (reference: `config/default.toml` in the repo). `log_level` sets the log filter (`RUST_LOG` wins); `flow_log_hook` names a program run on `agent_question`, `human_reply` and `agent_finished`; `keylog = true` (or `WAYMUX_KEYLOG=1`; `WAYMUX_KEYLOG=0` always disables) enables key telemetry in `$XDG_STATE_HOME/flow/keys.tsv`. After editing the config, `systemctl --user reload waymux` applies theme, spinner, sound, debounce and hook changes without losing agent state (address, port, PID file and log level need a restart); a broken file is logged in `journalctl --user -u waymux` and ignored. Theme: `$WAYMUX_COLORS`, else `~/.config/waymux/colors.toml`, else Omarchy's theme, else built-in colors. Gradient Cockpit frame: set the tmux option `@border_gradient` to a Waymaker palette (`aurora`, `#rrggbb,#rrggbb,...`); empty keeps the plain border.
