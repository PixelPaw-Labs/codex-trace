use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Extension, Path as UrlPath, State};
use axum::http::{header, HeaderName, HeaderValue, Method};
use axum::middleware::from_fn_with_state;
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

use crate::auth;
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
pub(crate) fn resolve_allowed_origins() -> Vec<String> {
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
        // The credential header has to be allowlisted or the preflight for
        // every dev-mode call fails: the dev server is a different origin from
        // the API, so `npm run dev` in a browser cannot authenticate at all.
        // Deliberately no `allow_credentials`: a cross-origin caller uses the
        // header (or `?token=`) carrier, never the cookie.
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static(crate::auth::TOKEN_HEADER),
        ])
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

/// Assemble the API router, the optional static-asset fallback, and the
/// middleware around them.
///
/// Layer order matters:
/// - `route_layer` applies the auth middleware to the registered API routes
///   only — the static fallback stays public (the SPA shell must load before it
///   can authenticate) and unknown paths still 404 rather than 401.
/// - The static fallback gets its own middleware that hands the credential to
///   the same-origin browser UI as a cookie (see `auth::attach_credential_cookie`).
/// - CORS is added last, so it runs first: preflights are answered before the
///   auth check, and 401 responses carry CORS headers so the browser can read
///   the `{"error"}` body.
fn build_router(state: Arc<HttpState>, static_dir: Option<String>) -> Router {
    let mut router = Router::new()
        .route("/api/settings", get(api_get_settings))
        .route("/api/settings/dir", post(api_set_sessions_dir))
        .route("/api/settings/origins", post(api_set_allowed_origins))
        .route(
            "/api/clients",
            get(api_list_clients).post(api_register_client),
        )
        .route("/api/clients/{id}/reissue", post(api_reissue_client))
        .route("/api/clients/{id}/revoke", post(api_revoke_client))
        .route("/api/whoami", get(api_whoami))
        .route("/api/sessions", post(api_discover_sessions))
        .route("/api/session/load", post(api_load_session))
        .route("/api/session/turn", post(api_load_turn))
        .route("/api/session/watch", post(api_watch_session))
        .route("/api/session/unwatch", post(api_unwatch_session))
        .route("/api/picker/watch", post(api_watch_picker))
        .route("/api/picker/unwatch", post(api_unwatch_picker))
        .route("/api/events", get(api_events))
        .route_layer(from_fn_with_state(state.clone(), auth::require_client));

    if let Some(dir) = static_dir {
        let serve = ServeDir::new(&dir).append_index_html_on_directories(true);
        let static_ui = Router::new()
            .fallback_service(serve)
            .layer(from_fn_with_state(
                state.clone(),
                auth::attach_credential_cookie,
            ));
        router = router.fallback_service(static_ui);
        eprintln!("HTTP API: serving static assets from {dir}");
    }

    let cors_state = state.app_state.clone();
    router.layer(build_cors(cors_state)).with_state(state)
}

async fn run_server(state: Arc<HttpState>) {
    let router = build_router(state, resolve_static_dir());

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

pub(crate) fn err_response(status: axum::http::StatusCode, msg: String) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

fn ok_json<T: serde::Serialize>(val: &T) -> Response {
    Json(val).into_response()
}

fn session_load_error_status(msg: &str) -> axum::http::StatusCode {
    if msg == crate::commands::session::NO_SESSION_PATH_PROVIDED {
        axum::http::StatusCode::BAD_REQUEST
    } else if msg.starts_with(crate::state::NO_TURN_AT_INDEX) {
        // Asking for a turn the session does not have is a bad request, not a
        // parse failure: a stale index in the frontend must not read as the
        // session being broken.
        axum::http::StatusCode::NOT_FOUND
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
    ok_json(&crate::commands::settings::build_settings_response(
        &guard,
        &app_state.auth_snapshot(),
        app_state.clients_snapshot(),
    ))
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
    ok_json(&crate::commands::settings::build_settings_response(
        &guard,
        &app_state.auth_snapshot(),
        app_state.clients_snapshot(),
    ))
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
    ok_json(&crate::commands::settings::build_settings_response(
        &guard,
        &app_state.auth_snapshot(),
        app_state.clients_snapshot(),
    ))
}

// ---------------------------------------------------------------------------
// Accepted clients
// ---------------------------------------------------------------------------

/// Registered clients — never their credentials.
async fn api_list_clients(State(state): State<Arc<HttpState>>) -> Response {
    ok_json(&crate::commands::clients::list_clients_impl(app_state(
        &state,
    )))
}

#[derive(Deserialize)]
struct RegisterClientBody {
    name: String,
}

/// Register a client and return its credential exactly once.
async fn api_register_client(
    State(state): State<Arc<HttpState>>,
    Json(body): Json<RegisterClientBody>,
) -> Response {
    match crate::commands::clients::register_client_impl(app_state(&state), &body.name) {
        Ok(issued) => ok_json(&issued),
        Err(e) => err_response(axum::http::StatusCode::BAD_REQUEST, e),
    }
}

/// Reissue a client's credential, invalidating every older one. When the client
/// is `web-ui` the new credential is also set as the same-origin cookie so the
/// Docker browser tab that asked keeps working.
async fn api_reissue_client(
    State(state): State<Arc<HttpState>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    match crate::commands::clients::reissue_client_impl(app_state(&state), &id) {
        Ok(issued) => {
            let mut response = ok_json(&issued);
            if issued.client.name == crate::clients::WEB_UI {
                if let Some(cookie) = auth::credential_cookie_header(&issued.credential) {
                    response.headers_mut().append(header::SET_COOKIE, cookie);
                }
            }
            response
        }
        Err(e) => err_response(axum::http::StatusCode::BAD_REQUEST, e),
    }
}

async fn api_revoke_client(
    State(state): State<Arc<HttpState>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    match crate::commands::clients::revoke_client_impl(app_state(&state), &id) {
        Ok(client) => ok_json(&client),
        Err(e) => err_response(axum::http::StatusCode::BAD_REQUEST, e),
    }
}

/// Who is calling? `client` is `null` only when verification is disabled
/// (`CODEXTRACE_API_AUTH=off`); every other caller was identified by the
/// middleware.
async fn api_whoami(identity: Option<Extension<auth::ClientIdentity>>) -> Response {
    let identity = identity.map(|Extension(i)| i);
    ok_json(&serde_json::json!({
        "auth_enabled": identity.is_some(),
        "client": identity,
    }))
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

/// The session's metadata and its lightweight turn index — no turn bodies.
async fn api_load_session(
    State(state): State<Arc<HttpState>>,
    Json(body): Json<PathBody>,
) -> Response {
    if body.path.is_empty() {
        return err_response(
            axum::http::StatusCode::BAD_REQUEST,
            crate::commands::session::NO_SESSION_PATH_PROVIDED.to_string(),
        );
    }
    match app_state(&state).load_session_index(&body.path) {
        Ok(index) => ok_json(&index),
        Err(e) => err_response(session_load_error_status(&e), e),
    }
}

#[derive(Deserialize)]
struct TurnBody {
    path: String,
    index: usize,
}

/// One turn, with its bodies, by position in the turn index.
async fn api_load_turn(
    State(state): State<Arc<HttpState>>,
    Json(body): Json<TurnBody>,
) -> Response {
    if body.path.is_empty() {
        return err_response(
            axum::http::StatusCode::BAD_REQUEST,
            crate::commands::session::NO_SESSION_PATH_PROVIDED.to_string(),
        );
    }
    match app_state(&state).load_turn(&body.path, body.index) {
        Ok(turn) => ok_json(&turn),
        Err(e) => err_response(session_load_error_status(&e), e),
    }
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
        let _ = build_cors(Arc::new(AppState::for_tests(auth::AuthMode::Disabled)));
    }

    // -----------------------------------------------------------------------
    // Client verification (router level)
    // -----------------------------------------------------------------------

    mod client_auth {
        use super::*;
        use crate::auth::{AuthMode, KeySource, TOKEN_COOKIE, TOKEN_HEADER};
        use crate::clients::WEB_UI;
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

        fn enabled_state() -> Arc<HttpState> {
            Arc::new(HttpState {
                app_state: Arc::new(AppState::for_tests(AuthMode::Enabled {
                    key: KEY.to_vec(),
                    source: KeySource::Ephemeral,
                })),
                app: None,
            })
        }

        fn disabled_state() -> Arc<HttpState> {
            Arc::new(HttpState {
                app_state: Arc::new(AppState::for_tests(AuthMode::Disabled)),
                app: None,
            })
        }

        fn router_of(state: &Arc<HttpState>) -> Router {
            build_router(state.clone(), None)
        }

        fn get_with(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
            let mut b = Request::builder().uri(uri);
            for (k, v) in headers {
                b = b.header(*k, *v);
            }
            b.body(Body::empty()).unwrap()
        }

        async fn status_of(router: Router, req: Request<Body>) -> StatusCode {
            router.oneshot(req).await.unwrap().status()
        }

        async fn json_of(router: Router, req: Request<Body>) -> serde_json::Value {
            let body = router.oneshot(req).await.unwrap().into_body();
            let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        #[tokio::test]
        async fn an_unauthenticated_api_request_is_rejected() {
            let state = enabled_state();
            let status = status_of(router_of(&state), get_with("/api/settings", &[])).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn the_rejection_names_where_to_get_a_credential() {
            let state = enabled_state();
            let json = json_of(router_of(&state), get_with("/api/settings", &[])).await;
            let msg = json["error"].as_str().unwrap();
            assert!(msg.contains("Accepted clients"), "{msg}");
            assert!(msg.contains("X-CodexTrace-Token"), "{msg}");
        }

        #[tokio::test]
        async fn every_carrier_authenticates_the_web_ui_client() {
            let state = enabled_state();
            let cred = state.app_state.web_ui_credential().unwrap();

            for req in [
                get_with("/api/whoami", &[(TOKEN_HEADER, &cred)]),
                get_with(
                    "/api/whoami",
                    &[("authorization", &format!("Bearer {cred}"))],
                ),
                get_with(&format!("/api/whoami?token={cred}"), &[]),
                get_with(
                    "/api/whoami",
                    &[("cookie", &format!("{TOKEN_COOKIE}={cred}"))],
                ),
            ] {
                let json = json_of(router_of(&state), req).await;
                assert_eq!(json["client"]["name"], WEB_UI, "{json}");
                assert_eq!(json["auth_enabled"], true);
            }
        }

        #[tokio::test]
        async fn a_credential_signed_with_another_key_is_rejected() {
            let state = enabled_state();
            let forged = crate::jwt::sign(
                &crate::jwt::Claims {
                    sub: uuid::Uuid::new_v4().to_string(),
                    name: WEB_UI.into(),
                    iat: crate::clients::now(),
                },
                b"a-completely-different-key-0000000",
            );
            let status = status_of(
                router_of(&state),
                get_with("/api/settings", &[(TOKEN_HEADER, &forged)]),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn a_valid_credential_for_an_unregistered_client_is_rejected() {
            let state = enabled_state();
            let stranger = crate::jwt::sign(
                &crate::jwt::Claims {
                    sub: uuid::Uuid::new_v4().to_string(),
                    name: "ghost".into(),
                    iat: crate::clients::now(),
                },
                KEY,
            );
            let status = status_of(
                router_of(&state),
                get_with("/api/settings", &[(TOKEN_HEADER, &stranger)]),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn revoking_one_client_does_not_lock_out_another() {
            let state = enabled_state();
            let keep = state.app_state.register_client("keeper").unwrap().1;
            let (drop_client, drop_cred) = state.app_state.register_client("dropper").unwrap();

            state.app_state.revoke_client(drop_client.id).unwrap();

            assert_eq!(
                status_of(
                    router_of(&state),
                    get_with("/api/settings", &[(TOKEN_HEADER, &drop_cred)])
                )
                .await,
                StatusCode::UNAUTHORIZED,
            );
            assert_eq!(
                status_of(
                    router_of(&state),
                    get_with("/api/settings", &[(TOKEN_HEADER, &keep)])
                )
                .await,
                StatusCode::OK,
            );
        }

        #[tokio::test]
        async fn a_reissue_kills_the_previous_credential_immediately() {
            let state = enabled_state();
            let (client, first) = state.app_state.register_client("rotator").unwrap();
            let (_, second) = state.app_state.reissue_client(client.id).unwrap();

            assert_eq!(
                status_of(
                    router_of(&state),
                    get_with("/api/settings", &[(TOKEN_HEADER, &first)])
                )
                .await,
                StatusCode::UNAUTHORIZED,
            );
            assert_eq!(
                status_of(
                    router_of(&state),
                    get_with("/api/settings", &[(TOKEN_HEADER, &second)])
                )
                .await,
                StatusCode::OK,
            );
        }

        #[tokio::test]
        async fn a_cors_preflight_is_never_blocked_by_the_auth_check() {
            let state = enabled_state();
            let req = Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/settings")
                .header("origin", "http://localhost:1420")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap();
            assert_ne!(
                status_of(router_of(&state), req).await,
                StatusCode::UNAUTHORIZED,
            );
        }

        /// The dev server is a different origin from the API, so the browser
        /// preflights every call that carries the credential header. If the
        /// header is not allowlisted the preflight fails and `npm run dev`
        /// cannot authenticate at all.
        #[tokio::test]
        async fn the_preflight_allows_the_credential_header() {
            let state = enabled_state();
            let req = Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/settings/dir")
                .header("origin", "http://localhost:1420")
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    format!("content-type,{TOKEN_HEADER}"),
                )
                .body(Body::empty())
                .unwrap();

            let resp = router_of(&state).oneshot(req).await.unwrap();
            let allowed = resp
                .headers()
                .get("access-control-allow-headers")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();

            assert!(allowed.contains(TOKEN_HEADER), "allowed headers: {allowed}");
            assert!(
                allowed.contains("content-type"),
                "allowed headers: {allowed}"
            );
        }

        /// A cross-origin caller authenticates with the header or `?token=`
        /// carrier, never the cookie, so the API must not invite credentialed
        /// cross-origin requests.
        #[tokio::test]
        async fn the_preflight_does_not_allow_credentials() {
            let state = enabled_state();
            let req = Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/settings")
                .header("origin", "http://localhost:1420")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap();

            let resp = router_of(&state).oneshot(req).await.unwrap();
            assert!(resp
                .headers()
                .get("access-control-allow-credentials")
                .is_none());
        }

        #[tokio::test]
        async fn an_unknown_path_is_404_not_401() {
            let state = enabled_state();
            let status = status_of(router_of(&state), get_with("/nope", &[])).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn verification_off_lets_every_request_through_without_an_identity() {
            let state = disabled_state();
            let json = json_of(router_of(&state), get_with("/api/whoami", &[])).await;
            assert_eq!(json["auth_enabled"], false);
            assert!(json["client"].is_null());
        }

        #[tokio::test]
        async fn settings_report_the_auth_mode_and_clients_but_never_a_credential() {
            let state = enabled_state();
            let cred = state.app_state.web_ui_credential().unwrap();
            let json = json_of(
                router_of(&state),
                get_with("/api/settings", &[(TOKEN_HEADER, &cred)]),
            )
            .await;

            assert_eq!(json["api_auth_enabled"], true);
            assert_eq!(json["api_auth_source"], "ephemeral");
            assert_eq!(json["clients"][0]["name"], WEB_UI);
            assert!(
                !json.to_string().contains(&cred),
                "the settings payload must never carry a credential"
            );
        }
    }
}
