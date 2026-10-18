#!/usr/bin/env bash
# ==============================================================================
# scenario — Interactive & scriptable Waymux simulator inside the Docker sandbox
# ==============================================================================
# Lets you trigger AI agent lifecycle states, inspect Tmux/daemon state, seed
# inline diff review notes, or run a live state-transition demo.
#
# Usage:
#   scenario status                       Show daemon health, agent states & panes
#   scenario state <pane> <state> [title] Set pane state (working|question|permission|idle|error)
#   scenario cycle                        Cycle simulated agents through live states
#   scenario notes                        Seed .git/info/lines.json review notes & list them
#   scenario send-notes                   Dispatch pending review notes into claude pane
#   scenario reset                        Reset all simulated agents to idle
# ==============================================================================

set -euo pipefail

find_pane_by_window() {
  local query="$1"
  tmux list-panes -a -F '#{pane_current_command} #{session_name}:#{pane_current_command} #{session_name}:#{window_index} #{session_name}:#{window_name} #{pane_id}' 2>/dev/null \
    | awk -v q="$query" '$1 == q || $2 == q || $3 == q || $4 == q { print $5; exit }'
}

set_pane_state() {
  local pane="$1"
  local state="$2"
  local title="${3:-}"

  case "$state" in
    working|busy)
      if [[ -n "$title" ]]; then
        printf '{"title":"%s"}' "$title" | TMUX_PANE="$pane" waymux hook tmux PreInvocation >/dev/null
      else
        echo '{}' | TMUX_PANE="$pane" waymux hook tmux PreToolUse >/dev/null
      fi
      ;;
    question|awaiting_input)
      echo '{"toolCall":{"name":"ask_question"}}' | TMUX_PANE="$pane" waymux hook tmux PreToolUse >/dev/null
      ;;
    permission)
      echo '{"toolCall":{"name":"request_permission"}}' | TMUX_PANE="$pane" waymux hook tmux PreToolUse >/dev/null
      ;;
    idle)
      echo '{}' | TMUX_PANE="$pane" waymux hook tmux Stop >/dev/null
      ;;
    closed|reset)
      echo '{}' | TMUX_PANE="$pane" waymux hook tmux SessionEnd >/dev/null
      ;;
    *)
      echo "Unknown state: $state (use: working, question, permission, idle, closed)" >&2
      return 1
      ;;
  esac
}

seed_notes() {
  local repo="${1:-/home/dev/workspace/api-service}"
  local notes_file="${repo}/.git/info/lines.json"
  mkdir -p "$(dirname "$notes_file")"
  cat > "$notes_file" << 'EOF'
{
  "notes": [
    {
      "file": "src/auth.rs",
      "line": 14,
      "comment": "Avoid .unwrap() on untrusted Authorization header; return StatusCode::UNAUTHORIZED instead.",
      "status": "New"
    },
    {
      "file": "src/db.rs",
      "line": 8,
      "comment": "Make pool size configurable via DATABASE_MAX_CONNS env var with a default of 32.",
      "status": "New"
    }
  ]
}
EOF
  echo "✓ Seeded 2 pending review notes in ${notes_file}"
}

cmd="${1:-help}"
shift || true

case "$cmd" in
  status)
    echo "━━━ Daemon Health & Active States ━━━"
    waymux status || true
    echo ""
    echo "━━━ Tmux Panes & Window Badges ━━━"
    tmux list-panes -a -F '  #{pane_id}  [#{session_name}:#{window_index}:#{window_name}]  cmd=#{pane_current_command}  state=#{@ai_agent_state_raw}  badge=#{@ai_agent_state}  title=#{@ai_agent_title}'
    ;;

  state)
    pane="${1:-}"
    state="${2:-working}"
    title="${3:-}"
    if [[ -z "$pane" ]]; then
      echo "Usage: scenario state <pane_id|session:window> <working|question|permission|idle|closed> [title]" >&2
      exit 1
    fi
    if [[ "$pane" != %* ]]; then
      resolved="$(find_pane_by_window "$pane")"
      if [[ -z "$resolved" ]]; then
        echo "Could not resolve pane for '$pane'" >&2
        exit 1
      fi
      pane="$resolved"
    fi
    set_pane_state "$pane" "$state" "$title"
    echo "✓ Pane $pane -> $state"
    ;;

  notes)
    seed_notes "/home/dev/workspace/api-service"
    echo ""
    waymux review-notes list "/home/dev/workspace/api-service"
    ;;

  send-notes)
    claude_pane="$(find_pane_by_window "claude")"
    waymux review-notes send "/home/dev/workspace/api-service" "api-service" "2" "$claude_pane"
    echo "✓ Dispatched pending review notes to claude ($claude_pane)"
    ;;

  cycle)
    p_claude="$(find_pane_by_window "api-service:claude")"
    p_codex="$(find_pane_by_window "api-service:codex")"
    p_opencode="$(find_pane_by_window "web-app:opencode")"
    echo "▶ Cycling simulated AI agent states (watch the Tmux status bar or press Prefix+Space)..."
    set_pane_state "$p_claude" working "Refactor JWT auth middleware"
    set_pane_state "$p_codex" working "Optimize connection pool"
    set_pane_state "$p_opencode" working "Update dashboard theme"
    sleep 1
    set_pane_state "$p_claude" permission
    echo "  • claude ($p_claude) -> permission (󱅭)"
    sleep 1
    set_pane_state "$p_codex" question
    echo "  • codex ($p_codex) -> awaiting_input (󱜻)"
    sleep 1
    set_pane_state "$p_opencode" idle
    echo "  • opencode ($p_opencode) -> idle (󱥂)"
    echo "✓ Done! Try:"
    echo "  • Prefix + Space  (Cockpit HUD)"
    echo "  • Prefix + i      (Alert Triage Popup)"
    echo "  • Prefix + I      (Alert Triage Jump)"
    echo "  • Ctrl + G        (Lazygitrs Diff & Review Notes)"
    ;;

  reset)
    for p in $(tmux list-panes -a -F '#{pane_id}'); do
      set_pane_state "$p" idle || true
    done
    echo "✓ All panes reset to idle."
    ;;

  *)
    cat << 'EOF'
scenario — Waymux Sandbox Simulator

Commands:
  scenario status                       Show daemon health, agent states & Tmux badges
  scenario cycle                        Transition claude/codex/opencode into permission/question/idle
  scenario state <pane> <state> [title] Set state (working|question|permission|idle|closed)
  scenario notes                        Seed 2 inline diff review notes in api-service
  scenario send-notes                   Send pending review notes to claude pane via bracketed paste
  scenario reset                        Reset all panes to idle
EOF
    ;;
esac
