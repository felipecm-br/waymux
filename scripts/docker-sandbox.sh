#!/usr/bin/env bash
# ==============================================================================
# Waymux Isolated Docker Sandbox Runner
# ==============================================================================
# Launches an isolated Debian + Tmux 3.8 container to test the full Waymux AI
# Cockpit stack (`waymux`, `wm`, `lazygitrs`, hooks, triage, and review notes)
# without touching your host Tmux server or dotfiles.
#
# Modes:
#   ./scripts/docker-sandbox.sh [demo]           Interactive Tmux session (default)
#   ./scripts/docker-sandbox.sh test             Headless 8-step integration test suite
#   ./scripts/docker-sandbox.sh bash             Interactive shell inside the container
#   ./scripts/docker-sandbox.sh --from-source …  Compile waymux/wm/lazygitrs in Docker
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SCRIPT_DIR"

FROM_SOURCE=false
if [[ "${1:-}" == "--from-source" ]]; then
  FROM_SOURCE=true
  shift
fi

MODE="${1:-demo}"

WAYMUX_BIN="${WAYMUX_BIN:-$(command -v waymux || echo "$HOME/.local/bin/waymux")}"
WM_BIN="${WM_BIN:-$(command -v wm || echo "$HOME/.local/bin/wm")}"
LAZYGITRS_BIN="${LAZYGITRS_BIN:-$(command -v lazygitrs || echo "$HOME/.cargo/bin/lazygitrs")}"

DOCKER_ARGS=(--rm)
if [[ "$MODE" != "test" ]] && [[ -t 0 ]] && [[ -t 1 ]]; then
  DOCKER_ARGS+=(-it)
fi

if [[ "$FROM_SOURCE" == "true" ]] || [[ ! -x "$WAYMUX_BIN" ]]; then
  IMAGE_TAG="waymux-sandbox:full"
  echo "🐳 Building standalone Waymux sandbox image ($IMAGE_TAG) from source..."
  docker build -f docker/Dockerfile --target full -t "$IMAGE_TAG" .
else
  IMAGE_TAG="waymux-sandbox:runtime"
  echo "🐳 Building lightweight Waymux sandbox runtime ($IMAGE_TAG)..."
  docker build -q -f docker/Dockerfile --target runtime -t "$IMAGE_TAG" . >/dev/null

  DOCKER_ARGS+=(-v "${WAYMUX_BIN}:/usr/local/bin/waymux:ro")
  if [[ -x "$WM_BIN" ]]; then
    DOCKER_ARGS+=(-v "${WM_BIN}:/usr/local/bin/wm:ro")
  fi
  if [[ -x "$LAZYGITRS_BIN" ]]; then
    DOCKER_ARGS+=(-v "${LAZYGITRS_BIN}:/usr/local/bin/lazygitrs:ro")
  fi
fi

exec docker run "${DOCKER_ARGS[@]}" "$IMAGE_TAG" "$@"
