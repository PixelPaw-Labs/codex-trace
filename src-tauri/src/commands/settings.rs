use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::settings::Settings;
use crate::state::AppState;

#[derive(Serialize)]
pub struct SettingsResponse {
    pub sessions_dir: Option<String>,
    pub default_dir: String,
    /// Extra CORS origins allowed to call the local HTTP API, configured via the
    /// Settings UI. Unioned at request time with the built-in defaults and
    /// `CODEXTRACE_ALLOWED_ORIGINS` (see `http_api::build_cors`).
    pub allowed_origins: Vec<String>,
    /// Whether the HTTP API requires a client credential (false only under
    /// `CODEXTRACE_API_AUTH=off`). See `crate::auth`.
    pub api_auth_enabled: bool,
    /// Where the signing key comes from: `"file"` (persisted config dir),
    /// `"ephemeral"` (config dir unusable at startup — credentials minted this
    /// run die with the process), or `"disabled"`.
    pub api_auth_source: &'static str,
    /// The accepted clients (never their credentials). See `crate::clients`.
    pub clients: Vec<crate::clients::Client>,
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

pub fn build_settings_response(
    settings: &Settings,
    auth: &crate::auth::AuthMode,
    clients: Vec<crate::clients::Client>,
) -> SettingsResponse {
    SettingsResponse {
        sessions_dir: settings.sessions_dir.clone(),
        default_dir: platform_default_dir(),
        allowed_origins: settings.allowed_origins.clone(),
        api_auth_enabled: auth.is_enabled(),
        api_auth_source: auth.source(),
        clients,
    }
}

#[tauri::command]
pub async fn get_settings(state: State<'_, Arc<AppState>>) -> Result<SettingsResponse, String> {
    let guard = state.settings.lock().map_err(|e| e.to_string())?;
    Ok(build_settings_response(
        &guard,
        &state.auth_snapshot(),
        state.clients_snapshot(),
    ))
}

#[tauri::command]
pub async fn set_sessions_dir(
    path: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<SettingsResponse, String> {
    if let Some(ref p) = path {
        let pb = std::path::PathBuf::from(p);
        if !pb.exists() {
            return Err(format!("path does not exist: {p}"));
        }
        if !pb.is_dir() {
            return Err(format!("path is not a directory: {p}"));
        }
    }

    let mut guard = state.settings.lock().map_err(|e| e.to_string())?;
    guard.sessions_dir = path;
    crate::settings::save_settings(&guard)?;
    Ok(build_settings_response(
        &guard,
        &state.auth_snapshot(),
        state.clients_snapshot(),
    ))
}
