use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::errors::LauncherError;
use crate::health::HealthChecker;
use crate::logs::LogManager;
use crate::state::{LauncherState, StateMachine};

const STOP_TIMEOUT_SECS: u64 = 30;
const DEFAULT_HTTP_PORT: u16 = 8080;

/// Internal state shared between the main thread and background monitor task.
struct ProcessState {
    /// PID of the running vere process, if any.
    pid: Option<u32>,
}

/// Manages the vere runtime process lifecycle: start, stop, restart.
///
/// Designed to be stored as Tauri managed state. Cloning shares the
/// underlying state via `Arc`.
#[derive(Clone)]
pub struct RuntimeManager {
    state_machine: StateMachine,
    vere_path: PathBuf,
    pier_path: PathBuf,
    http_port: u16,
    /// If set, boot a fake ship with `-F <name>` on first run.
    fake_ship: Option<String>,
    inner: Arc<Mutex<ProcessState>>,
    log_manager: LogManager,
    health: HealthChecker,
}

impl RuntimeManager {
    pub fn new(
        state_machine: StateMachine,
        vere_path: PathBuf,
        pier_path: PathBuf,
        log_manager: LogManager,
    ) -> Self {
        let http_port = DEFAULT_HTTP_PORT;
        let health = HealthChecker::new(http_port, log_manager.clone());
        Self {
            state_machine,
            vere_path,
            pier_path,
            http_port,
            fake_ship: None,
            inner: Arc::new(Mutex::new(ProcessState { pid: None })),
            log_manager,
            health,
        }
    }

    pub fn with_http_port(mut self, port: u16) -> Self {
        self.http_port = port;
        self.health = HealthChecker::new(port, self.log_manager.clone());
        self
    }

    /// Enable fake ship mode. On first boot (pier doesn't exist yet), vere
    /// will be invoked with `-F <name>` to create a fake ship.
    pub fn with_fake_ship(mut self, name: String) -> Self {
        self.fake_ship = Some(name);
        self
    }

    /// Returns the fake ship name, if fake mode is enabled.
    pub fn fake_ship(&self) -> Option<&str> {
        self.fake_ship.as_deref()
    }

    /// Start the vere runtime process.
    ///
    /// Transitions: current -> Starting -> Running.
    /// Spawns background tasks for stdout/stderr capture, process monitoring,
    /// and health checking.
    pub async fn start(&self) -> Result<u32, LauncherError> {
        // Enforce single-instance: if a PID is tracked we cannot start another.
        {
            let state = self.inner.lock().unwrap();
            if state.pid.is_some() {
                return Err(LauncherError::Runtime {
                    reason: "a vere process is already running".into(),
                });
            }
        }

        self.state_machine.transition(LauncherState::Starting)?;
        self.log_manager.add_launcher_line("Transitioning to Starting");

        // Ensure the binary is executable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&self.vere_path) {
                let mut perms = metadata.permissions();
                let mode = perms.mode();
                if mode & 0o111 == 0 {
                    perms.set_mode(mode | 0o111);
                    let _ = std::fs::set_permissions(&self.vere_path, perms);
                }
            }
        }

        // Determine whether this is a fake first-boot.
        // `-F <name>` creates a new fake ship; the pier directory is created
        // by vere in the working directory we set (pier_path's parent).
        let needs_fake_boot = self.fake_ship.is_some() && !self.pier_path.join(".urb").exists();

        // Spawn vere.
        // -t disables terminal/tty assumptions (required when running as a child process).
        let mut cmd = Command::new(&self.vere_path);
        cmd.arg("-t");

        if let (true, Some(ref name)) = (needs_fake_boot, &self.fake_ship) {
            self.log_manager.add_launcher_line(&format!(
                "Spawning vere (fake mode): {} -t -F {} --http-port {}",
                self.vere_path.display(),
                name,
                self.http_port,
            ));
            cmd.arg("-F").arg(name);
            // vere -F creates `<name>/` in the current directory, so set cwd
            // to the pier_path's parent so the pier lands at pier_path.
            if let Some(parent) = self.pier_path.parent() {
                std::fs::create_dir_all(parent).ok();
                cmd.current_dir(parent);
            }
        } else {
            self.log_manager.add_launcher_line(&format!(
                "Spawning vere: {} -t --http-port {} {}",
                self.vere_path.display(),
                self.http_port,
                self.pier_path.display()
            ));
            cmd.arg(&self.pier_path);
        }

        cmd.arg("--http-port").arg(self.http_port.to_string());

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                self.log_manager
                    .add_launcher_line(&format!("Failed to spawn vere: {e}"));
                self.state_machine.force_error(
                    format!("Failed to spawn vere: {e}"),
                    Some(format!("path: {}", self.vere_path.display())),
                );
                LauncherError::Runtime {
                    reason: format!("failed to spawn vere: {e}"),
                }
            })?;

        let pid = child.id().ok_or_else(|| {
            self.log_manager
                .add_launcher_line("Failed to get vere process ID");
            self.state_machine
                .force_error("Failed to get vere process ID".into(), None);
            LauncherError::Runtime {
                reason: "failed to get process ID after spawn".into(),
            }
        })?;

        // Record PID.
        {
            let mut state = self.inner.lock().unwrap();
            state.pid = Some(pid);
        }

        // Transition to Running.
        let started_at = chrono::Utc::now().to_rfc3339();
        if let Err(e) = self.state_machine.transition(LauncherState::Running {
            pid,
            started_at,
        }) {
            return Err(e);
        }

        self.log_manager
            .add_launcher_line(&format!("vere started with PID {pid}"));

        // Spawn stdout reader.
        if let Some(stdout) = child.stdout.take() {
            let lm = self.log_manager.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    lm.add_vere_line(&format!("[stdout] {line}"));
                }
            });
        }

        // Spawn stderr reader.
        if let Some(stderr) = child.stderr.take() {
            let lm = self.log_manager.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    lm.add_vere_line(&format!("[stderr] {line}"));
                }
            });
        }

        // Start health check polling.
        self.health.reset();
        self.health.start_polling();

        // Spawn monitor task -- waits for exit and updates state machine.
        let sm = self.state_machine.clone();
        let inner = Arc::clone(&self.inner);
        let lm = self.log_manager.clone();
        let hc = self.health.clone();
        tokio::spawn(async move {
            monitor_process(child, sm, inner, lm, hc).await;
        });

        Ok(pid)
    }

    /// Stop the running vere process gracefully.
    ///
    /// Sends SIGTERM, waits up to 30 seconds, then SIGKILL.
    /// Transitions: Running -> Stopping -> Stopped.
    pub async fn stop(&self) -> Result<(), LauncherError> {
        let pid = {
            let state = self.inner.lock().unwrap();
            state.pid
        };

        let pid = pid.ok_or_else(|| LauncherError::Runtime {
            reason: "no vere process is running".into(),
        })?;

        self.health.reset();
        self.state_machine.transition(LauncherState::Stopping)?;
        self.log_manager
            .add_launcher_line(&format!("Stopping vere (PID {pid})"));

        // Send SIGTERM.
        #[cfg(unix)]
        {
            // SAFETY: pid is a valid u32 from Child::id(). kill(2) is safe to
            // call with any pid/signal combo; it simply returns an error if the
            // process doesn't exist.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }

        // Poll until the monitor task clears the PID (process exited) or timeout.
        let deadline =
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(STOP_TIMEOUT_SECS);

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

            let still_running = self.inner.lock().unwrap().pid.is_some();
            if !still_running {
                self.log_manager.add_launcher_line("vere stopped cleanly");
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }

        // Timeout -- escalate to SIGKILL.
        self.log_manager.add_launcher_line(&format!(
            "vere did not exit after {STOP_TIMEOUT_SECS}s, sending SIGKILL"
        ));
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }

        // Give a moment for the monitor task to observe the exit.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        Ok(())
    }

    /// Restart: stop the current process, then start a new one.
    pub async fn restart(&self) -> Result<u32, LauncherError> {
        let needs_stop = self.inner.lock().unwrap().pid.is_some();
        if needs_stop {
            self.stop().await?;
        }
        self.start().await
    }

    /// Returns the PID of the running vere process, if any.
    pub fn pid(&self) -> Option<u32> {
        self.inner.lock().unwrap().pid
    }

    /// Returns the last `n` captured log lines.
    pub fn recent_logs(&self, n: usize) -> Vec<String> {
        self.log_manager.recent_lines(n)
    }

    /// Returns `true` if the ship's HTTP port is responding.
    pub fn is_ship_ready(&self) -> bool {
        self.health.is_ready()
    }

    /// Returns the configured HTTP port.
    pub fn http_port(&self) -> u16 {
        self.http_port
    }

    /// Returns a reference to the log manager for external use.
    pub fn log_manager(&self) -> &LogManager {
        &self.log_manager
    }
}

/// Background task that waits for the child process to exit and transitions the
/// state machine accordingly.
async fn monitor_process(
    mut child: tokio::process::Child,
    state_machine: StateMachine,
    inner: Arc<Mutex<ProcessState>>,
    log_manager: LogManager,
    health: HealthChecker,
) {
    let result = child.wait().await;

    // Reset health check -- port will no longer respond.
    health.reset();

    // Clear PID so stop() polling and start() guard see the process as gone.
    {
        let mut state = inner.lock().unwrap();
        state.pid = None;
    }

    // Determine whether this was an expected stop or an unexpected exit.
    let current = state_machine.current();

    if matches!(current, LauncherState::Stopping) {
        log_manager.add_launcher_line("vere process exited during graceful stop");
        let _ = state_machine.transition(LauncherState::Stopped);
        return;
    }

    // Unexpected exit while Running (or Starting).
    match result {
        Ok(status) if status.success() => {
            if matches!(current, LauncherState::Starting) {
                log_manager
                    .add_launcher_line("vere exited immediately after starting (exit code 0)");
                let _ = state_machine.transition(LauncherState::Crashed {
                    exit_code: Some(0),
                    message: "vere exited immediately after starting".into(),
                });
            } else {
                log_manager.add_launcher_line("vere self-terminated cleanly (exit code 0)");
                let _ = state_machine.transition(LauncherState::Stopped);
            }
        }
        Ok(status) => {
            log_manager
                .add_launcher_line(&format!("vere exited unexpectedly with status: {status}"));
            let _ = state_machine.transition(LauncherState::Crashed {
                exit_code: status.code(),
                message: format!("vere exited with status: {status}"),
            });
        }
        Err(e) => {
            log_manager.add_launcher_line(&format!("failed to wait on vere process: {e}"));
            let _ = state_machine.transition(LauncherState::Crashed {
                exit_code: None,
                message: format!("failed to wait on vere process: {e}"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Helper: create a RuntimeManager pointing at a fake vere script.
    fn make_manager(vere_path: &Path, pier_path: &Path) -> RuntimeManager {
        let sm = StateMachine::new();
        // Advance to Prepared so start() can transition to Starting.
        sm.transition(LauncherState::Prepared).unwrap();
        let lm = LogManager::new();
        RuntimeManager::new(sm, vere_path.to_path_buf(), pier_path.to_path_buf(), lm)
    }

    /// Write a small shell script that acts as a fake vere binary.
    fn write_fake_vere(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("fake-vere");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn start_spawns_process_and_transitions_to_running() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 10\n");

        let mgr = make_manager(&vere, &pier);
        let pid = mgr.start().await.unwrap();
        assert!(pid > 0);
        assert_eq!(mgr.state_machine.current().label(), "Running");
        assert!(mgr.pid().is_some());

        mgr.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stop_transitions_to_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 60\n");
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        mgr.stop().await.unwrap();

        // Give monitor task a moment to observe exit.
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        assert_eq!(mgr.state_machine.current().label(), "Stopped");
        assert!(mgr.pid().is_none());
    }

    #[tokio::test]
    async fn unexpected_exit_transitions_to_crashed() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nexit 42\n");
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let state = mgr.state_machine.current();
        assert_eq!(state.label(), "Crashed");
        if let LauncherState::Crashed { exit_code, .. } = state {
            assert_eq!(exit_code, Some(42));
        }
    }

    #[tokio::test]
    async fn clean_exit_transitions_to_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 0.3\nexit 0\n");
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

        assert_eq!(mgr.state_machine.current().label(), "Stopped");
    }

    #[tokio::test]
    async fn cannot_start_twice() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 60\n");
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        let err = mgr.start().await.unwrap_err();
        assert!(err.to_string().contains("already running"));

        mgr.stop().await.unwrap();
    }

    #[tokio::test]
    async fn restart_stops_then_starts() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 60\n");
        let mgr = make_manager(&vere, &pier);
        let pid1 = mgr.start().await.unwrap();

        let pid2 = mgr.restart().await.unwrap();

        assert_ne!(pid1, pid2);
        assert_eq!(mgr.state_machine.current().label(), "Running");

        mgr.stop().await.unwrap();
    }

    #[tokio::test]
    async fn captures_stdout_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(
            dir.path(),
            "#!/bin/sh\necho 'hello from stdout'\necho 'hello from stderr' >&2\nsleep 1\n",
        );
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let logs = mgr.recent_logs(50);
        let has_stdout = logs.iter().any(|l| l.contains("hello from stdout"));
        let has_stderr = logs.iter().any(|l| l.contains("hello from stderr"));
        assert!(has_stdout, "should capture stdout; got: {logs:?}");
        assert!(has_stderr, "should capture stderr; got: {logs:?}");

        mgr.stop().await.unwrap();
    }

    #[test]
    fn recent_logs_returns_last_n_lines() {
        let lm = LogManager::new();
        let mgr = RuntimeManager::new(
            StateMachine::new(),
            PathBuf::from("/dev/null"),
            PathBuf::from("/tmp"),
            lm,
        );

        // Manually fill log lines via log_manager.
        for i in 0..10 {
            mgr.log_manager().add_vere_line(&format!("line {i}"));
        }

        let last3 = mgr.recent_logs(3);
        assert_eq!(last3.len(), 3);
        assert!(last3[0].contains("line 7"));
        assert!(last3[1].contains("line 8"));
        assert!(last3[2].contains("line 9"));

        let all = mgr.recent_logs(100);
        assert_eq!(all.len(), 10);
    }

    #[tokio::test]
    async fn spawn_failure_forces_error_state() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let mgr = make_manager(&dir.path().join("no-such-binary"), &pier);
        let result = mgr.start().await;

        assert!(result.is_err());
        assert_eq!(mgr.state_machine.current().label(), "Error");
    }

    #[test]
    fn with_http_port_overrides_default() {
        let lm = LogManager::new();
        let mgr = RuntimeManager::new(
            StateMachine::new(),
            PathBuf::from("/tmp/vere"),
            PathBuf::from("/tmp/pier"),
            lm,
        )
        .with_http_port(9090);

        assert_eq!(mgr.http_port, 9090);
    }

    #[tokio::test]
    async fn logs_lifecycle_events() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 60\n");
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        let logs = mgr.recent_logs(50);
        assert!(
            logs.iter().any(|l| l.contains("[launcher]") && l.contains("Starting")),
            "should log transition to Starting; got: {logs:?}"
        );
        assert!(
            logs.iter().any(|l| l.contains("[launcher]") && l.contains("PID")),
            "should log PID; got: {logs:?}"
        );

        mgr.stop().await.unwrap();

        let logs = mgr.recent_logs(50);
        assert!(
            logs.iter().any(|l| l.contains("[launcher]") && l.contains("Stopping")),
            "should log stop; got: {logs:?}"
        );
    }

    #[tokio::test]
    async fn health_resets_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 60\n");
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        // Health won't actually become ready (no HTTP server), but verify reset works.
        assert!(!mgr.is_ship_ready());

        mgr.stop().await.unwrap();
        assert!(!mgr.is_ship_ready());
    }
}
