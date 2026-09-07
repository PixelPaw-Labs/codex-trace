use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::settings::Settings;
use crate::state::AppState;

#[derive(Serialize)]
pub struct SettingsResponse {
    pub sessions_dir: Option<String>,
    pub default_dir: String,
}

pub fn platform_default_dir() -> String {
    dirs::home_dir()
        .map(|h| {
            h.join(".codex")
                .join("sessions")
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_default()
}

pub fn build_settings_response(settings: &Settings) -> SettingsResponse {
    SettingsResponse {
        sessions_dir: settings.sessions_dir.clone(),
        default_dir: platform_default_dir(),
    }
}

pub fn validate_sessions_dir(path: &str) -> Result<(), String> {
    if crate::parser::remote::is_remote_spec(path) {
        return crate::parser::remote::test_connection(path);
    }
    let pb = std::path::PathBuf::from(path);
    if !pb.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    if !pb.is_dir() {
        return Err(format!("path is not a directory: {path}"));
    }
    Ok(())
}

#[tauri::command]
pub async fn get_settings(state: State<'_, Arc<AppState>>) -> Result<SettingsResponse, String> {
    let guard = state.settings.lock().map_err(|e| e.to_string())?;
    Ok(build_settings_response(&guard))
}

#[tauri::command]
pub async fn set_sessions_dir(
    path: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<SettingsResponse, String> {
    if let Some(ref p) = path {
        validate_sessions_dir(p)?;
    }

    let mut guard = state.settings.lock().map_err(|e| e.to_string())?;
    guard.sessions_dir = path;
    crate::settings::save_settings(&guard)?;
    Ok(build_settings_response(&guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_sessions_dir_validation_requires_a_complete_spec() {
        assert!(validate_sessions_dir("ssh://").is_err());
        assert!(validate_sessions_dir("ssh://dev").is_err());
    }
}
