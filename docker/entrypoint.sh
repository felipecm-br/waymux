#!/usr/bin/env bash
# ==============================================================================
# Waymux Docker Sandbox Entrypoint
# ==============================================================================
# Modes:
#   demo  (default)  Start waymux daemon, seed sample Git repos + fake AI agents
#                    (claude, codex, opencode), and attach to interactive Tmux.
#   test             Run headless end-to-end integration tests and exit 0/1.
#   <cmd>            Start waymux daemon and exec <cmd> (e.g. `bash`, `zsh`).
# ==============================================================================

set -euo pipefail

export HOME="${HOME:-/home/dev}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/runtime-dev}"
export PATH="/usr/local/bin:${HOME}/.local/bin:/usr/local/share/waymux-sandbox/fake-bin:${PATH}"

mkdir -p "$XDG_RUNTIME_DIR/waymux" "$HOME/.config/waymux"

# 1. Start waymux daemon if not already running
if ! waymux health >/dev/null 2>&1; then
  waymux daemon >/tmp/waymux-daemon.log 2>&1 &
  for _ in $(seq 1 50); do
    if waymux health >/dev/null 2>&1; then
      break
    fi
    sleep 0.05
  done
fi

# 2. Seed realistic sample Git workspaces for Cockpit preview, diffs, and review notes
seed_workspaces() {
  local ws="/home/dev/workspace"
  if [[ -d "$ws/api-service/.git" ]]; then
    return 0
  fi

  mkdir -p "$ws/api-service/src" "$ws/web-app/src"

  git config --global user.name "Waymux Sandbox"
  git config --global user.email "sandbox@waymux.local"
  git config --global init.defaultBranch main

  # Repo 1: api-service (with committed baseline + unstaged diff + inline review notes)
  (
    cd "$ws/api-service"
    git init -q
    cat > src/auth.rs << 'EOF'
pub struct AuthMiddleware;

impl AuthMiddleware {
    pub fn verify_header(header: &str) -> bool {
        header.starts_with("Bearer ")
    }
}
EOF
    cat > src/db.rs << 'EOF'
pub const MAX_CONNECTIONS: u32 = 16;

pub fn connect_pool() -> &'static str {
    "postgres://localhost:5432/app"
}
EOF
    git add .
    git commit -q -m "feat: initial auth and db modules"

    # Introduce realistic working-tree changes so `cockpit-diff` and `lazygitrs` show rich diffs
    cat > src/auth.rs << 'EOF'
pub struct AuthMiddleware;

impl AuthMiddleware {
    pub fn verify_header(header: &str) -> Result<&str, &'static str> {
        if !header.starts_with("Bearer ") {
            return Err("missing bearer prefix");
        }
        // Pending review note targets line 14 below:
        let token = header.strip_prefix("Bearer ").unwrap();
        if token.len() < 16 {
            return Err("token too short");
        }
        Ok(token)
    }
}
EOF
    cat > src/db.rs << 'EOF'
// Pending review note targets line 8 below:
pub const MAX_CONNECTIONS: u32 = 100;

pub fn connect_pool() -> &'static str {
    "postgres://localhost:5432/app?pool_timeout=5"
}
EOF
  )

  # Repo 2: web-app
  (
    cd "$ws/web-app"
    git init -q
    cat > src/theme.ts << 'EOF'
export const palette = {
  bg: "#1e1e2e",
  fg: "#cdd6f4",
  accent: "#89b4fa",
};
EOF
    git add .
    git commit -q -m "feat(ui): add base palette"

    cat > src/theme.ts << 'EOF'
export const palette = {
  bg: "#1e1e2e",
  fg: "#cdd6f4",
  accent: "#89b4fa",
  urgent: "#f38ba8",
  warning: "#f9e2af",
};
EOF
  )

  /usr/local/bin/scenario notes >/dev/null
}

# 3. Provision Tmux topology with fake AI agents (`claude`, `codex`, `opencode`)
provision_tmux_topology() {
  local ws="/home/dev/workspace"
  seed_workspaces

  if tmux has-session -t api-service 2>/dev/null; then
    return 0
  fi

  # Session 1: api-service
  # Window 1: interactive control shell with quick-start guide
  tmux new-session -d -s api-service -n control -c "$ws/api-service"
  # Window 2: simulated `claude` agent waiting for tool permission
  tmux new-window -t api-service -n claude -c "$ws/api-service" \
    "bash -c 'printf \"\\033[1;35m[claude]\\033[0m Refactoring JWT auth middleware in src/auth.rs...\\nWaiting for tool permission (y/n) or injected prompt:\\n\\n\"; exec /usr/local/share/waymux-sandbox/fake-bin/claude'"
  # Window 3: simulated `codex` agent asking a clarifying question
  tmux new-window -t api-service -n codex -c "$ws/api-service" \
    "bash -c 'printf \"\\033[1;36m[codex]\\033[0m Inspecting connection pool in src/db.rs...\\nQuestion: Should MAX_CONNECTIONS default to 32 or 64?\\n\\n\"; exec /usr/local/share/waymux-sandbox/fake-bin/codex'"

  # Session 2: web-app
  # Window 1: simulated `opencode` agent actively working
  tmux new-session -d -s web-app -n opencode -c "$ws/web-app" \
    "bash -c 'printf \"\\033[1;32m[opencode]\\033[0m Updating dashboard theme tokens in src/theme.ts...\\n\\n\"; exec /usr/local/share/waymux-sandbox/fake-bin/opencode'"

  # Give the fake agent processes a moment to exec so /proc/<pid>/comm is populated
  sleep 0.2

  local p_claude p_codex p_opencode
  p_claude="$(tmux list-panes -t api-service:claude -F '#{pane_id}')"
  p_codex="$(tmux list-panes -t api-service:codex -F '#{pane_id}')"
  p_opencode="$(tmux list-panes -t web-app:opencode -F '#{pane_id}')"

  printf '{"title":"Refactor JWT auth middleware"}' | TMUX_PANE="$p_claude" waymux hook tmux PreInvocation >/dev/null
  echo '{"toolCall":{"name":"request_permission"}}' | TMUX_PANE="$p_claude" waymux hook tmux PreToolUse >/dev/null

  printf '{"title":"Optimize DB connection pool"}' | TMUX_PANE="$p_codex" waymux hook tmux PreInvocation >/dev/null
  echo '{"toolCall":{"name":"ask_question"}}' | TMUX_PANE="$p_codex" waymux hook tmux PreToolUse >/dev/null

  printf '{"title":"Update dashboard theme tokens"}' | TMUX_PANE="$p_opencode" waymux hook tmux PreInvocation >/dev/null

  tmux select-window -t api-service:control
  tmux send-keys -t api-service:control "clear && scenario status && echo '' && echo '💡 Try: scenario --help | Press Ctrl+B then Space for Cockpit HUD | Press Ctrl+G for Lazygitrs'" C-m
}

run_smoke_tests() {
  echo "━━━ Waymux Isolated Docker Smoke Test Suite ━━━"

  echo "[1/8] Checking binary versions..."
  waymux --version
  acpd --version
  awt --help | head -n 1
  if command -v wm >/dev/null 2>&1; then
    wm --version
  fi
  if command -v lazygitrs >/dev/null 2>&1; then
    lazygitrs --version
  fi

  echo "[2/8] Verifying daemon health..."
  waymux health

  echo "[3/8] Provisioning Tmux sessions & simulated AI agents (claude, codex, opencode)..."
  provision_tmux_topology

  echo "[4/8] Verifying daemon agent state tracking (waymux list)..."
  local list_out
  list_out="$(waymux list)"
  echo "$list_out"
  echo "$list_out" | grep -q "permission"
  echo "$list_out" | grep -q "awaiting_input"
  echo "$list_out" | grep -q "working"

  echo "[5/8] Verifying native /proc scanner & 12-column TSV output (waymux cockpit-items)..."
  local items_out
  items_out="$(waymux cockpit-items)"
  echo "$items_out" | grep -E "Refactor JWT auth|Optimize DB connection|Update dashboard theme"
  local col_count
  col_count="$(echo "$items_out" | grep -v '^#' | head -n 1 | awk -F'\t' '{print NF}')"
  if [[ "$col_count" -lt 12 ]]; then
    echo "Expected >= 12 TSV columns in cockpit-items, got $col_count" >&2
    exit 1
  fi
  echo "  ✓ cockpit-items emitted $col_count TSV columns"

  echo "[6/8] Verifying cockpit-preview & cockpit-diff..."
  local p_claude preview_out diff_out
  p_claude="$(tmux list-panes -t api-service:2 -F '#{pane_id}')"
  preview_out="$(waymux cockpit-preview api-service 2 "$p_claude" /home/dev/workspace/api-service)"
  echo "$preview_out" | sed -n '1,12p'
  diff_out="$(waymux cockpit-diff api-service 2 "$p_claude" /home/dev/workspace/api-service)"
  echo "$diff_out" | sed -n '1,16p'

  echo "[7/8] Verifying review-notes count, list, and bracketed-paste dispatch..."
  local note_count
  note_count="$(waymux review-notes count /home/dev/workspace/api-service)"
  echo "  Pending notes before send: $note_count"
  [[ "$note_count" == "2" ]]
  waymux review-notes send /home/dev/workspace/api-service api-service 2 "$p_claude"
  sleep 0.2
  local captured
  captured="$(tmux capture-pane -p -t "$p_claude")"
  echo "$captured" | grep -q "Please review and address the following 2 code review comments"
  echo "  ✓ Bracketed-paste prompt verified inside claude pane ($p_claude)"

  echo "[8/8] Verifying guarded agent-action & badge dismissal..."
  waymux agent-action approve permission "$p_claude"
  waymux dismiss "$p_claude"
  echo "  ✓ Guarded approve & dismiss succeeded"

  echo ""
  echo "✅ ALL 8 WAYMUX DOCKER SANDBOX CHECKS PASSED!"
}

mode="${1:-demo}"

case "$mode" in
  demo)
    provision_tmux_topology
    exec tmux attach-session -t api-service
    ;;
  test)
    run_smoke_tests
    ;;
  *)
    exec "$@"
    ;;
esac
