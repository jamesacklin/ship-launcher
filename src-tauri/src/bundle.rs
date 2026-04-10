use std::path::{Path, PathBuf};

use crate::errors::LauncherError;

const VERE_BIN: &str = "bin/vere";
const PIER_ARCHIVE: &str = "assets/pier.tar.zst";

/// Locations of bundled assets inside the app's resource directory.
#[derive(Debug, Clone)]
pub struct BundledAssets {
    pub vere: PathBuf,
    pub pier_archive: PathBuf,
}

impl BundledAssets {
    /// Locate all expected bundled assets under `resource_dir`.
    ///
    /// `resource_dir` is the Tauri resource path — on macOS this is
    /// `<App>.app/Contents/Resources/`.
    pub fn locate(resource_dir: &Path) -> Result<Self, LauncherError> {
        let vere = resource_dir.join(VERE_BIN);
        let pier_archive = resource_dir.join(PIER_ARCHIVE);

        if !vere.exists() {
            return Err(LauncherError::BundledAssetNotFound { path: vere });
        }
        if !pier_archive.exists() {
            return Err(LauncherError::BundledAssetNotFound {
                path: pier_archive,
            });
        }

        Ok(Self {
            vere,
            pier_archive,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_resource_dir(suffix: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("ship-launcher-test-bundle-{suffix}"));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("bin")).unwrap();
        fs::create_dir_all(tmp.join("assets")).unwrap();
        fs::write(tmp.join(VERE_BIN), b"fake-vere").unwrap();
        fs::write(tmp.join(PIER_ARCHIVE), b"fake-archive").unwrap();
        tmp
    }

    #[test]
    fn locate_succeeds_with_required_assets() {
        let dir = make_resource_dir("required");
        let assets = BundledAssets::locate(&dir).unwrap();
        assert!(assets.vere.ends_with("bin/vere"));
        assert!(assets.pier_archive.ends_with("assets/pier.tar.zst"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locate_fails_missing_vere() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-bundle-no-vere");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("assets")).unwrap();
        std::fs::write(tmp.join(PIER_ARCHIVE), b"archive").unwrap();

        let err = BundledAssets::locate(&tmp).unwrap_err();
        assert!(err.to_string().contains("bin/vere"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn locate_fails_missing_archive() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-bundle-no-archive");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("bin")).unwrap();
        std::fs::write(tmp.join(VERE_BIN), b"vere").unwrap();

        let err = BundledAssets::locate(&tmp).unwrap_err();
        assert!(err.to_string().contains("pier.tar.zst"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
