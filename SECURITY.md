# Security Policy

## Supported versions

Only the latest release gets security fixes. Upgrade with the installer (`install.sh`), which
verifies each archive's SHA-256 and, with `WAYMUX_VERIFY_ATTESTATION=1`, its signed build
provenance.

## Reporting a vulnerability

Report it privately through GitHub: **Security → Report a vulnerability** on
<https://github.com/felipecm-br/waymux/security/advisories/new>. Please do not open a public
issue. Expect a first reply within a week; a fix and an advisory follow once it is confirmed.

## What is in scope

Waymux runs as your user and drives your terminals, so these matter most:

- **The control plane** (`127.0.0.1:4040`): loopback only, every route except `/health` and
  `/ready` needs the bearer token from `$XDG_RUNTIME_DIR/waymux/token` (directory `0700`, file
  `0600`, compared in constant time). A way to reach it without the token, or from another
  user, is a vulnerability.
- **Untrusted input**: hook stdin from agents, `/rpc` bodies, lazygitrs `lines.json`, tmux, git
  and `gh` output. A crash, hang or command execution from any of them is in scope; these
  parsers are fuzzed (`fuzz/`).
- **Command construction**: values are passed to shells as positional arguments or quoted with
  `sys::shell_quote`. Any way to inject a command through a path, branch, session name or
  `$SHELL` is in scope.
- **Prompt injection into panes** (`waymux inject`, review notes): text reaching a pane that
  does not run an agent.
- **Release integrity**: archives, checksums, attestations and `install.sh`.

Out of scope: anything that already requires running code as your user, and programs you
configure Waymux to run yourself (`flow_log_hook`, `awt-*.sh` helpers, extension scripts).
