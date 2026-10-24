# 0004. Passing `-u` on Every `tmux` Invocation

## Context

Waymux queries tmux with tab-separated format strings (`tmux list-panes -F "#{pane_id}\t..."`) and writes Nerd Font glyphs into window names and status options. When `waymux` is invoked from an environment without a UTF-8 `LC_ALL`, `LC_CTYPE`, or `LANG` (such as `env -i`, minimal systemd environments, cron, or clean test harnesses), tmux's client defaults to ASCII mode and replaces both non-ASCII UTF-8 bytes and control characters like `\t` in `-F` output with `_`.

When tabs become `_`, tab-separated rows fail to split: agent states read as empty, `window-delete` misses the active-agent guard, and `triage --jump` finds no waiting panes.

## Decision

Centralize all tmux process spawning in `src/tmux_cmd.rs` (`command()` and `async_command()`) and pass `-u` as the first argument on every call.

## Consequences

- Tab-separated `-F` output and Nerd Font glyphs survive regardless of the caller's locale environment variables.
- `tests/tmux.rs` intentionally runs with a cleared environment (`env_clear()`) so any regression that bypasses `tmux_cmd` fails immediately.
