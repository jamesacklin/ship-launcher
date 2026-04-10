use std::fs;
use std::path::{Path, PathBuf};

const LOCK_FILE_NAME: &str = "launcher.lock";

/// A file-based PID lock that prevents multiple launcher instances from
/// running simultaneously. Stale locks from crashed processes are detected
/// and cleaned up automatically.
pub struct InstanceLock {
    lock_path: PathBuf,
}

impl InstanceLock {
    /// Attempt to acquire the instance lock.
    ///
    /// If a lock file exists with a PID that is still alive, returns `Err`
    /// (another instance is running). If the PID is dead, the stale lock is
    /// removed and a new lock is written.
    pub fn acquire(run_dir: &Path) -> Result<Self, LockError> {
        let _ = fs::create_dir_all(run_dir);
        let lock_path = run_dir.join(LOCK_FILE_NAME);

        if lock_path.exists() {
            match fs::read_to_string(&lock_path) {
                Ok(contents) => {
                    if let Ok(pid) = contents.trim().parse::<u32>() {
                        if is_process_alive(pid) {
                            return Err(LockError::AlreadyRunning { pid });
                        }
                        // Stale lock — process is dead, clean up.
                    }
                    // Malformed or stale — remove it.
                    let _ = fs::remove_file(&lock_path);
                }
                Err(_) => {
                    let _ = fs::remove_file(&lock_path);
                }
            }
        }

        let pid = std::process::id();
        fs::write(&lock_path, pid.to_string()).map_err(|e| LockError::Io {
            reason: format!("failed to write lock file: {e}"),
        })?;

        Ok(Self { lock_path })
    }

    /// Release the lock by removing the lock file.
    pub fn release(&self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// Check whether a process with the given PID is still alive.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn is_process_alive(_pid: u32) -> bool {
    // Conservative: assume alive on non-Unix platforms.
    true
}

#[derive(Debug)]
pub enum LockError {
    AlreadyRunning { pid: u32 },
    Io { reason: String },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyRunning { pid } => {
                write!(f, "Another launcher instance is already running (PID {pid})")
            }
            LockError::Io { reason } => write!(f, "Lock file error: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn acquire_creates_lock_file() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-lock-acquire");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let lock = InstanceLock::acquire(&tmp).unwrap();
        let contents = fs::read_to_string(tmp.join(LOCK_FILE_NAME)).unwrap();
        assert_eq!(contents, std::process::id().to_string());

        drop(lock);
        assert!(!tmp.join(LOCK_FILE_NAME).exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn stale_lock_is_cleaned_up() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-lock-stale");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Write a lock file with a PID that (almost certainly) doesn't exist.
        fs::write(tmp.join(LOCK_FILE_NAME), "99999999").unwrap();

        let lock = InstanceLock::acquire(&tmp);
        assert!(lock.is_ok());

        drop(lock);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn malformed_lock_is_cleaned_up() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-lock-malformed");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join(LOCK_FILE_NAME), "not-a-pid").unwrap();

        let lock = InstanceLock::acquire(&tmp);
        assert!(lock.is_ok());

        drop(lock);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn double_acquire_fails_for_live_process() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-lock-double");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let lock1 = InstanceLock::acquire(&tmp).unwrap();
        let lock2 = InstanceLock::acquire(&tmp);
        assert!(lock2.is_err());

        drop(lock1);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn release_removes_lock_file() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-lock-release");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let lock = InstanceLock::acquire(&tmp).unwrap();
        assert!(tmp.join(LOCK_FILE_NAME).exists());

        lock.release();
        assert!(!tmp.join(LOCK_FILE_NAME).exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
