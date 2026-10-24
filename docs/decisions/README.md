# Architectural Decision Records

Short records of engineering decisions in Waymux that might otherwise look like oversights or unfinished hardening. Before relaxing or tightening one of these settings, read the matching record.

| ID | Decision |
| --- | --- |
| [0001](0001-systemd-user-unit-sandboxing.md) | User `waymux.service` uses seccomp (`6.1` exposure), not mount-namespace or `W^X` options |
| [0002](0002-pinned-ci-rust-toolchain.md) | CI pins `RUST_STABLE` and bumps it deliberately with new `clippy::pedantic` lints |
| [0003](0003-branch-protection-single-maintainer.md) | Branch protection requires all six CI checks on PRs while keeping admin bypass for single-maintainer workflow |
| [0004](0004-tmux-utf8-flag.md) | Every `tmux` invocation passes `-u` to preserve tab-separated `-F` rows and Nerd Font glyphs in non-UTF-8 locales |
