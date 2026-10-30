#!/usr/bin/env bash
# awt-reap.sh — Reaps stale AI agent processes and orphan worktree Tmux sessions
# Usage: awt reap [--hours N] [-y|--yes] [-n|--dry-run]
set -euo pipefail

R='\033[0m'
BOLD='\033[1m'
DIM='\033[2m'
C_CYAN='\033[38;2;131;192;146m'
C_YELLOW='\033[38;2;219;188;127m'
C_RED='\033[38;2;230;126;128m'
C_BLUE='\033[38;2;127;187;179m'
C_GRAY='\033[38;2;114;135;152m'

max_hours=4
auto_yes=0
dry_run=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --hours=*)
      max_hours="${1#*=}"
      ;;
    --hours)
      shift
      max_hours="${1:-4}"
      ;;
    -y|--yes)
      auto_yes=1
      ;;
    -n|--dry-run)
      dry_run=1
      ;;
    -h|--help)
      cat << 'HELP'
awt reap — Reap stale AI agents and orphan worktree sessions

USAGE:
    awt reap [flags]

FLAGS:
    --hours <N>     Age threshold in hours to consider an agent stale (default: 4)
    -y, --yes       Terminate identified stale/orphan sessions without confirmation
    -n, --dry-run   List candidates without terminating them
    -h, --help      Show this help message
HELP
      exit 0
      ;;
    *)
      echo "awt-reap.sh: unknown argument '$1'; nothing was reaped (--help)" >&2
      exit 2
      ;;
  esac
  shift
done

now_ms=$(date +%s000)
threshold_ms=$(( max_hours * 3600 * 1000 ))

# Query the waymux daemon for registered agent states
token_file="${XDG_RUNTIME_DIR:-/run/user/$UID}/waymux/token"
waymux_json=""
if [[ -f "$token_file" ]]; then
  token="$(cat "$token_file" 2>/dev/null || true)"
  if [[ -n "$token" ]]; then
    waymux_json=$(curl -s -m 0.25 -X POST http://127.0.0.1:4040/rpc \
      -H "Authorization: Bearer $token" \
      -H "Content-Type: application/json" \
      -d '{"jsonrpc":"2.0","method":"agentState/list","id":1}' 2>/dev/null || true)
  fi
fi

declare -a candidate_targets=()
declare -a candidate_types=()
declare -a candidate_details=()
declare -a candidate_reasons=()

# 1. Check tmux sessions for orphan worktrees (path no longer exists)
while IFS='|' read -r sess ppath; do
  case "$sess" in
    _lazygitrs*|_popups*|\.*) continue ;;
  esac
  if [[ "$sess" =~ / && ! -d "$ppath" ]]; then
    candidate_targets+=("session:$sess")
    candidate_types+=("orphan-session")
    candidate_details+=("$sess")
    candidate_reasons+=("Worktree directory deleted ($ppath)")
  fi
done < <(tmux list-sessions -F '#{session_name}|#{pane_current_path}' 2>/dev/null || true)

# 2. Check active agent panes for idle stale time (> max_hours)
while IFS='|' read -r sess idx pid pane_os_pid ppath pcmd; do
  case "$sess" in
    _lazygitrs*|_popups*|\.*) continue ;;
  esac

  agent_found=""
  if [[ -n "$pane_os_pid" ]]; then
    agent_found=$(ps -s "$pane_os_pid" -o comm= 2>/dev/null | grep -E '^(agy|agy-bin|antigravity|opencode|claude|devin|codex)$' | head -n 1 || true)
  fi
  [[ -z "$agent_found" && "$pcmd" =~ ^(agy|antigravity|opencode|claude|devin|codex)$ ]] && agent_found="$pcmd"

  if [[ -n "$agent_found" ]]; then
    st=""
    ts=0
    if [[ -n "$waymux_json" ]]; then
      st=$(echo "$waymux_json" | jq -r ".result[\"$pid\"].state // empty" 2>/dev/null || true)
      ts=$(echo "$waymux_json" | jq -r ".result[\"$pid\"].last_timestamp // 0" 2>/dev/null || echo 0)
    fi

    if (( ts > 0 )); then
      diff_ms=$(( now_ms - ts ))
      if (( diff_ms > threshold_ms )) && [[ "$st" == "idle" || "$st" == "error" || "$st" == "awaiting_input" ]]; then
        diff_h=$(( diff_ms / 3600000 ))
        diff_m=$(( (diff_ms % 3600000) / 60000 ))
        candidate_targets+=("pane:${sess}:${idx}")
        candidate_types+=("stale-agent")
        candidate_details+=("${agent_found} in ${sess}:${idx}")
        candidate_reasons+=("Inactive for ${diff_h}h ${diff_m}m (state: $st)")
      fi
    fi
  fi
done < <(tmux list-panes -a -F '#{session_name}|#{window_index}|#{pane_id}|#{pane_pid}|#{pane_current_path}|#{pane_current_command}' 2>/dev/null || true)

total_candidates=${#candidate_targets[@]}

printf "${BOLD}󰈈 Scanning AI agents and worktree sessions (stale threshold: %sh)...${R}\n\n" "$max_hours"

if [[ $total_candidates -eq 0 ]]; then
  printf "${C_CYAN}✔ No stale or orphan agents found.${R} Fleet is healthy and active.\n"
  exit 0
fi

# Print table of candidates
printf "${C_YELLOW}${BOLD}Found %d candidate(s) for reaping:${R}\n\n" "$total_candidates"
for i in "${!candidate_targets[@]}"; do
  c_tgt="${candidate_targets[$i]}"
  c_det="${candidate_details[$i]}"
  c_rs="${candidate_reasons[$i]}"
  printf "  ${C_RED}󰑮 %-30s${R}  ${C_GRAY}(%s)${R}\n" "$c_det" "$c_rs"
done
printf "\n"

if [[ $dry_run -eq 1 ]]; then
  printf "${DIM}[Dry run mode: no processes or sessions were terminated]${R}\n"
  exit 0
fi

# Confirmation prompt
if [[ $auto_yes -eq 0 ]]; then
  if command -v gum >/dev/null 2>&1; then
    if ! gum confirm --prompt.foreground="214" "Reap these $total_candidates stale agent/session(s)?"; then
      printf "${DIM}Reap aborted.${R}\n"
      exit 0
    fi
  else
    read -r -p "Reap these $total_candidates stale agent/session(s)? [y/N] " confirm </dev/tty
    if [[ "$confirm" != [yY]* ]]; then
      printf "${DIM}Reap aborted.${R}\n"
      exit 0
    fi
  fi
fi

reaped_count=0
for i in "${!candidate_targets[@]}"; do
  c_tgt="${candidate_targets[$i]}"
  c_det="${candidate_details[$i]}"
  printf "${DIM}Reaping %s...${R} " "$c_det"

  if [[ "$c_tgt" =~ ^session:(.*) ]]; then
    sess_to_kill="${BASH_REMATCH[1]}"
    tmux kill-session -t "$sess_to_kill" 2>/dev/null || true
    printf "${C_CYAN}✔ session killed${R}\n"
    reaped_count=$((reaped_count + 1))
  elif [[ "$c_tgt" =~ ^pane:(.*) ]]; then
    pane_target="${BASH_REMATCH[1]}"
    tmux send-keys -t "$pane_target" C-c 2>/dev/null || true
    sleep 0.1
    tmux kill-window -t "$pane_target" 2>/dev/null || tmux kill-pane -t "$pane_target" 2>/dev/null || true
    printf "${C_CYAN}✔ agent terminated${R}\n"
    reaped_count=$((reaped_count + 1))
  fi
done

printf "\n${C_CYAN}${BOLD}✔ Successfully reaped %d stale agent target(s).${R}\n" "$reaped_count"
exit 0
