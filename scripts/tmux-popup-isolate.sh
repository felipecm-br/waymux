#!/usr/bin/env bash
# waymux: scripts/tmux-popup-isolate.sh
# Hardened, zero-flicker Tmux popup isolator.
# Freezes terminal backdrop during active AI streaming to eliminate redraw flicker.
set -euo pipefail

POPUP_ARGS=()
CMD_ARGS=()

# 1. Parse all valid Tmux display-popup flags
while [[ $# -gt 0 ]]; do
    case "$1" in
        # Options taking an argument
        -w|-h|-x|-y|-d|-S|-s|-b|-T|-c|-e|-t)
            [[ $# -ge 2 ]] || { echo "Error: $1 requires an argument" >&2; exit 1; }
            POPUP_ARGS+=("$1" "$2")
            shift 2
            ;;
        # Boolean / flag options (including -EE)
        -E|-EE|-B|-C|-k|-N)
            POPUP_ARGS+=("$1")
            shift 1
            ;;
        --)
            shift
            CMD_ARGS+=("$@")
            break
            ;;
        *)
            CMD_ARGS+=("$@")
            break
            ;;
    esac
done

# Safe check for argument presence without string-matching false positives
has_arg() {
    local target="$1"
    for arg in "${POPUP_ARGS[@]}"; do
        if [[ "$arg" == "$target" ]]; then
            return 0
        fi
    done
    return 1
}

# Apply default styling and dimensions if omitted
has_arg "-w" || POPUP_ARGS+=(-w 90%)
has_arg "-h" || POPUP_ARGS+=(-h 88%)
has_arg "-b" || { has_arg "-B" || POPUP_ARGS+=(-b rounded); }
has_arg "-E" || has_arg "-EE" || POPUP_ARGS+=(-E)

if [[ ${#CMD_ARGS[@]} -eq 0 ]]; then
    CMD_ARGS=("$SHELL")
fi

# If outside Tmux, execute command directly
if [[ -z "${TMUX:-}" ]]; then
    exec "${CMD_ARGS[@]}"
fi

# 2. Conditional Idle Bypass: Check AI agent streaming state
AI_STATE="$(tmux display-message -p '#{@ai_agent_state_raw}' 2>/dev/null || echo 'idle')"

if [[ "$AI_STATE" != "busy" && "$AI_STATE" != "working" ]]; then
    # Zero overhead: spawn popup directly without backdrop pane creation
    tmux display-popup "${POPUP_ARGS[@]}" "${CMD_ARGS[@]}" || {
        rc=$?
        # SIGHUP (129), SIGINT (130), or cancel (1) are standard popup termination statuses
        if [[ $rc -eq 129 || $rc -eq 130 || $rc -eq 1 ]]; then
            exit 0
        fi
        exit "$rc"
    }
    exit 0
fi

# 3. Active AI Streaming: Create isolated frozen snapshot backdrop
CURRENT_PANE="$(tmux display-message -p '#{pane_id}')"
RAW_PANE="${CURRENT_PANE#%}"
ORIG_SESS="$(tmux display-message -p '#{session_name}')"
WAS_ZOOMED="$(tmux display-message -p '#{window_zoomed_flag}')"
BACKDROP_FILE="/tmp/tmux-backdrop-${UID:-$(id -u)}-${RAW_PANE}.ansi"

# Atomic file creation with strict 0600 permissions (eliminates TOCTOU race)
(umask 077 && : > "$BACKDROP_FILE")
tmux capture-pane -ep -t "$CURRENT_PANE" > "$BACKDROP_FILE" 2>/dev/null || true

# Turn off automatic rename to prevent status bar flicker
tmux set-option -w -t "$CURRENT_PANE" automatic-rename off 2>/dev/null || true

BACKDROP_PANE=""

cleanup() {
    local exit_code=$?
    if [[ -n "$BACKDROP_PANE" ]]; then
        tmux kill-pane -t "$BACKDROP_PANE" 2>/dev/null || true
    fi
    tmux set-option -w -t "$CURRENT_PANE" automatic-rename on 2>/dev/null || true
    rm -f "$BACKDROP_FILE"

    # Restore session focus and pre-existing zoom
    local current_sess
    current_sess="$(tmux display-message -p '#{session_name}' 2>/dev/null || true)"
    if [[ "$current_sess" == "$ORIG_SESS" ]]; then
        tmux select-pane -t "$CURRENT_PANE" 2>/dev/null || true
        if [[ "$WAS_ZOOMED" == "1" ]]; then
            tmux resize-pane -Z -t "$CURRENT_PANE" 2>/dev/null || true
        fi
    fi
    if [[ $exit_code -eq 129 || $exit_code -eq 130 || $exit_code -eq 1 ]]; then
        exit 0
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

# Spawn frozen background pane
BACKDROP_PANE="$(tmux split-window -d -P -F '#{pane_id}' -t "$CURRENT_PANE" "cat '$BACKDROP_FILE'; tail -f /dev/null" 2>/dev/null || true)"

if [[ -n "$BACKDROP_PANE" ]]; then
    tmux resize-pane -Z -t "$BACKDROP_PANE" 2>/dev/null || true
fi

# Run popup over frozen snapshot
tmux display-popup "${POPUP_ARGS[@]}" "${CMD_ARGS[@]}" || true
exit 0
