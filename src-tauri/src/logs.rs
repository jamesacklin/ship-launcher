use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;

const MAX_LOG_LINES: usize = 500;
const MAX_LOG_FILE_BYTES: u64 = 10 * 1024 * 1024; // 10 MB

/// Internal state for the log manager.
struct LogState {
    /// In-memory ring buffer of recent log lines.
    lines: Vec<String>,
    /// Path to the vere log file on disk, if initialized.
    vere_log_path: Option<PathBuf>,
    /// Path to the launcher log file on disk, if initialized.
    launcher_log_path: Option<PathBuf>,
}

/// Manages log capture, in-memory ring buffer, and disk persistence.
///
/// All vere stdout/stderr output and launcher events flow through here.
/// Clone-safe via `Arc`; designed to be shared across async tasks.
#[derive(Clone)]
pub struct LogManager {
    inner: Arc<Mutex<LogState>>,
}

impl LogManager {
    /// Create a new LogManager with no disk persistence.
    ///
    /// Call [`init_disk_logs`] to enable writing to log files.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogState {
                lines: Vec::new(),
                vere_log_path: None,
                launcher_log_path: None,
            })),
        }
    }

    /// Initialize disk logging to the given directory.
    ///
    /// Creates the directory if needed. Truncates existing log files if they
    /// exceed 10 MB (simple rotation strategy).
    pub fn init_disk_logs(&self, logs_dir: &Path) -> std::io::Result<()> {
        fs::create_dir_all(logs_dir)?;

        let vere_path = logs_dir.join("vere.log");
        let launcher_path = logs_dir.join("launcher.log");

        truncate_if_oversized(&vere_path)?;
        truncate_if_oversized(&launcher_path)?;

        let mut state = self.inner.lock().unwrap();
        state.vere_log_path = Some(vere_path);
        state.launcher_log_path = Some(launcher_path);

        Ok(())
    }

    /// Record a line of vere output (stdout or stderr).
    ///
    /// The line is timestamped, added to the ring buffer, and appended to
    /// `vere.log` on disk (if disk logging is initialized).
    pub fn add_vere_line(&self, line: &str) {
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let stamped = format!("{timestamp} {line}");

        let mut state = self.inner.lock().unwrap();

        if state.lines.len() >= MAX_LOG_LINES {
            state.lines.remove(0);
        }
        state.lines.push(stamped.clone());

        if let Some(ref path) = state.vere_log_path {
            let _ = append_line(path, &stamped);
        }
    }

    /// Record a launcher event (not vere output).
    ///
    /// Prefixed with `[launcher]`, timestamped, added to the ring buffer,
    /// and appended to `launcher.log` on disk.
    pub fn add_launcher_line(&self, line: &str) {
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let stamped = format!("{timestamp} [launcher] {line}");

        let mut state = self.inner.lock().unwrap();

        if state.lines.len() >= MAX_LOG_LINES {
            state.lines.remove(0);
        }
        state.lines.push(stamped.clone());

        if let Some(ref path) = state.launcher_log_path {
            let _ = append_line(path, &stamped);
        }
    }

    /// Returns the last `n` lines from the ring buffer.
    pub fn recent_lines(&self, n: usize) -> Vec<String> {
        let state = self.inner.lock().unwrap();
        let len = state.lines.len();
        if n >= len {
            state.lines.clone()
        } else {
            state.lines[len - n..].to_vec()
        }
    }

    /// Clear all in-memory log lines.
    pub fn clear(&self) {
        self.inner.lock().unwrap().lines.clear();
    }
}

/// Truncate the file to zero bytes if it exceeds the size limit.
fn truncate_if_oversized(path: &Path) -> std::io::Result<()> {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > MAX_LOG_FILE_BYTES {
            File::create(path)?;
        }
    }
    Ok(())
}

/// Append a single line (with newline) to the given file.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_respects_max_lines() {
        let lm = LogManager::new();
        for i in 0..MAX_LOG_LINES + 50 {
            lm.add_vere_line(&format!("line {i}"));
        }
        let all = lm.recent_lines(MAX_LOG_LINES + 100);
        assert_eq!(all.len(), MAX_LOG_LINES);
        // Oldest should be line 50
        assert!(all[0].contains("line 50"));
    }

    #[test]
    fn recent_lines_returns_last_n() {
        let lm = LogManager::new();
        for i in 0..10 {
            lm.add_vere_line(&format!("line {i}"));
        }
        let last3 = lm.recent_lines(3);
        assert_eq!(last3.len(), 3);
        assert!(last3[0].contains("line 7"));
        assert!(last3[1].contains("line 8"));
        assert!(last3[2].contains("line 9"));
    }

    #[test]
    fn recent_lines_when_fewer_than_requested() {
        let lm = LogManager::new();
        lm.add_vere_line("only line");
        let lines = lm.recent_lines(100);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("only line"));
    }

    #[test]
    fn launcher_lines_are_prefixed() {
        let lm = LogManager::new();
        lm.add_launcher_line("app started");
        let lines = lm.recent_lines(1);
        assert!(lines[0].contains("[launcher]"));
        assert!(lines[0].contains("app started"));
    }

    #[test]
    fn lines_are_timestamped() {
        let lm = LogManager::new();
        lm.add_vere_line("test");
        let lines = lm.recent_lines(1);
        // Should contain ISO-8601 timestamp pattern
        assert!(lines[0].contains("T"), "expected timestamp, got: {}", lines[0]);
    }

    #[test]
    fn clear_empties_buffer() {
        let lm = LogManager::new();
        lm.add_vere_line("a");
        lm.add_vere_line("b");
        assert_eq!(lm.recent_lines(10).len(), 2);
        lm.clear();
        assert_eq!(lm.recent_lines(10).len(), 0);
    }

    #[test]
    fn disk_logging_writes_files() {
        let dir = tempfile::tempdir().unwrap();
        let lm = LogManager::new();
        lm.init_disk_logs(dir.path()).unwrap();

        lm.add_vere_line("[stdout] hello vere");
        lm.add_launcher_line("started up");

        let vere_log = fs::read_to_string(dir.path().join("vere.log")).unwrap();
        assert!(vere_log.contains("hello vere"));

        let launcher_log = fs::read_to_string(dir.path().join("launcher.log")).unwrap();
        assert!(launcher_log.contains("started up"));
    }

    #[test]
    fn truncates_oversized_log_on_init() {
        let dir = tempfile::tempdir().unwrap();
        let vere_path = dir.path().join("vere.log");

        // Write > 10 MB of data.
        {
            let mut f = File::create(&vere_path).unwrap();
            let chunk = vec![b'x'; 1024];
            for _ in 0..(11 * 1024) {
                f.write_all(&chunk).unwrap();
            }
        }
        assert!(fs::metadata(&vere_path).unwrap().len() > MAX_LOG_FILE_BYTES);

        let lm = LogManager::new();
        lm.init_disk_logs(dir.path()).unwrap();

        // File should now be truncated (empty).
        assert_eq!(fs::metadata(&vere_path).unwrap().len(), 0);
    }

    #[test]
    fn keeps_small_log_on_init() {
        let dir = tempfile::tempdir().unwrap();
        let vere_path = dir.path().join("vere.log");
        fs::write(&vere_path, "existing content\n").unwrap();

        let lm = LogManager::new();
        lm.init_disk_logs(dir.path()).unwrap();

        let content = fs::read_to_string(&vere_path).unwrap();
        assert!(content.contains("existing content"));
    }

    #[test]
    fn works_without_disk_init() {
        let lm = LogManager::new();
        lm.add_vere_line("in memory only");
        lm.add_launcher_line("also in memory");
        let lines = lm.recent_lines(10);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn mixed_vere_and_launcher_lines_interleave() {
        let lm = LogManager::new();
        lm.add_vere_line("[stdout] vere output");
        lm.add_launcher_line("launcher event");
        lm.add_vere_line("[stderr] vere error");

        let lines = lm.recent_lines(10);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("vere output"));
        assert!(lines[1].contains("[launcher]"));
        assert!(lines[2].contains("vere error"));
    }
}
