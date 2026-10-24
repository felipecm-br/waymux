# syntax=docker/dockerfile:1
# ==============================================================================
# Waymux Isolated Docker Sandbox
# ==============================================================================
# Targets:
#   runtime          Base Debian Tmux 3.8 environment (mounts host binaries for
#                    instant <3s startup and zero Rust build-cache disk bloat).
#   lite             Compiles `waymux` from the local working tree in Docker.
#   full  (default)  Compiles `waymux` + `wm` (v0.3.2) + `lazygitrs` (v0.1.3-cockpit)
#                    from source in Docker for a 100% standalone image.
#
# Usage:
#   ./scripts/docker-sandbox.sh demo              # Instant interactive Tmux demo
#   ./scripts/docker-sandbox.sh test              # Instant headless smoke test
#   ./scripts/docker-sandbox.sh --from-source     # Full standalone multi-stage build
# ==============================================================================

# Pinned by digest (Dependabot bumps them); sid-slim provides glibc >= 2.43 so
# host-mounted binaries built on rolling distributions run inside `runtime`.
ARG RUST_IMAGE=rust:1.99-slim@sha256:aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6
ARG RUNTIME_IMAGE=debian:sid-slim@sha256:fb75970a5ba2a81ca409ed0cabcf05d53e058086fd397f7a63943ded5bd15f84

# ------------------------------------------------------------------------------
# Stage 1: waymux from the local working tree
# ------------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS build-waymux
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin waymux \
 && install -D -m 755 target/release/waymux /out/waymux \
 && rm -rf target "${CARGO_HOME}/registry" "${CARGO_HOME}/git"

# ------------------------------------------------------------------------------
# Stage 2: Waymaker (`wm`) pinned by tag
# ------------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS build-wm
ARG WAYMAKER_REPO=https://github.com/felipecm-br/waymaker.git
ARG WAYMAKER_REF=v0.3.2
RUN git clone --depth 1 --branch "${WAYMAKER_REF}" "${WAYMAKER_REPO}" /src
WORKDIR /src
RUN cargo build --release -p waymaker-cli \
 && install -D -m 755 target/release/wm /out/wm \
 && rm -rf target "${CARGO_HOME}/registry" "${CARGO_HOME}/git"

# ------------------------------------------------------------------------------
# Stage 3: lazygitrs pinned by tag
# ------------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS build-lazygitrs
ARG LAZYGITRS_REPO=https://github.com/felipecm-br/lazygitrs.git
ARG LAZYGITRS_REF=v0.1.3-cockpit
RUN git clone --depth 1 --branch "${LAZYGITRS_REF}" "${LAZYGITRS_REPO}" /src
WORKDIR /src
RUN cargo build --release \
 && install -D -m 755 target/release/lazygitrs /out/lazygitrs \
 && rm -rf target "${CARGO_HOME}/registry" "${CARGO_HOME}/git"

# ------------------------------------------------------------------------------
# Runtime base: tmux 3.8 + CLI tools the Cockpit shells out to
# ------------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS runtime
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends \
      tmux git ca-certificates procps less jq curl zsh vim-tiny \
      bat git-delta ncurses-term \
 && ln -sf /usr/bin/batcat /usr/local/bin/bat \
 && rm -rf /var/lib/apt/lists/*

# Unprivileged user. XDG_RUNTIME_DIR is fixed so `docker exec` shells see the
# same auth token ($XDG_RUNTIME_DIR/waymux/token) as the daemon started by entrypoint.
RUN useradd -m -u 1000 -s /bin/bash dev \
 && install -d -m 700 -o dev -g dev /tmp/runtime-dev
ENV HOME=/home/dev \
    XDG_RUNTIME_DIR=/tmp/runtime-dev \
    LANG=C.UTF-8 \
    COLORTERM=truecolor

# Plugin + reference config + sandbox helpers
COPY tmux/waymux.tmux /usr/local/share/waymux/tmux/waymux.tmux
COPY --chown=dev:dev config/default.toml /home/dev/.config/waymux/config.toml
COPY --chown=dev:dev docker/sandbox-tmux.conf /home/dev/.tmux.conf
COPY docker/entrypoint.sh docker/scenario.sh /usr/local/share/waymux-sandbox/
RUN chmod 755 /usr/local/share/waymux-sandbox/*.sh \
 && ln -sf /usr/local/share/waymux-sandbox/scenario.sh /usr/local/bin/scenario \
 && chmod 755 /usr/local/share/waymux/tmux/waymux.tmux \
 && ln -sf /usr/local/bin/waymux /usr/local/bin/acpd \
 && ln -sf /usr/local/bin/waymux /usr/local/bin/awt

# Fake AI agents: copies of `cat` named after real agents so the Cockpit's native
# /proc/<pid>/comm scanner detects them AND any injected prompt/confirmation is echoed live.
RUN install -d /usr/local/share/waymux-sandbox/fake-bin \
 && for agent in claude codex opencode agy; do \
      cp "$(command -v cat)" "/usr/local/share/waymux-sandbox/fake-bin/${agent}"; \
    done \
 && install -d -o dev -g dev /home/dev/.local/bin \
 && ln -sf /usr/local/bin/waymux /home/dev/.local/bin/waymux \
 && ln -sf /usr/local/bin/waymux /home/dev/.local/bin/acpd \
 && ln -sf /usr/local/bin/waymux /home/dev/.local/bin/awt \
 && ln -sf /usr/local/bin/wm /home/dev/.local/bin/wm \
 && ln -sf /usr/local/bin/lazygitrs /home/dev/.local/bin/lazygitrs \
 && chown -h dev:dev /home/dev/.local/bin/*

USER dev
WORKDIR /home/dev
ENTRYPOINT ["/usr/local/share/waymux-sandbox/entrypoint.sh"]
CMD ["demo"]

# ------------------------------------------------------------------------------
# Target `lite`: waymux compiled in Docker
# ------------------------------------------------------------------------------
FROM runtime AS lite
USER root
COPY --from=build-waymux /out/waymux /usr/local/bin/waymux
USER dev

# ------------------------------------------------------------------------------
# Target `full` (default): waymux + wm + lazygitrs compiled in Docker
# ------------------------------------------------------------------------------
FROM lite AS full
USER root
COPY --from=build-wm /out/wm /usr/local/bin/wm
COPY --from=build-lazygitrs /out/lazygitrs /usr/local/bin/lazygitrs
USER dev
