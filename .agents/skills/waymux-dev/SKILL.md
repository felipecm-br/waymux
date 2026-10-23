---
name: waymux-dev
description: "Develop, test, and release the Waymux codebase (Rust CLI + Tokio daemon for tmux AI-agent orchestration). Use when changing anything under src/, tests/, scripts/awt/, install.sh, the Cockpit preset or the systemd unit; when adding a CLI subcommand, RPC method, hook event or adapter; when benchmarking hot paths; when updating the Docker sandbox; or when cutting a release (version bump, CHANGELOG, tag, update-daemon.sh)."
---

# Waymux Development

Read `AGENTS.md` first: it holds the module map and the engineering conventions. This skill is the workflow around them.

## The loop

1. Branch off `main` (never commit on `main`).
2. Change the code and add the test in the same commit.
3. Run the gates; all three must be clean:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test
   ```
4. For anything touching tmux flows, also run the sandbox: `WAYMUX_BIN=$PWD/target/release/waymux ./scripts/docker-sandbox.sh test` (after `cargo build --release`). CI runs it too.
5. Commit with a conventional subject (`fix(cockpit): ...`) and a body that says what was wrong and how it was checked. Add a CHANGELOG `[Unreleased]` entry for user-visible changes.

## Where a test goes

| Change | Test |
| --- | --- |
| Pure logic (parsing, ordering, colors, classification) | `#[cfg(test)]` unit test in the same file |
| CLI behavior, exit codes, anything that talks to the daemon | `tests/cli.rs`: use `Sandbox::new()` and `spawn_daemon()`; it isolates `HOME`, `XDG_RUNTIME_DIR`, `TMUX_TMPDIR` and the port |
| tmux panes, popups, bracketed paste, Cockpit rendering | `docker/entrypoint.sh` step in the Docker sandbox |
| HTTP routes and auth | Unit test that sends requests through `api_router(...)` with `tower::ServiceExt::oneshot` |

Never run tests that call tmux, play sounds or write `$XDG_RUNTIME_DIR/waymux` against the developer's real session: go through `tests/cli.rs` or Docker. Check that a new test fails when the code it covers is broken.

## Recipes

**New CLI subcommand**
1. Add a `CommandSpec` to `COMMANDS` in `src/lib.rs` (usage starts with the name; `plumbing: true` if only the Cockpit or tmux bindings call it).
2. Add the router arm in `entry()`, before the daemon fallback. Keep it synchronous: no Tokio, no heavy init.
3. If it needs an external tool or file, add a check to `src/doctor.rs`.
4. If it only prints, the SIGPIPE reset already applies. If it writes to a socket on a hot path, add it to the exclusion list in `restore_default_sigpipe`.
5. Add a `tests/cli.rs` case for its exit codes, and document it in the README command table.

**New RPC method:** add the arm in `call()` (`src/api/rpc.rs`) with a `Deserialize` params struct parsed by `strict()` (params required) or `lenient()` (omitted params mean defaults; the struct needs `Default`). Return `Ok(value)` or `Err(RpcError::new(code, msg))`; the envelope, id and notification handling are done for you. Build tmux with `crate::tmux_cmd::async_command()` and finish with `run(cmd, "...")` or `stdout(cmd)` + `rows(...)`. Add a unit test through `respond()` and a row in the README and in the `waymux` skill.

**New hook event or agent:** extend `run_tmux_hook` / `resolve_window_icon` in `src/hook.rs`, add the event to `CLAUDE_HOOKS` in `src/init.rs` if Claude Code emits it, and cover the stdin JSON shape with a unit test.

**Shelling out:** use `Command::new(..).args(..)`. When `sh -c` is unavoidable, pass values as positional arguments (`sh -c 'cd "$1" && ...' sh "$path"`). When tmux needs one command string, quote each word with `sys::shell_quote`. Use `sys::find_in_path` to probe for tools.

**Bundled scripts (`scripts/awt/`):** these are the shipped copies of the author's dotfile helpers; `~/.config/waymaker/scripts` overrides them at runtime. Keep them free of personal paths (use env vars with defaults, like `AWT_SWEEP_ROOTS`), run `bash -n` on them, and compare `--dry-run` output with the old version. They use `set -euo pipefail`, so a `&&` list or `return 1` as the last command of a loop exits the script.

## Performance budgets

| Path | Budget | How to measure |
| --- | --- | --- |
| `--version`, `health`, `list`, `status`, `dismiss` | < 1.5 ms | Average of 30 runs of the release binary |
| `hook tmux <Event>` | < 10 ms | Same, against a sandboxed daemon |
| `cockpit-items` | ~17 ms with 10 panes | Same, in a live tmux session |

`scripts/bench.sh` measures the first two rows: 30 runs each against a sandboxed daemon (own `HOME`, `XDG_RUNTIME_DIR`, `TMUX_TMPDIR`, port). Copy the previous binary aside first and pass it as the baseline; run twice and report both, because the machine is noisy right after a build:

```bash
cp ~/.local/bin/waymux /tmp/waymux-old    # `cp`, not an alias: the script refuses a missing binary
cargo build --release && ./scripts/bench.sh target/release/waymux /tmp/waymux-old
```

The daemon is always the first binary's, so a daemon-side change shows in both columns; compare against an earlier run of the old build instead. `--check` exits 1 over budget (`BENCH_SLACK=4` in CI). `cockpit-items` needs a live tmux session; time it with a 30-iteration `date +%s%N` loop.

## Dependencies

`cargo deny check` (config in `deny.toml`) runs in CI on every push and weekly. When it flags an advisory, `cargo update -p <crate>` within semver, raise the minimum in `Cargo.toml`, and note the RUSTSEC id in the commit. Add a license to `allow` only if it is permissive.

## Release

1. Bump `version` in `Cargo.toml`, move `[Unreleased]` in `CHANGELOG.md` under the new version with today's date, and commit `chore(release): bump version to X.Y.Z`.
2. Tag `vX.Y.Z` and push the tag; `.github/workflows/release.yml` builds Linux (musl, gnu) and macOS archives, each with a `.sha256` that `install.sh` requires.
3. Locally: `./scripts/update-daemon.sh` installs the binary and restarts `waymux.service`. Ask before running it, because it restarts the user's live daemon. Then `waymux doctor` and `cargo clean`.
