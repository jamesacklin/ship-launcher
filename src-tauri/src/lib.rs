pub mod bundle;
pub mod errors;
pub mod extract;
pub mod health;
pub mod logs;
pub mod manifest;
pub mod paths;
pub mod pier;
pub mod runtime;
pub mod state;
pub mod version;

use logs::LogManager;
use paths::AppPaths;
use runtime::RuntimeManager;
use state::{LauncherState, StateMachine};

#[tauri::command]
fn get_app_paths() -> Result<AppPaths, errors::LauncherError> {
    AppPaths::resolve()
}

#[tauri::command]
fn get_status(
    state_machine: tauri::State<'_, StateMachine>,
) -> Result<LauncherState, errors::LauncherError> {
    Ok(state_machine.current())
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state_machine = StateMachine::new();

    // Resolve paths for the runtime manager.
    // In a full app these would come from bundle/manifest resolution; for now
    // we use sensible defaults that can be overridden via env vars.
    let app_paths = AppPaths::resolve().expect("failed to resolve app paths");

    // Initialize log manager with disk persistence.
    let log_manager = LogManager::new();
    if let Err(e) = log_manager.init_disk_logs(&app_paths.logs_dir) {
        eprintln!("Warning: failed to initialize disk logging: {e}");
    }
    log_manager.add_launcher_line("Launcher starting up");

    let vere_path = std::env::var("SHIP_LAUNCHER_VERE_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| app_paths.data_dir.join("bin").join("vere"));

    let runtime = RuntimeManager::new(
        state_machine.clone(),
        vere_path,
        app_paths.pier_dir.clone(),
        log_manager,
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state_machine)
        .manage(runtime)
        .invoke_handler(tauri::generate_handler![
            get_app_paths,
            get_status,
            start_ship,
            stop_ship,
            restart_ship,
            get_recent_logs,
            get_ship_ready,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
