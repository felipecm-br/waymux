# AGENTS.md

## Project Overview

**Waymux** (`waymux` / `acpd`) is a unified Rust orchestrator and headless JSON-RPC 2.0 control plane daemon that bridges AI coding agent lifecycle events (`agy`, `claude`, `codex`, `opencode`, `crush`), Tmux state/UI surfaces, Cockpit HUD popups (`wm`), inline code review notes (`lazygitrs`), and Git worktrees (`awt`).

- **Language:** Rust (edition 2024)
- **Binaries:** `waymux` (`src/main.rs`) and the legacy `acpd` alias (`src/bin/acpd.rs`), both calling `waymux::entry()` in `src/lib.rs`
- **Fast-Path CLI:** Synchronous execution (`<1.5ms`) before Tokio runtime initialization
- **Platforms:** Linux is fully supported. On macOS the daemon, hooks, status bar and triage work; `/proc`-based Cockpit CPU/RAM and stall detection are Linux-only
- **Daemon Runtime:** Tokio multi-threaded async runtime + Axum 0.8 (`127.0.0.1:4040`)
- **Deployment:** User-level systemd service (`systemd/waymux.service`, `Type=notify`, `Alias=acpd.service`)

---

## Architecture & Module Map

- `src/lib.rs`: `COMMANDS` table (drives `--help` and `<command> --help`), synchronous CLI fast-path router, and async Tokio daemon entrypoint. Only the bare binary, `daemon` or a leading `--config` start the daemon; anything else unknown exits 2. Print-only commands restore the default SIGPIPE.
- `src/main.rs` & `src/bin/acpd.rs`: Thin binary entrypoints invoking `waymux::entry()`.
- `src/api/`: Axum control plane on the authenticated routes:
  - `mod.rs`: `api_router`, Bearer auth middleware (constant-time compare) and `/api/status`.
  - `rpc.rs`: `/rpc` JSON-RPC 2.0 (single and batch requests, notifications, string/number ids, `-32700`/`-32600`/`-32601`/`-32602`/`-32000` errors) and every `agentState/*` and `tmux.*` method.
  - `state.rs`: `ApiState`, sequence-tracked 300ms idle debounce (`dispatch_update`), pane rehydration, the stale-pane reaper and the stall detector.
- `src/health.rs`: unauthenticated `/health` and `/ready` probes.
- `src/config.rs`: `config.toml` model (`Config`, `ThemeConfig`, `SoundConfig`, `Spinner`) with built-in defaults.
- `src/signals.rs` & `src/pid.rs`: SIGTERM/SIGINT listener feeding graceful shutdown, SIGHUP feeding the config reload in `daemon.rs` (`ApiState::reload` → `OutputAdapter::reload`), and the single-instance PID file.
- `src/daemon.rs`: Background task supervisor, SIGHUP config reload, systemd watchdog heartbeat (`WatchdogSec=30s`, pinged while the pane-state lock is acquirable), 30s stale pane reaper, and 5s proactive stall detector (20s zero-output + zero-CPU delta heuristic).
- `src/adapters/`: Output sinks behind the `OutputAdapter` trait:
  - `mod.rs`: `AgentState`, `AgentUpdate`, `OutputAdapter`, and the urgency/icon helpers the sinks share.
  - `tmux.rs`: tmux window/pane options, multi-pane urgency aggregation, desktop notifications and the working spinner.
  - `waybar.rs`: Waybar status JSON.
  - `sound.rs`: event-isolated sound playback (completion chimes skipped for the pane in view and deferred to a typing pause; blocking alerts immediate).
- `src/icons.rs`: Nerd Font icon registry (`[icons]` and `[theme.states.*].icon` over built-ins) behind `waymux icon` / `waymux icons`, the hook's window rename, the Cockpit and the state badges.
- `src/auth.rs`: Session token generator (`0600` permissions at `$XDG_RUNTIME_DIR/waymux/token` mirrored to `$XDG_RUNTIME_DIR/acpd/token`).
- `src/doctor.rs`: `waymux doctor` setup checks (daemon, token, tmux, git, `wm`, `lazygitrs`, `delta`, `gh`, sound player, awt scripts, Claude Code hooks, theme, and on Linux an installed systemd unit missing directives the shipped one has) with a fix per gap; exits 1 on a required failure.
- `src/sys.rs`: `find_in_path` (`command -v`), `shell_quote` (one POSIX word), and `default_sound_player` (`pw-play` → `paplay` → `aplay` → `afplay`).
- `src/rpc_client.rs`: Zero-dependency synchronous TCP HTTP client for CLI subcommands (`status`, `list`, `dismiss`, `health`, `agentState/list`, `/api/status`).
- `src/tmux_cmd.rs`: The one place tmux is spawned (`command()` / `async_command()`, always with `-u` so tab-separated `-F` rows and glyphs survive a non-UTF-8 locale; unit tests get a private `-L waymux-unit-test` socket), synchronous helpers, and atomic bracketed-paste buffer injection (`load-buffer` + `paste-buffer -p`).
- `src/theme.rs`: Theme `colors.toml` parser (path from `status_palette::theme_colors_path`: `$WAYMUX_COLORS`, `~/.config/waymux/colors.toml`, then Omarchy's active theme; built-in colors otherwise), TrueColor ANSI escape generator, and ANSI stripper.
- `src/status_palette.rs`: Semantic agent-state colors (built-in light/dark palettes, `[theme.states.*].color` overrides) shared by the status bar, Waybar and the Cockpit.
- `src/cockpit/`: Native Cockpit HUD engine:
  - `mod.rs`: `waymux cockpit` popup launcher, `@SEGMENT_BG`/`@FG`/`@ACCENT_COLOR` Waymaker color resolver, and `__ACTION__` dispatch loop.
  - `items.rs`: `waymux cockpit-items` (native `/proc/<pid>/stat` & `/proc/<pid>/comm` scanner, Git status read in parallel per pane path, PR cache in `$XDG_RUNTIME_DIR/waymux/pr-cache`, 12-column TSV emitter; ~17ms with 10 panes).
  - `preview.rs`: `waymux cockpit-preview` (contextual agent card, PR badge, review notes, terminal snapshot).
  - `diff.rs`: `waymux cockpit-diff` (diff header card, untracked files, flushed stdout into `delta --side-by-side` / `bat`).
  - `help.rs`: `waymux cockpit-help` (telemetry HUD and keybinding cheat-sheet).
  - `notes.rs`: `waymux review-notes` (`serde_json` parser/mutator for `<git-dir>/info/lines.json`).
  - `actions.rs`: `waymux agent-action`, `waymux window-create`, `waymux window-delete`.
- `src/presets/`: Embedded Waymaker preset (`cockpit-picker.toml`) materialized dynamically to `$XDG_RUNTIME_DIR/waymux/cockpit-picker.toml`.
- `src/triage.rs`: `waymux triage --popup` and `waymux triage --jump` alert navigation over a stateless queue (`service_class`: permission → question → error/stalled, oldest first), shared with the Cockpit sort.
- `src/hook.rs`: `waymux hook {tmux|lazygit}` native sub-10ms AI agent lifecycle hooks.
- `src/inject.rs`: `waymux inject` bracketed-paste prompt injector with staleness guard.
- `src/wt/mod.rs`: `waymux wt` (and `argv[0] == "awt"`) Git worktree orchestrator. Delegates to `awt-*.sh` helpers found in `~/.config/waymaker/scripts`, `~/.config/matchmaker/scripts`, then `$WAYMUX_SHARE_DIR/scripts` (default `~/.local/share/waymux/scripts`); a missing helper exits 127 with the searched paths.
- `scripts/awt/`: The bundled `awt-*.sh` helpers that `install.sh` copies to `~/.local/share/waymux/scripts`. Keep them free of author-specific paths (`AWT_SWEEP_ROOTS`, `AWT_SWEEP_KEEP_BRANCHES` hold the personal defaults).
- `src/init.rs`: `waymux init {--tmux|--systemd|--hooks}`: plugin line, the shipped `systemd/waymux.service` (embedded with `include_str!`), and a Claude Code hooks block.
- `src/fuzzing.rs` & `fuzz/`: feature `fuzzing` exposes one entry point per untrusted input (hook stdin, `lines.json`, `config.toml`, `/rpc` bodies) for the `cargo fuzz` targets in `fuzz/fuzz_targets/`; `fuzz/seeds/` holds real samples and every past crash. CI fuzzes each target 60s weekly.
- `src/test_strategies.rs`: test-only `proptest` strategies (agent/tmux/`gh`-shaped JSON and hostile strings) shared by the property tests; `proptest-regressions/` keeps shrunk failures so they replay on every run.
- `tests/common/mod.rs`: the shared harness for the end-to-end tests below (`Env`: temp `HOME`, runtime and tmux dirs, a `PATH` with only `git`, repository helpers).
- `tests/tmux.rs`: End-to-end tests of `agent-action`, `window-create`, `window-delete` and `triage --jump` against a private tmux server per test (`Env::tmux`), skipped without tmux.
- `tests/wt.rs`: End-to-end tests of `waymux wt` with fake `awt-*.sh` helpers that record their arguments (lookup order, exit 127, forwarded arguments) and real `git worktree` creation (`-c`, `-W`, post-create hook).
- `tests/cockpit.rs`: End-to-end tests of `cockpit-diff`, `cockpit-preview`, `cockpit-help` and `review-notes` against real temporary git repositories, with only `git` on `PATH` (no `delta`/`bat`, no tmux).
- `tests/cli.rs`: End-to-end tests that start the built binary on a free port with its own `HOME`, `XDG_RUNTIME_DIR` and `TMUX_TMPDIR`, so they never touch the developer's tmux server, sounds or token.

---

## Agent Skills

Skills live in `.agents/skills/` and are symlinked into `.claude/skills/` for Claude Code:

- `waymux`: using, setting up and troubleshooting Waymux (CLI, RPC methods, states, hooks, `doctor`).
- `waymux-dev`: the development loop, where each kind of test goes, recipes for new commands/RPC methods/hooks, performance budgets, and the release steps.
- `rust-daemon`: general Rust + systemd daemon patterns.

Update the matching skill in the same commit as a change to commands, RPC methods, states or the release flow.

---

## Development & Verification Commands

### Build & Install Locally

```bash
# Build release binaries and restart user systemd service
./scripts/update-daemon.sh
```

### Quality Gates (Required Before Every Commit)

All three checks must pass with **0 warnings and 0 failures**:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

> Current baseline: **127 unit tests** (in `src/`, 15 of them `proptest` properties) and **34 integration tests** (`tests/cli.rs`, `tests/cockpit.rs`, `tests/tmux.rs`, `tests/wt.rs`) passing, with **0 Clippy warnings** and **71.7% line coverage** (CI fails below 70%: `cargo llvm-cov --summary-only --fail-under-lines 70`).

### Dependency Audit & Performance Budgets (also run in CI)

```bash
cargo deny check                 # RustSec advisories, licenses, sources (deny.toml)
cargo build --release && ./scripts/bench.sh --check   # hot-path budgets; add a baseline binary to compare
```

### End-to-End Sandbox

```bash
# 8-step suite in an isolated tmux container (also runs in CI)
WAYMUX_BIN=$PWD/target/release/waymux ./scripts/docker-sandbox.sh test
```

---

CI also runs Clippy and the tests on macOS, a `cargo check` on the MSRV (Rust 1.88) and the Docker suite.

## Engineering Conventions

1. **Preserve the Synchronous Fast-Path:** Never initialize Tokio or heavy global state before matching CLI subcommands in `src/lib.rs`. CLI subcommands (`hook`, `dismiss`, `cockpit-items`, `triage`) are invoked on interactive hot-paths and must start in `<1.5ms`.
2. **Spawn tmux Only Through `tmux_cmd`:** Use `tmux_cmd::command()` / `async_command()` (or the helpers built on them), never `Command::new("tmux")`, so unit tests can never reach the developer's tmux server.
3. **Use `anyhow::Result` & Safe Error Handling:** Avoid `.unwrap()` in production code paths; handle missing files, dead PIDs, or unreachable sockets gracefully.
4. **Flush Before Piping:** When printing Rust headers before spawning child processes that inherit stdout (e.g., `delta` or `bat` in `cockpit/diff.rs`), always call `std::io::stdout().flush()` first.
5. **Never Splice Values Into Shell Strings:** Pass paths and names to `sh -c` as positional arguments (`"$1"`), or quote each word with `sys::shell_quote` when tmux needs one command string (`display-popup`, `split-window`).
6. **No Author-Specific Paths:** Resolve paths from `$HOME`, `$XDG_RUNTIME_DIR` and config, with a working fallback when an optional file (Omarchy theme, sounds, dotfile scripts) is absent. Per-user runtime files go in `presets::runtime_dir()`, never `/tmp`.
7. **New Commands Go In `COMMANDS`:** Add the entry (usage, one-line about, `plumbing` flag) and the router arm together; `waymux doctor` should learn about any new external dependency.
8. **Lints:** `[lints.clippy]` in `Cargo.toml` enables `clippy::pedantic`, minus the API-surface lints (`must_use_candidate`, `missing_*_doc`) and `too_many_lines`. Allow anything else only on the item, with a comment saying why.
9. **Atomic Commits:** Keep commits focused, tested, and formatted.
10. **Storage & Worktree Hygiene:**
   - Before finishing any task, remove temporary files or scratch worktrees created during the task, and only those. Never delete files or worktrees you did not create without asking the user.
   - `target/` grows without bound as Rust test binaries accumulate across edits. Run `cargo clean` in completed feature worktrees and after every release (once `./scripts/update-daemon.sh` has installed the updated binaries).
