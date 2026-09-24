#!/usr/bin/env bash
# waymux.tmux: TPM-compliant plugin & configuration loader for the waymux stack
set -euo pipefail

CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$CURRENT_DIR/../scripts" 2>/dev/null && pwd || echo "$HOME/.local/share/waymux/scripts")"
ISOLATOR="$SCRIPTS_DIR/tmux-popup-isolate.sh"

get_tmux_option() {
    local option="$1"
    local default_value="$2"
    local option_value
    option_value=$(tmux show-option -gqv "$option")
    if [[ -z "$option_value" ]]; then
        echo "$default_value"
    else
        echo "$option_value"
    fi
}

set_tmux_option() {
    local option="$1"
    local value="$2"
    tmux set-option -gq "$option" "$value"
}

# 1. Keybindings Configuration
# Diff & Code Review: default Ctrl+G (table root)
KEY_DIFF=$(get_tmux_option "@waymux_key_diff" "C-g")
if [[ -n "$KEY_DIFF" && -x "$ISOLATOR" ]]; then
    tmux bind-key -n "$KEY_DIFF" run-shell "$ISOLATOR -w 90% -h 88% lazygitrs"
fi

# Workspace / Session Switcher: default Prefix + s
KEY_SESSION=$(get_tmux_option "@waymux_key_session" "s")
if [[ -n "$KEY_SESSION" && -x "$ISOLATOR" ]]; then
    tmux bind-key "$KEY_SESSION" run-shell "$ISOLATOR -w 75% -h 60% wm session"
fi

# Quick Jump to Last Session: default Prefix + C-j
KEY_LAST=$(get_tmux_option "@waymux_key_last" "C-j")
if [[ -n "$KEY_LAST" ]]; then
    tmux bind-key "$KEY_LAST" run-shell "wm connect --last 2>/dev/null || true"
fi

# 2. Status Bar Format Interpolation
# Replaces #{waymux_status} and #{waymux_spinner} with live broker options
update_status_option() {
    local option_name="$1"
    local option_val
    option_val=$(tmux show-option -gqv "$option_name")
    if [[ "$option_val" == *"#{waymux_status}"* || "$option_val" == *"#{waymux_spinner}"* ]]; then
        option_val="${option_val//\#\{waymux_status\}/\#{@ai_agent_status\}}"
        option_val="${option_val//\#\{waymux_spinner\}/\#{@ai_agent_spinner\}}"
        tmux set-option -gq "$option_name" "$option_val"
    fi
}

update_status_option "status-right"
update_status_option "status-left"
