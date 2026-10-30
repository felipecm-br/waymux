#!/usr/bin/env bash
# waymux.tmux: TPM-compliant plugin & configuration loader for the Waymux AI Cockpit Stack
set -euo pipefail

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

# 1. Event-driven status bar (0% idle CPU polling)
tmux set-option -gq status-interval 0

# 2. Auto-dismiss agent attention badges when focusing a pane
tmux set-hook -g pane-focus-in 'run-shell -b "waymux dismiss #{pane_id} >/dev/null 2>&1 || true"'

# 3. Keybindings Configuration (Home-Row Anchored, H=0)
# Cockpit HUD: default Prefix + Space
KEY_COCKPIT=$(get_tmux_option "@waymux_key_cockpit" "Space")
if [[ -n "$KEY_COCKPIT" ]]; then
    tmux bind-key "$KEY_COCKPIT" if-shell -F '#{==:#{session_name},_popups}' 'detach-client' 'run-shell "waymux cockpit"'
fi

# Alert Triage Popup: default Prefix + i
KEY_TRIAGE_POPUP=$(get_tmux_option "@waymux_key_triage_popup" "i")
if [[ -n "$KEY_TRIAGE_POPUP" ]]; then
    tmux bind-key "$KEY_TRIAGE_POPUP" if-shell -F '#{==:#{session_name},_popups}' 'detach-client' 'run-shell "waymux triage --popup"'
fi

# Alert Triage Jump (Zero-Modal): default Prefix + I and Ctrl+Shift+I
KEY_TRIAGE_JUMP=$(get_tmux_option "@waymux_key_triage_jump" "I")
if [[ -n "$KEY_TRIAGE_JUMP" ]]; then
    tmux bind-key "$KEY_TRIAGE_JUMP" run-shell "waymux triage --jump"
fi
tmux bind-key -n 'C-S-i' run-shell "waymux triage --jump"
tmux bind-key -n 'C-S-I' run-shell "waymux triage --jump"

# Diff & Code Review: default Ctrl+G (root table)
KEY_DIFF=$(get_tmux_option "@waymux_key_diff" "C-g")
if [[ -n "$KEY_DIFF" ]] && command -v lazygitrs >/dev/null 2>&1; then
    tmux bind-key -n "$KEY_DIFF" display-popup -E -w 90% -h 88% -b rounded -d "#{pane_current_path}" "lazygitrs"
fi

# Workspace / Session Switcher: default Prefix + s
KEY_SESSION=$(get_tmux_option "@waymux_key_session" "s")
if [[ -n "$KEY_SESSION" ]] && command -v wm >/dev/null 2>&1; then
    tmux bind-key "$KEY_SESSION" display-popup -E -w 75% -h 60% -b rounded "wm session"
fi

# Agent popups (set the option to "off" to leave a key unbound)
# Quota & launch recommendation: default Prefix + u
KEY_QUOTA=$(get_tmux_option "@waymux_key_quota" "u")
if [[ "$KEY_QUOTA" != off ]]; then
    tmux bind-key "$KEY_QUOTA" run-shell "waymux quota --popup '#{pane_current_path}'"
fi

# Agent × model launcher (new window): default Prefix + a
KEY_LAUNCH=$(get_tmux_option "@waymux_key_launch" "a")
if [[ "$KEY_LAUNCH" != off ]]; then
    tmux bind-key "$KEY_LAUNCH" run-shell "waymux launch --popup '#{pane_current_path}'"
fi

# In-place model / effort switch for the pane's agent: default Prefix + A
KEY_SWITCH=$(get_tmux_option "@waymux_key_switch" "A")
if [[ "$KEY_SWITCH" != off ]]; then
    tmux bind-key "$KEY_SWITCH" run-shell "waymux switch --popup '#{pane_id}'"
fi

# Quick Jump to Last Session: default Prefix + L
KEY_LAST=$(get_tmux_option "@waymux_key_last" "L")
if [[ -n "$KEY_LAST" ]]; then
    tmux bind-key "$KEY_LAST" switch-client -l
fi
