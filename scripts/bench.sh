#!/usr/bin/env bash
# Times the hot-path commands against their budgets (see the waymux-dev skill).
#
#   scripts/bench.sh [--check] [BIN] [BASELINE_BIN]
#
# Runs each command RUNS times (default 30) against a sandboxed daemon with its own
# HOME, XDG_RUNTIME_DIR, TMUX_TMPDIR and port, so it never touches your session.
# With BASELINE_BIN, prints both clients side by side (the daemon is always BIN's). With --check, exits 1 when
# an average exceeds its budget times BENCH_SLACK (default 1; CI uses a larger
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
slack=${BENCH_SLACK:-1}
for exe in "$bin" ${baseline:+"$baseline"}; do
  [[ -x "$exe" ]] || { echo "bench: $exe is not an executable" >&2; exit 2; }
done

sandbox=$(mktemp -d "${TMPDIR:-/tmp}/waymux-bench.XXXXXX")
daemon_pid=""
cleanup() {
  [[ -n "$daemon_pid" ]] && kill "$daemon_pid" 2>/dev/null && wait "$daemon_pid" 2>/dev/null
  rm -rf "$sandbox"
}
trap cleanup EXIT

port=$((20000 + RANDOM % 30000))
mkdir -p "$sandbox/tmux"
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
export TMUX_PANE=%1

"$bin" daemon --config "$sandbox/config.toml" >/dev/null 2>&1 &
daemon_pid=$!
for _ in $(seq 100); do
  "$bin" health >/dev/null 2>&1 && break
  sleep 0.05
done
"$bin" health >/dev/null 2>&1 || { echo "bench: daemon did not start on port $port" >&2; exit 1; }

# Average wall time of one run in microseconds.
time_us() {
  local exe=$1 stdin=$2
  shift 2
  local start end
  start=$(date +%s%N)
  for _ in $(seq "$runs"); do
    "$exe" "$@" <<<"$stdin" >/dev/null 2>&1 || true
  done
  end=$(date +%s%N)
  echo $(((end - start) / runs / 1000))
}

ms() { printf '%d.%02d' $(($1 / 1000)) $((($1 % 1000) / 10)); }

# budget_us | stdin | command...
cases=(
  "1500||--version"
  "1500||health"
  "1500||list"
  "1500||status"
  "1500||dismiss %1"
  '10000|{"hook_event_name":"Stop"}|hook tmux Stop claude'
)

if [[ -n "$baseline" ]]; then
  printf '%-22s %10s %10s %10s\n' command budget new baseline
else
  printf '%-22s %10s %10s\n' command budget avg
fi
failed=0
for case in "${cases[@]}"; do
  IFS='|' read -r budget stdin cmd <<<"$case"
  read -ra args <<<"$cmd"
  avg=$(time_us "$bin" "$stdin" "${args[@]}")
  line=$(printf '%-22s %8sms %8sms' "$cmd" "$(ms "$budget")" "$(ms "$avg")")
  if [[ -n "$baseline" ]]; then
    base=$(time_us "$baseline" "$stdin" "${args[@]}")
    line+=$(printf ' %8sms' "$(ms "$base")")
  fi
  if ((avg > budget * slack)); then
    line+="  over budget"
    failed=1
  fi
  echo "$line"
done

if ((check && failed)); then
  echo "bench: a command exceeded its budget (BENCH_SLACK=$slack)" >&2
  exit 1
fi
