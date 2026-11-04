#!/usr/bin/env bash
# Times the hot-path commands against their budgets (see the waymux-dev skill).
#
#   scripts/bench.sh [--check] [BIN] [BASELINE_BIN]
#
# Runs each command RUNS times (default 30, after WARMUP=5 discarded runs) against
# a sandboxed daemon and a 10-pane private tmux server with 10 dirty git repos
# under its own HOME, XDG_RUNTIME_DIR, TMUX_TMPDIR and port, so it never touches
# your session. Reports the median wall time. With BASELINE_BIN, prints both
# clients side by side (the daemon is always BIN's). With --check, exits 1 when
# a median exceeds its budget times BENCH_SLACK (default 1; CI uses a larger
# value because shared runners are noisy).
set -euo pipefail

check=0
if [[ "${1:-}" == "--check" ]]; then
  check=1
  shift
fi
bin=$(realpath "${1:-target/release/waymux}")
baseline=${2:+$(realpath "$2")}
runs=${RUNS:-30}
warmup=${WARMUP:-5}
slack=${BENCH_SLACK:-1}
for exe in "$bin" ${baseline:+"$baseline"}; do
  [[ -x "$exe" ]] || { echo "bench: $exe is not an executable" >&2; exit 2; }
done

sandbox=$(mktemp -d "${TMPDIR:-/tmp}/waymux-bench.XXXXXX")
daemon_pid=""
cleanup() {
  tmux kill-server 2>/dev/null || true
  [[ -n "$daemon_pid" ]] && kill "$daemon_pid" 2>/dev/null && wait "$daemon_pid" 2>/dev/null || true
  rm -rf "$sandbox" 2>/dev/null || rm -rf "$sandbox"
}
trap cleanup EXIT

port=$((20000 + RANDOM % 30000))
mkdir -p "$sandbox/tmux" "$sandbox/bin" "$sandbox/repos/repo-0"
cat >"$sandbox/config.toml" <<EOF
listen_addr = "127.0.0.1"
port = $port
pid_file = "$sandbox/waymux.pid"
shutdown_timeout_secs = 1
idle_debounce_ms = 0

[sound]
enabled = false
EOF
export HOME="$sandbox" XDG_RUNTIME_DIR="$sandbox" TMUX_TMPDIR="$sandbox/tmux" WAYMUX_PORT="$port"
unset TMUX

# 10 dirty git repositories in 10 windows of a private tmux server.
git -C "$sandbox/repos/repo-0" init -q -b main
echo "line 1" >"$sandbox/repos/repo-0/file.txt"
git -C "$sandbox/repos/repo-0" add file.txt
git -C "$sandbox/repos/repo-0" \
  -c user.name=bench -c user.email=bench@local -c commit.gpgsign=false \
  commit -qm init
echo "line 2" >>"$sandbox/repos/repo-0/file.txt"
for i in $(seq 1 9); do
  cp -r "$sandbox/repos/repo-0" "$sandbox/repos/repo-$i"
done
cp "$(command -v sleep)" "$sandbox/bin/claude"
tmux -f /dev/null new-session -d -s bench -c "$sandbox/repos/repo-0" "$sandbox/bin/claude 3600"
for i in $(seq 1 9); do
  tmux new-window -d -t bench -c "$sandbox/repos/repo-$i" "sleep 3600"
done
TMUX_PANE=$(tmux display-message -p -t bench:0 '#{pane_id}')
export TMUX_PANE

"$bin" daemon --config "$sandbox/config.toml" >/dev/null 2>&1 &
daemon_pid=$!
for _ in $(seq 100); do
  "$bin" health >/dev/null 2>&1 && break
  sleep 0.05
done
"$bin" health >/dev/null 2>&1 || { echo "bench: daemon did not start on port $port" >&2; exit 1; }

# Median wall time of one run in microseconds (after $warmup discarded runs).
time_us() {
  local exe=$1 stdin=$2
  shift 2
  local i start end
  for ((i = 0; i < warmup; i++)); do
    "$exe" "$@" <<<"$stdin" >/dev/null 2>&1 || true
  done
  local samples=()
  for ((i = 0; i < runs; i++)); do
    start=${EPOCHREALTIME/.}
    "$exe" "$@" <<<"$stdin" >/dev/null 2>&1 || true
    end=${EPOCHREALTIME/.}
    samples+=($((end - start)))
  done
  printf '%s\n' "${samples[@]}" | sort -n | sed -n "$((runs / 2 + 1))p"
}

ms() { printf '%d.%02d' $(($1 / 1000)) $((($1 % 1000) / 10)); }

# budget_us | stdin | command...
cases=(
  "1500||--version"
  "1500||health"
  "1500||list"
  "1500||status"
  "1500||dismiss $TMUX_PANE"
  '10000|{"hook_event_name":"Stop"}|hook tmux Stop claude'
  "20000||cockpit-items --refresh"
  "8000||cockpit-items"
  "10000||quota --badge"
  "30000||switch --peek"
)

if [[ -n "$baseline" ]]; then
  printf '%-24s %10s %10s %10s\n' command budget new baseline
else
  printf '%-24s %10s %10s\n' command budget median
fi
failed=0
for case in "${cases[@]}"; do
  IFS='|' read -r budget stdin cmd <<<"$case"
  read -ra args <<<"$cmd"
  med=$(time_us "$bin" "$stdin" "${args[@]}")
  line=$(printf '%-24s %8sms %8sms' "$cmd" "$(ms "$budget")" "$(ms "$med")")
  if [[ -n "$baseline" ]]; then
    base=$(time_us "$baseline" "$stdin" "${args[@]}")
    line+=$(printf ' %8sms' "$(ms "$base")")
  fi
  if ((med > budget * slack)); then
    line+="  over budget"
    failed=1
  fi
  echo "$line"
done

if ((check && failed)); then
  echo "bench: a command exceeded its budget (BENCH_SLACK=$slack)" >&2
  exit 1
fi
