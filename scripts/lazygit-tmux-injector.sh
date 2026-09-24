#!/usr/bin/env bash
# waymux: scripts/lazygit-tmux-injector.sh
# Injects review notes from lazygitrs into the active AI agent pane in Tmux.
# Supports multi-line markdown prompts via bracketed paste (tmux paste-buffer -p).
set -euo pipefail

LOG_FILE="${WAYMUX_LOG_DIR:-/tmp}/waymux-injector.log"
echo "[$(date -Iseconds)] Injector invoked: $0 $*" >> "$LOG_FILE" 2>/dev/null || true

WORKSPACE_DIR="${1:-}"
PROMPT="${2:-}"

if [[ -z "$WORKSPACE_DIR" || -z "$PROMPT" ]]; then
    echo "Error: Missing workspace directory or prompt payload" >&2
    exit 1
fi

SAFE_PATH=$(echo -n "$WORKSPACE_DIR" | sed 's/[^a-zA-Z0-9]/_/g')

# Search for active pane trackers in priority order
PANE=""
for candidate in \
    "/tmp/waymux-active-pane-${SAFE_PATH}.txt" \
    "/tmp/agy-active-pane-${SAFE_PATH}.txt" \
    "/tmp/agent-active-pane-${SAFE_PATH}.txt"; do
    if [[ -f "$candidate" ]]; then
        PANE="$(cat "$candidate" 2>/dev/null || true)"
        if [[ -n "$PANE" ]]; then
            break
        fi
    fi
done

if [[ -z "$PANE" ]]; then
    # Fallback: check if there is an active pane recorded by tmux session environment
    PANE="$(tmux show-environment -g "WAYMUX_ACTIVE_PANE_${SAFE_PATH}" 2>/dev/null | cut -d= -f2- || true)"
fi

if [[ -z "$PANE" ]]; then
    echo "Warning: No active AI agent pane tracked for workspace: $WORKSPACE_DIR" >> "$LOG_FILE" 2>/dev/null || true
    exit 1
fi

# Staleness guard: verify pane exists and is active
if ! tmux list-panes -a -F '#{pane_id}' 2>/dev/null | grep -q "^${PANE}$"; then
    echo "Warning: Target pane $PANE no longer exists" >> "$LOG_FILE" 2>/dev/null || true
    exit 1
fi

# Primary path: atomic bracketed paste preserving multi-line markdown lists and code fences
if printf '%s' "$PROMPT" | tmux load-buffer - 2>/dev/null; then
    if tmux paste-buffer -t "$PANE" -p 2>/dev/null; then
        tmux send-keys -t "$PANE" Enter
        echo "Successfully injected review note into pane $PANE via bracketed paste" >> "$LOG_FILE" 2>/dev/null || true
        exit 0
    fi
fi

# Fallback path: single-line literal typing
SINGLE_LINE_PROMPT=$(printf '%s' "$PROMPT" | tr '\n' ' ')
tmux send-keys -t "$PANE" -l "$SINGLE_LINE_PROMPT"
tmux send-keys -t "$PANE" Enter
echo "Injected review note into pane $PANE via single-line fallback" >> "$LOG_FILE" 2>/dev/null || true
