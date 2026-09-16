use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::parser::discover::{page_of, SessionPage};
use crate::state::AppState;
use crate::watcher::start_picker_watcher;

#[tauri::command]
pub async fn list_sessions(
    sessions_dir: String,
    offset: Option<usize>,
    limit: Option<usize>,
    query: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<SessionPage, String> {
    let mut sessions = state.discover_sessions_cached(&sessions_dir)?;
    state.apply_watched_ongoing(&mut sessions);
    Ok(page_of(
        sessions,
        query.as_deref(),
        offset.unwrap_or(0),
        limit,
    ))
}

#[tauri::command]
pub async fn watch_picker(
    sessions_dir: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    state.stop_picker_watcher()?;
    let handle = start_picker_watcher(sessions_dir, state.inner().clone(), Some(app));
    state.set_picker_watcher(handle)
}

#[tauri::command]
pub async fn unwatch_picker(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.stop_picker_watcher()
}
