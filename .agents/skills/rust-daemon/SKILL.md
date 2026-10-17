---
name: rust-daemon
description: Build production-ready Rust daemons that integrate with systemd on Linux. Use this skill whenever the user wants to create a background service, long-running process, or system daemon in Rust — including signal handling (SIGTERM, SIGHUP, SIGUSR1/2), systemd unit file generation, structured logging via the tracing crate, TOML/YAML configuration, PID file management, graceful shutdown orchestration, health check endpoints, readiness/liveness probes, and systemd socket activation. Also activate when the user mentions daemonizing a Rust binary, writing a .service file, or asks about sd_notify, Type=notify, or watchdog patterns in Rust.
---

# Rust Daemon Skill

This skill guides you through building production-grade Rust daemons that run as systemd services on Linux. It covers the full lifecycle: from project scaffolding to deployment-ready unit files.

## When to use this

Activate whenever the task involves:
- Creating a new Rust daemon/service from scratch
- Adding systemd integration to an existing Rust binary
- Implementing signal handling or graceful shutdown
- Writing systemd unit files for a Rust project
- Setting up health checks or readiness probes
- Socket activation patterns

## Project scaffolding

When starting a new daemon project, set up this structure:

```
my-daemon/
├── Cargo.toml
├── src/
│   ├── main.rs          # Entry point, signal setup, runtime bootstrap
│   ├── config.rs        # Configuration loading and validation
│   ├── daemon.rs        # Core daemon logic and run loop
│   ├── health.rs        # Health check / readiness endpoint
│   ├── signals.rs       # Signal handler registration
│   └── pid.rs           # PID file management
├── config/
│   └── default.toml     # Default configuration template
└── systemd/
    └── my-daemon.service  # systemd unit file
```

## Core dependencies

Use this `Cargo.toml` dependency block as a starting point. Adjust versions to latest stable at the time of generation:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
axum = "0.8"                       # If the daemon exposes an HTTP interface
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tracing-journald = "0.3"           # Direct journald integration
serde = { version = "1", features = ["derive"] }
toml = "0.8"                       # Config file parsing
nix = { version = "0.29", features = ["signal", "fs"] }  # Unix signal handling
sd-notify = "0.4"                  # systemd readiness notification
anyhow = "1"                       # Error handling
```

Only include dependencies the daemon actually needs — if there's no HTTP interface, drop `axum`. If the user prefers YAML config, swap `toml` for `serde_yaml`.

## Signal handling

Register handlers for standard daemon signals. Use `tokio::signal::unix` for async signal handling within the Tokio runtime:

```rust
use tokio::signal::unix::{signal, SignalKind};

pub async fn signal_listener(shutdown_tx: tokio::sync::watch::Sender<bool>) {
    let mut sigterm = signal(SignalKind::terminate())
        .expect("failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt())
        .expect("failed to register SIGINT handler");
    let mut sighup = signal(SignalKind::hangup())
        .expect("failed to register SIGHUP handler");

    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM, initiating graceful shutdown");
        }
        _ = sigint.recv() => {
            tracing::info!("received SIGINT, initiating graceful shutdown");
        }
        _ = sighup.recv() => {
            tracing::info!("received SIGHUP, reloading configuration");
            // Trigger config reload instead of shutdown
            // reload_config().await;
            return; // Don't shut down on SIGHUP
        }
    }

    let _ = shutdown_tx.send(true);
}
```

The key pattern here: use a `watch` channel to broadcast shutdown intent to all tasks. Each long-running task should `select!` on its work and the shutdown receiver, allowing it to finish in-flight operations before exiting.

**SIGHUP** is conventionally used for configuration reload, not shutdown — handle it separately from SIGTERM/SIGINT.

## Graceful shutdown

The shutdown pattern should flow like this:

1. Signal received → set shutdown flag via `watch` channel
2. Stop accepting new work (close listener sockets, stop polling, etc.)
3. Drain in-flight work with a timeout
4. Clean up resources (flush logs, close DB connections, remove PID file)
5. Notify systemd of stopping (if using `Type=notify`)
6. Exit with code 0

```rust
pub async fn run(config: Config) -> anyhow::Result<()> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Spawn the signal listener
    let signal_handle = tokio::spawn(signal_listener(shutdown_tx));

    // Spawn your daemon's main work
    let work_handle = tokio::spawn(daemon_loop(shutdown_rx.clone()));

    // Notify systemd we're ready
    sd_notify::notify(false, &[sd_notify::NotifyState::Ready])?;

    // Wait for shutdown signal
    signal_handle.await?;

    // Give in-flight work time to finish
    let drain_timeout = tokio::time::Duration::from_secs(
        config.shutdown_timeout_secs.unwrap_or(30)
    );
    match tokio::time::timeout(drain_timeout, work_handle).await {
        Ok(Ok(())) => tracing::info!("clean shutdown complete"),
        Ok(Err(e)) => tracing::error!("work task panicked: {e}"),
        Err(_) => tracing::warn!("shutdown timed out, forcing exit"),
    }

    // Notify systemd we're stopping
    sd_notify::notify(false, &[sd_notify::NotifyState::Stopping])?;

    Ok(())
}
```

Always set a shutdown timeout. Daemons that hang on shutdown are the bane of system administrators — a configurable timeout (defaulting to 30s) keeps the service manageable.

## Configuration

Use a layered configuration approach: defaults → config file → environment variables → CLI args.

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub pid_file: Option<String>,
    pub shutdown_timeout_secs: Option<u64>,
    pub log_level: Option<String>,
}

fn default_listen_addr() -> String { "127.0.0.1".to_string() }
fn default_port() -> u16 { 8080 }

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
```

Provide a `config/default.toml` template alongside the binary so users know what's configurable:

```toml
# default.toml — reference configuration for my-daemon
listen_addr = "127.0.0.1"
port = 8080
# pid_file = "/run/my-daemon/my-daemon.pid"
shutdown_timeout_secs = 30
log_level = "info"
```

## Logging with tracing

Set up structured logging that integrates with journald when running under systemd, and falls back to stdout for development:

```rust
use tracing_subscriber::{fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_logging(log_level: &str) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

    // Try journald first (works when running under systemd)
    if let Ok(journald_layer) = tracing_journald::layer() {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(journald_layer)
            .init();
        return;
    }

    // Fall back to stdout with structured formatting
    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_target(true).with_thread_ids(true))
        .init();
}
```

This dual-mode approach means the daemon logs natively to journald in production (where `journalctl -u my-daemon` just works) but still gives readable output when developing locally.

## PID file management

PID files let other tools (monitoring, init scripts) know if the daemon is running. Always clean up on exit:

```rust
use std::fs;
use std::io::Write;
use std::process;

pub struct PidFile {
    path: String,
}

impl PidFile {
    pub fn create(path: &str) -> anyhow::Result<Self> {
        // Check for stale PID file
        if let Ok(existing) = fs::read_to_string(path) {
            if let Ok(pid) = existing.trim().parse::<i32>() {
                // Check if process is still running
                if nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    None
                ).is_ok() {
                    anyhow::bail!(
                        "daemon already running with PID {pid} (PID file: {path})"
                    );
                }
                // Stale PID file, remove it
                tracing::warn!("removing stale PID file for PID {pid}");
            }
        }

        let mut file = fs::File::create(path)?;
        write!(file, "{}", process::id())?;
        Ok(PidFile { path: path.to_string() })
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            tracing::error!("failed to remove PID file {}: {e}", self.path);
        }
    }
}
```

The `Drop` implementation ensures the PID file gets cleaned up even if the daemon exits unexpectedly (as long as it's not killed with SIGKILL). The stale-file check prevents confusing "already running" errors after crashes.

## Health checks and readiness probes

Expose a lightweight HTTP endpoint for health checks. This integrates with systemd's `Type=notify` and with external monitoring:

```rust
use axum::{routing::get, Router, Json};
use serde::Serialize;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    uptime_secs: u64,
}

async fn health_check(
    start_time: axum::extract::Extension<std::time::Instant>,
) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy",
        uptime_secs: start_time.elapsed().as_secs(),
    })
}

pub fn health_router() -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/ready", get(|| async { "ok" }))
}
```

For daemons that don't expose HTTP, implement health checks via the PID file or a Unix domain socket instead of adding a full HTTP stack just for monitoring.

## systemd unit file

Generate a unit file tailored to the daemon. Use `Type=notify` when the daemon calls `sd_notify`:

```ini
[Unit]
Description=My Daemon Service
Documentation=https://github.com/user/my-daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExecStart=/usr/local/bin/my-daemon --config /etc/my-daemon/config.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5s
WatchdogSec=30s

# Security hardening
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=/var/lib/my-daemon /run/my-daemon

# Resource limits
LimitNOFILE=65536
MemoryMax=512M

# Runtime directory for PID file and sockets
RuntimeDirectory=my-daemon
StateDirectory=my-daemon

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=my-daemon

[Install]
WantedBy=multi-user.target
```

Important choices in this unit file:
- **`Type=notify`** means systemd waits for the daemon to signal readiness via `sd_notify` before considering it "started" — much more reliable than `Type=simple`.
- **`WatchdogSec`** enables the systemd watchdog: the daemon must periodically call `sd_notify` with `Watchdog=1` or systemd will restart it. Implement this if your daemon has a main loop.
- **`ExecReload`** sends SIGHUP, which the signal handler should catch and use to reload config.
- **Security directives** (`NoNewPrivileges`, `ProtectSystem`, etc.) are defense-in-depth — always include them and only relax as needed.

### Watchdog integration

If using `WatchdogSec`, add periodic watchdog pings in the daemon's main loop:

```rust
async fn daemon_loop(mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let mut watchdog_interval = tokio::time::interval(
        tokio::time::Duration::from_secs(15) // Must be < WatchdogSec / 2
    );

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => break,
            _ = watchdog_interval.tick() => {
                sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog])
                    .ok(); // Don't crash if not running under systemd
            }
            // ... your actual daemon work here ...
        }
    }
}
```

## Socket activation

systemd can open sockets on behalf of the daemon and hand them over at startup. This enables zero-downtime restarts and faster boot times because systemd can buffer incoming connections while the daemon initializes.

```rust
use std::os::unix::io::FromRawFd;
use std::net::TcpListener;

/// Retrieve socket-activated file descriptors from systemd.
/// Returns None if not running under socket activation.
pub fn get_systemd_socket() -> Option<TcpListener> {
    // SD_LISTEN_FDS_START is always 3
    const SD_LISTEN_FDS_START: i32 = 3;

    let listen_fds: i32 = std::env::var("LISTEN_FDS")
        .ok()?
        .parse()
        .ok()?;

    if listen_fds < 1 {
        return None;
    }

    // Safety: systemd guarantees this FD is valid when LISTEN_FDS is set
    let listener = unsafe {
        TcpListener::from_raw_fd(SD_LISTEN_FDS_START)
    };

    // Set non-blocking for tokio compatibility
    listener.set_nonblocking(true).ok()?;
    Some(listener)
}
```

Pair this with a `.socket` unit:

```ini
# my-daemon.socket
[Unit]
Description=My Daemon Socket

[Socket]
ListenStream=127.0.0.1:8080
NoDelay=true

[Install]
WantedBy=sockets.target
```

When using socket activation, the daemon should check for passed file descriptors first and only fall back to binding its own socket if not activated:

```rust
let listener = if let Some(systemd_listener) = get_systemd_socket() {
    tracing::info!("using systemd socket activation");
    tokio::net::TcpListener::from_std(systemd_listener)?
} else {
    tracing::info!("binding to {}:{}", config.listen_addr, config.port);
    tokio::net::TcpListener::bind(
        format!("{}:{}", config.listen_addr, config.port)
    ).await?
};
```

## Local Token Authentication

When a daemon exposes an HTTP/RPC control API locally, protect endpoints using auto-generated session tokens written to a secure file with restricted permissions (`0600`):

```rust
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn get_or_create_token(token_path: &Path) -> anyhow::Result<String> {
    if token_path.exists() {
        return Ok(fs::read_to_string(token_path)?.trim().to_string());
    }

    if let Some(parent) = token_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let token = format!("{:x}", rand::random::<u128>());
    fs::write(token_path, &token)?;
    fs::set_permissions(token_path, fs::Permissions::from_mode(0600))?;

    Ok(token)
}
```

In Axum, extract and validate the Bearer token or fallback query/path parameter:

```rust
use axum::{http::{HeaderMap, StatusCode}, middleware::Next, response::Response, extract::Request};

pub async fn auth_middleware(
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected_token = get_expected_token(); // From app state or environment
    if let Some(auth_header) = headers.get("Authorization").and_then(|h| h.to_str().ok()) {
        if let Some(token) = auth_header.strip_prefix("Bearer ") {
            if token.trim() == expected_token {
                return Ok(next.run(request).await);
            }
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}
```

## Periodic Background Maintenance Tasks

Daemons often need to purge dead sessions, stale resources, or sync external UI surfaces periodically. Spawn a dedicated background task using Tokio timers alongside the primary runtime:

```rust
pub fn spawn_cleanup_task(
    cleanup_interval_secs: u64,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(
            tokio::time::Duration::from_secs(cleanup_interval_secs)
        );
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => break,
                _ = interval.tick() => {
                    if let Err(e) = purge_stale_resources().await {
                        tracing::error!("failed periodic cleanup task: {e}");
                    }
                }
            }
        }
    });
}
```

## Debounced State Updates & Race Condition Protection

When external events trigger rapid state transitions (e.g. `working` -> `idle`), avoid UI flicker or out-of-order execution by attaching sequence IDs and timestamps to per-entity state mutations:

```rust
pub struct EntityState {
    pub current_state: String,
    pub sequence_id: u64,
    pub last_updated: std::time::Instant,
}

// When receiving a state update:
// 1. Increment sequence_id for the entity.
// 2. Discard incoming updates with stale sequence_ids or timestamps.
// 3. For 'idle' debouncing, delay state transition using tokio::spawn + tokio::time::sleep,
//    verifying sequence_id hasn't changed before executing UI changes.
```

## Putting it all together — main.rs

Here's how all the pieces compose in `main.rs`:

```rust
use anyhow::Result;

mod config;
mod daemon;
mod health;
mod pid;
mod signals;

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    let config_path = std::env::args()
        .nth(2)  // --config <path>
        .unwrap_or_else(|| "/etc/my-daemon/config.toml".to_string());
    let config = config::Config::load(&config_path)?;

    // Initialize logging
    let log_level = config.log_level.as_deref().unwrap_or("info");
    // init_logging(log_level);

    tracing::info!("starting my-daemon v{}", env!("CARGO_PKG_VERSION"));

    // Create PID file
    let _pid = if let Some(ref path) = config.pid_file {
        Some(pid::PidFile::create(path)?)
    } else {
        None
    };

    // Run the daemon (handles signals, shutdown, sd_notify internally)
    daemon::run(config).await?;

    tracing::info!("my-daemon stopped cleanly");
    Ok(())
}
```

## Deployment checklist

When the daemon is ready for deployment, verify:

- [ ] `cargo build --release` produces a static-enough binary
- [ ] The systemd unit file is installed to `/etc/systemd/system/`
- [ ] Config file is deployed to the expected path
- [ ] `systemctl daemon-reload` has been run
- [ ] `systemctl enable --now my-daemon` starts and enables at boot
- [ ] `journalctl -u my-daemon -f` shows structured logs
- [ ] `systemctl reload my-daemon` triggers config reload via SIGHUP
- [ ] `systemctl stop my-daemon` triggers graceful shutdown
- [ ] Health check endpoint responds correctly
- [ ] PID file is created and cleaned up properly
