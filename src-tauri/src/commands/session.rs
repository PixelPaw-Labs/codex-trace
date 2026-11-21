use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::parser::session::parse_session;
use crate::state::AppState;
use crate::watcher::start_session_watcher;

pub const NO_SESSION_PATH_PROVIDED: &str = "no session path provided";

/// Prefix of the error returned when a session path has no rollout file on
/// disk, plain or zstd-compressed. `http_api` matches on it to answer 404
/// rather than a generic 500 with the raw IO error.
pub const NO_LOCAL_SESSION_DATA: &str = "no local session data";

pub fn load_session_from_path(path: &str) -> Result<crate::parser::session::CodexSession, String> {
    if path.is_empty() {
        return Err(NO_SESSION_PATH_PROVIDED.to_string());
    }
    let p = std::path::Path::new(path);
    let resolved = crate::parser::compression::resolve_rollout_path(p).ok_or_else(|| {
        format!(
            "{NO_LOCAL_SESSION_DATA} at {path} (the session may have run against a remote app server, e.g. `codex exec --remote`, which does not write a local rollout file)"
        )
    })?;
    parse_session(&resolved)
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

    #[test]
    fn load_session_from_path_reports_missing_local_session_data() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rollout-remote-only.jsonl");

        let result = load_session_from_path(path.to_str().unwrap());

        let err = result.unwrap_err();
        assert!(
            err.starts_with(NO_LOCAL_SESSION_DATA),
            "expected error to start with {NO_LOCAL_SESSION_DATA:?}, got {err:?}"
        );
    }

    #[test]
    fn load_session_from_path_accepts_a_compressed_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("rollout-cold.jsonl");
        let compressed = tmp.path().join("rollout-cold.jsonl.zst");
        let content = r#"{"type":"session_meta","payload":{"id":"cold-session","timestamp":"2026-06-04T00:00:00Z"}}"#;
        std::fs::write(
            &compressed,
            zstd::encode_all(content.as_bytes(), 3).unwrap(),
        )
        .unwrap();

        // The plain path doesn't exist, only its `.zst` sibling does; the
        // existence check must still let the real read through to `parse_session`.
        let result = load_session_from_path(plain.to_str().unwrap());

        assert!(result.is_ok(), "{:?}", result.err());
    }
}
