use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::logs::LogManager;

const POLL_INTERVAL_SECS: u64 = 2;
const POLL_TIMEOUT_SECS: u64 = 120;
const CONNECT_TIMEOUT_MILLIS: u64 = 1000;

/// Monitors the local HTTP port to determine when the ship is ready.
///
/// After `start_polling()` is called, a background task probes
/// `127.0.0.1:<port>` every 2 seconds. Once a TCP connection succeeds the
/// ready flag is set. If no connection succeeds within 120 seconds a warning
/// is logged but the runtime is left in `Running` state.
///
/// Clone-safe via `Arc`.
#[derive(Clone)]
pub struct HealthChecker {
    ready: Arc<AtomicBool>,
    port: u16,
    log_manager: LogManager,
}

impl HealthChecker {
    pub fn new(port: u16, log_manager: LogManager) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(false)),
            port,
            log_manager,
        }
    }

    /// Returns `true` once the ship's HTTP port has responded.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// Reset the readiness flag (e.g., when the runtime stops or crashes).
    pub fn reset(&self) {
        self.ready.store(false, Ordering::Relaxed);
    }

    /// Begin polling in the background. Returns immediately.
    ///
    /// The spawned task tries to TCP-connect to the configured port every
    /// [`POLL_INTERVAL_SECS`] seconds. On success it sets the ready flag and
    /// exits. On timeout ([`POLL_TIMEOUT_SECS`] seconds) it logs a warning
    /// and exits without changing the ready flag.
    pub fn start_polling(&self) {
        let ready = Arc::clone(&self.ready);
        let port = self.port;
        let log_manager = self.log_manager.clone();

        tokio::spawn(async move {
            poll_until_ready(ready, port, log_manager).await;
        });
    }
}

/// Core polling loop, extracted for testability.
async fn poll_until_ready(ready: Arc<AtomicBool>, port: u16, log_manager: LogManager) {
    let start = tokio::time::Instant::now();
    let timeout = tokio::time::Duration::from_secs(POLL_TIMEOUT_SECS);
    let interval = tokio::time::Duration::from_secs(POLL_INTERVAL_SECS);
    let connect_timeout = tokio::time::Duration::from_millis(CONNECT_TIMEOUT_MILLIS);

    log_manager.add_launcher_line(&format!(
        "Health check: polling localhost:{port} every {POLL_INTERVAL_SECS}s (timeout {POLL_TIMEOUT_SECS}s)"
    ));

    loop {
        if start.elapsed() >= timeout {
            log_manager.add_launcher_line(&format!(
                "Health check: timed out after {POLL_TIMEOUT_SECS}s \u{2014} ship may still be booting"
            ));
            break;
        }

        let result = tokio::time::timeout(
            connect_timeout,
            tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")),
        )
        .await;

        match result {
            Ok(Ok(_stream)) => {
                ready.store(true, Ordering::Relaxed);
                log_manager.add_launcher_line(&format!(
                    "Health check: localhost:{port} is responding \u{2014} ship is ready"
                ));
                return;
            }
            _ => {
                // Connection refused or timed out; wait and retry.
                tokio::time::sleep(interval).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detects_listening_port() {
        // Bind a TCP listener on an ephemeral port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let lm = LogManager::new();
        let hc = HealthChecker::new(port, lm.clone());

        assert!(!hc.is_ready());
        hc.start_polling();

        // Wait for the polling task to detect the listener.
        tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;

        assert!(hc.is_ready());

        let logs = lm.recent_lines(10);
        assert!(logs.iter().any(|l| l.contains("ship is ready")));
    }

    #[tokio::test]
    async fn not_ready_when_port_closed() {
        // Use a port that nothing is listening on.
        let lm = LogManager::new();
        let hc = HealthChecker::new(19999, lm);

        assert!(!hc.is_ready());
        // Don't start polling here since it would run for 120s.
    }

    #[tokio::test]
    async fn reset_clears_ready_flag() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let lm = LogManager::new();
        let hc = HealthChecker::new(port, lm);
        hc.start_polling();

        tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
        assert!(hc.is_ready());

        hc.reset();
        assert!(!hc.is_ready());
    }

    #[tokio::test]
    async fn poll_until_ready_succeeds_on_open_port() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let ready = Arc::new(AtomicBool::new(false));
        let lm = LogManager::new();

        poll_until_ready(Arc::clone(&ready), port, lm).await;

        assert!(ready.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn poll_logs_health_check_start() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let lm = LogManager::new();
        let hc = HealthChecker::new(port, lm.clone());
        hc.start_polling();

        tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;

        let logs = lm.recent_lines(10);
        assert!(logs.iter().any(|l| l.contains("Health check: polling")));
    }
}
