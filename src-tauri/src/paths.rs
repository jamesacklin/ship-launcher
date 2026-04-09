use std::path::PathBuf;

use crate::errors::LauncherError;

const APP_NAME: &str = "ship-launcher";
const ENV_APP_SUPPORT_DIR: &str = "SHIP_LAUNCHER_DATA_DIR";

/// Resolved filesystem paths for the launcher.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppPaths {
    /// Root of app data (e.g. ~/Library/Application Support/ship-launcher)
    pub data_dir: PathBuf,
    /// Extracted pier lives here
    pub pier_dir: PathBuf,
    /// Runtime and launcher logs
    pub logs_dir: PathBuf,
    /// PID files, lock files, runtime state
    pub run_dir: PathBuf,
}

impl AppPaths {
    /// Resolve all app paths.
    ///
    /// Checks `SHIP_LAUNCHER_DATA_DIR` env var first; falls back to the
    /// platform's standard application-support directory.
    pub fn resolve() -> Result<Self, LauncherError> {
        let data_dir = if let Ok(override_dir) = std::env::var(ENV_APP_SUPPORT_DIR) {
            PathBuf::from(override_dir)
        } else {
            dirs::data_dir()
                .ok_or(LauncherError::NoAppSupportDir)?
                .join(APP_NAME)
        };

        Ok(Self {
            pier_dir: data_dir.join("pier"),
            logs_dir: data_dir.join("logs"),
            run_dir: data_dir.join("run"),
            data_dir,
        })
    }

    /// Create all directories if they don't already exist.
    pub fn ensure_dirs(&self) -> Result<(), LauncherError> {
        for dir in [&self.data_dir, &self.pier_dir, &self.logs_dir, &self.run_dir] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_env_override() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-paths");
        unsafe { std::env::set_var(ENV_APP_SUPPORT_DIR, &tmp) };

        let paths = AppPaths::resolve().unwrap();
        assert_eq!(paths.data_dir, tmp);
        assert_eq!(paths.pier_dir, tmp.join("pier"));
        assert_eq!(paths.logs_dir, tmp.join("logs"));
        assert_eq!(paths.run_dir, tmp.join("run"));

        unsafe { std::env::remove_var(ENV_APP_SUPPORT_DIR) };
    }

    #[test]
    fn resolve_falls_back_to_platform_dir() {
        unsafe { std::env::remove_var(ENV_APP_SUPPORT_DIR) };

        let paths = AppPaths::resolve().unwrap();
        let expected_parent = dirs::data_dir().unwrap();
        assert_eq!(paths.data_dir, expected_parent.join(APP_NAME));
    }

    #[test]
    fn ensure_dirs_creates_all() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-ensure");
        unsafe { std::env::set_var(ENV_APP_SUPPORT_DIR, &tmp) };

        let paths = AppPaths::resolve().unwrap();
        paths.ensure_dirs().unwrap();

        assert!(paths.data_dir.is_dir());
        assert!(paths.pier_dir.is_dir());
        assert!(paths.logs_dir.is_dir());
        assert!(paths.run_dir.is_dir());

        // cleanup
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::remove_var(ENV_APP_SUPPORT_DIR) };
    }
}
