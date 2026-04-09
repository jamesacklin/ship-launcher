use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::errors::LauncherError;
use crate::state::{LauncherState, StateMachine};

const MAX_LOG_LINES: usize = 500;
const STOP_TIMEOUT_SECS: u64 = 30;
const DEFAULT_HTTP_PORT: u16 = 8080;

/// Internal state shared between the main thread and background monitor task.
struct ProcessState {
    /// PID of the running vere process, if any.
    pid: Option<u32>,
    /// Recent log lines captured from stdout/stderr (ring buffer).
    log_lines: Vec<String>,
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
    inner: Arc<Mutex<ProcessState>>,
}

impl RuntimeManager {
    pub fn new(state_machine: StateMachine, vere_path: PathBuf, pier_path: PathBuf) -> Self {
        Self {
            state_machine,
            vere_path,
            pier_path,
            http_port: DEFAULT_HTTP_PORT,
            inner: Arc::new(Mutex::new(ProcessState {
                pid: None,
                log_lines: Vec::new(),
            })),
        }
    }

    pub fn with_http_port(mut self, port: u16) -> Self {
        self.http_port = port;
        self
    }

    /// Start the vere runtime process.
    ///
    /// Transitions: current → Starting → Running.
    /// Spawns a background task that monitors stdout/stderr and detects exit.
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

        self.state_machine
            .transition(LauncherState::Starting)?;

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

        // Spawn vere.
        // -t disables terminal/tty assumptions (required when running as a child process).
        let mut child = Command::new(&self.vere_path)
            .arg("-t")
            .arg("--http-port")
            .arg(self.http_port.to_string())
            .arg(&self.pier_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                // Roll back to Error since we failed to spawn.
                self.state_machine.force_error(
                    format!("Failed to spawn vere: {e}"),
                    Some(format!("path: {}", self.vere_path.display())),
                );
                LauncherError::Runtime {
                    reason: format!("failed to spawn vere: {e}"),
                }
            })?;

        let pid = child.id().ok_or_else(|| {
            self.state_machine.force_error(
                "Failed to get vere process ID".into(),
                None,
            );
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
            // Process may have crashed before we got here; monitor task handles it.
            return Err(e);
        }

        // Spawn stdout reader.
        if let Some(stdout) = child.stdout.take() {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut state = inner.lock().unwrap();
                    if state.log_lines.len() >= MAX_LOG_LINES {
                        state.log_lines.remove(0);
                    }
                    state.log_lines.push(format!("[stdout] {line}"));
                }
            });
        }

        // Spawn stderr reader.
        if let Some(stderr) = child.stderr.take() {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut state = inner.lock().unwrap();
                    if state.log_lines.len() >= MAX_LOG_LINES {
                        state.log_lines.remove(0);
                    }
                    state.log_lines.push(format!("[stderr] {line}"));
                }
            });
        }

        // Spawn monitor task — waits for exit and updates state machine.
        let sm = self.state_machine.clone();
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            monitor_process(child, sm, inner).await;
        });

        Ok(pid)
    }

    /// Stop the running vere process gracefully.
    ///
    /// Sends SIGTERM, waits up to 30 seconds, then SIGKILL.
    /// Transitions: Running → Stopping → Stopped.
    pub async fn stop(&self) -> Result<(), LauncherError> {
        let pid = {
            let state = self.inner.lock().unwrap();
            state.pid
        };

        let pid = pid.ok_or_else(|| LauncherError::Runtime {
            reason: "no vere process is running".into(),
        })?;

        self.state_machine
            .transition(LauncherState::Stopping)?;

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
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }

        // Timeout — escalate to SIGKILL.
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
        // If already stopped/crashed, go straight to start.
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
        let state = self.inner.lock().unwrap();
        let len = state.log_lines.len();
        if n >= len {
            state.log_lines.clone()
        } else {
            state.log_lines[len - n..].to_vec()
        }
    }
}

/// Background task that waits for the child process to exit and transitions the
/// state machine accordingly.
async fn monitor_process(
    mut child: tokio::process::Child,
    state_machine: StateMachine,
    inner: Arc<Mutex<ProcessState>>,
) {
    let result = child.wait().await;

    // Clear PID so stop() polling and start() guard see the process as gone.
    {
        let mut state = inner.lock().unwrap();
        state.pid = None;
    }

    // Determine whether this was an expected stop or an unexpected exit.
    let current = state_machine.current();

    if matches!(current, LauncherState::Stopping) {
        // stop() initiated this — transition to Stopped.
        let _ = state_machine.transition(LauncherState::Stopped);
        return;
    }

    // Unexpected exit while Running (or Starting).
    match result {
        Ok(status) if status.success() => {
            if matches!(current, LauncherState::Starting) {
                // Process exited before we even reached Running — unexpected.
                let _ = state_machine.transition(LauncherState::Crashed {
                    exit_code: Some(0),
                    message: "vere exited immediately after starting".into(),
                });
            } else {
                let _ = state_machine.transition(LauncherState::Stopped);
            }
        }
        Ok(status) => {
            let _ = state_machine.transition(LauncherState::Crashed {
                exit_code: status.code(),
                message: format!("vere exited with status: {status}"),
            });
        }
        Err(e) => {
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
        RuntimeManager::new(sm, vere_path.to_path_buf(), pier_path.to_path_buf())
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

        // Script that sleeps briefly then exits 0.
        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 10\n");

        let mgr = make_manager(&vere, &pier);
        let pid = mgr.start().await.unwrap();
        assert!(pid > 0);
        assert_eq!(mgr.state_machine.current().label(), "Running");
        assert!(mgr.pid().is_some());

        // Clean up: stop the process.
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

        // Script that exits immediately with code 42.
        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nexit 42\n");
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        // Wait for the monitor to detect exit.
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

        // Script that runs briefly then exits cleanly (sleep ensures we reach Running).
        let vere = write_fake_vere(dir.path(), "#!/bin/sh\nsleep 0.3\nexit 0\n");
        let mgr = make_manager(&vere, &pier);
        mgr.start().await.unwrap();

        // Wait for process to finish + monitor to detect exit.
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

        // Second start should fail.
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

        // Should be a different process.
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

        // Give time for output to be captured.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let logs = mgr.recent_logs(10);
        let has_stdout = logs.iter().any(|l| l.contains("hello from stdout"));
        let has_stderr = logs.iter().any(|l| l.contains("hello from stderr"));
        assert!(has_stdout, "should capture stdout; got: {logs:?}");
        assert!(has_stderr, "should capture stderr; got: {logs:?}");

        mgr.stop().await.unwrap();
    }

    #[tokio::test]
    async fn recent_logs_returns_last_n_lines() {
        let mgr = RuntimeManager::new(
            StateMachine::new(),
            PathBuf::from("/dev/null"),
            PathBuf::from("/tmp"),
        );

        // Manually fill log lines.
        {
            let mut state = mgr.inner.lock().unwrap();
            for i in 0..10 {
                state.log_lines.push(format!("line {i}"));
            }
        }

        let last3 = mgr.recent_logs(3);
        assert_eq!(last3, vec!["line 7", "line 8", "line 9"]);

        let all = mgr.recent_logs(100);
        assert_eq!(all.len(), 10);
    }

    #[tokio::test]
    async fn spawn_failure_forces_error_state() {
        let dir = tempfile::tempdir().unwrap();
        let pier = dir.path().join("pier");
        std::fs::create_dir_all(&pier).unwrap();

        // Point at a non-existent binary.
        let mgr = make_manager(&dir.path().join("no-such-binary"), &pier);
        let result = mgr.start().await;

        assert!(result.is_err());
        assert_eq!(mgr.state_machine.current().label(), "Error");
    }

    #[test]
    fn with_http_port_overrides_default() {
        let mgr = RuntimeManager::new(
            StateMachine::new(),
            PathBuf::from("/tmp/vere"),
            PathBuf::from("/tmp/pier"),
        )
        .with_http_port(9090);

        assert_eq!(mgr.http_port, 9090);
    }
}
