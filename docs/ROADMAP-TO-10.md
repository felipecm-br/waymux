# Roadmap to 10/10: handoff

Where Waymux stands after the October 2026 audit, what keeps it from a 10, and the work items
that close the gap. Each item says why it matters, where to change things, how to know it is
done, and how to check. Read `AGENTS.md` first; its conventions and quality gates apply to
every item here.

## Where it stands (v0.2.4, 2026-10-24): about 9/10

| Area | State |
|---|---|
| Tests | 140 unit (15 `proptest` properties) and 37 end-to-end (`tests/cli.rs`, `cockpit.rs`, `tmux.rs`, `wt.rs`): real daemon, real git, private tmux server |
| Coverage | 80.0% lines; CI fails below 78% (`cargo llvm-cov --summary-only --fail-under-lines 78`) |
| Fuzzing | `fuzz/`: four libFuzzer targets (hook stdin, `lines.json`, `config.toml`, `/rpc`); seeds replay past crashes; weekly CI job |
| Lints | `clippy::pedantic`, `unsafe_code = "deny"`, 0 warnings |
| Toolchain | MSRV 1.88 (`rust-version`, CI job); CI stable pinned at 1.99 (`RUST_STABLE` in `ci.yml`) |
| Platforms | Linux and macOS in CI; releases for x86_64/aarch64, glibc, musl, macOS |
| systemd | `Type=notify`, SIGHUP reload, watchdog, seccomp sandbox (`systemd-analyze security`: 6.1) |
| Supply chain | actions and Docker base images pinned to SHAs/digests, read-only tokens, cargo-deny, Dependabot (cargo, actions, docker), OpenSSF Scorecard, CodeQL SAST, build provenance and SBOM on releases, checksum-verifying installer |
| Repository | branch protection (six required checks, admin bypass, no force-push), `SECURITY.md`, private vulnerability reporting, secret scanning |

The items below are what remain. Items 1 to 4 are small and close most of the distance;
5 and 6 are the larger engineering work.

## 1. Make key telemetry opt-in — Done

**Why.** `waymux keylog` (`src/keylog.rs`) writes every Cockpit key press to
`$XDG_STATE_HOME/flow/keys.tsv` for every user unless `WAYMUX_KEYLOG=0`. Recording a user's
input by default is a privacy problem, and it is the pattern the audit removed from the flow
log (now `flow_log_hook`, off unless configured).

**What.** Log only when enabled: a `keylog = true` key in `config.toml` (read the same way the
CLI reads other settings, without starting Tokio) or `WAYMUX_KEYLOG=1`. Keep `WAYMUX_KEYLOG=0`
as an override that always wins. Document it next to `flow_log_hook` in the README and
`config/default.toml`, and add an upgrade note to the CHANGELOG (the author's own setup needs
`keylog = true`).

**Done when.** A fresh `HOME` produces no `keys.tsv` after Cockpit use, and the setting turns
it on. `keylog.rs` coverage rises from 33% with tests for off, on, and the override (now 97.8%).

**Check.** `cargo test keylog`; an end-to-end case in `tests/cockpit.rs` asserting the file is
absent with the default config.

**Effort.** Small.

## 2. Reject unknown config keys — Done

**Why.** `Config` (`src/config.rs`) silently ignores unknown keys, so a typo such as
`flow_log_hok = "..."` disables the feature with no message. SIGHUP reload makes this easier
to hit.

**What.** `#[serde(deny_unknown_fields)]` on `Config`, `ThemeConfig`, `SoundConfig`,
`AgentStateTheme` (not on `Spinner` maps or `[icons]`, which are open tables). At startup an
unknown key is an error naming the key and file; on reload it is logged and the running config
is kept (that path already exists in `daemon::reload`). Add a `doctor` row that parses the
config and reports the first error.

**Done when.** `waymux daemon --config` with a typo exits non-zero naming the key; a reload
with a typo keeps the old config; `doctor` shows the error.

**Check.** A unit test in `config.rs`; extend
`tests/cli.rs::sighup_reloads_the_config_and_keeps_it_when_the_new_one_is_broken` with a typo
case; re-run `fuzz/config` for 60s.

**Effort.** Small. First check that `config/default.toml` and the docs use only known keys.

## 3. Drop the avoidable `unsafe` — Done

**Why.** Three `unsafe` blocks remain. Two are `libc::isatty` (`src/hook.rs:12`,
`src/cockpit/mod.rs:20`) that std's `IsTerminal` (stable since 1.70) replaces. The third,
`signal(SIGPIPE, SigDfl)` in `src/lib.rs`, is needed and documented.

**What.** Replace both `isatty` calls with `std::io::IsTerminal`. Then add
`unsafe_code = "deny"` under `[lints.rust]` in `Cargo.toml`, with a local
`#[allow(unsafe_code)]` and its existing `SAFETY` comment on the SIGPIPE block.
(`src/fuzzing.rs` sets environment variables and needs the same local allow.)

**Done when.** `grep -rn 'unsafe' src` lists only the SIGPIPE block and `fuzzing.rs`, both
with `SAFETY` comments, and the lint keeps new ones out.

**Check.** The gates; `./scripts/bench.sh --check` (the `hook` path must stay under 10ms).

**Effort.** Small.

## 4. Score the repository with OpenSSF Scorecard and fix its findings — Done

**Why.** No external measurement exists yet. Scorecard is the standard one, and it will flag
the remaining supply-chain gaps below.

**What.**
- Add `.github/workflows/scorecard.yml` using `ossf/scorecard-action` (pin by SHA like the
  other actions; `permissions: read-all` plus `security-events: write` and `id-token: write`),
  publishing results, and add the badge to the README.
- **Pinned dependencies:** `docker/Dockerfile` uses `ARG RUST_IMAGE=rust:latest` and
  `RUNTIME_IMAGE=debian:sid-slim`. Pin both by digest (`rust:1.99-slim@sha256:...`,
  `debian:sid-slim@sha256:...`), and add a `docker` entry to `.github/dependabot.yml` so
  digests stay current.
- **SAST:** add a static-analysis workflow. CodeQL supports Rust; pin it and run it on push
  and weekly (`.github/workflows/codeql.yml`).
- **Fuzzing credit:** Scorecard may not count `cargo fuzz` in a plain workflow. Wrap the
  existing targets in ClusterFuzzLite (`.clusterfuzzlite/`), which reuses `fuzz/`.
- **CII Best Practices:** register at bestpractices.dev (owner web signup) and add the badge;
  most criteria are already met (`SECURITY.md`, tests, CI, signed releases).

**Done when.** Scorecard runs weekly and its score is at least 8. (`Code-Review` is capped for
a single-maintainer repository; see `docs/decisions/`.)

**Effort.** Medium (mostly configuration).

## 5. Make the Cockpit loop testable (`src/cockpit/mod.rs`, 84.3% coverage) — Done

**Why.** `run_cockpit` is the largest untested code: a 400-line loop that spawns the `wm`
picker, reads the chosen `__ACTION__` row, and dispatches on string prefixes. A regression in
a key binding (ship, kill, PR, lazygit) is only caught by hand or by the Docker suite.

**What.** Split it in two, following the build-then-render splits already done in
`cockpit/items.rs` (`3acc10e`) and `triage.rs` (`ea53939`):
1. A pure `parse_action(chosen: &str) -> Action` (an enum: `Jump`, `Ship`, `OpenPr`,
   `LazyGit`, `SwitchAwt`, `Kill`, ...) with the row fields it needs.
2. `execute(action, &Ctx) -> Flow` (`Flow::Reopen | Flow::Exit(code)`), where `Ctx` holds the
   few side-effecting operations (tmux, spawn) behind a trait so tests can record calls.

This also removes the `#[allow(clippy::needless_continue, clippy::redundant_else)]` on
`run_cockpit`: every branch returns a `Flow` instead of `continue`/`return`.

**Done when.** Every `__ACTION__` prefix has a `parse_action` test, every `Action` has an
`execute` test with a recording `Ctx`, `cockpit/mod.rs` is above 60%, and the CI floor rises
to 75% in the same commit.

**Check.** The gates; `./scripts/docker-sandbox.sh test` (8/8) since this is the interactive
path; a manual run of the Cockpit (`Space` in tmux) through each key in the footer.

**Effort.** Large. Do it on its own branch, in steps that each keep the Docker suite green.

## 6. Prove the tests catch bugs: mutation testing

**Why.** Coverage shows what runs, not what is checked. During the audit, deliberately broken
code (`ship` without `--push`, a broken `-W` stash, a disabled reload) was the only proof that
the new tests catch regressions. That should be systematic.

**What.** Run `cargo mutants` (cargo-mutants) on the modules with logic, starting with
`triage.rs`, `cockpit/items.rs`, `api/rpc.rs`, `api/state.rs`, `hook.rs`. Kill surviving
mutants with tests, or mark the ones that are equivalent with `#[mutants::skip]` and a reason.
Add a weekly CI job (`cargo mutants --in-diff` on PRs once the baseline is clean).

**Done when.** No unexplained surviving mutants in the listed modules; the CI job is green.

**Effort.** Medium to large, depending on how many mutants survive at first.

## 7. Raise the remaining low-coverage files — Done

After item 5: `presets/mod.rs` (25% → 94.5%), `tmux_cmd.rs` (61% → 89.3%: bracketed paste
via `tests/tmux.rs`), `inject.rs` (62% → 93.8%), `hook.rs` (62% → 83.9%). Total line
coverage reaches 80.0% and the CI floor is raised to 78%.

**Effort.** Medium, incremental.

## 8. Record the decisions that look like gaps

Some things are deliberately not "maximal". Write them as short decision records in
`docs/decisions/` so nobody undoes them:

- **systemd exposure 6.1, not lower.** Mount-namespace options (`ProtectSystem=`,
  `ProtectHome=`, `PrivateTmp=`) need unprivileged user namespaces in a user unit, which some
  distributions restrict, and the daemon must reach the tmux socket, PipeWire and wherever
  `flow_log_hook` writes. `MemoryDenyWriteExecute=` would break a JIT-based hook. Tested with
  `systemd-run --user` (see the `fix(systemd)` commit).
- **CI stable toolchain pinned** (`RUST_STABLE`): bumped on purpose because pedantic lints
  change with each release. Dependabot cannot bump it.
- **Branch protection with admin bypass:** one maintainer; CI is required on PRs, direct
  pushes stay possible.
- **`tmux -u` on every call** (`tmux_cmd.rs`): without it a non-UTF-8 locale turns tabs into
  `_` and breaks every tab-separated row.

**Effort.** Small.

## 9. Keep CI fast and stable as it grows

Not a gap yet, but watch it: the suite now runs six required jobs plus the weekly fuzz job.
Add `Swatinem/rust-cache` (pinned) to the Rust jobs if they pass 5 minutes, and keep
`tests/tmux.rs` and `tests/wt.rs` free of sleeps longer than `common::eventually`'s polling.

## 10. Housekeeping found along the way

- `waymux doctor` could report the installed binary version against the latest release, so
  an outdated install (and its mismatch with the systemd unit's watchdog) shows up there.

## Working on these

- One item per branch; the commits in an item are atomic and pass the gates in `AGENTS.md`
  (`cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo test`). For anything touching tmux, the Cockpit or `wt`, also run the Docker sandbox.
- Open a PR: `main` requires `check-and-test`, `macos`, `msrv`, `coverage`, `dependencies` and
  `docker-e2e` to pass.
- Update the matching skill (`.agents/skills/waymux*`), `AGENTS.md` baseline numbers, and the
  CHANGELOG `[Unreleased]` section in the same commit as the change.
- When an item is done, mark it here with the commit or PR, and update the table above.
