use std::path::Path;

use crate::errors::LauncherError;
use crate::logs::LogManager;
use crate::state::{LauncherState, StateMachine};
use crate::version;

fn os_name() -> &'static str {
    #[cfg(target_os = "macos")]
    { "macos" }
    #[cfg(target_os = "linux")]
    { "linux" }
    #[cfg(target_os = "windows")]
    { "windows" }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    compile_error!("unsupported target OS")
}

fn arch_name() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    { "aarch64" }
    #[cfg(target_arch = "x86_64")]
    { "x86_64" }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    compile_error!("unsupported target architecture")
}

fn build_download_url(ver: &str) -> String {
    format!(
        "https://github.com/urbit/vere/releases/download/vere-v{ver}/{os}-{arch}.tgz",
        os = os_name(),
        arch = arch_name(),
    )
}

/// Fetch the latest vere release version from the GitHub API.
pub async fn fetch_latest_version(client: &reqwest::Client) -> Result<String, LauncherError> {
    let resp = client
        .get("https://api.github.com/repos/urbit/vere/releases/latest")
        .header("User-Agent", "ship-launcher")
        .send()
        .await
        .map_err(|e| LauncherError::Download {
            reason: format!("failed to query GitHub releases: {e}"),
        })?;

    if !resp.status().is_success() {
        return Err(LauncherError::Download {
            reason: format!("GitHub API returned {}", resp.status()),
        });
    }

    let body: serde_json::Value =
        resp.json().await.map_err(|e| LauncherError::Download {
            reason: format!("failed to parse GitHub response: {e}"),
        })?;

    let tag = body["tag_name"]
        .as_str()
        .ok_or_else(|| LauncherError::Download {
            reason: "no tag_name in GitHub release response".into(),
        })?;

    let parsed = version::parse_version(tag).ok_or_else(|| LauncherError::Download {
        reason: format!("could not parse version from tag: {tag}"),
    })?;

    Ok(format!("{}.{}", parsed.major, parsed.minor))
}

/// Read the pier's `.vere.txt` to determine the expected vere version.
pub fn version_from_pier(pier_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(pier_dir.join(".vere.txt")).ok()?;
    let parsed = version::parse_version(raw.trim())?;
    Some(format!("{}.{}", parsed.major, parsed.minor))
}

/// Ensure a vere binary exists at `vere_path`. If missing, download the
/// appropriate version from GitHub releases.
pub async fn ensure_vere(
    vere_path: &Path,
    pier_dir: &Path,
    state_machine: &StateMachine,
    log_manager: &LogManager,
) -> Result<(), LauncherError> {
    if vere_path.is_file() {
        log_manager.add_launcher_line("Vere binary found, skipping download");
        return Ok(());
    }

    // Determine which version to download.
    let ver = match version_from_pier(pier_dir) {
        Some(v) => {
            log_manager.add_launcher_line(&format!(
                "Pier expects vere v{v}, downloading that version"
            ));
            v
        }
        None => {
            log_manager.add_launcher_line(
                "No .vere.txt found, fetching latest release version from GitHub",
            );
            let client = reqwest::Client::new();
            let v = fetch_latest_version(&client).await?;
            log_manager.add_launcher_line(&format!("Latest vere release: v{v}"));
            v
        }
    };

    state_machine
        .transition(LauncherState::Extracting {
            message: format!("Downloading vere v{ver}..."),
        })
        .ok();

    let url = build_download_url(&ver);
    log_manager.add_launcher_line(&format!("Downloading {url}"));

    // Create parent directory.
    if let Some(parent) = vere_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LauncherError::Download {
            reason: format!("download request failed: {e}"),
        })?;

    if !resp.status().is_success() {
        return Err(LauncherError::Download {
            reason: format!("download failed with HTTP {}", resp.status()),
        });
    }

    let tgz_tmp = vere_path.with_extension("tgz.tmp");
    let bytes = resp.bytes().await.map_err(|e| LauncherError::Download {
        reason: format!("failed to read download body: {e}"),
    })?;
    std::fs::write(&tgz_tmp, &bytes)?;

    // Extract the binary from the tarball.
    state_machine
        .transition(LauncherState::Extracting {
            message: "Extracting vere binary...".into(),
        })
        .ok();

    let tgz_file = std::fs::File::open(&tgz_tmp)?;
    let gz = flate2::read::GzDecoder::new(tgz_file);
    let mut archive = tar::Archive::new(gz);

    let vere_tmp = vere_path.with_extension("tmp");
    let mut found = false;

    for entry in archive.entries().map_err(|e| LauncherError::Download {
        reason: format!("failed to read tarball entries: {e}"),
    })? {
        let mut entry = entry.map_err(|e| LauncherError::Download {
            reason: format!("failed to read tarball entry: {e}"),
        })?;

        let path = entry
            .path()
            .map_err(|e| LauncherError::Download {
                reason: format!("failed to read entry path: {e}"),
            })?
            .into_owned();

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        // The binary is named like `vere-v4.3-macos-aarch64`.
        if name.starts_with("vere-") || name == "vere" {
            entry.unpack(&vere_tmp).map_err(|e| LauncherError::Download {
                reason: format!("failed to extract vere binary: {e}"),
            })?;
            found = true;
            break;
        }
    }

    if !found {
        let _ = std::fs::remove_file(&tgz_tmp);
        return Err(LauncherError::Download {
            reason: "tarball did not contain a vere binary".into(),
        });
    }

    // Set executable permission on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&vere_tmp, std::fs::Permissions::from_mode(0o755))?;
    }

    // Atomic rename to final path.
    std::fs::rename(&vere_tmp, vere_path)?;
    let _ = std::fs::remove_file(&tgz_tmp);

    log_manager.add_launcher_line(&format!(
        "Vere v{ver} installed to {}",
        vere_path.display()
    ));
    Ok(())
}

/// Dock the vere binary into the pier so the pier becomes self-contained.
///
/// `vere dock <pier>` copies the binary into `<pier>/.bin/live/` and creates
/// a `<pier>/.run` hard link to it. If `.bin/` exists (already docked) but
/// `.run` is missing, we restore the hard link. Only runs `vere dock` when
/// `.bin/` is absent entirely.
/// After docking, the standalone binary at `vere_path` is deleted.
pub async fn dock_vere(
    vere_path: &Path,
    pier_path: &Path,
    log_manager: &LogManager,
) -> Result<(), LauncherError> {
    let run_path = pier_path.join(".run");
    if run_path.exists() {
        log_manager.add_launcher_line("Pier already has .run binary");
        return Ok(());
    }

    let live_dir = pier_path.join(".bin").join("live");

    // Already docked (.bin/ exists) but .run is missing — restore the hard link.
    if live_dir.is_dir() {
        if let Some(docked_bin) = find_vere_in_dir(&live_dir) {
            log_manager.add_launcher_line(&format!(
                "Pier already docked, restoring .run hard link from {}",
                docked_bin.display()
            ));
            std::fs::hard_link(&docked_bin, &run_path).map_err(|e| {
                LauncherError::Download {
                    reason: format!("failed to restore .run hard link: {e}"),
                }
            })?;
            return Ok(());
        }
    }

    // Not docked at all — run `vere dock`.
    if !vere_path.is_file() {
        return Err(LauncherError::Download {
            reason: format!(
                "cannot dock: vere binary not found at {}",
                vere_path.display()
            ),
        });
    }

    log_manager.add_launcher_line(&format!(
        "Docking vere into pier at {}",
        pier_path.display()
    ));

    let output = tokio::process::Command::new(vere_path)
        .arg("dock")
        .arg(pier_path)
        .output()
        .await
        .map_err(|e| LauncherError::Download {
            reason: format!("failed to run vere dock: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LauncherError::Download {
            reason: format!("vere dock failed (exit {}): {}", output.status, stderr.trim()),
        });
    }

    if !run_path.exists() {
        return Err(LauncherError::Download {
            reason: "vere dock succeeded but .run file not found in pier".into(),
        });
    }

    log_manager.add_launcher_line("Dock complete, removing standalone vere binary");
    let _ = std::fs::remove_file(vere_path);

    Ok(())
}

/// Find a vere binary in a directory (looks for files starting with "vere-").
fn find_vere_in_dir(dir: &Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("vere-") || name_str == "vere" {
            return Some(entry.path());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_name_is_known() {
        let name = os_name();
        assert!(
            ["macos", "linux", "windows"].contains(&name),
            "unexpected os: {name}"
        );
    }

    #[test]
    fn arch_name_is_known() {
        let name = arch_name();
        assert!(
            ["aarch64", "x86_64"].contains(&name),
            "unexpected arch: {name}"
        );
    }

    #[test]
    fn build_url_format() {
        let url = build_download_url("4.3");
        assert!(url.starts_with("https://github.com/urbit/vere/releases/download/vere-v4.3/"));
        assert!(url.ends_with(".tgz"));
        assert!(url.contains(os_name()));
        assert!(url.contains(arch_name()));
    }

    #[test]
    fn version_from_pier_reads_vere_txt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".vere.txt"), "vere-v4.3\n").unwrap();
        assert_eq!(version_from_pier(dir.path()), Some("4.3".into()));
    }

    #[test]
    fn version_from_pier_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(version_from_pier(dir.path()), None);
    }
}
