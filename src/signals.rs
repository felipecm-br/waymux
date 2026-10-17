use tokio::signal::unix::{SignalKind, signal};

pub async fn signal_listener(shutdown_tx: tokio::sync::watch::Sender<bool>) {
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
    let mut sighup = signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, initiating graceful shutdown");
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("received SIGINT, initiating graceful shutdown");
                break;
            }
            _ = sighup.recv() => {
                tracing::info!("received SIGHUP, reloading configuration not implemented yet");
            }
        }
    }

    let _ = shutdown_tx.send(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_signal_listener_sighup_does_not_shutdown() {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(signal_listener(shutdown_tx));

        // Allow signal_listener task to run and register signal handlers
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Send SIGHUP to self
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGHUP).unwrap();

        // Give signal_listener time to handle SIGHUP
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Verify shutdown was NOT triggered
        assert!(!*shutdown_rx.borrow());

        // Send SIGTERM to self
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).unwrap();

        // Wait for listener task to exit
        let _ = handle.await;

        // Verify shutdown WAS triggered
        assert!(*shutdown_rx.borrow());
    }
}
