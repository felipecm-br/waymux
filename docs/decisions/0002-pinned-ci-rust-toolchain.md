# 0002. Pinned Stable Rust Toolchain in CI (`RUST_STABLE`)

## Context

`Cargo.toml` enables `clippy::pedantic` and CI runs `cargo clippy --all-targets --all-features -- -D warnings`. Each Rust point release introduces new pedantic lints or refines existing ones, which can cause unrelated pull requests or scheduled CI runs to fail as soon as GitHub Actions updates its preinstalled `stable` toolchain.

## Decision

Pin the stable Rust version in `.github/workflows/ci.yml` via `env.RUST_STABLE` (alongside the separate `msrv` job that checks `rust-version` from `Cargo.toml`). Dependabot does not bump `RUST_STABLE`.

## Consequences

- Scheduled runs, Dependabot PRs, and feature branches never fail solely because a new Rust release landed upstream.
- Upgrading `RUST_STABLE` is an explicit maintenance commit: run `cargo +<new> clippy --all-targets --all-features -- -D warnings`, satisfy any new lints, and bump `RUST_STABLE` in the same commit.
