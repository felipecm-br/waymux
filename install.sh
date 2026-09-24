#!/usr/bin/env bash
# waymux: The Terminal AI Multiplexer Stack
# One-command zero-compilation universal installer
# Usage: curl -fsSL https://raw.githubusercontent.com/fcmiranda/waymux/main/install.sh | bash
set -euo pipefail

# Visual Palette
BOLD='\033[1m'
CYAN='\033[0;36m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

info() { printf "${CYAN}==>${NC} ${BOLD}%s${NC}\n" "$*"; }
success() { printf "${GREEN}✔${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}⚠${NC} %s\n" "$*"; }
error() { printf "${RED}✖${NC} %s\n" "$*" >&2; exit 1; }

printf "${CYAN}${BOLD}"
cat << 'EOF'
                                          
 _ _ _ _____ _ _ _____ _ _ _ _ 
| | | |  _  | | |     | | |_'_|
| | | |     |_  | | | | | |_'_|
|_____|__|__|___|_|_|_|___|_,_|
  Terminal AI Multiplexer Stack
EOF
printf "${NC}\n"

# 1. Target Directories
PREFIX="${WAYMUX_PREFIX:-$HOME/.local}"
BIN_DIR="${WAYMUX_BIN_DIR:-$PREFIX/bin}"
SHARE_DIR="${WAYMUX_SHARE_DIR:-$PREFIX/share/waymux}"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"

mkdir -p "$BIN_DIR" "$SHARE_DIR"

# 2. OS and Architecture Detection
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64|amd64)
        TARGET_ARCH="x86_64"
        ;;
    aarch64|arm64)
        TARGET_ARCH="aarch64"
        ;;
    *)
        error "Unsupported architecture: $ARCH"
        ;;
esac

case "$OS" in
    linux)
        # Prefer fully-static musl binaries for universal compatibility
        ACPD_TARGET="${TARGET_ARCH}-unknown-linux-musl"
        WAYMAKER_TARGET="${TARGET_ARCH}-unknown-linux-musl"
        LAZYGITRS_TARGET="${TARGET_ARCH}-unknown-linux-musl"
        ;;
    darwin)
        ACPD_TARGET="${TARGET_ARCH}-unknown-linux-musl" # Note: acpd broker runs on Linux systemd; macOS users use bridge
        WAYMAKER_TARGET="${TARGET_ARCH}-apple-darwin"
        LAZYGITRS_TARGET="${TARGET_ARCH}-apple-darwin"
        ;;
    *)
        error "Unsupported operating system: $OS"
        ;;
esac

info "Detected environment: $OS ($TARGET_ARCH)"

# 3. Pinned Versions (Tier 1 Engines)
ACPD_VERSION="v0.1.0"
WAYMAKER_VERSION="v0.1.0"
LAZYGITRS_VERSION="v0.1.0-cockpit"

TEMP_DIR="$(mktemp -d /tmp/waymux-install-XXXXXX)"
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT INT TERM

download_and_verify() {
    local repo="$1"
    local version="$2"
    local bin_name="$3"
    local target="$4"
    local asset_name="${bin_name}-${version}-${target}.tar.gz"
    local checksum_name="${bin_name}-${version}-${target}.sha256"
    local download_url="https://github.com/${repo}/releases/download/${version}/${asset_name}"
    local checksum_url="https://github.com/${repo}/releases/download/${version}/${checksum_name}"

    info "Downloading $bin_name ($version) for $target..."
    if ! curl -fsSL "$download_url" -o "$TEMP_DIR/$asset_name"; then
        warn "Could not download $download_url (falling back to gnu if on Linux)..."
        if [[ "$OS" == "linux" && "$target" == *"-musl" ]]; then
            local fallback_target="${TARGET_ARCH}-unknown-linux-gnu"
            asset_name="${bin_name}-${version}-${fallback_target}.tar.gz"
            checksum_name="${bin_name}-${version}-${fallback_target}.sha256"
            download_url="https://github.com/${repo}/releases/download/${version}/${asset_name}"
            checksum_url="https://github.com/${repo}/releases/download/${version}/${checksum_name}"
            curl -fsSL "$download_url" -o "$TEMP_DIR/$asset_name" || error "Failed downloading $bin_name"
        else
            error "Failed downloading $bin_name"
        fi
    fi

    # Verify SHA256 Checksum if available
    if curl -fsSL "$checksum_url" -o "$TEMP_DIR/$checksum_name" 2>/dev/null; then
        (
            cd "$TEMP_DIR"
            if command -v sha256sum >/dev/null 2>&1; then
                sha256sum -c "$checksum_name" >/dev/null 2>&1 || warn "Checksum check failed for $asset_name"
            elif command -v shasum >/dev/null 2>&1; then
                shasum -a 256 -c "$checksum_name" >/dev/null 2>&1 || warn "Checksum check failed for $asset_name"
            fi
        )
    fi

    tar -xzf "$TEMP_DIR/$asset_name" -C "$TEMP_DIR"
    
    # Locate extracted binary
    local extracted_bin
    extracted_bin="$(find "$TEMP_DIR" -type f -name "$bin_name" | head -n 1)"
    if [[ -z "$extracted_bin" ]]; then
        error "Binary $bin_name not found inside archive"
    fi

    install -m 755 "$extracted_bin" "$BIN_DIR/$bin_name"
    success "Installed $bin_name -> $BIN_DIR/$bin_name"
}

# Download Engines
if [[ "$OS" == "linux" ]]; then
    download_and_verify "fcmiranda/acpd" "$ACPD_VERSION" "acpd" "$ACPD_TARGET"
fi
download_and_verify "fcmiranda/waymaker" "$WAYMAKER_VERSION" "wm" "$WAYMAKER_TARGET"
download_and_verify "fcmiranda/lazygitrs" "$LAZYGITRS_VERSION" "lazygitrs" "$LAZYGITRS_TARGET"

# 4. Sesh Compatibility Symlink
if [[ ! -e "$BIN_DIR/sesh" ]]; then
    ln -sf "$BIN_DIR/wm" "$BIN_DIR/sesh"
    success "Created sesh drop-in compatibility symlink -> $BIN_DIR/sesh"
fi

# 5. Install Scripts and Tmux Plugin
SCRIPT_SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd || echo "")"
if [[ -n "$SCRIPT_SOURCE_DIR" && -d "$SCRIPT_SOURCE_DIR/scripts" ]]; then
    cp -r "$SCRIPT_SOURCE_DIR/scripts" "$SHARE_DIR/"
    cp -r "$SCRIPT_SOURCE_DIR/tmux" "$SHARE_DIR/"
    chmod +x "$SHARE_DIR/scripts/"*.sh 2>/dev/null || true
    chmod +x "$SHARE_DIR/tmux/"*.tmux 2>/dev/null || true
else
    # Download standalone scripts if running via curl pipe
    mkdir -p "$SHARE_DIR/scripts" "$SHARE_DIR/tmux"
    curl -fsSL "https://raw.githubusercontent.com/fcmiranda/waymux/main/scripts/tmux-popup-isolate.sh" -o "$SHARE_DIR/scripts/tmux-popup-isolate.sh"
    curl -fsSL "https://raw.githubusercontent.com/fcmiranda/waymux/main/scripts/lazygit-tmux-injector.sh" -o "$SHARE_DIR/scripts/lazygit-tmux-injector.sh"
    curl -fsSL "https://raw.githubusercontent.com/fcmiranda/waymux/main/tmux/waymux.tmux" -o "$SHARE_DIR/tmux/waymux.tmux"
    chmod +x "$SHARE_DIR/scripts/"*.sh "$SHARE_DIR/tmux/"*.tmux
fi
success "Installed scripts and tmux plugin -> $SHARE_DIR"

# 6. Global Gitignore Configuration (.lines.json protection)
GLOBAL_GITIGNORE="$(git config --global core.excludesFile 2>/dev/null || echo "$HOME/.gitignore_global")"
if [[ ! -f "$GLOBAL_GITIGNORE" ]]; then
    touch "$GLOBAL_GITIGNORE"
    git config --global core.excludesFile "$GLOBAL_GITIGNORE"
fi
if ! grep -q "^[[:space:]]*\.lines\.json" "$GLOBAL_GITIGNORE" 2>/dev/null; then
    printf "\n# waymux review notes protection\n.lines.json\n" >> "$GLOBAL_GITIGNORE"
    success "Configured global gitignore (.lines.json) -> $GLOBAL_GITIGNORE"
fi

# 7. Systemd Service Activation (Linux)
if [[ "$OS" == "linux" ]] && command -v systemctl >/dev/null 2>&1; then
    if systemctl --user is-active dbus >/dev/null 2>&1 || [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
        mkdir -p "$SYSTEMD_USER_DIR"
        cat << 'EOF' > "$SYSTEMD_USER_DIR/acpd.service"
[Unit]
Description=ACPD: Agent Client Protocol Daemon (waymux broker)
Documentation=https://github.com/fcmiranda/waymux
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExecStart=%h/.local/bin/acpd
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=3s
NoNewPrivileges=yes
LimitNOFILE=65536
MemoryMax=512M
RuntimeDirectory=acpd
StandardOutput=journal
StandardError=journal
SyslogIdentifier=acpd

[Install]
WantedBy=default.target
EOF
        systemctl --user daemon-reload 2>/dev/null || true
        systemctl --user enable --now acpd.service 2>/dev/null || true
        success "Activated user systemd service -> acpd.service"
    fi
fi

# 8. Final Instructions
printf "\n${GREEN}${BOLD}✔ waymux stack successfully installed!${NC}\n\n"
printf "Next steps:\n"
printf "1. Ensure ${CYAN}%s${NC} is in your ${BOLD}\$PATH${NC}.\n" "$BIN_DIR"
printf "2. Add waymux plugin to your ${CYAN}~/.tmux.conf${NC}:\n"
printf "   ${BOLD}run-shell \"%s/tmux/waymux.tmux\"${NC}\n\n" "$SHARE_DIR"
printf "3. Keybindings active out of the box:\n"
printf "   • ${CYAN}Ctrl+G${NC}        Inline Diff Review & AI Prompt Dispatch (lazygitrs)\n"
printf "   • ${CYAN}Prefix + s${NC}    Instant Workspace & Session Switcher (waymaker)\n"
printf "   • ${CYAN}Prefix + C-j${NC}  Jump to Last Session (zero friction)\n\n"
