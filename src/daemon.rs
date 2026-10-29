use crate::adapters::{SoundAdapter, TmuxAdapter, WaybarAdapter};
use crate::api::api_router;
use crate::config::Config;
use crate::health::health_router;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;

/// `config_path` is the `--config` argument; without it a reload searches the default paths
/// again, so a config file created after startup is picked up.
pub async fn run(config: Config, config_path: Option<String>) -> anyhow::Result<()> {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    // Capacity 1: SIGHUPs that arrive during a reload collapse into one more reload.
    let (reload_tx, mut reload_rx) = tokio::sync::mpsc::channel(1);

    let signal_handle = tokio::spawn(crate::signals::signal_listener(shutdown_tx, reload_tx));

    tracing::info!("Daemon binding to {}:{}", config.listen_addr, config.port);
    let listener = TcpListener::bind(format!("{}:{}", config.listen_addr, config.port)).await?;

    let start_time = Arc::new(Instant::now());

    let (active_spinner_name, active_spinner) = config.active_spinner();
    tokio::spawn(publish_spinner_name(active_spinner_name));

    let mut adapters = crate::api::ApiState::new(
        vec![
            Box::new(TmuxAdapter::new(config.theme.clone(), active_spinner)),
            Box::new(WaybarAdapter::new(config.theme.clone())),
            Box::new(SoundAdapter::new(config.sound.clone())),
        ],
        0,
    );
    adapters.phone_token = Arc::new(crate::api::phone::PhoneToken::load());
    adapters.hook_tokens = Arc::new(crate::api::hook_tokens::HookTokens::load());
    adapters.reload(&config);

    adapters.rehydrate_from_tmux().await;

    let reload_state = adapters.clone();
    let bound = (config.listen_addr.clone(), config.port);
    tokio::spawn(async move {
        while reload_rx.recv().await.is_some() {
            reload(&reload_state, config_path.as_deref(), &bound).await;
        }
    });

    let cleanup_state = adapters.clone();
    let mut cleanup_shutdown_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = cleanup_shutdown_rx.changed() => break,
                _ = interval.tick() => {
                    cleanup_state.clean_stale_panes().await;
                }
            }
        }
    });

    let stall_state = adapters.clone();
    let mut stall_shutdown_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = stall_shutdown_rx.changed() => break,
                _ = interval.tick() => {
                    stall_state.check_pane_stalls().await;
                }
            }
        }
    });

    if let Some(interval) = watchdog_interval() {
        tracing::info!("systemd watchdog: heartbeat every {:?}", interval);
        tokio::spawn(watchdog(adapters.clone(), interval, shutdown_rx.clone()));
    }

    let token = Arc::new(crate::auth::generate_and_save_token()?);

    let app = axum::Router::new()
        .merge(health_router(start_time))
        .merge(api_router(adapters, token));

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
            })
            .await
    });

    sd_notify::notify(false, &[sd_notify::NotifyState::Ready])?;

    signal_handle.await?;

    match tokio::time::timeout(
        std::time::Duration::from_secs(config.shutdown_timeout_secs.unwrap_or(30)),
        server_handle,
    )
    .await
    {
        Ok(Ok(Ok(()))) => tracing::info!("axum server cleanly shutdown"),
        Ok(Ok(Err(e))) => tracing::error!("axum server error: {}", e),
        Ok(Err(e)) => tracing::error!("server task panicked: {}", e),
        Err(_) => tracing::error!("server shutdown timed out"),
    }

    sd_notify::notify(false, &[sd_notify::NotifyState::Stopping])?;

    Ok(())
}

/// Tells tmux-side scripts which spinner the status bar animates.
async fn publish_spinner_name(name: String) {
    let _ = crate::tmux_cmd::async_command()
        .args(["set", "-g", "@ai_agent_spinner", &name])
        .output()
        .await;
}

/// SIGHUP: re-read the config and apply what can change while running. A config that fails
/// to load is logged and the running one kept. The listen address needs a restart.
async fn reload(state: &crate::api::ApiState, config_path: Option<&str>, bound: &(String, u16)) {
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Reloading]);
    match Config::resolve(config_path) {
        Ok((config, loaded_from)) => {
            if (&config.listen_addr, config.port) != (&bound.0, bound.1) {
                tracing::warn!(
                    "listen address {}:{} takes effect after a restart; still on {}:{}",
                    config.listen_addr,
                    config.port,
                    bound.0,
                    bound.1
                );
            }
            state.reload(&config);
            publish_spinner_name(config.active_spinner().0).await;
            tracing::info!(
                "reloaded config from {}",
                loaded_from.as_deref().unwrap_or("built-in defaults")
            );
        }
        Err(e) => tracing::error!("config reload failed, keeping the running config: {e:#}"),
    }
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
}

/// Half of `WatchdogSec=`, or None when systemd is not watching this process.
fn watchdog_interval() -> Option<std::time::Duration> {
    let mut usec = 0;
    // `false`: leave WATCHDOG_USEC alone; removing variables is unsound once threads run.
    (sd_notify::watchdog_enabled(false, &mut usec) && usec > 0)
        .then(|| std::time::Duration::from_micros(usec / 2))
}

/// Sends WATCHDOG=1 only while the pane-state lock every request takes can be acquired, so
/// a deadlock there stops the heartbeat and systemd restarts the daemon.
async fn watchdog(
    state: crate::api::ApiState,
    interval: std::time::Duration,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => break,
            _ = ticker.tick() => {
                if tokio::time::timeout(interval, state.pane_states.lock()).await.is_ok() {
                    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
                } else {
                    tracing::error!("pane state lock held for {:?}; skipping watchdog ping", interval);
                }
            }
        }
    }
}
