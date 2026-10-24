# 0003. Branch Protection and Single-Maintainer Code Review

## Context

Waymux is maintained by a single author (`felipecm-br`). GitHub branch protection on `main` requires all six CI status checks (`check-and-test`, `macos`, `msrv`, `coverage`, `dependencies`, `docker-e2e`) and blocks force-pushes and branch deletion, while allowing administrator bypass.

External supply-chain scanners such as OpenSSF Scorecard evaluate whether every commit on `main` has an independent human pull-request approval (`Code-Review`).

## Decision

Keep the six required CI checks and no-force-push protection on `main` with administrator bypass enabled, and accept that OpenSSF Scorecard's `Code-Review` check is structurally capped while the project has one maintainer.

## Consequences

- Pull requests (including Dependabot PRs) are gated by the full Linux, macOS, MSRV, coverage, `cargo-deny`, and Docker E2E suite.
- The maintainer is not deadlocked waiting for a second human reviewer who does not exist.
