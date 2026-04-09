use std::path::Path;
use std::process::Command;

use crate::errors::LauncherError;

/// A parsed vere version with major and minor components.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VereVersion {
    pub major: u32,
    pub minor: u32,
    pub raw: String,
}

/// Result of a version compatibility check.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VersionCheckResult {
    pub bundled_version: VereVersion,
    pub pier_version: Option<VereVersion>,
    pub warnings: Vec<String>,
}

/// Parse a version string into major.minor components.
///
/// Accepts formats like:
/// - `"urbit 4.3"` (from `vere --version` first line)
/// - `"vere-v4.3"` (from `.vere.txt` or manifest)
/// - `"4.3"` (bare version)
pub fn parse_version(raw: &str) -> Option<VereVersion> {
    let trimmed = raw.trim();

    // Strip known prefixes to get to the version number
    let version_part = trimmed
        .strip_prefix("urbit ")
        .or_else(|| trimmed.strip_prefix("vere-v"))
        .or_else(|| trimmed.strip_prefix("vere-"))
        .or_else(|| trimmed.strip_prefix("v"))
        .unwrap_or(trimmed);

    let mut parts = version_part.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;

    Some(VereVersion {
        major,
        minor,
        raw: trimmed.to_string(),
    })
}

/// Run the bundled vere binary with `--version` and parse the version from output.
pub fn get_bundled_version(vere_path: &Path) -> Result<VereVersion, LauncherError> {
    let output = Command::new(vere_path)
        .arg("--version")
        .output()
        .map_err(|e| LauncherError::VersionIncompatible {
            reason: format!("failed to execute vere binary at {}: {}", vere_path.display(), e),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("").trim();

    parse_version(first_line).ok_or_else(|| LauncherError::VersionIncompatible {
        reason: format!(
            "could not parse version from vere output: {:?}",
            first_line
        ),
    })
}

/// Check compatibility between the bundled vere binary and the pier's expected version.
///
/// - Runs the bundled vere with `--version` to get its version
/// - Reads the pier's `.vere.txt` for the pier's expected version
/// - Exact match: proceed silently
/// - Minor mismatch (same major): warn, proceed
/// - Major mismatch: return error
/// - If `.vere.txt` is absent: log info, proceed without check
pub fn check_version_compatibility(
    vere_path: &Path,
    pier_vere_version: Option<&str>,
) -> Result<VersionCheckResult, LauncherError> {
    let bundled = get_bundled_version(vere_path)?;

    let mut warnings = Vec::new();

    let pier_version = match pier_vere_version {
        Some(raw) => match parse_version(raw) {
            Some(v) => Some(v),
            None => {
                warnings.push(format!(
                    "could not parse pier's .vere.txt version string: {:?}; skipping compatibility check",
                    raw
                ));
                None
            }
        },
        None => None,
    };

    if let Some(ref pier_ver) = pier_version {
        if bundled.major != pier_ver.major {
            return Err(LauncherError::VersionIncompatible {
                reason: format!(
                    "major version mismatch: bundled vere is {} but pier expects {}. \
                     These versions are incompatible.",
                    bundled.raw, pier_ver.raw
                ),
            });
        }

        if bundled.minor != pier_ver.minor {
            warnings.push(format!(
                "minor version difference: bundled vere is {} but pier expects {}",
                bundled.raw, pier_ver.raw
            ));
        }
    }

    Ok(VersionCheckResult {
        bundled_version: bundled,
        pier_version,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_version ---

    #[test]
    fn parse_urbit_format() {
        let v = parse_version("urbit 4.3").unwrap();
        assert_eq!(v.major, 4);
        assert_eq!(v.minor, 3);
        assert_eq!(v.raw, "urbit 4.3");
    }

    #[test]
    fn parse_vere_v_format() {
        let v = parse_version("vere-v4.3").unwrap();
        assert_eq!(v.major, 4);
        assert_eq!(v.minor, 3);
    }

    #[test]
    fn parse_vere_no_v_format() {
        let v = parse_version("vere-3.1").unwrap();
        assert_eq!(v.major, 3);
        assert_eq!(v.minor, 1);
    }

    #[test]
    fn parse_bare_version() {
        let v = parse_version("4.3").unwrap();
        assert_eq!(v.major, 4);
        assert_eq!(v.minor, 3);
    }

    #[test]
    fn parse_v_prefix() {
        let v = parse_version("v4.3").unwrap();
        assert_eq!(v.major, 4);
        assert_eq!(v.minor, 3);
    }

    #[test]
    fn parse_with_whitespace() {
        let v = parse_version("  vere-v4.3\n").unwrap();
        assert_eq!(v.major, 4);
        assert_eq!(v.minor, 3);
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert!(parse_version("not a version").is_none());
        assert!(parse_version("").is_none());
        assert!(parse_version("vere-vabc").is_none());
    }

    // --- check_version_compatibility ---

    #[test]
    fn exact_match_no_warnings() {
        // Use the real bundled vere binary for this test
        let vere = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/bin/vere");

        if !vere.exists() {
            eprintln!("skipping test: vere binary not present");
            return;
        }

        let bundled = get_bundled_version(&vere).unwrap();
        // Construct a pier version string matching the bundled version
        let pier_str = format!("vere-v{}.{}", bundled.major, bundled.minor);

        let result = check_version_compatibility(&vere, Some(&pier_str)).unwrap();
        assert!(result.warnings.is_empty());
        assert!(result.pier_version.is_some());
    }

    #[test]
    fn minor_mismatch_warns() {
        let vere = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/bin/vere");

        if !vere.exists() {
            eprintln!("skipping test: vere binary not present");
            return;
        }

        let bundled = get_bundled_version(&vere).unwrap();
        // Use same major but different minor
        let pier_str = format!("vere-v{}.{}", bundled.major, bundled.minor + 1);

        let result = check_version_compatibility(&vere, Some(&pier_str)).unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("minor version difference"));
    }

    #[test]
    fn major_mismatch_errors() {
        let vere = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/bin/vere");

        if !vere.exists() {
            eprintln!("skipping test: vere binary not present");
            return;
        }

        let bundled = get_bundled_version(&vere).unwrap();
        // Use a different major version
        let pier_str = format!("vere-v{}.{}", bundled.major + 100, bundled.minor);

        let err = check_version_compatibility(&vere, Some(&pier_str)).unwrap_err();
        assert!(matches!(err, LauncherError::VersionIncompatible { .. }));
        assert!(err.to_string().contains("major version mismatch"));
    }

    #[test]
    fn missing_pier_version_proceeds() {
        let vere = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/bin/vere");

        if !vere.exists() {
            eprintln!("skipping test: vere binary not present");
            return;
        }

        let result = check_version_compatibility(&vere, None).unwrap();
        assert!(result.pier_version.is_none());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn unparseable_pier_version_warns() {
        let vere = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/bin/vere");

        if !vere.exists() {
            eprintln!("skipping test: vere binary not present");
            return;
        }

        let result = check_version_compatibility(&vere, Some("garbage")).unwrap();
        assert!(result.pier_version.is_none());
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("could not parse"));
    }

    #[test]
    fn nonexistent_binary_errors() {
        let err = get_bundled_version(Path::new("/nonexistent/vere")).unwrap_err();
        assert!(matches!(err, LauncherError::VersionIncompatible { .. }));
        assert!(err.to_string().contains("failed to execute"));
    }

    #[test]
    fn get_bundled_version_works() {
        let vere = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/bin/vere");

        if !vere.exists() {
            eprintln!("skipping test: vere binary not present");
            return;
        }

        let v = get_bundled_version(&vere).unwrap();
        assert!(v.major > 0, "expected major version > 0, got {}", v.major);
        assert!(v.raw.contains("urbit"), "expected raw to contain 'urbit', got {:?}", v.raw);
    }
}
