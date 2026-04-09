use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::errors::LauncherError;
use crate::manifest::PierManifest;
use crate::paths::AppPaths;

const TEMP_DIR_PREFIX: &str = ".extract-tmp-";
const INSTALL_MARKER_NAME: &str = ".install-marker.json";

/// Recorded after a successful extraction so subsequent launches can skip it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstallMarker {
    pub extracted_at: String,
    pub archive_sha256: String,
    pub format_version: u32,
}

impl InstallMarker {
    fn path_in(pier_dir: &Path) -> PathBuf {
        pier_dir.join(INSTALL_MARKER_NAME)
    }

    fn write(&self, pier_dir: &Path) -> Result<(), LauncherError> {
        let json = serde_json::to_string_pretty(self).map_err(|e| LauncherError::Extraction {
            reason: format!("failed to serialize install marker: {e}"),
        })?;
        fs::write(Self::path_in(pier_dir), json)?;
        Ok(())
    }

    fn read(pier_dir: &Path) -> Result<Self, LauncherError> {
        let contents = fs::read_to_string(Self::path_in(pier_dir))?;
        serde_json::from_str(&contents).map_err(|e| LauncherError::Extraction {
            reason: format!("failed to parse install marker: {e}"),
        })
    }
}

/// Returns true if the pier already exists with a valid install marker matching
/// the manifest, so extraction can be skipped.
pub fn should_skip_extraction(pier_dir: &Path, manifest: &PierManifest) -> bool {
    if !pier_dir.is_dir() {
        return false;
    }
    let marker = match InstallMarker::read(pier_dir) {
        Ok(m) => m,
        Err(_) => return false,
    };
    marker.archive_sha256 == manifest.archive_sha256
        && marker.format_version == manifest.format_version
}

/// Remove any leftover `.extract-tmp-*` directories inside `data_dir`.
pub fn cleanup_partial_extractions(data_dir: &Path) -> Result<(), LauncherError> {
    let entries = match fs::read_dir(data_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(name_str) = name.to_str() {
            if name_str.starts_with(TEMP_DIR_PREFIX) && entry.path().is_dir() {
                fs::remove_dir_all(entry.path())?;
            }
        }
    }
    Ok(())
}

/// Compute the SHA-256 hex digest of a file and compare against `expected`.
pub fn verify_checksum(archive_path: &Path, expected: &str) -> Result<(), LauncherError> {
    let mut file = fs::File::open(archive_path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        return Err(LauncherError::ChecksumMismatch {
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Extract a `.tar.zst` or `.tar.gz` archive into `dest_dir`.
pub fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), LauncherError> {
    let ext = archive_path
        .to_str()
        .unwrap_or_default()
        .to_lowercase();

    let file = fs::File::open(archive_path)?;

    if ext.ends_with(".tar.zst") {
        let decoder = zstd::Decoder::new(file)?;
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dest_dir)?;
    } else if ext.ends_with(".tar.gz") || ext.ends_with(".tgz") {
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dest_dir)?;
    } else {
        return Err(LauncherError::Extraction {
            reason: format!(
                "unsupported archive format: {}",
                archive_path.display()
            ),
        });
    }

    Ok(())
}

/// After extraction, find the pier root directory and verify its structure.
///
/// Expects the archive to contain a single top-level directory (the ship name)
/// with a `.urb` subdirectory inside.
///
/// Returns the path to the pier root (e.g. `dest_dir/<ship-name>`).
pub fn validate_extracted_pier(dest_dir: &Path) -> Result<PathBuf, LauncherError> {
    let entries: Vec<_> = fs::read_dir(dest_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    if entries.is_empty() {
        return Err(LauncherError::PierValidation {
            reason: "extraction produced no directories".into(),
        });
    }

    // Look for the directory that contains .urb
    let pier_root = if entries.len() == 1 {
        entries[0].path()
    } else {
        // Multiple top-level dirs — find the one with .urb
        entries
            .iter()
            .find(|e| e.path().join(".urb").is_dir())
            .map(|e| e.path())
            .ok_or_else(|| LauncherError::PierValidation {
                reason: "no directory containing .urb found after extraction".into(),
            })?
    };

    if !pier_root.join(".urb").is_dir() {
        return Err(LauncherError::PierValidation {
            reason: format!(
                "extracted directory {} does not contain .urb",
                pier_root.display()
            ),
        });
    }

    Ok(pier_root)
}

/// Run the full first-launch extraction pipeline.
///
/// 1. Clean up any partial temp dirs from interrupted extractions
/// 2. Check whether extraction can be skipped
/// 3. Verify archive checksum against manifest
/// 4. Extract archive to a temp directory
/// 5. Validate the extracted pier structure
/// 6. Atomically move the pier root to the final `pier/` path
/// 7. Write an install marker
pub fn run_extraction(
    paths: &AppPaths,
    manifest: &PierManifest,
    archive_path: &Path,
) -> Result<(), LauncherError> {
    // 1. Clean up partial extractions from previous interrupted runs
    cleanup_partial_extractions(&paths.data_dir)?;

    // 2. Skip if already extracted with matching marker
    if should_skip_extraction(&paths.pier_dir, manifest) {
        return Ok(());
    }

    // 3. Verify archive checksum
    verify_checksum(archive_path, &manifest.archive_sha256)?;

    // 4. Extract to a temp directory inside data_dir (same filesystem for atomic rename)
    let temp_dir = paths.data_dir.join(format!(
        "{}{}",
        TEMP_DIR_PREFIX,
        std::process::id()
    ));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }
    fs::create_dir_all(&temp_dir)?;

    let extraction_result = extract_archive(archive_path, &temp_dir);
    if let Err(e) = extraction_result {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(e);
    }

    // 5. Validate the extracted pier
    let pier_root = match validate_extracted_pier(&temp_dir) {
        Ok(p) => p,
        Err(e) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(e);
        }
    };

    // 6. Atomic move — remove any existing pier_dir first
    if paths.pier_dir.exists() {
        fs::remove_dir_all(&paths.pier_dir)?;
    }
    fs::rename(&pier_root, &paths.pier_dir).map_err(|e| {
        let _ = fs::remove_dir_all(&temp_dir);
        LauncherError::Extraction {
            reason: format!("failed to move pier to final location: {e}"),
        }
    })?;

    // Clean up the now-empty temp dir
    let _ = fs::remove_dir_all(&temp_dir);

    // 7. Write install marker
    let marker = InstallMarker {
        extracted_at: chrono::Utc::now().to_rfc3339(),
        archive_sha256: manifest.archive_sha256.clone(),
        format_version: manifest.format_version,
    };
    marker.write(&paths.pier_dir)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Helper: create a minimal .tar.gz archive containing `<ship_name>/.urb/`
    fn create_test_tar_gz(dest: &Path, ship_name: &str) -> String {
        let buf = Vec::new();
        let encoder = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
        let mut ar = tar::Builder::new(encoder);

        // Add <ship_name>/.urb/ as an empty directory
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        let dir_path = format!("{ship_name}/");
        ar.append_data(&mut header.clone(), &dir_path, io::empty())
            .unwrap();

        let urb_path = format!("{ship_name}/.urb/");
        ar.append_data(&mut header, &urb_path, io::empty())
            .unwrap();

        let compressed = ar.into_inner().unwrap().finish().unwrap();

        let archive_path = dest.join("pier.tar.gz");
        fs::write(&archive_path, &compressed).unwrap();

        // Compute SHA-256
        let mut hasher = Sha256::new();
        hasher.update(&compressed);
        format!("{:x}", hasher.finalize())
    }

    /// Helper: create a minimal .tar.zst archive containing `<ship_name>/.urb/`
    fn create_test_tar_zst(dest: &Path, ship_name: &str) -> String {
        let buf = Vec::new();
        let mut ar = tar::Builder::new(buf);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        let dir_path = format!("{ship_name}/");
        ar.append_data(&mut header.clone(), &dir_path, io::empty())
            .unwrap();

        let urb_path = format!("{ship_name}/.urb/");
        ar.append_data(&mut header, &urb_path, io::empty())
            .unwrap();

        let tar_data = ar.into_inner().unwrap();
        let compressed = zstd::encode_all(tar_data.as_slice(), 3).unwrap();

        let archive_path = dest.join("pier.tar.zst");
        fs::write(&archive_path, &compressed).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(&compressed);
        format!("{:x}", hasher.finalize())
    }

    fn test_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ship-launcher-test-extract-{name}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_manifest(sha: &str) -> PierManifest {
        PierManifest {
            format_version: 1,
            ship: "~sampel-palnet".into(),
            exported_at: "2026-04-09T15:30:00Z".into(),
            archive_name: "pier.tar.zst".into(),
            archive_sha256: sha.into(),
            vere_version: "vere-v3.1".into(),
            launcher_min_version: None,
            notes: None,
        }
    }

    fn make_paths(data_dir: &Path) -> AppPaths {
        AppPaths {
            pier_dir: data_dir.join("pier"),
            logs_dir: data_dir.join("logs"),
            run_dir: data_dir.join("run"),
            data_dir: data_dir.to_path_buf(),
        }
    }

    #[test]
    fn verify_checksum_matches() {
        let dir = test_dir("cksum-ok");
        let content = b"hello world";
        let path = dir.join("test.bin");
        fs::write(&path, content).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(content);
        let expected = format!("{:x}", hasher.finalize());

        verify_checksum(&path, &expected).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_checksum_mismatch() {
        let dir = test_dir("cksum-bad");
        let path = dir.join("test.bin");
        fs::write(&path, b"hello").unwrap();

        let err = verify_checksum(&path, "0000000000000000").unwrap_err();
        assert!(matches!(err, LauncherError::ChecksumMismatch { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_tar_gz() {
        let dir = test_dir("extract-gz");
        let _sha = create_test_tar_gz(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.gz");
        let dest = dir.join("out");
        fs::create_dir_all(&dest).unwrap();

        extract_archive(&archive, &dest).unwrap();
        assert!(dest.join("sampel-palnet").join(".urb").is_dir());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_tar_zst() {
        let dir = test_dir("extract-zst");
        let _sha = create_test_tar_zst(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.zst");
        let dest = dir.join("out");
        fs::create_dir_all(&dest).unwrap();

        extract_archive(&archive, &dest).unwrap();
        assert!(dest.join("sampel-palnet").join(".urb").is_dir());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_unsupported_format() {
        let dir = test_dir("extract-unsup");
        let path = dir.join("pier.tar.bz2");
        fs::write(&path, b"nope").unwrap();

        let err = extract_archive(&path, &dir).unwrap_err();
        assert!(matches!(err, LauncherError::Extraction { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_pier_ok() {
        let dir = test_dir("validate-ok");
        fs::create_dir_all(dir.join("sampel-palnet/.urb")).unwrap();

        let root = validate_extracted_pier(&dir).unwrap();
        assert_eq!(root, dir.join("sampel-palnet"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_pier_no_urb() {
        let dir = test_dir("validate-nourb");
        fs::create_dir_all(dir.join("sampel-palnet")).unwrap();

        let err = validate_extracted_pier(&dir).unwrap_err();
        assert!(matches!(err, LauncherError::PierValidation { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_pier_empty() {
        let dir = test_dir("validate-empty");
        fs::create_dir_all(&dir).unwrap();

        let err = validate_extracted_pier(&dir).unwrap_err();
        assert!(matches!(err, LauncherError::PierValidation { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_extraction_when_marker_matches() {
        let dir = test_dir("skip-ok");
        let pier_dir = dir.join("pier");
        fs::create_dir_all(&pier_dir).unwrap();

        let marker = InstallMarker {
            extracted_at: "2026-04-09T00:00:00Z".into(),
            archive_sha256: "abc123".into(),
            format_version: 1,
        };
        marker.write(&pier_dir).unwrap();

        let manifest = make_manifest("abc123");
        assert!(should_skip_extraction(&pier_dir, &manifest));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_skip_when_checksum_differs() {
        let dir = test_dir("skip-diff");
        let pier_dir = dir.join("pier");
        fs::create_dir_all(&pier_dir).unwrap();

        let marker = InstallMarker {
            extracted_at: "2026-04-09T00:00:00Z".into(),
            archive_sha256: "old_hash".into(),
            format_version: 1,
        };
        marker.write(&pier_dir).unwrap();

        let manifest = make_manifest("new_hash");
        assert!(!should_skip_extraction(&pier_dir, &manifest));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_skip_when_no_pier() {
        let dir = test_dir("skip-nopier");
        let manifest = make_manifest("abc123");
        assert!(!should_skip_extraction(&dir.join("pier"), &manifest));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_temp_dirs() {
        let dir = test_dir("cleanup");
        fs::create_dir_all(dir.join(".extract-tmp-12345")).unwrap();
        fs::create_dir_all(dir.join(".extract-tmp-99999")).unwrap();
        fs::create_dir_all(dir.join("keep-this")).unwrap();

        cleanup_partial_extractions(&dir).unwrap();

        assert!(!dir.join(".extract-tmp-12345").exists());
        assert!(!dir.join(".extract-tmp-99999").exists());
        assert!(dir.join("keep-this").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_extraction_tar_gz() {
        let dir = test_dir("full-gz");
        let paths = make_paths(&dir);
        paths.ensure_dirs().unwrap();

        let sha = create_test_tar_gz(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.gz");
        let manifest = make_manifest(&sha);

        run_extraction(&paths, &manifest, &archive).unwrap();

        assert!(paths.pier_dir.join(".urb").is_dir());
        assert!(paths.pier_dir.join(INSTALL_MARKER_NAME).is_file());

        // Read back the marker and verify
        let marker = InstallMarker::read(&paths.pier_dir).unwrap();
        assert_eq!(marker.archive_sha256, sha);
        assert_eq!(marker.format_version, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_extraction_tar_zst() {
        let dir = test_dir("full-zst");
        let paths = make_paths(&dir);
        paths.ensure_dirs().unwrap();

        let sha = create_test_tar_zst(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.zst");
        let manifest = make_manifest(&sha);

        run_extraction(&paths, &manifest, &archive).unwrap();

        assert!(paths.pier_dir.join(".urb").is_dir());
        assert!(paths.pier_dir.join(INSTALL_MARKER_NAME).is_file());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_extraction_skips_when_marker_valid() {
        let dir = test_dir("full-skip");
        let paths = make_paths(&dir);
        paths.ensure_dirs().unwrap();

        let sha = create_test_tar_zst(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.zst");
        let manifest = make_manifest(&sha);

        // First extraction
        run_extraction(&paths, &manifest, &archive).unwrap();

        // Remove the archive to prove second call doesn't try to read it
        fs::remove_file(&archive).unwrap();

        // Second call should skip without error
        run_extraction(&paths, &manifest, &archive).unwrap();

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_extraction_bad_checksum() {
        let dir = test_dir("full-badsum");
        let paths = make_paths(&dir);
        paths.ensure_dirs().unwrap();

        let _sha = create_test_tar_gz(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.gz");
        let manifest = make_manifest("wrong_hash");

        let err = run_extraction(&paths, &manifest, &archive).unwrap_err();
        assert!(matches!(err, LauncherError::ChecksumMismatch { .. }));

        // pier_dir should not exist
        assert!(!paths.pier_dir.join(".urb").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_extraction_cleans_up_old_temp_dirs() {
        let dir = test_dir("full-clean");
        let paths = make_paths(&dir);
        paths.ensure_dirs().unwrap();

        // Plant a stale temp dir
        fs::create_dir_all(dir.join(".extract-tmp-stale")).unwrap();

        let sha = create_test_tar_gz(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.gz");
        let manifest = make_manifest(&sha);

        run_extraction(&paths, &manifest, &archive).unwrap();

        // Stale temp dir should be gone
        assert!(!dir.join(".extract-tmp-stale").exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
