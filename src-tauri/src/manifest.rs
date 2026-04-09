use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::LauncherError;

/// Parsed contents of `pier-manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PierManifest {
    pub format_version: u32,
    /// Urbit ship name, e.g. "~sampel-palnet"
    pub ship: String,
    /// ISO-8601 timestamp of the export
    pub exported_at: String,
    /// Filename of the pier archive inside the bundle
    pub archive_name: String,
    /// Hex-encoded SHA-256 of the archive file
    pub archive_sha256: String,
    /// Expected vere version string (e.g. "vere-v3.1")
    pub vere_version: String,
    /// Minimum launcher version that can open this bundle
    #[serde(default)]
    pub launcher_min_version: Option<String>,
    /// Free-form notes
    #[serde(default)]
    pub notes: Option<String>,
}

impl PierManifest {
    /// Read and parse a manifest from a file path.
    pub fn from_file(path: &Path) -> Result<Self, LauncherError> {
        if !path.exists() {
            return Err(LauncherError::ManifestNotFound {
                path: path.to_path_buf(),
            });
        }

        let contents = std::fs::read_to_string(path)?;
        Self::from_str(&contents)
    }

    /// Parse a manifest from a JSON string.
    pub fn from_str(json: &str) -> Result<Self, LauncherError> {
        let manifest: Self = serde_json::from_str(json).map_err(|e| {
            LauncherError::ManifestParseError {
                reason: e.to_string(),
            }
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate required fields have sensible values.
    fn validate(&self) -> Result<(), LauncherError> {
        if self.format_version == 0 {
            return Err(LauncherError::ManifestValidation {
                reason: "format_version must be >= 1".into(),
            });
        }
        if self.ship.is_empty() {
            return Err(LauncherError::ManifestValidation {
                reason: "ship name is empty".into(),
            });
        }
        if self.archive_name.is_empty() {
            return Err(LauncherError::ManifestValidation {
                reason: "archive_name is empty".into(),
            });
        }
        if self.archive_sha256.is_empty() {
            return Err(LauncherError::ManifestValidation {
                reason: "archive_sha256 is empty".into(),
            });
        }
        if self.vere_version.is_empty() {
            return Err(LauncherError::ManifestValidation {
                reason: "vere_version is empty".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_json() -> &'static str {
        r#"{
            "format_version": 1,
            "ship": "~sampel-palnet",
            "exported_at": "2026-04-09T15:30:00Z",
            "archive_name": "pier.tar.zst",
            "archive_sha256": "abc123def456",
            "vere_version": "vere-v3.1"
        }"#
    }

    #[test]
    fn parse_valid_manifest() {
        let m = PierManifest::from_str(valid_json()).unwrap();
        assert_eq!(m.format_version, 1);
        assert_eq!(m.ship, "~sampel-palnet");
        assert_eq!(m.archive_name, "pier.tar.zst");
        assert_eq!(m.archive_sha256, "abc123def456");
        assert_eq!(m.vere_version, "vere-v3.1");
        assert!(m.launcher_min_version.is_none());
        assert!(m.notes.is_none());
    }

    #[test]
    fn parse_with_optional_fields() {
        let json = r#"{
            "format_version": 1,
            "ship": "~zod",
            "exported_at": "2026-04-09T15:30:00Z",
            "archive_name": "pier.tar.zst",
            "archive_sha256": "deadbeef",
            "vere_version": "vere-v3.1",
            "launcher_min_version": "0.1.0",
            "notes": "test export"
        }"#;
        let m = PierManifest::from_str(json).unwrap();
        assert_eq!(m.launcher_min_version.as_deref(), Some("0.1.0"));
        assert_eq!(m.notes.as_deref(), Some("test export"));
    }

    #[test]
    fn reject_missing_required_field() {
        let json = r#"{
            "format_version": 1,
            "ship": "~zod",
            "exported_at": "2026-04-09T15:30:00Z"
        }"#;
        let err = PierManifest::from_str(json).unwrap_err();
        assert!(matches!(err, LauncherError::ManifestParseError { .. }));
    }

    #[test]
    fn reject_corrupt_json() {
        let err = PierManifest::from_str("not json at all").unwrap_err();
        assert!(matches!(err, LauncherError::ManifestParseError { .. }));
    }

    #[test]
    fn reject_empty_ship() {
        let json = r#"{
            "format_version": 1,
            "ship": "",
            "exported_at": "2026-04-09T15:30:00Z",
            "archive_name": "pier.tar.zst",
            "archive_sha256": "abc",
            "vere_version": "vere-v3.1"
        }"#;
        let err = PierManifest::from_str(json).unwrap_err();
        match err {
            LauncherError::ManifestValidation { reason } => {
                assert!(reason.contains("ship"));
            }
            _ => panic!("expected ManifestValidation"),
        }
    }

    #[test]
    fn reject_zero_format_version() {
        let json = r#"{
            "format_version": 0,
            "ship": "~zod",
            "exported_at": "2026-04-09T15:30:00Z",
            "archive_name": "pier.tar.zst",
            "archive_sha256": "abc",
            "vere_version": "vere-v3.1"
        }"#;
        let err = PierManifest::from_str(json).unwrap_err();
        match err {
            LauncherError::ManifestValidation { reason } => {
                assert!(reason.contains("format_version"));
            }
            _ => panic!("expected ManifestValidation"),
        }
    }

    #[test]
    fn reject_empty_archive_sha256() {
        let json = r#"{
            "format_version": 1,
            "ship": "~zod",
            "exported_at": "2026-04-09T15:30:00Z",
            "archive_name": "pier.tar.zst",
            "archive_sha256": "",
            "vere_version": "vere-v3.1"
        }"#;
        let err = PierManifest::from_str(json).unwrap_err();
        match err {
            LauncherError::ManifestValidation { reason } => {
                assert!(reason.contains("archive_sha256"));
            }
            _ => panic!("expected ManifestValidation"),
        }
    }

    #[test]
    fn from_file_not_found() {
        let err = PierManifest::from_file(Path::new("/nonexistent/manifest.json")).unwrap_err();
        assert!(matches!(err, LauncherError::ManifestNotFound { .. }));
    }

    #[test]
    fn from_file_valid() {
        let tmp = std::env::temp_dir().join("ship-launcher-test-manifest.json");
        std::fs::write(&tmp, valid_json()).unwrap();
        let m = PierManifest::from_file(&tmp).unwrap();
        assert_eq!(m.ship, "~sampel-palnet");
        let _ = std::fs::remove_file(&tmp);
    }
}
