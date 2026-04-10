pub mod bundle;
pub mod click;
pub mod download;
pub mod errors;
pub mod extract;
pub mod health;
pub mod lock;
pub mod logs;
pub mod manifest;
pub mod paths;
pub mod pier;
pub mod runtime;
pub mod state;
pub mod version;

use std::path::PathBuf;

use logs::LogManager;
use paths::AppPaths;
use runtime::RuntimeManager;
use serde::Serialize;
use state::{LauncherState, StateMachine};
use tauri::Manager;

/// Diagnostics snapshot returned by `get_diagnostics`.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostics {
    pub app_support_path: String,
    pub pier_path: String,
    pub bundled_vere_version: Option<String>,
    pub pier_vere_version: Option<String>,
    pub current_state: LauncherState,
    pub pid: Option<u32>,
    pub last_exit_code: Option<i32>,
    pub last_error: Option<String>,
    pub ship_name: Option<String>,
    pub http_port: u16,
}

/// Status payload returned by `get_status`.
#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    #[serde(flatten)]
    pub state: LauncherState,
    pub ship_name: Option<String>,
    pub is_ready: bool,
}

#[tauri::command]
fn get_app_paths(app_paths: tauri::State<'_, AppPaths>) -> Result<AppPaths, errors::LauncherError> {
    Ok(app_paths.inner().clone())
}

#[tauri::command]
fn get_status(
    state_machine: tauri::State<'_, StateMachine>,
    runtime: tauri::State<'_, RuntimeManager>,
    app_paths: tauri::State<'_, AppPaths>,
) -> Result<StatusResponse, errors::LauncherError> {
    let state = state_machine.current();
    let ship_name = detect_ship_name_from_pier(&app_paths.pier_dir)
        .or_else(|| runtime.fake_ship().map(|s| s.to_string()));
    let is_ready = runtime.is_ship_ready();
    Ok(StatusResponse {
        state,
        ship_name,
        is_ready,
    })
}

#[tauri::command]
async fn prepare_ship(
    state_machine: tauri::State<'_, StateMachine>,
    runtime: tauri::State<'_, RuntimeManager>,
    app_paths: tauri::State<'_, AppPaths>,
) -> Result<(), errors::LauncherError> {
    let sm = state_machine.inner().clone();
    let rt = runtime.inner().clone();
    let paths = app_paths.inner().clone();
    let log_manager = rt.log_manager().clone();

    // Ensure directories exist.
    paths.ensure_dirs()?;

    // Ensure vere binary exists (download if needed).
    // SHIP_LAUNCHER_VERE_PATH env var overrides — skip download.
    if std::env::var("SHIP_LAUNCHER_VERE_PATH").is_err() {
        let vere_path = paths.data_dir.join("bin").join("vere");
        let pier_for_version = if rt.fake_ship().is_some() {
            paths.data_dir.join(rt.fake_ship().unwrap())
        } else {
            paths.pier_dir.clone()
        };
        download::ensure_vere(&vere_path, &pier_for_version, &sm, &log_manager).await?;
    }

    let is_fake_mode = rt.fake_ship().is_some();

    // In fake mode, skip extraction entirely — vere -F will create the pier.
    if is_fake_mode {
        let name = rt.fake_ship().unwrap();
        log_manager.add_launcher_line(&format!("Fake mode: skipping extraction for ~{name}"));

        let current = sm.current();
        if matches!(current, LauncherState::Uninitialized) {
            sm.transition(LauncherState::Prepared)?;
        }

        log_manager.add_launcher_line("Auto-starting runtime (fake mode)...");
        rt.start().await?;
        return Ok(());
    }

    // If pier already exists, skip extraction and go to Prepared.
    let pier_exists = paths.pier_dir.join(".urb").is_dir();

    if pier_exists {
        log_manager.add_launcher_line("Pier already exists, skipping extraction");
        // Transition to Prepared (skip Extracting). Handle case where we're already past Uninitialized.
        let current = sm.current();
        if matches!(current, LauncherState::Uninitialized) {
            sm.transition(LauncherState::Prepared)?;
        }
    } else {
        // Transition to Extracting
        sm.transition(LauncherState::Extracting {
            message: "Looking for bundled pier archive...".into(),
        })?;
        log_manager.add_launcher_line("No pier found, attempting extraction from bundle");

        // Try to find bundled assets. In dev mode these may not exist.
        // Check for a manifest + archive via env var or known paths.
        let archive_path = std::env::var("SHIP_LAUNCHER_ARCHIVE_PATH")
            .map(PathBuf::from)
            .ok();
        let manifest_path = std::env::var("SHIP_LAUNCHER_MANIFEST_PATH")
            .map(PathBuf::from)
            .ok();

        if let (Some(archive), Some(manifest_file)) = (archive_path, manifest_path) {
            if archive.exists() && manifest_file.exists() {
                let manifest = manifest::PierManifest::from_file(&manifest_file)?;

                log_manager.add_launcher_line("Verifying archive checksum...");
                sm.transition(LauncherState::Extracting {
                    message: "Verifying archive checksum...".into(),
                })
                .ok(); // May already be in Extracting

                extract::run_extraction(&paths, &manifest, &archive)?;

                log_manager.add_launcher_line("Extraction complete");
            } else {
                let msg = format!(
                    "Archive or manifest not found (archive: {}, manifest: {})",
                    archive.display(),
                    manifest_file.display()
                );
                log_manager.add_launcher_line(&msg);
                sm.force_error(
                    "Bundled pier archive not found".into(),
                    Some(msg),
                );
                return Err(errors::LauncherError::Extraction {
                    reason: "bundled pier archive not found".into(),
                });
            }
        } else {
            log_manager.add_launcher_line(
                "No SHIP_LAUNCHER_ARCHIVE_PATH or SHIP_LAUNCHER_MANIFEST_PATH set, \
                 and no bundled assets found. Place a pier at the pier directory manually.",
            );
            sm.force_error(
                "No pier archive available".into(),
                Some("Set SHIP_LAUNCHER_ARCHIVE_PATH and SHIP_LAUNCHER_MANIFEST_PATH env vars, or place a pier manually".into()),
            );
            return Err(errors::LauncherError::Extraction {
                reason: "no pier archive available for extraction".into(),
            });
        }

        // Transition to Prepared after extraction.
        sm.transition(LauncherState::Prepared)?;
    }

    // Validate pier structure (best-effort: create a dummy manifest for validation if no manifest available).
    let manifest_path = std::env::var("SHIP_LAUNCHER_MANIFEST_PATH")
        .map(PathBuf::from)
        .ok();
    if let Some(ref mp) = manifest_path {
        if mp.exists() {
            if let Ok(manifest) = manifest::PierManifest::from_file(mp) {
                log_manager.add_launcher_line("Validating pier structure...");
                match pier::validate_pier(&paths.pier_dir, &manifest) {
                    Ok(result) => {
                        for w in &result.warnings {
                            log_manager.add_launcher_line(&format!("Warning: {w}"));
                        }
                        if let Some(ref v) = result.info.vere_version {
                            log_manager
                                .add_launcher_line(&format!("Pier vere version: {v}"));
                        }
                    }
                    Err(e) => {
                        log_manager
                            .add_launcher_line(&format!("Pier validation warning: {e}"));
                    }
                }
            }
        }
    }

    // Version compatibility check (best-effort).
    let vere_path_str = std::env::var("SHIP_LAUNCHER_VERE_PATH").ok();
    if let Some(ref vp) = vere_path_str {
        let vere_path = PathBuf::from(vp);
        if vere_path.exists() {
            // Read pier's .vere.txt
            let pier_vere = std::fs::read_to_string(paths.pier_dir.join(".vere.txt"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            log_manager.add_launcher_line("Checking runtime version compatibility...");
            match version::check_version_compatibility(&vere_path, pier_vere.as_deref()) {
                Ok(result) => {
                    log_manager.add_launcher_line(&format!(
                        "Bundled vere version: {}",
                        result.bundled_version.raw
                    ));
                    for w in &result.warnings {
                        log_manager.add_launcher_line(&format!("Warning: {w}"));
                    }
                }
                Err(e) => {
                    log_manager
                        .add_launcher_line(&format!("Version check failed: {e}"));
                }
            }
        }
    }

    log_manager.add_launcher_line("Ship preparation complete");

    // Auto-start the runtime.
    log_manager.add_launcher_line("Auto-starting runtime...");
    rt.start().await?;

    Ok(())
}

#[tauri::command]
async fn start_ship(
    runtime: tauri::State<'_, RuntimeManager>,
) -> Result<u32, errors::LauncherError> {
    runtime.start().await
}

#[tauri::command]
async fn stop_ship(
    runtime: tauri::State<'_, RuntimeManager>,
) -> Result<(), errors::LauncherError> {
    runtime.stop().await
}

#[tauri::command]
async fn restart_ship(
    runtime: tauri::State<'_, RuntimeManager>,
) -> Result<u32, errors::LauncherError> {
    runtime.restart().await
}

#[tauri::command]
fn get_recent_logs(
    runtime: tauri::State<'_, RuntimeManager>,
    count: Option<usize>,
) -> Vec<String> {
    runtime.recent_logs(count.unwrap_or(100))
}

#[tauri::command]
fn get_ship_ready(runtime: tauri::State<'_, RuntimeManager>) -> bool {
    runtime.is_ship_ready()
}

#[tauri::command]
fn open_ship(runtime: tauri::State<'_, RuntimeManager>) -> Result<(), errors::LauncherError> {
    let port = runtime.http_port();
    let url = format!("http://localhost:{port}");
    open::that(&url).map_err(|e| errors::LauncherError::Runtime {
        reason: format!("failed to open browser: {e}"),
    })
}

#[tauri::command]
fn reveal_data_dir(
    app_paths: tauri::State<'_, AppPaths>,
) -> Result<(), errors::LauncherError> {
    open::that(&app_paths.data_dir).map_err(|e| errors::LauncherError::Runtime {
        reason: format!("failed to open directory: {e}"),
    })
}

#[tauri::command]
fn get_diagnostics(
    state_machine: tauri::State<'_, StateMachine>,
    runtime: tauri::State<'_, RuntimeManager>,
    app_paths: tauri::State<'_, AppPaths>,
) -> Diagnostics {
    let current_state = state_machine.current();

    // Extract last exit code and error from state.
    let (last_exit_code, last_error) = match &current_state {
        LauncherState::Crashed {
            exit_code, message, ..
        } => (*exit_code, Some(message.clone())),
        LauncherState::Error { message, detail, .. } => {
            let full = if let Some(d) = detail {
                format!("{message}: {d}")
            } else {
                message.clone()
            };
            (None, Some(full))
        }
        _ => (None, None),
    };

    // Read pier's .vere.txt
    let pier_vere_version = std::fs::read_to_string(app_paths.pier_dir.join(".vere.txt"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Get bundled vere version (best-effort).
    let vere_path = std::env::var("SHIP_LAUNCHER_VERE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| app_paths.data_dir.join("bin").join("vere"));
    let bundled_vere_version = if vere_path.exists() {
        version::parse_version(
            &std::process::Command::new(&vere_path)
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default(),
        )
        .map(|v| v.raw)
    } else {
        None
    };

    let ship_name = detect_ship_name_from_pier(&app_paths.pier_dir)
        .or_else(|| runtime.fake_ship().map(|s| s.to_string()));

    Diagnostics {
        app_support_path: app_paths.data_dir.display().to_string(),
        pier_path: app_paths.pier_dir.display().to_string(),
        bundled_vere_version,
        pier_vere_version,
        current_state,
        pid: runtime.pid(),
        last_exit_code,
        last_error,
        ship_name,
        http_port: runtime.http_port(),
    }
}

#[tauri::command]
async fn get_login_code(
    app_paths: tauri::State<'_, AppPaths>,
    runtime: tauri::State<'_, RuntimeManager>,
) -> Result<String, errors::LauncherError> {
    // Determine the pier path (same logic as run()).
    let pier_path = if runtime.fake_ship().is_some() {
        app_paths
            .data_dir
            .join(runtime.fake_ship().unwrap())
    } else {
        app_paths.pier_dir.clone()
    };
    click::get_code(&pier_path).await
}

#[tauri::command]
async fn retry_boot(
    state_machine: tauri::State<'_, StateMachine>,
    runtime: tauri::State<'_, RuntimeManager>,
) -> Result<(), errors::LauncherError> {
    let log_manager = runtime.log_manager().clone();

    // Stop vere if running.
    if runtime.pid().is_some() {
        log_manager.add_launcher_line("Stopping runtime before retry...");
        runtime.stop().await?;
        // Wait for state to settle.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    log_manager.add_launcher_line("Retrying boot sequence");

    // Force back to Uninitialized to re-trigger prepare/start.
    state_machine.force_error("Retry initiated".into(), None);
    state_machine.transition(LauncherState::Uninitialized)?;

    log_manager.add_launcher_line("Retry — returned to Uninitialized");
    Ok(())
}

/// Helper to detect ship name from pier's install marker.
fn detect_ship_name_from_pier(pier_dir: &std::path::Path) -> Option<String> {
    extract::InstallMarker::read(pier_dir)
        .ok()
        .and_then(|m| m.ship_name)
        .filter(|s| !s.is_empty())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state_machine = StateMachine::new();

    let app_paths = AppPaths::resolve().expect("failed to resolve app paths");

    // Ensure run directory exists before acquiring lock.
    let _ = std::fs::create_dir_all(&app_paths.run_dir);

    // Acquire file-based instance lock. Stale locks from prior crashes are
    // detected and cleaned up automatically.
    let _instance_lock = match lock::InstanceLock::acquire(&app_paths.run_dir) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("{e}");
            // The single-instance plugin below will focus the existing window.
            // If we get here without the plugin catching it, just exit.
            std::process::exit(1);
        }
    };

    // Initialize log manager with disk persistence.
    let log_manager = LogManager::new();
    if let Err(e) = log_manager.init_disk_logs(&app_paths.logs_dir) {
        eprintln!("Warning: failed to initialize disk logging: {e}");
    }
    log_manager.add_launcher_line("Launcher starting up");

    let vere_path = std::env::var("SHIP_LAUNCHER_VERE_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| app_paths.data_dir.join("bin").join("vere"));

    let fake_ship = std::env::var("SHIP_LAUNCHER_FAKE_SHIP").ok();

    // In fake mode, the pier path is data_dir/<ship-name> (vere -F creates it there).
    let pier_path = if let Some(ref name) = fake_ship {
        app_paths.data_dir.join(name)
    } else {
        app_paths.pier_dir.clone()
    };

    let mut runtime = RuntimeManager::new(
        state_machine.clone(),
        vere_path,
        pier_path,
        log_manager.clone(),
    );

    if let Some(ref name) = fake_ship {
        log_manager.add_launcher_line(&format!("Fake ship mode enabled: ~{name}"));
        runtime = runtime.with_fake_ship(name.clone());
    }

    let runtime_for_exit = runtime.clone();
    let log_for_exit = log_manager.clone();
    let sm_for_exit = state_machine.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second instance was launched — focus the existing window.
            if let Some(window) = app.webview_windows().values().next() {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .manage(state_machine)
        .manage(runtime)
        .manage(app_paths)
        .invoke_handler(tauri::generate_handler![
            get_app_paths,
            get_status,
            prepare_ship,
            start_ship,
            stop_ship,
            restart_ship,
            get_recent_logs,
            get_ship_ready,
            open_ship,
            reveal_data_dir,
            get_diagnostics,
            get_login_code,
            retry_boot,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = &event {
                if let Some(pid) = runtime_for_exit.pid() {
                    log_for_exit.add_launcher_line("Window closed — stopping vere");
                    let _ = sm_for_exit.transition(LauncherState::Stopping);
                    // Kill the entire process group so the vere worker subprocess
                    // is also terminated (vere spawns a `work` child).
                    #[cfg(unix)]
                    unsafe {
                        // Try process group first (negative PID).
                        libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
                        // Also signal the parent directly in case it's not a
                        // process group leader.
                        libc::kill(pid as libc::pid_t, libc::SIGTERM);
                    }
                    log_for_exit.add_launcher_line("Sent SIGTERM to vere process group, exiting");
                }
            }
        });
}
