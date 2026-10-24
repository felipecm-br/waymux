# 0001. User `waymux.service` Sandboxing Stop-Line (`6.1` Exposure)

## Context

`systemd/waymux.service` runs as a user-level systemd service (`systemctl --user`) and spawns `tmux`, sound players (`pw-play`, `paplay`, `aplay`), and the user's configured `flow_log_hook`.

`systemd-analyze --user security waymux.service` scores the shipped unit at **6.1 MEDIUM** (down from 9.0 UNSAFE before seccomp filtering was added in commit `19c7afa`). Reaching a lower score would require enabling mount-namespace directives (`ProtectSystem=`, `ProtectHome=`, `PrivateTmp=`, `PrivateDevices=`) and `MemoryDenyWriteExecute=yes`.

## Decision

Keep the seccomp and capability restrictions (`NoNewPrivileges=yes`, `UMask=0077`, `LockPersonality=yes`, `RestrictRealtime=yes`, `RestrictSUIDSGID=yes`, `RestrictNamespaces=yes`, `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`, `SystemCallArchitectures=native`, `SystemCallErrorNumber=EPERM`, `SystemCallFilter=@system-service` and `~@mount @swap @reboot @clock @module @raw-io @obsolete @debug @cpu-emulation`), and **do not** enable mount-namespace directives or `MemoryDenyWriteExecute=yes`.

## Consequences

1. **Portability across Linux distributions:** In a `--user` unit, `ProtectSystem=`, `ProtectHome=`, and `PrivateTmp=` require unprivileged user namespaces, which Ubuntu AppArmor profiles and hardened kernels restrict for user services.
2. **Filesystem & socket reachability:** The daemon must reach the user's tmux socket (`/tmp/tmux-<uid>/default` or `$TMUX_TMPDIR`), PipeWire/PulseAudio sockets in `$XDG_RUNTIME_DIR`, sound assets under `$HOME`, and whatever path `flow_log_hook` writes to.
3. **Hook runtime compatibility:** `NoNewPrivileges=yes` causes seccomp filters to be inherited by every child process. `MemoryDenyWriteExecute=yes` would break any `flow_log_hook` implemented in a JIT runtime (Python, Node.js, Bun, LuaJIT).
