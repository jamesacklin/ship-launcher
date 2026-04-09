pub mod bundle;
pub mod errors;
pub mod extract;
pub mod manifest;
pub mod paths;
pub mod pier;
pub mod state;
pub mod version;

use paths::AppPaths;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(StateMachine::new())
        .invoke_handler(tauri::generate_handler![get_app_paths, get_status])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
