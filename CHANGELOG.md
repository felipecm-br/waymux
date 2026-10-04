# Changelog

All notable changes to **waymux** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-10-04

### Changed
- **Waymaker Engine Bump**: Upgraded `waymaker` (`wm`) to `v0.2.0` with enhanced session matching and Nucleo optimizations.
- **Dynamic Release Asset Resolution**: Refactored `.github/workflows/release.yml` to dynamically resolve engine repositories and pinned versions from `waymux-manifest.json` via `jq`.
- **Hardened Popup Isolation**: Synchronized `scripts/tmux-popup-isolate.sh` with graceful exit code handling (`129`, `130`, `1`) to eliminate spurious termination errors on popup dismiss/cancel.
- **Release Matrix Refinement**: Streamlined release targets to match upstream binary availability (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`).

## [0.1.0] - 2026-09-24

### Added
- **Unified Terminal AI Multiplexer Stack**: Integrated distribution bundling `acpd`, `waymaker`, `lazygitrs`, and hardened Tmux backdrop isolation.
- **Zero-Flicker Backdrop Isolation**: `tmux-popup-isolate.sh` freezes active terminal panes during rapid AI token streaming so popups never flicker or redraw-corrupt.
- **Universal Zero-Compilation Installer**: Single command `install.sh` with automated architecture detection, sha256 checksum verification, and instant activation.
- **Biomechanical Keyboard Ergonomics**:
  - `Ctrl+g`: Instant full-screen diff review with inline AI prompts (`lazygitrs`).
  - `Prefix + s`: Sub-millisecond workspace and session picker (`waymaker`).
  - `Prefix + C-j`: Seamless zero-latency toggle to previous session.
- **Multi-Agent Protocol Integration**: `acpd` async broker managing multi-pane transitions, live spinners, and audio cues without PTY tearing.
- **Native Worktree Port Discovery**: Headless review note dispatch injected directly into active AI agent panes via bracketed paste.
- **Global Review Notes Protection**: Automated configuration of `.git/info/lines.json` and global gitignore to eliminate repository status pollution.
