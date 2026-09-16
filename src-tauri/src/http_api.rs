use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue, Method};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;

use crate::state::AppState;
use crate::watcher::{start_picker_watcher, start_session_watcher};

#[derive(Clone)]
pub struct HttpState {
    pub app_state: Arc<AppState>,
    pub app: Option<AppHandle>,
}

pub const DEFAULT_HTTP_HOST: &str = "127.0.0.1";
pub const DEFAULT_HTTP_PORT: u16 = 11424;

fn pick_host(raw: Option<String>) -> String {
    raw.filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_HTTP_HOST.to_string())
}

fn pick_port(raw: Option<String>) -> u16 {
    raw.and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(DEFAULT_HTTP_PORT)
}

pub fn resolve_bind_addr() -> (String, u16) {
    (
        pick_host(std::env::var("CODEXTRACE_HTTP_HOST").ok()),
        pick_port(std::env::var("CODEXTRACE_HTTP_PORT").ok()),
    )
}

pub fn resolve_static_dir() -> Option<String> {
    std::env::var("CODEXTRACE_STATIC_DIR")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Origins the browser UI is served from. In web/dev mode the frontend runs on
/// `localhost:1420` and calls the API on port 11424 — a distinct origin — so
/// these are allowlisted for CORS. The Tauri desktop webview talks to the
/// backend over the IPC bridge (never HTTP), and the Docker image serves the UI
/// same-origin, so neither needs an entry here. Extra origins can be added at
/// launch via `CODEXTRACE_ALLOWED_ORIGINS` or at runtime from Settings
/// (`Settings.allowed_origins`, checked per-request in `build_cors`); the two
/// are unioned, not replaced.
const DEFAULT_ALLOWED_ORIGINS: [&str; 2] = ["http://localhost:1420", "http://127.0.0.1:1420"];

/// Split a raw `CODEXTRACE_ALLOWED_ORIGINS` value into individual origins,
/// dropping empty entries and surrounding whitespace.
fn parse_extra_origins(raw: Option<String>) -> Vec<String> {
    raw.into_iter()
        .flat_map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Resolve the *static* half of the CORS origin allowlist: the built-in
/// dev/web origins plus any added via the `CODEXTRACE_ALLOWED_ORIGINS` env var
/// (comma-separated). Read once at startup because env vars cannot change at
/// runtime. The *live* half (origins configured from Settings) is checked
/// per-request in `build_cors`'s predicate.
fn resolve_allowed_origins() -> Vec<String> {
    let mut origins: Vec<String> = DEFAULT_ALLOWED_ORIGINS
        .iter()
        .map(|s| s.to_string())
        .collect();
    origins.extend(parse_extra_origins(
        std::env::var("CODEXTRACE_ALLOWED_ORIGINS").ok(),
    ));
    origins
}

/// Exact string match against either half of the allowlist. Deliberately not a
/// prefix or suffix match: `http://localhost:1420.evil.com` must not pass
/// because it starts with an allowed origin.
fn origin_allowed(origin: &str, static_origins: &[String], live_origins: &[String]) -> bool {
    static_origins.iter().any(|o| o == origin) || live_origins.iter().any(|o| o == origin)
}

/// Build a CORS layer scoped to the allowlisted origins. This replaces a
/// permissive `*` policy under which any website the user visited could read
/// local Codex session data (prompts, code, tool output) cross-origin while the
/// app was running.
///
/// The origin check is a live predicate rather than a static list so that
/// origins added from Settings take effect immediately, matching every other
/// setting — no server restart.
fn build_cors(app_state: Arc<AppState>) -> CorsLayer {
    let static_origins = resolve_allowed_origins();
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(
            move |origin: &HeaderValue, _parts: &axum::http::request::Parts| {
                let Ok(origin_str) = origin.to_str() else {
                    return false;
                };
                let live_origins = app_state
                    .settings
                    .lock()
                    .map(|g| g.allowed_origins.clone())
                    .unwrap_or_default();
                origin_allowed(origin_str, &static_origins, &live_origins)
            },
        ))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE])
}

/// Start the HTTP server from a Tauri AppHandle (desktop/web mode).
pub async fn start_http_server(app: AppHandle) {
    let app_state: Arc<AppState> = app.state::<Arc<AppState>>().inner().clone();
    run_server(Arc::new(HttpState {
        app_state,
        app: Some(app),
    }))
    .await;
}

/// Start the HTTP server without Tauri (headless mode).
pub async fn start_http_server_headless(state: Arc<AppState>) {
    run_server(Arc::new(HttpState {
        app_state: state,
        app: None,
    }))
    .await;
}

async fn run_server(state: Arc<HttpState>) {
    let mut router = Router::new()
        .route("/api/settings", get(api_get_settings))
        .route("/api/settings/dir", post(api_set_sessions_dir))
        .route("/api/settings/origins", post(api_set_allowed_origins))
        .route("/api/sessions", post(api_discover_sessions))
        .route("/api/session/load", post(api_load_session))
        .route("/api/session/watch", post(api_watch_session))
        .route("/api/session/unwatch", post(api_unwatch_session))
        .route("/api/picker/watch", post(api_watch_picker))
        .route("/api/picker/unwatch", post(api_unwatch_picker))
        .route("/api/events", get(api_events));

    if let Some(dir) = resolve_static_dir() {
        let serve = ServeDir::new(&dir).append_index_html_on_directories(true);
        router = router.fallback_service(serve);
        eprintln!("HTTP API: serving static assets from {dir}");
    }

    let cors_state = state.app_state.clone();
    let router = router.layer(build_cors(cors_state)).with_state(state);

    let (host, port) = resolve_bind_addr();
    let addr = format!("{host}:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("HTTP API: failed to bind {addr}: {e}");
            return;
        }
    };
    eprintln!("HTTP API: listening on http://{addr}");

    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("HTTP API: server error: {e}");
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn app_state(state: &HttpState) -> &AppState {
    &state.app_state
}

fn err_response(status: axum::http::StatusCode, msg: String) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

fn ok_json<T: serde::Serialize>(val: &T) -> Response {
    Json(val).into_response()
}

fn session_load_error_status(msg: &str) -> axum::http::StatusCode {
    if msg == crate::commands::session::NO_SESSION_PATH_PROVIDED {
        axum::http::StatusCode::BAD_REQUEST
    } else {
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

async fn api_get_settings(State(state): State<Arc<HttpState>>) -> Response {
    let app_state = app_state(&state);
    let guard = match app_state.settings.lock() {
        Ok(g) => g,
        Err(e) => {
            return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    };
    ok_json(&crate::commands::settings::build_settings_response(&guard))
}

#[derive(Deserialize)]
struct SetDirBody {
    path: Option<String>,
}

async fn api_set_sessions_dir(
    State(state): State<Arc<HttpState>>,
    Json(body): Json<SetDirBody>,
) -> Response {
    let app_state = app_state(&state);

    if let Some(ref p) = body.path {
        let pb = std::path::PathBuf::from(p);
        if !pb.exists() {
            return err_response(
                axum::http::StatusCode::BAD_REQUEST,
                format!("path does not exist: {p}"),
            );
        }
        if !pb.is_dir() {
            return err_response(
                axum::http::StatusCode::BAD_REQUEST,
                format!("path is not a directory: {p}"),
            );
        }
    }

    let mut guard = match app_state.settings.lock() {
        Ok(g) => g,
        Err(e) => {
            return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    };
    guard.sessions_dir = body.path;
    if let Err(e) = crate::settings::save_settings(&guard) {
        return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    ok_json(&crate::commands::settings::build_settings_response(&guard))
}

#[derive(Deserialize)]
struct SetOriginsBody {
    origins: Vec<String>,
}

async fn api_set_allowed_origins(
    State(state): State<Arc<HttpState>>,
    Json(body): Json<SetOriginsBody>,
) -> Response {
    let app_state = app_state(&state);

    let validated = match crate::commands::cors::sanitize_and_validate_origins(body.origins) {
        Ok(v) => v,
        Err(e) => return err_response(axum::http::StatusCode::BAD_REQUEST, e),
    };

    let mut guard = match app_state.settings.lock() {
        Ok(g) => g,
        Err(e) => {
            return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    };
    guard.allowed_origins = validated;
    if let Err(e) = crate::settings::save_settings(&guard) {
        return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    ok_json(&crate::commands::settings::build_settings_response(&guard))
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DiscoverBody {
    dir: String,
}

async fn api_discover_sessions(
    State(state): State<Arc<HttpState>>,
    Json(body): Json<DiscoverBody>,
) -> Response {
    let app_state = app_state(&state);
    let mut sessions = match app_state.discover_sessions_cached(&body.dir) {
        Ok(s) => s,
        Err(e) => return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    app_state.apply_watched_ongoing(&mut sessions);
    ok_json(&sessions)
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

async fn api_load_session(Json(body): Json<PathBody>) -> Response {
    let session = match crate::commands::session::load_session_from_path(&body.path) {
        Ok(s) => s,
        Err(e) => return err_response(session_load_error_status(&e), e),
    };
    ok_json(&session)
}

// ---------------------------------------------------------------------------
// Watch / unwatch
// ---------------------------------------------------------------------------

async fn api_watch_session(
    State(state): State<Arc<HttpState>>,
    Json(body): Json<PathBody>,
) -> Response {
    let app_state = app_state(&state);
    let session = match crate::commands::session::load_session_from_path(&body.path) {
        Ok(s) => s,
        Err(e) => return err_response(session_load_error_status(&e), e),
    };
    if let Err(e) = app_state.stop_session_watcher() {
        return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    app_state.set_watched_ongoing(body.path.clone(), session.is_ongoing);
    let handle = start_session_watcher(body.path, state.app_state.clone(), state.app.clone());
    if let Err(e) = app_state.set_session_watcher(handle) {
        return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    ok_json(&serde_json::json!({ "ok": true }))
}

async fn api_unwatch_session(State(state): State<Arc<HttpState>>) -> Response {
    let app_state = app_state(&state);
    app_state.clear_watched_ongoing();
    match app_state.stop_session_watcher() {
        Ok(()) => ok_json(&serde_json::json!({ "ok": true })),
        Err(e) => err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct WatchPickerBody {
    #[serde(rename = "sessionsDir")]
    sessions_dir: String,
}

async fn api_watch_picker(
    State(state): State<Arc<HttpState>>,
    Json(body): Json<WatchPickerBody>,
) -> Response {
    let app_state = app_state(&state);
    if let Err(e) = app_state.stop_picker_watcher() {
        return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let handle = start_picker_watcher(
        body.sessions_dir,
        state.app_state.clone(),
        state.app.clone(),
    );
    if let Err(e) = app_state.set_picker_watcher(handle) {
        return err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    ok_json(&serde_json::json!({ "ok": true }))
}

async fn api_unwatch_picker(State(state): State<Arc<HttpState>>) -> Response {
    let app_state = app_state(&state);
    match app_state.stop_picker_watcher() {
        Ok(()) => ok_json(&serde_json::json!({ "ok": true })),
        Err(e) => err_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ---------------------------------------------------------------------------
// SSE events
// ---------------------------------------------------------------------------

async fn api_events(
    State(state): State<Arc<HttpState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let app_state = app_state(&state);
    let rx = app_state.event_tx.subscribe();

    let stream = BroadcastStream::new(rx).filter_map(|result| {
        result
            .ok()
            .map(|sse_event| Ok(Event::default().event(sse_event.event).data(sse_event.data)))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_host_uses_default_when_missing() {
        assert_eq!(pick_host(None), DEFAULT_HTTP_HOST);
    }

    #[test]
    fn pick_host_uses_default_when_empty() {
        assert_eq!(pick_host(Some(String::new())), DEFAULT_HTTP_HOST);
    }

    #[test]
    fn pick_host_uses_provided_value() {
        assert_eq!(pick_host(Some("0.0.0.0".to_string())), "0.0.0.0");
    }

    #[test]
    fn pick_port_uses_default_when_missing() {
        assert_eq!(pick_port(None), DEFAULT_HTTP_PORT);
    }

    #[test]
    fn pick_port_uses_default_when_unparsable() {
        assert_eq!(
            pick_port(Some("not-a-number".to_string())),
            DEFAULT_HTTP_PORT
        );
    }

    #[test]
    fn pick_port_uses_parsed_value() {
        assert_eq!(pick_port(Some("8080".to_string())), 8080);
    }

    // -----------------------------------------------------------------------
    // CORS allowlist
    // -----------------------------------------------------------------------

    #[test]
    fn parse_extra_origins_is_empty_when_missing() {
        assert!(parse_extra_origins(None).is_empty());
    }

    #[test]
    fn parse_extra_origins_splits_and_trims() {
        assert_eq!(
            parse_extra_origins(Some(" http://a.example , http://b.example ".to_string())),
            vec!["http://a.example", "http://b.example"],
        );
    }

    #[test]
    fn parse_extra_origins_drops_empty_entries() {
        assert!(parse_extra_origins(Some(" , ,".to_string())).is_empty());
    }

    #[test]
    fn default_origins_are_the_dev_web_ui() {
        assert_eq!(
            DEFAULT_ALLOWED_ORIGINS,
            ["http://localhost:1420", "http://127.0.0.1:1420"],
        );
    }

    #[test]
    fn default_origins_parse_to_valid_header_values() {
        for origin in DEFAULT_ALLOWED_ORIGINS {
            assert!(HeaderValue::from_str(origin).is_ok(), "{origin}");
        }
    }

    #[test]
    fn origin_allowed_matches_a_static_origin() {
        let static_origins = vec!["http://localhost:1420".to_string()];
        assert!(origin_allowed(
            "http://localhost:1420",
            &static_origins,
            &[]
        ));
    }

    #[test]
    fn origin_allowed_matches_a_live_settings_origin() {
        let live = vec!["https://trace.example".to_string()];
        assert!(origin_allowed("https://trace.example", &[], &live));
    }

    #[test]
    fn origin_allowed_rejects_an_unlisted_origin() {
        let static_origins = vec!["http://localhost:1420".to_string()];
        assert!(!origin_allowed(
            "https://evil.example",
            &static_origins,
            &[]
        ));
    }

    #[test]
    fn origin_allowed_rejects_a_prefix_extension_of_an_allowed_origin() {
        // A suffix/prefix match would let `http://localhost:1420.evil.com`
        // through; the comparison must stay exact.
        let static_origins = vec!["http://localhost:1420".to_string()];
        assert!(!origin_allowed(
            "http://localhost:1420.evil.com",
            &static_origins,
            &[]
        ));
        assert!(!origin_allowed(
            "http://evil.com/http://localhost:1420",
            &static_origins,
            &[]
        ));
    }

    #[test]
    fn origin_allowed_is_case_sensitive_and_scheme_sensitive() {
        let static_origins = vec!["http://localhost:1420".to_string()];
        assert!(!origin_allowed(
            "https://localhost:1420",
            &static_origins,
            &[]
        ));
        assert!(!origin_allowed(
            "http://LOCALHOST:1420",
            &static_origins,
            &[]
        ));
    }

    #[test]
    fn origin_allowed_rejects_everything_when_both_lists_are_empty() {
        assert!(!origin_allowed("http://localhost:1420", &[], &[]));
    }

    #[test]
    fn build_cors_constructs_without_panicking() {
        let _ = build_cors(Arc::new(AppState::new()));
    }
}
