# Changelog

All notable changes to **Waymux** are documented in this file.

## [Unreleased]

## [0.2.4] - 2026-10-24

> **Upgrading:** install the binary and the systemd unit together (`install.sh`, or `waymux init --systemd` then `systemctl --user daemon-reload`): the new unit's watchdog restarts an older binary every 30s. The daemon no longer runs `~/.config/tmux/flow-log.sh` by itself; set `flow_log_hook = "~/.config/tmux/flow-log.sh"` in `config.toml` to keep it.

### 󰌌 Key Telemetry, A 7-Item Cockpit Footer

- **`waymux keylog` (`src/keylog.rs`, `src/presets/cockpit-picker.toml`, `src/cockpit/mod.rs`)**: nothing showed which shortcuts were used and which were never pressed. `keylog <context> <key> [state] [target] [origin]` appends one TSV line to `$XDG_STATE_HOME/flow/keys.tsv` and always exits 0; `WAYMUX_KEYLOG=0` turns it off. Every Cockpit key starts with an `ExecuteAsync` keylog call carrying the row's state and `session:window`; `Enter` and `Esc` are logged by the Cockpit, and the picker gets the origin window in `WAYMUX_KEYLOG_ORIGIN`, so a report can tell when a shorter path existed (e.g. `Enter` on a waiting agent, `g` on the window the Cockpit came from).
- **Footer (`src/presets/cockpit-picker.toml`)**: 9 key-action pairs, over the 7-item chunk cap, and `r Review` named something other than the review state (`r` sends lazygitrs notes; `g` clears `󰷋`). The footer is `Enter Jump │ y/n Ack │ i Input │ g Git │ r Notes │ ? Help │ Esc Exit`; `a` and `Tab` still work and stay in the `?` help.
- **State glyph defaults (`src/icons.rs`, `config/default.toml`)**: idle `󰗡`, stalled `󰔟`, permission `󰳌` and error `` (codicon chat-sparkle-error) by default, matching the per-state silhouettes the author's config already set; idle and stalled no longer differ only by fill.
- **Flaky daemon tests (`tests/cli.rs`)**: `spawn_daemon` waited 10 s when the daemon died at once (the free port found at setup was taken before it bound) and piped stderr nobody read. A daemon that exits is retried on a fresh port, stderr goes to a file, and the file is quoted when no port works.

### Audit Follow-ups

- **Sound tests leave `$HOME` alone (`src/adapters/sound.rs`)**: `test_resolve_sound_path_isolation` swapped `$HOME` with `unsafe { set_var }` while other tests ran on parallel threads, a data race under edition 2024. Path resolution now takes the home directory as an argument (`resolve_sound_path_in`), and `expand_home` rewrites only a leading `~/`.
- **`log_level` in `config.toml` takes effect (`src/lib.rs`)**: tracing started before the config was read, so only `RUST_LOG` changed the log level. The daemon now reads the config first; `RUST_LOG` still wins, a bare level (`debug`) applies to Waymux and axum, and a value with `=` is a full filter directive.
- **Dead code removed (`src/adapters/tmux.rs`, `src/config.rs`, `src/api/mod.rs`)**: three unused tmux helpers and the `#[allow(dead_code)]` that hid the ignored `log_level`.
- **Flow events go to a configured hook (`src/api/state.rs`, `src/config.rs`)**: the daemon ran `~/.config/tmux/flow-log.sh` when it existed and otherwise appended to `~/.local/state/flow/events.tsv` for every user. It now runs only `flow_log_hook` from `config.toml` (`~/` expands), with the same `<event> <pane> <extra>` arguments, and writes nothing when unset. To keep the old behaviour, set `flow_log_hook = "~/.config/tmux/flow-log.sh"`.
- **SIGHUP reloads the config (`src/daemon.rs`, `src/signals.rs`, `src/adapters/`)**: the unit shipped `ExecReload=kill -HUP`, but the daemon only logged "not implemented". `systemctl --user reload waymux` now re-reads the config and applies theme, spinner, sounds, `idle_debounce_ms` and `flow_log_hook` without dropping agent state, telling systemd `RELOADING=1` then `READY=1`. A config that fails to parse is logged and the running one kept; a changed listen address warns that it needs a restart.
- **systemd watchdog (`systemd/waymux.service`, `src/daemon.rs`)**: `WatchdogSec=30s`; the daemon sends `WATCHDOG=1` every 15s, but only while it can take the pane-state lock every request needs, so a deadlocked daemon is restarted instead of silently freezing the status bar. Install the new unit and binary together (`install.sh`, `waymux init --systemd`): an older binary under this unit is restarted every 30s.
- **Sandboxed unit (`systemd/waymux.service`)**: seccomp limits on the daemon and every child (`SystemCallFilter=@system-service`, `RestrictAddressFamilies`, `RestrictNamespaces`, `RestrictRealtime`, `RestrictSUIDSGID`, `LockPersonality`, `SystemCallArchitectures=native`, `UMask=0077`), with blocked calls failing as `EPERM` instead of killing the process. `systemd-analyze security` drops from 9.0 to 6.1. Mount-namespace options are left out on purpose: user units need unprivileged user namespaces for them, which some distributions restrict, and the daemon must reach the tmux socket, PipeWire and wherever `flow_log_hook` writes. Dropped `network-online.target`, which a loopback daemon does not need.
- **`doctor` checks the installed unit (`src/doctor.rs`)**: on Linux it warns when `~/.config/systemd/user/waymux.service` lacks a directive the shipped unit has, and prints the `init --systemd` command that replaces it. Changed values pass, so a customized unit is not flagged.
- **CI least privilege (`.github/workflows/`)**: workflows default to `contents: read`, and only the two release jobs that publish assets get `contents: write`. Every action is pinned to a commit SHA with its version in a comment, and checkouts no longer leave the job token in `.git/config` (`persist-credentials: false`).
- **Dependabot (`.github/dependabot.yml`)**: weekly PRs for Cargo crates and GitHub Actions, minor and patch bumps grouped into one PR per ecosystem, majors one at a time.
- **Signed releases and an SBOM (`.github/workflows/release.yml`, `install.sh`)**: every release archive gets a GitHub build-provenance attestation, checked with `gh attestation verify <archive> --repo felipecm-br/waymux` or by `install.sh` under `WAYMUX_VERIFY_ATTESTATION=1`; a CycloneDX SBOM (`waymux-<tag>.cdx.json`) is attached to each release.
- **macOS CI (`.github/workflows/ci.yml`)**: Clippy and the full test suite run on `macos-latest`, matching the macOS binaries the release ships.
- **`wt` no longer splices `$SHELL` into a shell string (`src/wt/mod.rs`)**: a new session's first command was built as `sh -c 'tmux wait-for -S <chan>; exec <shell> -l'`, so a `$SHELL` containing `'` or `;` broke it or ran extra commands. The shell and channel are now positional arguments, each quoted as one word (`ready_then_shell`, tested by running the string through `sh`).
- **Extension scripts are found like the awt helpers (`src/wt/mod.rs`, `src/cockpit/`)**: `awt-popup.sh`, `session-picker.sh`, `record-window-state.sh` and `flow-telemetry.py` were read only from `~/.config/tmux/`, and `wt popup` exited 0 silently without one. They are now searched in the awt helper dirs first, then `~/.config/tmux/`; `wt popup` exits 127 naming the searched dirs, and the Cockpit shows a tmux message.
- **tmux output survives a non-UTF-8 locale (`src/tmux_cmd.rs`)**: when waymux ran without a UTF-8 `LC_ALL`/`LC_CTYPE`/`LANG` (cron, `env -i`, some service managers), tmux printed the tabs in `-F` formats and every non-ASCII glyph as `_`. Tab-separated rows stopped splitting, so agent states read as empty: `window-delete` killed a busy agent's window without the confirming second press, and `triage --jump` found no waiting pane. Every tmux call now passes `-u`.
- **Wait reasons stay on one line (`src/hook.rs`)**: `tool_summary` collapsed whitespace in the tool's arguments but only trimmed the tool's name, so an agent sending a name with a newline produced a multi-line reason in the status bar and the Cockpit's rows. Found by the new `hook_input` fuzz target; the crashing input is kept as a fuzz seed.
- **Out-of-range ports no longer wrap (`src/hook.rs`)**: a session port read from JSON was cast with `as u16`, so 70000 became 4464. Values past 65535 now keep the default port. Found by enabling `clippy::pedantic`, now on for the crate (`[lints.clippy]` in `Cargo.toml`).
- **Declared MSRV (`Cargo.toml`, CI)**: `rust-version = "1.88"`, the first release with stable `let` chains (the README said 1.85, which fails with 65 errors). A CI job builds with exactly 1.88.

### 󰀏 Triage Covers Review, Cockpit Flags Shared Repositories

- **Triage visits unreviewed work (`src/triage.rs`, `src/cockpit/items.rs`)**: `triage --jump` and `--popup` served only waiting agents, so finished work marked `󰷋` waited for a Cockpit visit. After the waiting agents, the queue now holds idle agents whose work nobody has reviewed, oldest completion first, the order the Cockpit sorts them in; the jump labels them `review │ Ctrl+G reviews`. `pane_needs_review` applies the Cockpit's rule per pane and runs `git status` only when the work is newer than the last review and HEAD has not moved.
- **Shared repository badge (`src/cockpit/items.rs`)**: two agents in one repository mix their edits, share one diff and one review marker, and one review clears both. Agent rows in such a repository show an orange `󰀏` in place of the `⎇` branch glyph and match the filter `shared`.

### 🩺 Works Beyond One Machine

- **`waymux doctor` (`src/doctor.rs`)**: checks the daemon, token, tmux, git, `wm`, `lazygitrs`, `delta`, `gh`, `notify-send`, a sound player, the worktree helpers, Claude Code hooks and the theme file, and prints a fix for each gap. Exits 1 only when the daemon, tmux or git is missing.
- **Typos no longer start the daemon (`src/lib.rs`)**: `waymux stauts` or `waymux status --help` fell through to daemon mode. Only the bare binary, `daemon` or a leading `--config` start it; unknown commands exit 2. One `COMMANDS` table drives `--help` (plumbing listed separately) and `<command> --help`. Print-only commands restore the default SIGPIPE, so `waymux --help | head` no longer panics.
- **Bundled worktree helpers (`scripts/awt/`, `install.sh`, `src/wt/mod.rs`)**: `waymux wt` delegated to `awt-*.sh` scripts that existed only in the author's dotfiles and exited 0 silently without them. The ten helpers now ship with Waymux and install to `~/.local/share/waymux/scripts`; `~/.config/waymaker/scripts` still wins. A missing helper exits 127 and lists the searched paths. `awt sweep --all` scans `$AWT_SWEEP_ROOTS` and spares `$AWT_SWEEP_KEEP_BRANCHES` instead of hard-coded personal values.
- **Installer (`install.sh`)**: checksums fail closed (missing or mismatched `.sha256` stops the install unless `WAYMUX_SKIP_CHECKSUM=1`) and use `sha256sum` or `shasum`, so macOS installs verify instead of failing. `--with-stack` builds `wm` and `lazygitrs`; it used to be ignored.
- **`waymux init` (`src/init.rs`)**: `--hooks` prints a complete Claude Code hooks block (it printed four comment lines); `--tmux` loads the plugin instead of binding `prefix+c` over tmux's new-window; `--systemd` embeds the shipped unit. Unknown options exit 2.
- **Portable defaults**: theme colors resolve from `$WAYMUX_COLORS`, `~/.config/waymux/colors.toml`, then Omarchy; sound uses the first of `pw-play`, `paplay`, `aplay`, `afplay` (macOS `Glass.aiff` joins the fallbacks); the PID file defaults to `$XDG_RUNTIME_DIR/waymux/waymux.pid`; the flow log no longer falls back to the author's home.
- **macOS stall alerts (`src/api.rs`)**: without `/proc`, CPU always read 0 and quiet agents were flagged stalled. Stall detection is off where `/proc` is missing.

### 🛡️ Hardening

- **Shell quoting (`src/cockpit/`, `src/sys.rs`)**: the PR cache refresh and "open PR / checks" spliced worktree paths into `sh -c` strings inside single quotes; paths now travel as positional arguments, and tmux command strings quote each word with a tested `shell_quote`. The PR cache moved from `/tmp` to `$XDG_RUNTIME_DIR/waymux/pr-cache` and tolerates wrapper output before the JSON (a mise shim around `gh` blanked every PR badge).
- **Auth (`src/api.rs`)**: the middleware test never called the middleware; it now sends requests through `api_router` (401 without or with a wrong token, 200 with the right one). Token comparison is constant-time.
- **JSON-RPC 2.0 conformance (`src/api/rpc.rs`)**: `/rpc` required a numeric `id` and rejected anything else with axum's HTTP 400/415/422 and a plain-text body. It now accepts string ids, runs notifications (no `id`) with an HTTP 204 and no body, serves batches, and answers malformed JSON with `-32700` and invalid request objects (missing `method`, `jsonrpc` other than `"2.0"`, empty batch) with `-32600`. Existing clients are unaffected.
- **Token directory (`src/auth.rs`, `src/rpc_client.rs`)**: the client also looked for `/run/user/1001/...`, the author's uid. When the token falls back to `/tmp/waymux-<uid>`, the daemon now refuses a directory owned by another user instead of writing the token into it.
- **Review notes reach only an agent (`src/inject.rs`, `src/hook.rs`, `scripts/lazygit-tmux-injector.sh`)**: `waymux inject` pastes the prompt and presses Enter, but it accepted any non-editor pane, so a plain shell could run review-note text as commands. It now refuses shells unless the hooks recorded an agent state there. Workspace matching was a string prefix (`/src/api` matched `/src/api-old`, and a pane in `/` matched every workspace); it now compares whole path components and only accepts panes inside the workspace. The active-pane and lazygitrs-registration sentinels and the injector log (which holds prompt text) moved from world-writable `/tmp` to the private runtime dir, so another user can no longer choose the target pane or read your notes. The `notifyCommand` registered with lazygitrs, which lazygitrs runs with `sh -c`, spliced the workspace path inside double quotes; both words are now shell-quoted, so a directory named `$(...)` stays a path.
- **Private runtime dir (`src/presets/mod.rs`)**: `private_runtime_dir()` creates `$XDG_RUNTIME_DIR/waymux` (or `/tmp/waymux-<uid>`) as `0700` and refuses one owned by another user; the PID file, PR cache, sentinels and injector log use it.
- **Cockpit columns (`src/cockpit/items.rs`)**: a pane in a directory whose name holds a tab shifted every later column of its row; text fields now drop tab, CR and LF.
- **Unit tests stay off your tmux (`src/tmux_cmd.rs`)**: `cargo test` used to set options on pane `%1` of the running tmux server and clear the global `@ai_agent_bell`. Every tmux spawn goes through `tmux_cmd::command()` / `async_command()`, which use a private socket under test.

### ⚡ Performance & Build

- **Parallel git status (`src/cockpit/items.rs`)**: `cockpit-items` resolves git metadata for all pane paths on scoped threads; 10 panes went from 31–60 ms to 17–18 ms with identical output.
- **Release profile (`Cargo.toml`)**: thin LTO, one codegen unit and stripped symbols take the binary from 5.5 MB to 3.3 MB; tokio uses 9 named features instead of `full`.

### 🧪 Tests & CI

- **`tests/cli.rs`**: 8 end-to-end tests run the real binary against a sandboxed daemon (own port, `HOME`, `XDG_RUNTIME_DIR`, `TMUX_TMPDIR`): unknown commands, `--help`, a missing daemon, hook → list → dismiss, the PID guard, 401 without the token, `init --hooks` JSON and `doctor`.
- **CI (`.github/workflows/ci.yml`)**: a second job runs the 8-step Docker sandbox suite on every push.
- **Dependency audit (`deny.toml`)**: a CI job runs `cargo deny check` (RustSec advisories, licenses, sources, duplicate crates) on every push and weekly. Its first run flagged two unsound releases in the lockfile, `tokio` 1.52.3 (RUSTSEC-2026-0386) and `anyhow` 1.0.102 (RUSTSEC-2026-0190); both are updated (1.54.1, 1.0.104) and the minimums in `Cargo.toml` raised.
- **Property tests (`proptest`)**: every parser of outside input (the `/rpc` body, hook stdin, tmux/git/`gh` output, theme colors, `shell_quote` through a real `sh`) is checked against random inputs on every `cargo test`; this found the Cockpit tab bug. Line coverage rose from 47.6% to 56.8% (CI fails below 55%): `cockpit-items` (25% → 71%) and triage (20% → 54%) were split so their ordering and formatting run without tmux.
- **Hot-path budgets (`scripts/bench.sh`)**: times `--version`, `health`, `list`, `status`, `dismiss` and `hook tmux Stop` against a sandboxed daemon, optionally next to a baseline binary; `--check` fails over budget. CI runs it with 4x slack.
- **Faster `dismiss` (`src/api/rpc.rs`)**: the daemon cleared `@ai_agent_bell` and refreshed clients before replying, so focus changes took 2.1–2.7 ms; the cleanup now runs after the reply (0.84–0.91 ms).
- **`SessionStart` on long sessions (`src/hook.rs`)**: the hook read the whole transcript to get its first line; on a resumed 13 MB session it took 8.1 ms of its 10 ms budget and grew with the file. It reads the first line only (capped at 1 MiB): 2.7–3.3 ms.

### 󰸌 Gradient Cockpit Frame

- **`@border_gradient` (`src/cockpit/mod.rs`)**: when this tmux option holds a Waymaker gradient palette (a preset such as `aurora`, or `#rrggbb,#rrggbb,...`), `waymux cockpit` opens its popup borderless and passes `WM_BORDER_GRADIENT` / `WM_BORDER_TITLE`, so Waymaker draws a multi-color frame carrying the Cockpit badge. Unset or empty keeps the single-color tmux border.

### 󰈈 Cockpit Says Why An Agent Waits

- **Wait reasons (`src/hook.rs`, `src/rpc_client.rs`, `src/cockpit/items.rs`)**: the hook always sent `message: null`, so the Cockpit could show that an agent waited but not why. `PreToolUse` now sends a one-line tool summary (`Bash: cargo test -p auth`, `Edit: main.rs`, `AskUserQuestion: <question>`; bounded to 120 chars) and remembers it per pane in `$XDG_RUNTIME_DIR/waymux` (`0700`, since commands can hold secrets), so the permission `Notification` that follows carries it; without a preceding tool it falls back to the notification text. `agentState/list` already returned `message`; the client now reads it, and waiting rows show the reason in the title column instead of the task title.
- **`WAYMUX_PORT`**: client subcommands honor it (default `4040`) so end-to-end tests can drive a sandboxed second daemon.
- **Done, not reviewed (`src/cockpit/items.rs`, `src/cockpit/mod.rs`, `src/status_palette.rs`)**: an idle agent looked the same whether or not anyone had checked its work. A row now shows `󰷋` (mint `#7ee0b5` dark / `#166534` light) when the agent finished after the worktree was last opened for review and left something to look at: uncommitted changes, or a HEAD that moved since the review. Closing lazygitrs after a review from the Cockpit (`g`/`p`) touches `<git-dir>/waymux-reviewed`, which clears it; the dotfiles `Ctrl+G` popup does the same. A glance does not count: the session must last `WAYMUX_REVIEW_MIN_SECS` (20 s) or change HEAD or the porcelain status (stage, commit, discard). Unreviewed rows sort right after waiting agents, oldest first, and the Cockpit opens on the first one when no agent is waiting. Detection reads `.git`, the marker and `logs/HEAD` from the filesystem, no extra git processes. The completion time is the daemon's observed working → idle transition (`finished_at` in `agentState/list`), persisted on the pane as `@ai_agent_finished_at` so a daemon restart (e.g. a theme switch) neither re-flags reviewed work nor forgets unreviewed work; rehydration used to stamp every idle pane with the restart time.
- **Legible metadata (`src/presets/cockpit-picker.toml`, `src/presets/mod.rs`, `src/theme.rs`)**: non-selected rows were rendered `DIM` (about 0.5 opacity in ghostty) on top of ANSI darkgray metadata, which is 1.68:1 under lumon, so the rows you scan for urgency were the hardest to read. Rows are no longer dimmed (the selection keeps BOLD and the cyan bar). Theme `muted` is lifted toward the foreground until it reaches 4.5:1 for metadata text and 3:1 for separators and borders (`status_palette::ensure_contrast`), only when the theme falls short; the footer, separators and preview border get those colors through `__MUTED_TEXT__` / `__MUTED_LINE__` when the preset is materialized.

### 🔔 Completion Chimes Respect Focus & Breakpoints

- **Focus-aware, breakpoint-deferred completion chimes (`src/adapters.rs`)**: every alert used to play immediately, including the "done" chime for the pane you were looking at, and done chimes were most of the sound (92 of 95 in one week of logs). Now the completion chime is skipped when a focused client shows that pane, otherwise it waits for a 2 s typing pause (`#{client_activity}`, capped at 15 s), and agents finishing while it waits share one chime. Permission, question, stalled and error alerts still play immediately.

### 🛡️ Cockpit Safety & Triage Signals

- **Ship asks first (`src/cockpit/mod.rs`)**: `S` in the Cockpit merges, pushes to origin and deletes the branch, which cannot be undone. It now passes `--confirm` to `awt-merge.sh`, matching `S` in the awt popup. `split_merge_args` (`src/wt/mod.rs`) is extracted and tested so `--confirm` keeps reaching the script.
- **Guarded kills (`src/cockpit/actions.rs`)**: `waymux window-delete` checks every pane of the target window (or session) against the daemon and `@ai_agent_state_raw`, using the same rule as `close-window.sh` (anything but idle/done counts as in-flight). Idle targets still close on one press (`Prefix+u` restores them); a target with an agent at work arms on the first press and closes on a second press within 2 s. The redundant `D`, `X` and filter-mode `ctrl-x` aliases are removed from the embedded preset; `d`, `x` and `ctrl-d` remain.
- **Triage queue (`src/triage.rs`)**: `triage --jump` and `--popup` served urgent agents newest-first and cycled a stored position over a list that changes as agents are serviced, so handling A in `[A, B, C]` sent the next jump to C. They now use a stateless queue: `service_class` (permission → question → error/stalled), then the longest-waiting agent first, with second and millisecond timestamps normalized. The next target is the entry after the current pane (`--jump`) or after the last peeked pane (`--popup`), else the head.
- **Cockpit follows the same order (`src/cockpit/items.rs`, `src/cockpit/mod.rs`)**: row priority now derives from `service_class` (stalled moves next to error), waiting rows sort oldest-first, and when no selection is saved the cursor opens on the first agent waiting on you instead of the window you came from.
- **Semantic state colors (`src/status_palette.rs`)**: state colors came from the theme's ANSI slots, so monochrome themes (e.g. `lumon`) drew permission, question and stalled in the same blue, and the reference config gave permission and error the same red. The status bar, Waybar and the Cockpit now share one built-in palette per light/dark mode (read from `colors.toml`, falling back to background luminance): dark worst-case CIEDE2000 10.8 including simulated deuteranopia and protanopia, light 18.5 for normal vision, ≥ 4.5:1 contrast on every built-in Omarchy background. `[theme.states.<state>].color` is now optional and still overrides; Cockpit chrome (branches, diffstat, PR badges) keeps following the theme.

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
