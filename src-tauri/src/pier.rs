use std::fs;
use std::path::Path;

use crate::errors::LauncherError;
use crate::extract::InstallMarker;

/// Information extracted from inspecting the pier directory.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PierInfo {
    /// Runtime version from `.vere.txt`, if present.
    pub vere_version: Option<String>,
    /// Ship name detected from pier structure, if determinable.
    pub detected_ship_name: Option<String>,
}

/// Result of a successful pier validation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PierValidationResult {
    pub info: PierInfo,
    /// Non-fatal warnings (e.g., structural issues).
    pub warnings: Vec<String>,
}

/// Validate the extracted pier structure before starting vere.
///
/// Checks:
/// 1. Pier directory exists
/// 2. `.urb` subdirectory exists and expected layout is intact
/// 3. Reads `.vere.txt` if present and exposes the runtime version
/// 4. Detects ship name from install marker
pub fn validate_pier(
    pier_dir: &Path,
) -> Result<PierValidationResult, LauncherError> {
    if !pier_dir.is_dir() {
        return Err(LauncherError::PierValidation {
            reason: format!("pier directory does not exist: {}", pier_dir.display()),
        });
    }

    let urb_dir = pier_dir.join(".urb");
    if !urb_dir.is_dir() {
        return Err(LauncherError::PierValidation {
            reason: format!(
                "pier is missing .urb directory: {}",
                urb_dir.display()
            ),
        });
    }

    validate_urb_layout(&urb_dir)?;

    let warnings = Vec::new();
    let vere_version = read_vere_version(pier_dir);
    let detected_ship_name = detect_ship_name(pier_dir);

    Ok(PierValidationResult {
        info: PierInfo {
            vere_version,
            detected_ship_name,
        },
        warnings,
    })
}

/// Verify the `.urb` directory has an expected internal layout.
///
/// A real pier contains subdirectories like `log`, `chk`, `put`, `get`, `dev`.
/// If `.urb` is completely empty, that suggests corruption or incomplete extraction.
/// Unknown subdirectory names are tolerated (vere versions may differ).
fn validate_urb_layout(urb_dir: &Path) -> Result<(), LauncherError> {
    let known_subdirs = ["log", "chk", "put", "get", "dev"];

    let has_any = known_subdirs
        .iter()
        .any(|name| urb_dir.join(name).is_dir());

    if !has_any {
        let entry_count = fs::read_dir(urb_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);

        if entry_count == 0 {
            return Err(LauncherError::PierValidation {
                reason: ".urb directory is empty — pier may be corrupt or incomplete".into(),
            });
        }
    }

    Ok(())
}

/// Read the `.vere.txt` file from the pier root, returning the trimmed
/// version string if the file exists and is non-empty.
fn read_vere_version(pier_dir: &Path) -> Option<String> {
    let vere_txt = pier_dir.join(".vere.txt");
    fs::read_to_string(vere_txt)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Detect the ship name from the pier.
///
/// The archive's top-level directory name is the ship name, but after
/// extraction it's renamed to `pier/`. The install marker records the
/// original directory name, so we read it from there.
fn detect_ship_name(pier_dir: &Path) -> Option<String> {
    if let Ok(marker) = InstallMarker::read(pier_dir) {
        if let Some(name) = marker.ship_name {
            let trimmed = name.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ship-launcher-test-pier-{name}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn create_valid_pier(pier_dir: &Path) {
        fs::create_dir_all(pier_dir.join(".urb/log")).unwrap();
        fs::create_dir_all(pier_dir.join(".urb/chk")).unwrap();
    }

    fn write_install_marker(pier_dir: &Path, ship_name: Option<&str>) {
        let marker = InstallMarker {
            extracted_at: "2026-04-09T00:00:00Z".into(),
            ship_name: ship_name.map(|s| s.into()),
        };
        let json = serde_json::to_string_pretty(&marker).unwrap();
        fs::write(pier_dir.join(".install-marker.json"), json).unwrap();
    }

    #[test]
    fn valid_pier_no_extras() {
        let dir = test_dir("valid-basic");
        let pier = dir.join("pier");
        create_valid_pier(&pier);

        let result = validate_pier(&pier).unwrap();

        assert!(result.info.vere_version.is_none());
        assert!(result.info.detected_ship_name.is_none());
        assert!(result.warnings.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_pier_with_vere_txt() {
        let dir = test_dir("valid-vere-txt");
        let pier = dir.join("pier");
        create_valid_pier(&pier);
        fs::write(pier.join(".vere.txt"), "vere-v4.3\n").unwrap();

        let result = validate_pier(&pier).unwrap();

        assert_eq!(result.info.vere_version.as_deref(), Some("vere-v4.3"));
        assert!(result.warnings.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_pier_vere_txt_empty() {
        let dir = test_dir("valid-vere-empty");
        let pier = dir.join("pier");
        create_valid_pier(&pier);
        fs::write(pier.join(".vere.txt"), "   \n").unwrap();

        let result = validate_pier(&pier).unwrap();

        assert!(result.info.vere_version.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ship_name_from_marker() {
        let dir = test_dir("ship-match");
        let pier = dir.join("pier");
        create_valid_pier(&pier);
        write_install_marker(&pier, Some("soltel-novhex"));

        let result = validate_pier(&pier).unwrap();

        assert_eq!(
            result.info.detected_ship_name.as_deref(),
            Some("soltel-novhex")
        );
        assert!(result.warnings.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pier_dir_does_not_exist() {
        let dir = test_dir("no-pier");
        let pier = dir.join("pier");

        let err = validate_pier(&pier).unwrap_err();
        assert!(matches!(err, LauncherError::PierValidation { .. }));
        assert!(err.to_string().contains("does not exist"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pier_missing_urb_dir() {
        let dir = test_dir("no-urb");
        let pier = dir.join("pier");
        fs::create_dir_all(&pier).unwrap();

        let err = validate_pier(&pier).unwrap_err();
        assert!(matches!(err, LauncherError::PierValidation { .. }));
        assert!(err.to_string().contains(".urb"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pier_urb_dir_empty_fails() {
        let dir = test_dir("empty-urb");
        let pier = dir.join("pier");
        fs::create_dir_all(pier.join(".urb")).unwrap();

        let err = validate_pier(&pier).unwrap_err();
        assert!(matches!(err, LauncherError::PierValidation { .. }));
        assert!(err.to_string().contains("empty"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pier_urb_with_unknown_subdirs_ok() {
        let dir = test_dir("unknown-subdirs");
        let pier = dir.join("pier");
        fs::create_dir_all(pier.join(".urb/something-new")).unwrap();

        let result = validate_pier(&pier).unwrap();

        assert!(result.warnings.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pier_with_real_urb_layout() {
        let dir = test_dir("real-layout");
        let pier = dir.join("pier");
        fs::create_dir_all(pier.join(".urb/put")).unwrap();
        fs::create_dir_all(pier.join(".urb/log")).unwrap();
        fs::create_dir_all(pier.join(".urb/dev")).unwrap();
        fs::create_dir_all(pier.join(".urb/chk")).unwrap();
        fs::create_dir_all(pier.join(".urb/get")).unwrap();
        fs::write(pier.join(".vere.txt"), "vere-v4.3\n").unwrap();
        write_install_marker(&pier, Some("soltel-novhex"));

        let result = validate_pier(&pier).unwrap();

        assert_eq!(result.info.vere_version.as_deref(), Some("vere-v4.3"));
        assert_eq!(
            result.info.detected_ship_name.as_deref(),
            Some("soltel-novhex")
        );
        assert!(result.warnings.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_marker_means_no_ship_detection() {
        let dir = test_dir("no-marker");
        let pier = dir.join("pier");
        create_valid_pier(&pier);

        let result = validate_pier(&pier).unwrap();

        assert!(result.info.detected_ship_name.is_none());
        assert!(result.warnings.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn combined_vere_and_ship_from_marker() {
        let dir = test_dir("combined");
        let pier = dir.join("pier");
        create_valid_pier(&pier);
        fs::write(pier.join(".vere.txt"), "vere-v4.3\n").unwrap();
        write_install_marker(&pier, Some("sampel-palnet"));

        let result = validate_pier(&pier).unwrap();

        assert_eq!(result.info.vere_version.as_deref(), Some("vere-v4.3"));
        assert_eq!(
            result.info.detected_ship_name.as_deref(),
            Some("sampel-palnet")
        );
        assert!(result.warnings.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }
}
