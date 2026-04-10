use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::errors::LauncherError;
use crate::paths::AppPaths;

const TEMP_DIR_PREFIX: &str = ".extract-tmp-";
const INSTALL_MARKER_NAME: &str = ".install-marker.json";

/// Recorded after a successful extraction so subsequent launches can detect
/// the ship name and know that extraction completed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstallMarker {
    pub extracted_at: String,
    /// Ship name detected from the archive's top-level directory name.
    #[serde(default)]
    pub ship_name: Option<String>,
}

impl InstallMarker {
    fn path_in(pier_dir: &Path) -> PathBuf {
        pier_dir.join(INSTALL_MARKER_NAME)
    }

    pub fn write(&self, pier_dir: &Path) -> Result<(), LauncherError> {
        let json = serde_json::to_string_pretty(self).map_err(|e| LauncherError::Extraction {
            reason: format!("failed to serialize install marker: {e}"),
        })?;
        fs::write(Self::path_in(pier_dir), json)?;
        Ok(())
    }

    pub fn read(pier_dir: &Path) -> Result<Self, LauncherError> {
        let contents = fs::read_to_string(Self::path_in(pier_dir))?;
        serde_json::from_str(&contents).map_err(|e| LauncherError::Extraction {
            reason: format!("failed to parse install marker: {e}"),
        })
    }
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

/// Extract a `.tar.zst`, `.tar.gz`, or `.zip` archive into `dest_dir`.
pub fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), LauncherError> {
    let ext = archive_path
        .to_str()
        .unwrap_or_default()
        .to_lowercase();

    if ext.ends_with(".tar.zst") {
        let file = fs::File::open(archive_path)?;
        let decoder = zstd::Decoder::new(file)?;
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dest_dir)?;
    } else if ext.ends_with(".tar.gz") || ext.ends_with(".tgz") {
        let file = fs::File::open(archive_path)?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dest_dir)?;
    } else if ext.ends_with(".zip") {
        let file = fs::File::open(archive_path)?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| LauncherError::Extraction {
            reason: format!("failed to open zip archive: {e}"),
        })?;
        archive.extract(dest_dir).map_err(|e| LauncherError::Extraction {
            reason: format!("failed to extract zip archive: {e}"),
        })?;
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
/// 2. Extract archive to a temp directory
/// 3. Validate the extracted pier structure
/// 4. Atomically move the pier root to the final `pier/` path
/// 5. Write an install marker
pub fn run_extraction(
    paths: &AppPaths,
    archive_path: &Path,
) -> Result<(), LauncherError> {
    // 1. Clean up partial extractions from previous interrupted runs
    cleanup_partial_extractions(&paths.data_dir)?;

    // 2. Extract to a temp directory inside data_dir (same filesystem for atomic rename)
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

    // 3. Validate the extracted pier
    let pier_root = match validate_extracted_pier(&temp_dir) {
        Ok(p) => p,
        Err(e) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(e);
        }
    };

    // 4. Atomic move — remove any existing pier_dir first
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

    // 5. Write install marker (include ship name from original directory name)
    let ship_name = pier_root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());
    let marker = InstallMarker {
        extracted_at: chrono::Utc::now().to_rfc3339(),
        ship_name,
    };
    marker.write(&paths.pier_dir)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Helper: create a minimal .tar.gz archive containing `<ship_name>/.urb/`
    fn create_test_tar_gz(dest: &Path, ship_name: &str) {
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
        fs::write(dest.join("pier.tar.gz"), &compressed).unwrap();
    }

    /// Helper: create a minimal .tar.zst archive containing `<ship_name>/.urb/`
    fn create_test_tar_zst(dest: &Path, ship_name: &str) {
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
        fs::write(dest.join("pier.tar.zst"), &compressed).unwrap();
    }

    fn test_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ship-launcher-test-extract-{name}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
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
    fn extract_tar_gz() {
        let dir = test_dir("extract-gz");
        create_test_tar_gz(&dir, "sampel-palnet");
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
        create_test_tar_zst(&dir, "sampel-palnet");
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

        create_test_tar_gz(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.gz");

        run_extraction(&paths, &archive).unwrap();

        assert!(paths.pier_dir.join(".urb").is_dir());
        assert!(paths.pier_dir.join(INSTALL_MARKER_NAME).is_file());

        // Read back the marker and verify
        let marker = InstallMarker::read(&paths.pier_dir).unwrap();
        assert_eq!(marker.ship_name.as_deref(), Some("sampel-palnet"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_extraction_tar_zst() {
        let dir = test_dir("full-zst");
        let paths = make_paths(&dir);
        paths.ensure_dirs().unwrap();

        create_test_tar_zst(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.zst");

        run_extraction(&paths, &archive).unwrap();

        assert!(paths.pier_dir.join(".urb").is_dir());
        assert!(paths.pier_dir.join(INSTALL_MARKER_NAME).is_file());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_extraction_cleans_up_old_temp_dirs() {
        let dir = test_dir("full-clean");
        let paths = make_paths(&dir);
        paths.ensure_dirs().unwrap();

        // Plant a stale temp dir
        fs::create_dir_all(dir.join(".extract-tmp-stale")).unwrap();

        create_test_tar_gz(&dir, "sampel-palnet");
        let archive = dir.join("pier.tar.gz");

        run_extraction(&paths, &archive).unwrap();

        // Stale temp dir should be gone
        assert!(!dir.join(".extract-tmp-stale").exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
