# Waymux repository audit: 2026-11-04 (v0.11.0)

> **Phase 1 status (branch `fix/phase1-audit`):** S1, C1, S2 and C2 fixed; C3 withdrawn.

A full pass over code, performance, configuration, documentation, CI/supply chain and the TUI
surfaces, two weeks and nine releases after the October audit that produced
[`ROADMAP-TO-10.md`](../ROADMAP-TO-10.md). Every number below was measured on this machine on
the day (Arch Linux, kernel 7.1, tmux 3.8, release build of `daa6b71`). Commands are given so
each can be re-run.

## Verdict

The engineering base is still state of the art for a single-maintainer Rust CLI/daemon: gates
are green, the security model is deliberate, the supply chain is pinned and attested. What
slipped is **drift**. v0.5 → v0.11 added launch, switch, quota, phone, checkpoints, the delivery
queue, approvals and risk rules (about 9k lines), and the docs, test baseline, mutation scope,
fuzz targets and benchmarks did not follow. Two user-facing defects and one shell-injection
path also came in with that work.

| Area | Score | One-line reason |
|---|:---:|---|
| Code quality & conventions | 9 | pedantic Clippy clean, `unsafe` denied (2 documented exceptions), `tmux_cmd` and `sh -c "$1"` conventions held everywhere |
| Correctness & tests | 7.5 | 255 unit + 48 e2e pass; coverage 80.69%, **0.69 above the CI floor**; new modules thinly tested |
| Performance | 7 | fast path within budget; **`cockpit-items` 73 ms vs the documented ~17 ms**; no budget for new interactive commands |
| Security | 8 | scoped tokens, constant-time compare, 0700/0600 discipline; **tmux plugin shell injection**; `/m` has no security headers |
| Configuration | 9 | `deny_unknown_fields`, SIGHUP reload keeps the old config on error, systemd hardening recorded in ADR 0001 |
| Documentation | 6 | README current; **AGENTS.md module map missing 11 modules**, roadmap says v0.2.4, `waymux-dev` skill predates 4 commands |
| CI & supply chain | 8.5 | SHA-pinned, Scorecard, CodeQL, SBOM, provenance; **8 Dependabot PRs open up to 11 days**, no shellcheck |
| TUI ergonomics | 8 | H=0 bindings, bichromatic footers ≤ 7 chunks, cyan focus bar; leaked harness XML in titles, one preset styling gap |
| **Overall** | **8.0** | the 10/10 from October is not current: each gap below has a bounded fix |

## 1. Quality gates (measured)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass, 0 warnings |
| `cargo test` | 255 unit + 48 integration (16 cli, 11 cockpit, 11 tmux, 10 wt), 0 failed |
| `cargo deny check` | advisories, bans, licenses, sources ok |
| `cargo llvm-cov --summary-only` | **80.69% lines** (floor 80%), 81.62% functions |
| Release binary | 4.46 MB stripped, thin LTO, `codegen-units = 1` |
| CI (last 8 runs) | all green, incl. release `v0.11.0` |

AGENTS.md still states 237 unit tests and 81% coverage: the baseline has not been refreshed since
`docs(agents)` on 2026-10-30.

## 2. Findings

Severity: **P0** exploitable or data-losing · **P1** user-visible defect or security weakness ·
**P2** drift that will bite the next change · **P3** polish.

### Security

| # | Sev | Finding | Evidence | Fix |
|---|:--:|---|---|---|
| S1 | **P1** | The tmux plugin wraps `#{pane_current_path}` in single quotes inside `run-shell`. A directory whose name contains `'` breaks the command, and a crafted name (a cloned repo, an extracted archive) runs arbitrary shell when the user presses `prefix+u` or `prefix+a`. Violates convention 5. | `tmux/waymux.tmux:60`, `:66`. Reproduced on a private server: `run-shell "echo '#{pane_current_path}'"` in `…/it's` → `returned 2`. | Use tmux's own quoting: `run-shell "waymux quota --popup #{q:pane_current_path}"` (verified: renders `it\'s`). Add a `tests/tmux.rs` case that sources the plugin and fires the binding in a directory named `a'b;touch pwned`. |
| S2 | P2 | `/m` serves an inline-script page with only `Content-Type` and `Cache-Control`. The page holds the phone token in memory, so it should forbid framing, referrers and foreign script. | `src/api/mod.rs:183-195` | Add `Content-Security-Policy: default-src 'none'; script-src 'sha256-…'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'`, `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`. Compute the hash in a unit test from `PAGE` so an edit to the script fails CI instead of silently breaking the page. |
| S3 | P2 | `scripts/lazygit-tmux-injector.sh` writes predictable `/tmp/waymux-active-pane-*.txt` / `/tmp/agy-active-pane-*.txt` (symlink-clobber class). It and `scripts/tmux-popup-isolate.sh` are referenced only from the CHANGELOG. | `scripts/lazygit-tmux-injector.sh:34-35`; `grep -r` finds no caller | Delete both (superseded by `waymux inject` and the native popups), or move them to `presets::private_runtime_dir()`. |
| S4 | P3 | The CLI token lookup reads `/tmp/waymux-<uid>/token` without the ownership check that `private_runtime_dir()` applies to the same path. A planted token only causes a 401, but the two code paths should agree. | `src/rpc_client.rs:24-25` vs `src/presets/mod.rs:23-39` | Accept a `/tmp` candidate only when its directory passes the same uid/0700 test. |
| S5 | P3 | `adapters/waybar.rs` falls back to `/tmp` when `XDG_RUNTIME_DIR` is unset (convention 6). | `src/adapters/waybar.rs:13` | Use `presets::private_runtime_dir()`. |

Strengths worth keeping: 128-bit tokens from `/dev/urandom`, constant-time compare against every
stored token, scoped tokens that re-parse the body under a 64 KiB limit before dispatch, the
phone page reading its token from the URL fragment and clearing it with `history.replaceState`,
`textContent`-only DOM writes, and the daemon bound to `127.0.0.1`.

### Correctness & UX defects

| # | Sev | Finding | Evidence | Fix |
|---|:--:|---|---|---|
| C1 | **P1** | Synthetic prompts overwrite the agent's title. Claude Code delivers background-task results and slash-command bodies as `UserPromptSubmit` prompts (`<task-notification>…`, `<command-message>…`, `<system-reminder>…`), and `extract_session_title` copies them verbatim into `@ai_agent_title`. The Cockpit, triage and status bar then show markup instead of the task. | Live Cockpit row today: `waymaker/main … <task-notification> <ta… main working`. Code: `src/hook.rs:149` | If the trimmed prompt starts with `<` and an ASCII tag name, keep the current title (return `None` without unsetting it). Proptest: any prompt starting with `<tag>` never changes an existing title. |
| C2 | P2 | `cockpit-picker.toml` sets `[results.current_style] modifier = "BOLD"` but no `bg = "reset"`, so Waymaker's default `Color::Black` selection strip draws an opaque bar on transparent terminals. `menu-picker.toml` already has the reset. | `src/presets/cockpit-picker.toml:49-50` | Add `bg = "reset"`. |
| C3 | — | **Withdrawn.** This audit first recommended dimming the menu's other rows (`DIM`) instead of `fg = "gray"`. That reverses a measured decision: `f8d040a` stopped dimming rows because `DIM` on muted metadata falls to about 1.3:1, and `presets::tests` enforce it. `gray`/`white` are ANSI palette slots the terminal theme remaps, so the light-theme claim was not shown either. | `src/presets/mod.rs:144`, `:164`; CHANGELOG "Legible metadata" | None. Record the no-DIM rule as `docs/decisions/0005-no-dimmed-rows.md` so the next audit finds it. |
| C4 | P3 | `C-S-i` / `C-S-I` are bound in the root table unconditionally and with no `@waymux_key_*` option. They fire only when `extended-keys` is on, which the plugin does not set or check, so for most users they are dead bindings they cannot rename. | `tmux/waymux.tmux:41-42`; private server reports `extended-keys off` | Gate behind `@waymux_key_triage_root` (default `off`), and print a hint in `waymux doctor` when it is set but `extended-keys` is off. |
| C5 | P3 | The quota popup treats a lone `0x1b` as quit, so an arrow key (`ESC [ A`) closes it. | `src/quota/popup.rs:224` | After `0x1b`, poll 20 ms for a following byte; quit only on a bare `Esc`. |
| C6 | P3 | Triage popup is `80% × 75%`; the Cockpit (the other dense triage surface) is `85% × 75%`. | `src/triage.rs:337` vs `src/cockpit/mod.rs:560` | One `DENSE_POPUP` constant used by both. |

### Performance

Measured with `scripts/bench.sh` (sandboxed daemon, 50 runs, idle machine, two passes) and a
20-run loop for the interactive commands against the live tmux server (4 panes).

| Command | Budget | Measured | Status |
|---|---|---|---|
| `--version` | 1.5 ms | 0.39–0.59 ms | ok |
| `health` | 1.5 ms | 0.73–0.89 ms | ok |
| `list` | 1.5 ms | 0.92–1.22 ms | ok |
| `status` | 1.5 ms | 1.12–1.24 ms | ok, **83% of budget** |
| `dismiss %1` | 1.5 ms | 0.98–1.03 ms | ok |
| `hook tmux Stop` | 10 ms | 0.97–1.49 ms | ok |
| `cockpit-items` | ~17 ms (docs) | **72.9 ms** (4 panes) | **4× the documented figure** |
| `switch --peek` | none | 47.6 ms | no budget |
| `launch --peek` | none | 9.2 ms | no budget |
| `quota --badge` | none | 6.2 ms | no budget (runs in the status bar) |
| `doctor` | none | 10.3 ms | fine |

| # | Sev | Finding | Fix |
|---|:--:|---|---|
| P1 | **P1** | **`cockpit-items` is bound by serial git calls per repository.** Repos run in parallel, but each one does `git config --get-regexp` (filter/textconv probe) → `git status --porcelain -b` → `git diff --shortstat HEAD` in series. On `~/.dotfiles/main` that is 13.5 + 23.8 + 19.8 ms ≈ 57 ms of the 73 ms total. The Cockpit reloads after every `d`/`Ctrl+D` (plus a fixed `sleep 0.05`), so a delete takes about 125 ms before the list settles, above the 100 ms Doherty threshold. | (a) Replace the config probe with a native read of `.git/config` and `config.worktree` (fall back to the subprocess only when an `[include]` is present), saving one fork per repo. (b) Run `status` and `diff --shortstat` concurrently; they are independent once the probe has passed. (c) Pass `-c core.untrackedCache=true` to `status`. (d) Cache the result per repo in `runtime_dir()/git-meta`, keyed on the mtimes of `index` and `HEAD`, like the PR cache: a reopen with no change spawns no git at all. Target: ≤ 20 ms cold and ≤ 8 ms warm with 10 panes. |
| P2 | P2 | `scripts/bench.sh --check` failed every fast-path row when run right after `cargo build --release` (1.9–3.5 ms) and passed every row idle. CI uses a slack factor; locally the gate is noisy enough to be ignored. | Warm up with 5 discarded runs, report the median rather than the mean, and add `cockpit-items` (against a generated 10-pane private tmux server with 10 temp repos), `quota --badge` (≤ 10 ms) and `switch --peek` (≤ 30 ms) as budgeted rows. |
| P3 | P3 | Every Cockpit keypress spawns `waymux keylog …` through `ExecuteAsync`, even though key telemetry has been opt-in since roadmap item 1, so the default user pays a fork per key for a no-op. | Strip the `ExecuteAsync(… keylog …)` prefix from the bind arrays in `presets::materialize_*` when keylog is disabled. |
| P4 | P3 | `switch --peek` spends 11.6 ms on `display-message` and 4.4 ms on `capture-pane`, as two tmux round-trips. The hook likewise sends `set-option` and `refresh-client -S` separately. | Batch them into one invocation with `\;` (`tmux_cmd` already centralizes the spawn). |
| P5 | P3 | The 300 ms fixed sleep in `tests/tmux.rs:113` contradicts roadmap item 9 ("free of sleeps"). It asserts something did *not* happen, so it cannot use `eventually`. | Replace it with a positive signal: wait for the daemon's armed-risk state through `agentState/list` (or a marker file written by the arm path), then assert the answer file is absent. |

### Tests & verification depth

| # | Sev | Finding | Fix |
|---|:--:|---|---|
| T1 | **P1** | Coverage is 0.69 points above the floor. The lowest files are the newest, and the lowest of all is the module that **types into panes**: `delivery.rs` 39%, `quota/popup.rs` 52%, `launch/effort.rs` 63%, `api/phone.rs` 64%, `launch/mod.rs` 66%, `cockpit/actions.rs` 67%. One more untested feature will turn CI red for an unrelated reason. | Bring `delivery.rs` above 80% first (a recording `TmuxPort`, as roadmap item 5 did for the Cockpit), then `phone.rs` (snapshot/diff/prompt per token scope, through the existing `post_rpc` helper) and `effort.rs` (pure gauge rendering, table-driven). Raise the floor to 82% in the same PR. |
| T2 | P2 | Mutation testing covers 5 modules, all from before v0.5. `delivery`, `api/queue`, `api/approvals`, `risk`, `checkpoint` and `quota/claude` decide when keystrokes reach an agent or whether a risky call is allowed, and none of them is mutated. | Add them to `.cargo/mutants.toml` `examine_globs`; kill the survivors; keep the weekly full run. |
| T3 | P2 | Fuzzing covers 4 inputs. Untrusted JSON that arrived later is not fuzzed: quota provider output (`[quota] providers`), the `statusline` stdin from Claude Code, `switch` `rows_command` rows, and `launch` provider rows. | Add `fuzz/fuzz_targets/{quota_provider,statusline,switch_rows}.rs` with real samples in `fuzz/seeds/`. |
| T4 | P2 | Shell is about a quarter of the attack surface (`install.sh`, `scripts/awt/*.sh`, `docker/*.sh`, `tmux/waymux.tmux`) and is checked only with `bash -n`. v0.8.1 shipped a flag-parsing bug in exactly these scripts. | Add a `shellcheck -S warning` step to `check-and-test`, and a `bats` (or Rust e2e) smoke test of each `awt-*.sh --help` and unknown-flag exit code. |

### Documentation drift

| # | Sev | Finding | Fix |
|---|:--:|---|---|
| D1 | P2 | AGENTS.md's module map omits `activity`, `checkpoint`, `delivery`, `git`, `keylog`, `risk`, `launch/effort` and `api/{approvals,hook_tokens,phone,queue}`. AGENTS.md is what every agent working on the repo reads first. | Add one line per module. Add a test in `tests/cli.rs` (or a CI `grep`) that fails when a `src/**/*.rs` path is absent from AGENTS.md. |
| D2 | P2 | `ROADMAP-TO-10.md` describes v0.2.4 with 145 unit tests and every item done. It reads as current status and is not. | Mark it historical (`Status: closed 2026-10-24`) and link this audit as the successor. |
| D3 | P2 | The `waymux-dev` skill (last touched 2026-10-24) never mentions `quota`, `launch`, `switch`, `phone`, `checkpoint`, `queue`, approvals or risk rules; its budget table repeats the 17 ms figure. AGENTS.md requires the skill to change in the same commit as a command. | Add a recipe per subsystem (where tests go, which hook/provider contract applies) and the new budget rows from P2. |
| D4 | P3 | README and AGENTS claim `cockpit-items` takes ~17 ms and the hook takes ~8 ms. The hook is now ~1 ms; the Cockpit is 73 ms. | Regenerate the figures from `bench.sh` after P1 lands; quote the command next to each number. |
| D5 | P3 | `hook-token` appears in `--help` but nowhere in the README, the `waymux` skill or AGENTS.md, although it is how a jailed agent reports state. | One README subsection under Security, and a `waymux` skill entry. |
| D6 | P3 | No shell completions or man page. Every other subcommand-rich CLI in the stack (Waymaker, lazygitrs) ships them; `COMMANDS` already holds everything needed. | `waymux init --completions {zsh,bash,fish}` generated from `COMMANDS` (no clap dependency), installed by `install.sh`. |

### CI, dependencies & release

| # | Sev | Finding | Fix |
|---|:--:|---|---|
| R1 | P2 | 8 Dependabot PRs are open, the oldest from 2026-10-24: three crate majors (`toml` 0.8 → 1.x, `nix` 0.29 → 0.31, `sd-notify` 0.4 → 0.5), `actions/checkout` 4 → 7, `upload-artifact` 4 → 7, `attest-build-provenance` 3 → 4, and two grouped minor/patch PRs (both green). | Merge #2 and #9 now. Take the majors one PR each: `toml` 1.x changes error types (check `config.rs` messages and the `fuzz/config` seeds); `nix` 0.31 renames some `signal`/`unistd` items; `sd-notify` 0.5 changes `NotifyState`. |
| R2 | P3 | `NO_COLOR` is not honoured anywhere. `theme.rs` emits TrueColor unconditionally, which garbles `waymux quota`, `status` and `doctor` when piped into logs or CI. | Check `NO_COLOR` and `!stdout.is_terminal()` once in `theme.rs`; the plumbing commands that feed Waymaker keep ANSI. |
| R3 | P3 | The 300 ms debounce, 20 s stall threshold and 30 s reaper are constants. `idle_debounce_ms` is already configurable; the stall and reaper intervals are not. | Expose `stall_after_secs` and `reap_interval_secs` in `Config`, behind `deny_unknown_fields` like the rest. |

## 3. TUI ergonomics review (Cockpit, launch/switch menus, quota, triage)

### 3.1 KLM-GOMS and response time

| Flow | Today | After the plan | Driver |
|---|---|---|---|
| Open the Cockpit (`prefix Space`) | T_K 240 ms + T_R ≈ 73 ms (items) + popup draw | T_R ≤ 20 ms cold / ≤ 8 ms warm | P1 git-meta cache |
| Delete a window (`d`) and see the list settle | T_K 120 + T_R 50 (sleep) + 73 ≈ **243 ms** | 120 + ≤ 20 ≈ 140 ms | P1, drop the fixed sleep in favour of `window-delete` returning after `kill-window` |
| Step effort in a launch menu (`Ctrl+L`) | one `waymux … --effort` fork per press (~2 ms) | unchanged | already under the threshold |
| Read which agent needs you | T_M high when the title is `<task-notification>` (the user must open the pane) | T_M ≈ 0 | C1 |

T_H stays 0 and T_P stays 0 in every flow: no default binding needs the mouse, an arrow key, an
F-key or `Alt`. `mouse = true` in both presets is a convenience, not a requirement.

### 3.2 Home row validation

- All defaults are `prefix` + one letter (`Space`, `u`, `a`, `A`, `s`, `L`) or `Ctrl`+home-row
  (`C-g`, `C-h/l`, `C-d`, `C-r`, `C-p`, `C-t`, `C-s`), reachable with the CapsLock overload.
- `C-h` / `C-l` for effort in the menus mirror `h`/`l` tree semantics in Waymaker, and the
  arrows are kept only as secondary aliases. This is consistent.
- `Esc` unwinds in one step everywhere except the quota popup, where an arrow press is also read
  as `Esc` (C5).
- `C-S-i` (C4) is the only chord off the rule: a Shift+Ctrl chord on the right hand that needs
  `extended-keys`. Making it opt-in removes the one exception.

### 3.3 Visual semiotics

- Footers: both presets follow the bichromatic standard (`cyan,bold` key, muted action,
  `__MUTED_LINE__` `│` separators, no brackets) and stay at or under 7 chunks (Cockpit 7, menu 5).
- Focus: `Plain` nav bar with a cyan / lightcyan `│` at X = 1 and `BOLD` in both presets.
- Gap: the Cockpit's opaque selection strip (C2), a one-line TOML change. Rows are deliberately
  not dimmed (C3, withdrawn): the contrast floor beats generic two-tier dimming here.
- Hue reservation holds in chrome. The new effort gauge uses warm hues (`high` yellow, `xhigh`
  orange, `max` red), which is acceptable because effort is operational state (cost), not
  navigation, but **red now means two things in the same popup** (quota ≥ 90% and effort `max`).
  Consider ending the gauge's thermal scale at orange and leaving red to quota exhaustion and
  errors.

### 3.4 Layout

Popup geometry is consistent at `75% × 60%` for the ephemeral menus (quota, launch, switch,
session) and `85–90% × 75–88%` for the dense surfaces (Cockpit, lazygitrs), with the Cockpit's
preview at 60% on the right (40/60). The one outlier is triage at `80% × 75%` (C6).

```text
╭─ 󱂬 Cockpit ───────────────────────────────────────────────────────────────╮
│ > query                                                                    │
│ │ 󰳌 1 _dotfiles/main   new features were added…  main ⇅  +95 -43   31m  │ preview 60%
│   ⠋ 2 waymaker/main    fix effort gauge colors    main ⇅   -          2s │ (card, PR, notes,
│   · 3 lazygitrs/main   idle                       main ⇅   -          4h │  terminal snapshot)
│                                                                            │
╰─ Enter Jump │ y/n Ack │ i Input │ g Git │ r Notes │ ? Help │ Esc Exit ────╯
```

The rows are illustrative. Row 2 shows what C1 fixes: on the live server today that title
cell reads `<task-notification> <ta…`.

## 4. Improvement plan

Each phase is one branch and one PR. Every commit passes the AGENTS.md gates, and a phase that
touches tmux, the Cockpit or `wt` also passes `./scripts/docker-sandbox.sh test`.

### Phase 1: defects and security (P1s), about one day

1. **S1** quote `#{q:pane_current_path}` in `tmux/waymux.tmux`; e2e test with a hostile
   directory name. *Done when* the test creates no `pwned` file.
2. **C1** ignore synthetic `<tag>` prompts in `extract_session_title`; proptest. *Done when* a
   `<task-notification>` prompt leaves an existing title unchanged.
3. **S2** security headers on `/m`, with the CSP hash test.
4. **C2** `bg = "reset"` on the Cockpit's selected row. (C3 withdrawn; see the findings table.)
5. Release **v0.11.1** with CHANGELOG entries for 1–4.

### Phase 2: Cockpit latency (P1), two to three days

1. Add the `cockpit-items` bench row first, with a generated 10-repo private tmux fixture, so
   the improvement is measured against a baseline.
2. Native config probe, concurrent `status`/`diff`, `core.untrackedCache`.
3. Git-meta cache keyed on `index`/`HEAD` mtimes, invalidated by `window-delete` and
   `Ctrl+R`.
4. Drop `sleep 0.05` from the delete binds: make `window-delete` return only after
   `kill-window` completes.
5. Strip keylog `ExecuteAsync` calls when keylog is off (P3); batch tmux round-trips (P4).

*Done when* `bench.sh --check` holds `cockpit-items` ≤ 20 ms cold and ≤ 8 ms warm at 10 panes,
and README/AGENTS/skill quote the new figures (D4).

### Phase 3: verification depth (T1–T4), about one week, incremental

1. `delivery.rs` behind a recording tmux port, to ≥ 80%; then `phone.rs`, `effort.rs`,
   `launch/mod.rs`, `quota/popup.rs`.
2. Raise the coverage floor to 82%, and later 85%, in the same commits that earn it.
3. Extend `.cargo/mutants.toml` to `delivery`, `api/queue`, `api/approvals`, `risk`,
   `checkpoint`, `quota/claude`; kill the survivors.
4. Three new fuzz targets (quota provider, statusline, switch rows) with seeds.
5. `shellcheck` in CI plus `awt-*.sh` flag-handling smoke tests.
6. Replace the 300 ms sleep in `tests/tmux.rs:113` with a positive signal.

*Done when* coverage ≥ 85%, 0 unexplained surviving mutants in 11 modules, 7 fuzz targets
running weekly, and shellcheck clean.

### Phase 4: documentation and maintenance (D1–D6, R1–R3), two days

1. AGENTS.md module map and baseline numbers, plus the guard test that keeps the map complete.
2. `waymux-dev` skill: recipes for quota/launch/switch/phone/checkpoint/queue and the new
   budget rows; `waymux` skill and README entry for `hook-token`.
3. Close `ROADMAP-TO-10.md` as historical and point it at this audit.
4. Dependabot: merge the grouped PRs; one PR per major (`toml`, `nix`, `sd-notify`, the three
   action majors).
5. `NO_COLOR` / non-TTY handling; `stall_after_secs` and `reap_interval_secs` config keys.
6. Shell completions via `waymux init --completions`.
7. Delete the orphaned `scripts/lazygit-tmux-injector.sh` and `scripts/tmux-popup-isolate.sh`
   (S3); fix the `/tmp` fallbacks (S4, S5).

### Phase 5: polish (P3 TUI)

C4 (opt-in `C-S-i`), C5 (quota popup escape sequences), C6 (one dense-popup constant), and a
decision on the effort gauge's top colour (§3.3), recorded as `docs/decisions/0005-…` if red
stays.

## 5. Keeping it at 10

The October audit reached 10/10 and drift brought it back to 8 within two weeks, because the
gates check code but not the claims around it. Three cheap guards stop the next round:

1. **A module-map test** (D1): CI fails when a source file is missing from AGENTS.md.
2. **A budget for every interactive command** in `bench.sh` (P2): the 17 ms → 73 ms slide
   would have failed CI the day it happened.
3. **Release checklist line** in the `waymux-dev` skill: "new module → mutants scope, fuzz
   target if it parses untrusted input, skill recipe, AGENTS map". It goes in the same commit
   as the bump.

## Appendix: re-running the measurements

```bash
cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test
cargo deny check
cargo llvm-cov --summary-only            # per-file table; sort by the Lines Cover column
cargo build --release && RUNS=50 ./scripts/bench.sh   # run twice, idle machine
# Interactive commands against the live tmux server (20 runs each):
for c in cockpit-items "quota --badge" "launch --peek" "switch --peek"; do
  s=$(date +%s%N); for _ in $(seq 20); do target/release/waymux $c >/dev/null 2>&1 </dev/null; done
  echo "$c $(( ($(date +%s%N) - s) / 20000 ))us"; done
# Which git call dominates cockpit-items: put timing shims for git/tmux first on PATH.
```
