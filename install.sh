#!/usr/bin/env bash
# ==============================================================================
# Waymux Universal Installer
# ==============================================================================
# Installs the unified `waymux` binary (and `acpd` compatibility alias),
# optionally installs companion engines (`wm` / Waymaker and `lazygitrs`),
# configures the user-level systemd service (`waymux.service`), and outputs
# ready-to-use Tmux and AI agent hook snippets.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/felipecm-br/waymux/main/install.sh | bash
#   ./install.sh [--from-source] [--prefix ~/.local/bin] [--no-systemd] [--with-stack]
# ==============================================================================

set -euo pipefail

REPO="felipecm-br/waymux"
PREFIX="${PREFIX:-$HOME/.local/bin}"
SHARE_DIR="${WAYMUX_SHARE_DIR:-$HOME/.local/share/waymux}"
FROM_SOURCE=false
SETUP_SYSTEMD=true
WITH_STACK=false
RELEASE_TAG=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --from-source)
      FROM_SOURCE=true
      shift
      ;;
    --prefix)
      PREFIX="$2"
      shift 2
      ;;
    --no-systemd)
      SETUP_SYSTEMD=false
      shift
      ;;
    --with-stack)
      WITH_STACK=true
      shift
      ;;
    -h|--help)
      echo "Usage: install.sh [--from-source] [--prefix <dir>] [--no-systemd] [--with-stack]"
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

mkdir -p "$PREFIX" "$SHARE_DIR/tmux"

detect_target() {
  local os arch
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *)
      echo "Unsupported architecture: $arch" >&2
      return 1
      ;;
  esac

  case "$os" in
    linux) echo "${arch}-unknown-linux-musl" ;;
    darwin) echo "${arch}-apple-darwin" ;;
    *)
      echo "Unsupported OS: $os" >&2
      return 1
      ;;
  esac
}

# Prints the SHA-256 of a file with whichever tool the OS has (sha256sum on Linux, shasum on macOS).
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    return 1
  fi
}

# Verifies an archive against its published .sha256 file; fails closed unless
# WAYMUX_SKIP_CHECKSUM=1, so a missing or mismatched checksum never installs silently.
verify_checksum() {
  local archive="$1" sum_file="$2" expected actual
  if [[ "${WAYMUX_SKIP_CHECKSUM:-0}" == "1" ]]; then
    echo "! Skipping checksum verification (WAYMUX_SKIP_CHECKSUM=1)" >&2
    return 0
  fi
  if [[ ! -s "$sum_file" ]]; then
    echo "Error: no checksum published for $(basename "$archive"); set WAYMUX_SKIP_CHECKSUM=1 to install anyway." >&2
    return 1
  fi
  expected="$(awk '{print $1; exit}' "$sum_file")"
  actual="$(sha256_of "$archive")" || {
    echo "Error: neither sha256sum nor shasum is installed; cannot verify $(basename "$archive")." >&2
    return 1
  }
  if [[ "$expected" != "$actual" ]]; then
    echo "Error: checksum mismatch for $(basename "$archive") (expected $expected, got $actual)." >&2
    return 1
  fi
  echo "✓ Verified SHA-256 of $(basename "$archive")"
}

# With WAYMUX_VERIFY_ATTESTATION=1, also checks the archive's signed build provenance: it
# must have been built by this repository's release workflow. Needs an authenticated `gh`.
verify_attestation() {
  local archive="$1"
  [[ "${WAYMUX_VERIFY_ATTESTATION:-0}" == "1" ]] || return 0
  if ! command -v gh >/dev/null 2>&1; then
    echo "Error: WAYMUX_VERIFY_ATTESTATION=1 needs the GitHub CLI (gh)." >&2
    return 1
  fi
  if ! gh auth status >/dev/null 2>&1; then
    echo "Error: WAYMUX_VERIFY_ATTESTATION=1 needs an authenticated gh (run: gh auth login)." >&2
    return 1
  fi
  if ! gh attestation verify "$archive" --repo "$REPO" >/dev/null; then
    echo "Error: no valid build provenance for $(basename "$archive") from ${REPO}." >&2
    return 1
  fi
  echo "✓ Verified build provenance of $(basename "$archive")"
}

install_from_release() {
  local target tag url tmp_dir archive
  target="$(detect_target)" || return 1

  tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null | grep -m1 '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' || true)"
  if [[ -z "$tag" ]]; then
    return 1
  fi
  RELEASE_TAG="$tag"

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' RETURN

  for bin_name in "waymux" "acpd"; do
    archive="${bin_name}-${tag}-${target}.tar.gz"
    url="https://github.com/${REPO}/releases/download/${tag}/${archive}"
    if curl -fsSL "$url" -o "${tmp_dir}/${archive}" 2>/dev/null; then
      # The release publishes <bin>-<tag>-<target>.sha256 (no .tar.gz); <archive>.sha256 is
      # kept as a fallback for mirrors that name it after the archive.
      curl -fsSL "${url%.tar.gz}.sha256" -o "${tmp_dir}/${archive}.sha256" 2>/dev/null ||
        curl -fsSL "${url}.sha256" -o "${tmp_dir}/${archive}.sha256" 2>/dev/null || true
      verify_checksum "${tmp_dir}/${archive}" "${tmp_dir}/${archive}.sha256" || exit 1
      verify_attestation "${tmp_dir}/${archive}" || exit 1
      tar -xzf "${tmp_dir}/${archive}" -C "$tmp_dir"
      if [[ -f "${tmp_dir}/waymux" ]]; then
        install -m 755 "${tmp_dir}/waymux" "${PREFIX}/waymux"
      elif [[ -f "${tmp_dir}/acpd" ]]; then
        install -m 755 "${tmp_dir}/acpd" "${PREFIX}/waymux"
      fi
      ln -sf "${PREFIX}/waymux" "${PREFIX}/acpd"
      echo "✓ Installed pre-built waymux (${tag}, ${target}) to ${PREFIX}/waymux"
      return 0
    fi
  done

  return 1
}

install_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo is required for --from-source installation." >&2
    exit 1
  fi
  echo "Building waymux from source (release profile)..."
  if [[ -f "Cargo.toml" ]] && grep -q 'name = "waymux"' Cargo.toml 2>/dev/null; then
    cargo build --release --bin waymux
    install -m 755 target/release/waymux "${PREFIX}/waymux"
  else
    cargo install --git "https://github.com/${REPO}.git" --bin waymux --root "${PREFIX%/bin}" --force
  fi
  ln -sf "${PREFIX}/waymux" "${PREFIX}/acpd"
  echo "✓ Installed waymux to ${PREFIX}/waymux (and symlinked ${PREFIX}/acpd)"
}

if [[ "$FROM_SOURCE" == "true" ]]; then
  install_from_source
else
  if ! install_from_release; then
    echo "Pre-built binary not found for current platform; falling back to source build..."
    install_from_source
  fi
fi

# Install the tmux plugin and the awt-*.sh helpers that `waymux wt` runs.
# From a checkout they are copied; otherwise they come from the source archive
# of the installed release (or main for a source build).
install_assets() {
  local src_dir="" tmp_dir ref
  if [[ -f "tmux/waymux.tmux" && -d "scripts/awt" ]]; then
    src_dir="."
  else
    ref="${RELEASE_TAG:-main}"
    tmp_dir="$(mktemp -d)"
    local archive_url="https://github.com/${REPO}/archive/refs/heads/main.tar.gz"
    [[ -n "$RELEASE_TAG" ]] && archive_url="https://github.com/${REPO}/archive/refs/tags/${ref}.tar.gz"
    if curl -fsSL "$archive_url" | tar -xz -C "$tmp_dir" 2>/dev/null; then
      src_dir="$(find "$tmp_dir" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
    fi
  fi

  if [[ -z "$src_dir" || ! -f "$src_dir/tmux/waymux.tmux" ]]; then
    echo "! Could not fetch the tmux plugin and wt scripts; rerun from a checkout: ./install.sh --from-source" >&2
    return 0
  fi

  install -m 755 "$src_dir/tmux/waymux.tmux" "$SHARE_DIR/tmux/waymux.tmux"
  if [[ -d "$src_dir/scripts/awt" ]]; then
    mkdir -p "$SHARE_DIR/scripts"
    install -m 755 "$src_dir"/scripts/awt/*.sh "$SHARE_DIR/scripts/"
    echo "✓ Installed wt helper scripts to $SHARE_DIR/scripts (files in ~/.config/waymaker/scripts take precedence)"
  fi
  [[ -n "${tmp_dir:-}" ]] && rm -rf "$tmp_dir"
  return 0
}

# Companion TUIs: Waymaker (`wm`, the Cockpit picker) and lazygitrs (review notes).
install_stack() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "! --with-stack needs cargo to build wm and lazygitrs; install Rust from https://rustup.rs" >&2
    return 0
  fi
  local root="${PREFIX%/bin}"
  command -v wm >/dev/null 2>&1 || cargo install --git https://github.com/felipecm-br/waymaker.git waymaker-cli --root "$root" --locked || \
    echo "! Waymaker build failed; see https://github.com/felipecm-br/waymaker" >&2
  command -v lazygitrs >/dev/null 2>&1 || cargo install --git https://github.com/felipecm-br/lazygitrs.git --root "$root" --locked || \
    echo "! lazygitrs build failed; see https://github.com/felipecm-br/lazygitrs" >&2
}

install_assets
if [[ "$WITH_STACK" == "true" ]]; then
  install_stack
fi

if [[ "$SETUP_SYSTEMD" == "true" ]] && command -v systemctl >/dev/null 2>&1; then
  mkdir -p "$HOME/.config/systemd/user"
  "${PREFIX}/waymux" init --systemd > "$HOME/.config/systemd/user/waymux.service"
  systemctl --user daemon-reload 2>/dev/null || true
  systemctl --user enable --now waymux.service 2>/dev/null || true
  echo "✓ Enabled and started user systemd service: waymux.service"
elif [[ "$(uname -s)" == "Darwin" ]]; then
  echo "! No systemd on macOS: start the daemon with \`waymux daemon &\` (or a launchd agent)."
fi

echo ""
echo "━━━ Waymux Installation Complete ━━━"
echo "Binary:   ${PREFIX}/waymux"
echo "Plugin:   ${SHARE_DIR}/tmux/waymux.tmux"
echo "Health:   $( "${PREFIX}/waymux" health 2>/dev/null || echo 'run: systemctl --user start waymux.service' )"
echo ""
echo "Next steps:"
echo "  1. Load in ~/.tmux.conf:  run-shell \"${SHARE_DIR}/tmux/waymux.tmux\""
echo "  2. Configure AI hooks:    waymux init --hooks"
echo "  3. Check the setup:       waymux doctor"
