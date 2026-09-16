use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::parser::session::parse_session;
use crate::state::AppState;
use crate::watcher::start_session_watcher;

pub const NO_SESSION_PATH_PROVIDED: &str = "no session path provided";

pub fn load_session_from_path(path: &str) -> Result<crate::parser::session::CodexSession, String> {
    if path.is_empty() {
        return Err(NO_SESSION_PATH_PROVIDED.to_string());
    }
    let p = std::path::Path::new(path);
    parse_session(p)
}

/// The session's metadata and its lightweight turn index. Turn bodies are
/// fetched one at a time by [`load_turn`], so the frontend never holds a whole
/// transcript's tool output at once.
#[tauri::command]
pub async fn load_session(
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::parser::summary::SessionIndex, String> {
    if path.is_empty() {
        return Err(NO_SESSION_PATH_PROVIDED.to_string());
    }
    state.load_session_index(&path)
}

/// One turn, with its bodies, by position in the turn index.
#[tauri::command]
pub async fn load_turn(
    path: String,
    index: usize,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::parser::turn::CodexTurn, String> {
    if path.is_empty() {
        return Err(NO_SESSION_PATH_PROVIDED.to_string());
    }
    state.load_turn(&path, index)
}

#[tauri::command]
pub async fn watch_session(
    path: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let session = load_session_from_path(&path)?;
    state.stop_session_watcher()?;
    state.set_watched_ongoing(path.clone(), session.is_ongoing);
    let handle = start_session_watcher(path, state.inner().clone(), Some(app));
    state.set_session_watcher(handle)
}

#[tauri::command]
pub async fn unwatch_session(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.clear_watched_ongoing();
    state.clear_parsed_session();
    state.stop_session_watcher()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_session_from_path_rejects_empty_path() {
        let result = load_session_from_path("");

        assert_eq!(result.unwrap_err(), "no session path provided");
    }
}
