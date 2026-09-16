//! Client verification for the local HTTP API.
//!
//! Every `/api/*` route requires the caller to present a signed credential that
//! identifies it as a registered, non-revoked *client* (see `crate::clients`).
//! Without this, any local process — or, when Docker binds `0.0.0.0`, any LAN
//! host — could read session transcripts or rewrite `settings.json`; and with a
//! single shared secret nobody could tell clients apart or revoke just one.
//!
//! Resolution at startup (see [`resolve_auth`]):
//!
//! 1. `CODEXTRACE_API_AUTH=off` disables verification entirely (loud warning).
//! 2. Otherwise the HS256 signing key lives in `<config root>/api-secret`
//!    (mode `0600`), created on first run; the registry in `clients.json`; and
//!    the built-in clients' credentials in `clients/<name>.jwt`, rewritten
//!    whenever they are missing or no longer valid (reissue, new key).
//! 3. If the config dir is unusable the server runs on an in-memory key
//!    (`ephemeral`): still fail-closed, but only the same-origin web UI (which
//!    receives its credential from this very process) can authenticate.
//!
//! Accepted carriers on a request, in this order: `X-CodexTrace-Token` header,
//! `Authorization: Bearer`, `?token=` query (browser `EventSource` cannot set
//! headers), and the `codextrace_token` cookie the server itself sets on the
//! Docker same-origin UI (see [`attach_credential_cookie`]).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::Response;
use rand::Rng;
use serde::Serialize;
use uuid::Uuid;

use crate::clients::{self, Client, ClientRegistry};
use crate::http_api::{err_response, HttpState};
use crate::jwt::{self, Claims};

/// Request header carrying the credential (primary carrier for the web UI).
pub const TOKEN_HEADER: &str = "x-codextrace-token";
/// Cookie the server sets for the same-origin (Docker) browser UI.
pub const TOKEN_COOKIE: &str = "codextrace_token";
/// Query parameter carrier, needed by browser `EventSource` for `/api/events`.
pub const TOKEN_QUERY: &str = "token";
/// Env var that disables verification when set to `off`.
pub const ENV_AUTH: &str = "CODEXTRACE_API_AUTH";

/// Where the signing key came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// `<config root>/api-secret`.
    File,
    /// In-memory only: the config dir could not be used. Credentials minted
    /// this run die with the process, and the dev server cannot read them.
    Ephemeral,
}

/// Live verification mode, read on every request.
#[derive(Clone, PartialEq, Eq)]
pub enum AuthMode {
    /// `CODEXTRACE_API_AUTH=off`: every request is accepted, no identity attached.
    Disabled,
    Enabled {
        key: Vec<u8>,
        source: KeySource,
    },
}

/// Never print the signing key, even at debug level.
impl std::fmt::Debug for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthMode::Disabled => f.write_str("Disabled"),
            AuthMode::Enabled { key, source } => f
                .debug_struct("Enabled")
                .field("key", &format_args!("<redacted {} bytes>", key.len()))
                .field("source", source)
                .finish(),
        }
    }
}

impl AuthMode {
    pub fn is_enabled(&self) -> bool {
        matches!(self, AuthMode::Enabled { .. })
    }

    pub fn key(&self) -> Option<&[u8]> {
        match self {
            AuthMode::Disabled => None,
            AuthMode::Enabled { key, .. } => Some(key),
        }
    }

    /// Stable string for the frontend: `"disabled"`, `"file"`, or `"ephemeral"`.
    pub fn source(&self) -> &'static str {
        match self {
            AuthMode::Disabled => "disabled",
            AuthMode::Enabled {
                source: KeySource::File,
                ..
            } => "file",
            AuthMode::Enabled {
                source: KeySource::Ephemeral,
                ..
            } => "ephemeral",
        }
    }
}

/// The authenticated caller, attached to the request extensions by
/// [`require_client`] so handlers (e.g. `/api/whoami`) can tell who is asking.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClientIdentity {
    pub id: Uuid,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

pub fn secret_path(root: &Path) -> PathBuf {
    root.join("api-secret")
}

pub fn registry_path(root: &Path) -> PathBuf {
    root.join("clients.json")
}

/// `<config root>/clients/<name>.jwt` — where a built-in client's credential is
/// kept so the Vite dev server (`web-ui`) can read it.
pub fn builtin_credential_path(root: &Path, name: &str) -> PathBuf {
    root.join("clients").join(format!("{name}.jwt"))
}

/// 32 random bytes as 64 lowercase hex chars.
pub fn generate_secret_hex() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    // Byte-offset slicing below is only sound on ASCII; anything else is not
    // hex anyway (and must not panic at startup).
    if !hex.is_ascii() || hex.len() % 2 != 0 || hex.is_empty() {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Trimmed contents of a small secret file, or `None` when missing or empty.
pub(crate) fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn open_options_0600(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts
}

const EMPTY_FILE_RETRIES: u32 = 5;
const EMPTY_FILE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

/// Re-read a file another creator has just made, tolerating the moment between
/// its `create_new` and its `write_all`.
fn read_trimmed_with_retry(path: &Path) -> Option<String> {
    for attempt in 0..=EMPTY_FILE_RETRIES {
        if let Some(t) = read_trimmed(path) {
            return Some(t);
        }
        if attempt < EMPTY_FILE_RETRIES {
            std::thread::sleep(EMPTY_FILE_RETRY_DELAY);
        }
    }
    None
}

/// Read the signing key file, or create it with a fresh key if it does not
/// exist. `create_new` (O_EXCL) makes two backends racing on first run converge
/// on one key: the loser re-reads what the winner wrote.
pub(crate) fn load_or_create_secret(path: &Path) -> Result<String, String> {
    if let Some(existing) = read_trimmed(path) {
        return Ok(existing);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    match open_options_0600(&mut opts).open(path) {
        Ok(mut f) => {
            let secret = generate_secret_hex();
            f.write_all(secret.as_bytes())
                .and_then(|()| f.write_all(b"\n"))
                .map_err(|e| e.to_string())?;
            Ok(secret)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read_trimmed_with_retry(path)
            .ok_or_else(|| format!("{} exists but is empty", path.display())),
        Err(e) => Err(format!("cannot create {}: {e}", path.display())),
    }
}

/// Write `bytes` to `path` via a sibling temp file (created `0600`) + rename, so
/// concurrent readers see either the old contents or the new, never a truncated
/// file. Used for the registry and the built-in credential files.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("secret");
    let tmp = parent.join(format!("{file_name}.{}.tmp", &generate_secret_hex()[..8]));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    let mut write = || -> Result<(), String> {
        let mut f = open_options_0600(&mut opts)
            .open(&tmp)
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        f.write_all(bytes)
            .and_then(|()| f.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Startup resolution
// ---------------------------------------------------------------------------

/// Everything `AppState` needs to enforce client verification.
#[derive(Clone)]
pub struct ResolvedAuth {
    pub mode: AuthMode,
    pub registry: ClientRegistry,
    /// The `web-ui` client's current credential (issued as the same-origin
    /// cookie), or `None` when disabled / revoked.
    pub web_ui_credential: Option<String>,
    /// `Some` when the registry and built-in credentials are persisted.
    pub config_root: Option<PathBuf>,
    /// Non-fatal problems worth logging.
    pub warnings: Vec<String>,
}

/// Mint a credential for `client` at `iat`.
pub fn issue_credential(client: &Client, key: &[u8], iat: i64) -> String {
    jwt::sign(
        &Claims {
            sub: client.id.to_string(),
            name: client.name.clone(),
            iat,
        },
        key,
    )
}

/// Does `credential` (typically read from `clients/<name>.jwt`) still
/// authenticate `client` under `key`?
fn credential_is_current(credential: &str, client: &Client, key: &[u8]) -> bool {
    jwt::verify(credential, key).is_ok_and(|c| {
        c.sub == client.id.to_string() && c.iat >= client.issued_at && !client.is_revoked()
    })
}

/// Make sure every built-in client has a valid credential file under `root`,
/// (re)writing files that are missing or stale. Returns the `web-ui` credential
/// when that client is active.
fn ensure_builtin_credentials(
    root: &Path,
    registry: &ClientRegistry,
    key: &[u8],
    warnings: &mut Vec<String>,
) -> Option<String> {
    let mut web_ui = None;
    for client in registry.clients.iter().filter(|c| c.builtin) {
        if client.is_revoked() {
            continue;
        }
        let path = builtin_credential_path(root, &client.name);
        let credential = match read_trimmed(&path) {
            Some(existing) if credential_is_current(&existing, client, key) => existing,
            _ => {
                let fresh = issue_credential(client, key, clients::now().max(client.issued_at));
                if let Err(e) = write_private_atomic(&path, format!("{fresh}\n").as_bytes()) {
                    warnings.push(format!("could not write {}: {e}", path.display()));
                }
                fresh
            }
        };
        if client.name == clients::WEB_UI {
            web_ui = Some(credential);
        }
    }
    web_ui
}

fn ephemeral_key() -> Vec<u8> {
    hex_to_bytes(&generate_secret_hex()).unwrap_or_default()
}

/// Pure core of [`resolve_auth`]: `env_auth` is the raw `CODEXTRACE_API_AUTH`
/// value, `root` the config directory (or `None` when there is none).
pub fn resolve_auth_from(env_auth: Option<String>, root: Option<&Path>) -> ResolvedAuth {
    let mut warnings = Vec::new();
    if env_auth
        .as_deref()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("off"))
    {
        return ResolvedAuth {
            mode: AuthMode::Disabled,
            registry: ClientRegistry::default(),
            web_ui_credential: None,
            config_root: root.map(Path::to_path_buf),
            warnings,
        };
    }

    // Signing key: file-backed when possible, ephemeral otherwise (fail closed).
    let (key, source) = match root {
        Some(root) => match load_or_create_secret(&secret_path(root)) {
            Ok(hex) => match hex_to_bytes(&hex) {
                Some(bytes) if bytes.len() >= 16 => (bytes, KeySource::File),
                _ => {
                    warnings.push(format!(
                        "{} is not a hex key; using a one-off key for this run",
                        secret_path(root).display()
                    ));
                    (ephemeral_key(), KeySource::Ephemeral)
                }
            },
            Err(e) => {
                warnings.push(format!("could not persist the signing key ({e})"));
                (ephemeral_key(), KeySource::Ephemeral)
            }
        },
        None => {
            warnings.push("no config directory available".into());
            (ephemeral_key(), KeySource::Ephemeral)
        }
    };

    // Registry + built-ins.
    let persisted_root = root.filter(|_| source == KeySource::File);
    let mut registry = match persisted_root {
        Some(root) => ClientRegistry::load(&registry_path(root)).unwrap_or_else(|e| {
            // Never overwrite a registry we could not read: move it aside so the
            // user's clients can be recovered, then start fresh (every
            // previously issued credential stops working — say so loudly).
            let path = registry_path(root);
            let backup = path.with_file_name(format!("clients.json.corrupt-{}", clients::now()));
            match fs::rename(&path, &backup) {
                Ok(()) => warnings.push(format!(
                    "{e}; moved it to {} and started a fresh client registry — every previously \
                     issued credential is now invalid",
                    backup.display()
                )),
                Err(re) => warnings.push(format!(
                    "{e}; could not move it aside ({re}); starting a fresh client registry — \
                     every previously issued credential is now invalid"
                )),
            }
            ClientRegistry::default()
        }),
        None => ClientRegistry::default(),
    };
    if registry.ensure_builtins(clients::now()) {
        if let Some(root) = persisted_root {
            if let Err(e) = registry.save(&registry_path(root)) {
                warnings.push(format!("could not save the client registry: {e}"));
            }
        }
    }

    let web_ui_credential = match persisted_root {
        Some(root) => ensure_builtin_credentials(root, &registry, &key, &mut warnings),
        None => registry
            .find_by_name(clients::WEB_UI)
            .filter(|c| !c.is_revoked())
            .map(|c| issue_credential(c, &key, clients::now())),
    };

    ResolvedAuth {
        mode: AuthMode::Enabled { key, source },
        registry,
        web_ui_credential,
        config_root: persisted_root.map(Path::to_path_buf),
        warnings,
    }
}

/// Resolve the live verification setup from the environment and the config dir,
/// logging where things live (never any secret).
pub fn resolve_auth() -> ResolvedAuth {
    let root = crate::settings::config_root();
    let resolved = resolve_auth_from(std::env::var(ENV_AUTH).ok(), root.as_deref());
    for w in &resolved.warnings {
        eprintln!("HTTP API: WARNING — {w}");
    }
    match &resolved.mode {
        AuthMode::Disabled => eprintln!(
            "HTTP API: WARNING — client verification is DISABLED ({ENV_AUTH}=off). \
             Every local process can call the API."
        ),
        AuthMode::Enabled {
            source: KeySource::File,
            ..
        } => {
            if let Some(root) = &resolved.config_root {
                eprintln!(
                    "HTTP API: client verification on ({} clients registered; config: {})",
                    resolved.registry.clients.len(),
                    root.display()
                );
            }
        }
        AuthMode::Enabled {
            source: KeySource::Ephemeral,
            ..
        } => eprintln!(
            "HTTP API: client verification on with a ONE-OFF key: the config directory is \
             unusable, so only this process's own web UI can authenticate. Fix the config \
             directory and restart."
        ),
    }
    resolved
}

// ---------------------------------------------------------------------------
// Request-side carriers
// ---------------------------------------------------------------------------

/// Decode `%XX` escapes (as produced by `encodeURIComponent`). A `+` is left
/// literal. Malformed escapes are passed through unchanged.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Work on bytes: slicing the `&str` at byte offsets would panic on a
        // multi-byte character straddling the window.
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Value of `name` in a raw query string, percent-decoded.
fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| percent_decode(v))
}

/// Value of `name` in a `Cookie:` header (`a=1; b=2`).
fn cookie_value(header: &str, name: &str) -> Option<String> {
    header
        .split(';')
        .map(str::trim)
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_string())
}

/// Every credential candidate the client presented, in precedence order.
fn presented_credentials(req: &Request) -> Vec<String> {
    let mut out = Vec::new();
    let headers = req.headers();
    if let Some(v) = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) {
        out.push(v.trim().to_string());
    }
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
        {
            out.push(rest.trim().to_string());
        }
    }
    if let Some(q) = req.uri().query() {
        out.extend(query_param(q, TOKEN_QUERY));
    }
    if cookie_carrier_allowed(headers) {
        if let Some(c) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
            out.extend(cookie_value(c, TOKEN_COOKIE));
        }
    }
    out.retain(|t| !t.is_empty());
    out
}

/// The cookie is only meant for the same-origin UI. `SameSite=Strict` already
/// keeps cross-*site* pages from riding on it, but a page on another port of the
/// same host is same-*site*: a `<form method=POST>` auto-submitted from
/// `localhost:<other>` would carry the cookie to a body-less mutating route such
/// as `/api/clients/{id}/revoke` (a simple request, so CORS never blocks it).
/// Browsers label such requests `Sec-Fetch-Site: same-site` (or `cross-site`);
/// only `same-origin` / `none` (typed URL) — or no header at all, for
/// non-browser and older clients — may use the cookie carrier.
fn cookie_carrier_allowed(headers: &HeaderMap) -> bool {
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(site) => matches!(site.trim(), "same-origin" | "none"),
    }
}

/// axum middleware: reject `/api/*` requests that don't carry a valid credential
/// of a registered, non-revoked client. Attaches the caller's [`ClientIdentity`]
/// to the request on success. `OPTIONS` always passes so CORS preflights (sent
/// without custom headers) are never blocked.
pub async fn require_client(
    State(state): State<Arc<HttpState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
    let key = match state.app_state.auth.read() {
        Ok(guard) => guard.key().map(<[u8]>::to_vec),
        Err(_) => {
            return err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth state unavailable".to_string(),
            )
        }
    };
    let Some(key) = key else {
        return next.run(req).await; // disabled
    };
    for presented in presented_credentials(&req) {
        let Ok(claims) = jwt::verify(&presented, &key) else {
            continue;
        };
        if let Some(identity) = state.app_state.authenticate(&claims) {
            req.extensions_mut().insert(identity);
            return next.run(req).await;
        }
    }
    err_response(
        StatusCode::UNAUTHORIZED,
        "invalid or revoked client credential — register a client (or reissue this one) in \
         Settings > Accepted clients and send its credential as an X-CodexTrace-Token header or \
         Authorization: Bearer"
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// Same-origin cookie for the Docker static UI
// ---------------------------------------------------------------------------

/// Host header without its port; IPv6 literals lose their brackets too.
fn strip_port(host: &str) -> &str {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match host.rsplit_once(':') {
        // `a:b:c` with more than one colon is a bare IPv6 literal, not host:port.
        Some((h, _)) if !h.contains(':') => h,
        _ => host,
    }
}

/// Host component of an `http(s)://host[:port]` origin string.
fn host_of_origin(origin: &str) -> Option<String> {
    let uri: Uri = origin.parse().ok()?;
    uri.host().map(|h| h.trim_matches(['[', ']']).to_string())
}

/// Is this request `Host` one we're willing to hand the web UI's credential to?
///
/// Loopback names always qualify. Anything else must match the host part of an
/// allowlisted CORS origin (defaults + `CODEXTRACE_ALLOWED_ORIGINS` + Settings
/// UI). Without this check, a DNS-rebinding page pointed at a `0.0.0.0`-bound
/// Docker port would be issued the cookie and become a fully authenticated
/// same-origin client.
pub fn host_allowed(host: &str, origins: &[String]) -> bool {
    let h = strip_port(host);
    if h.is_empty() {
        return false;
    }
    if h.eq_ignore_ascii_case("localhost") || h == "127.0.0.1" || h == "::1" {
        return true;
    }
    origins
        .iter()
        .filter_map(|o| host_of_origin(o))
        .any(|oh| oh.eq_ignore_ascii_case(h))
}

/// `Set-Cookie` value carrying the web UI's credential. `HttpOnly` keeps page
/// scripts from reading it; `SameSite=Strict` keeps cross-site pages from riding
/// on it. No `Secure` flag: plain `http://` on localhost is the normal
/// deployment.
pub fn credential_cookie_header(credential: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!(
        "{TOKEN_COOKIE}={credential}; Path=/; HttpOnly; SameSite=Strict"
    ))
    .ok()
}

/// Static (defaults + env) ∪ live (Settings UI) CORS origins — the same union
/// `http_api::build_cors` checks.
fn allowed_origins(state: &HttpState) -> Vec<String> {
    let mut origins = crate::http_api::resolve_allowed_origins();
    if let Ok(g) = state.app_state.settings.lock() {
        origins.extend(g.allowed_origins.iter().cloned());
    }
    origins
}

fn is_html(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"))
}

/// Does this request path resolve to the SPA shell? `/` and any other directory,
/// which `ServeDir` answers with that directory's `index.html`, plus an explicit
/// `.html` request. A `304` has no `Content-Type` to recognise the shell by (see
/// [`is_shell_response`]), so its path is all we have.
fn is_shell_path(path: &str) -> bool {
    path.ends_with('/') || path.ends_with(".html")
}

/// Is this the SPA shell — the one response that carries the credential?
///
/// A `200` is identified by its content type. A `304 Not Modified` carries no
/// body and no `Content-Type` (it is only headers), so a revalidated shell is
/// identified by its request path instead. Missing this case would leave a
/// browser that had cached `index.html` with no credential at all, because the
/// cookie is session-scoped and only ever rides on the shell.
fn is_shell_response(resp: &Response, shell_path: bool) -> bool {
    is_html(resp) || (resp.status() == StatusCode::NOT_MODIFIED && shell_path)
}

/// axum middleware for the static-asset fallback: attach the `web-ui` client's
/// credential as a cookie to the SPA shell when the request `Host` is
/// allowlisted, so the Docker same-origin UI authenticates with zero frontend
/// code. No cookie when `web-ui` is revoked or verification is off.
///
/// The shell is also marked `no-cache`, because the credential rides on it: a
/// browser that serves the shell straight out of its cache never asks the server
/// anything, so it never receives a cookie either. `no-cache` still allows
/// storing and revalidating (a `304` keeps the transfer cheap and now carries
/// the cookie too) — it only forbids using the stored copy blind.
pub async fn attach_credential_cookie(
    State(state): State<Arc<HttpState>>,
    req: Request,
    next: Next,
) -> Response {
    // Only the shell can receive the cookie, so defer the allowlist work until
    // the response is known — asset requests skip it entirely.
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let shell_path = is_shell_path(req.uri().path());
    let mut resp = next.run(req).await;
    if !is_shell_response(&resp, shell_path) {
        return resp;
    }
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if host.is_some_and(|h| host_allowed(&h, &allowed_origins(&state))) {
        if let Some(cookie) = state
            .app_state
            .web_ui_credential()
            .as_deref()
            .and_then(credential_cookie_header)
        {
            resp.headers_mut().append(header::SET_COOKIE, cookie);
        }
    }
    resp
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn tmp_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // -- decoders ------------------------------------------------------------

    #[test]
    fn non_ascii_input_never_panics_the_decoders() {
        assert_eq!(hex_to_bytes("aéb"), None);
        assert_eq!(hex_to_bytes("éé"), None);
        assert_eq!(percent_decode("%aé"), "%aé");
        assert_eq!(percent_decode("é%41"), "éA");
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("%2"), "%2");
    }

    #[test]
    fn hex_to_bytes_round_trips_a_generated_secret() {
        let hex = generate_secret_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(hex_to_bytes(&hex).unwrap().len(), 32);
    }

    #[test]
    fn hex_to_bytes_rejects_odd_length_and_empty() {
        assert_eq!(hex_to_bytes(""), None);
        assert_eq!(hex_to_bytes("abc"), None);
        assert_eq!(hex_to_bytes("zz"), None);
    }

    #[test]
    fn percent_decode_handles_an_encoded_credential() {
        let token = "aaa.bbb.cc-_d";
        assert_eq!(percent_decode(token), token);
        assert_eq!(percent_decode("a%2Bb"), "a+b");
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let mode = AuthMode::Enabled {
            key: KEY.to_vec(),
            source: KeySource::File,
        };
        let shown = format!("{mode:?}");
        assert!(
            !shown.contains(std::str::from_utf8(KEY).unwrap()),
            "{shown}"
        );
        assert!(shown.contains("redacted"), "{shown}");
    }

    // -- key file ------------------------------------------------------------

    #[test]
    fn secret_is_created_once_and_reused() {
        let root = tmp_root();
        let path = secret_path(root.path());
        let first = load_or_create_secret(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(load_or_create_secret(&path).unwrap(), first, "reused");
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp_root();
        let path = secret_path(root.path());
        load_or_create_secret(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    #[test]
    fn write_private_atomic_replaces_and_leaves_no_temp_files() {
        let root = tmp_root();
        let path = root.path().join("clients.json");
        write_private_atomic(&path, b"first").unwrap();
        write_private_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        let strays: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "left a temp file behind");
    }

    // -- resolution ----------------------------------------------------------

    #[test]
    fn auth_off_disables_verification() {
        let root = tmp_root();
        let resolved = resolve_auth_from(Some("off".into()), Some(root.path()));
        assert!(!resolved.mode.is_enabled());
        assert_eq!(resolved.mode.source(), "disabled");
        assert!(resolved.web_ui_credential.is_none());
        assert!(
            !secret_path(root.path()).exists(),
            "no key is created when verification is off"
        );
    }

    #[test]
    fn auth_off_is_case_and_whitespace_insensitive() {
        let root = tmp_root();
        assert!(!resolve_auth_from(Some("  OFF ".into()), Some(root.path()))
            .mode
            .is_enabled());
    }

    #[test]
    fn any_other_auth_value_leaves_verification_on() {
        let root = tmp_root();
        for v in ["on", "", "yes", "0", "false"] {
            assert!(
                resolve_auth_from(Some(v.into()), Some(root.path()))
                    .mode
                    .is_enabled(),
                "{v:?} must not disable verification"
            );
        }
    }

    #[test]
    fn first_run_creates_the_key_registry_and_builtin_credential() {
        let root = tmp_root();
        let resolved = resolve_auth_from(None, Some(root.path()));

        assert_eq!(resolved.mode.source(), "file");
        assert!(secret_path(root.path()).exists());
        assert!(registry_path(root.path()).exists());
        assert_eq!(resolved.registry.clients.len(), 1);
        let cred_path = builtin_credential_path(root.path(), clients::WEB_UI);
        assert!(cred_path.exists());
        assert_eq!(
            resolved.web_ui_credential.as_deref(),
            read_trimmed(&cred_path).as_deref(),
        );
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    }

    #[test]
    fn the_builtin_credential_verifies_against_the_resolved_key() {
        let root = tmp_root();
        let resolved = resolve_auth_from(None, Some(root.path()));
        let key = resolved.mode.key().unwrap();
        let claims = jwt::verify(resolved.web_ui_credential.as_deref().unwrap(), key).unwrap();
        assert_eq!(claims.name, clients::WEB_UI);
        let client = resolved.registry.find_by_name(clients::WEB_UI).unwrap();
        assert_eq!(claims.sub, client.id.to_string());
        assert!(claims.iat >= client.issued_at);
    }

    #[test]
    fn a_second_start_reuses_the_same_key_and_credential() {
        let root = tmp_root();
        let first = resolve_auth_from(None, Some(root.path()));
        let second = resolve_auth_from(None, Some(root.path()));
        assert_eq!(first.mode.key(), second.mode.key());
        assert_eq!(first.web_ui_credential, second.web_ui_credential);
        assert_eq!(first.registry, second.registry);
    }

    #[test]
    fn a_stale_credential_file_is_rewritten() {
        let root = tmp_root();
        resolve_auth_from(None, Some(root.path()));
        let cred_path = builtin_credential_path(root.path(), clients::WEB_UI);
        fs::write(&cred_path, "not.a.credential\n").unwrap();

        let again = resolve_auth_from(None, Some(root.path()));
        let rewritten = read_trimmed(&cred_path).unwrap();
        assert_ne!(rewritten, "not.a.credential");
        assert_eq!(again.web_ui_credential.as_deref(), Some(rewritten.as_str()));
    }

    #[test]
    fn a_credential_issued_before_a_reissue_is_rewritten() {
        let root = tmp_root();
        let first = resolve_auth_from(None, Some(root.path()));
        let stale = first.web_ui_credential.clone().unwrap();

        // Simulate a reissue recorded by another process.
        let mut registry = first.registry.clone();
        let id = registry.find_by_name(clients::WEB_UI).unwrap().id;
        registry.reissue(id, clients::now() + 60).unwrap();
        registry.save(&registry_path(root.path())).unwrap();

        let again = resolve_auth_from(None, Some(root.path()));
        assert_ne!(again.web_ui_credential.as_deref(), Some(stale.as_str()));
    }

    #[test]
    fn a_non_hex_key_file_falls_back_to_an_ephemeral_key() {
        let root = tmp_root();
        fs::write(secret_path(root.path()), "definitely not hex").unwrap();
        let resolved = resolve_auth_from(None, Some(root.path()));
        assert_eq!(resolved.mode.source(), "ephemeral");
        assert!(resolved.config_root.is_none(), "nothing is persisted");
        assert!(resolved.web_ui_credential.is_some(), "still fail-closed");
        assert!(resolved.warnings.iter().any(|w| w.contains("hex")));
    }

    #[test]
    fn no_config_dir_falls_back_to_an_ephemeral_key() {
        let resolved = resolve_auth_from(None, None);
        assert_eq!(resolved.mode.source(), "ephemeral");
        assert!(resolved.mode.is_enabled(), "fail closed, never open");
        assert!(resolved.web_ui_credential.is_some());
    }

    #[test]
    fn corrupt_registry_is_moved_aside_not_overwritten() {
        let root = tmp_root();
        let path = registry_path(root.path());
        fs::write(&path, "{ definitely not json").unwrap();

        let resolved = resolve_auth_from(None, Some(root.path()));
        assert!(resolved.mode.is_enabled());
        assert_eq!(resolved.registry.clients.len(), 1, "fresh built-ins");
        assert!(
            resolved.warnings.iter().any(|w| w.contains("moved it to")),
            "{:?}",
            resolved.warnings
        );
        let saved: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("clients.json.corrupt-")
            })
            .collect();
        assert_eq!(saved.len(), 1, "the original is preserved for recovery");
    }

    #[test]
    fn a_revoked_web_ui_gets_no_credential() {
        let root = tmp_root();
        let first = resolve_auth_from(None, Some(root.path()));
        let mut registry = first.registry.clone();
        let id = registry.find_by_name(clients::WEB_UI).unwrap().id;
        registry.revoke(id, clients::now()).unwrap();
        registry.save(&registry_path(root.path())).unwrap();

        let again = resolve_auth_from(None, Some(root.path()));
        assert!(again.web_ui_credential.is_none());
    }

    // -- carriers ------------------------------------------------------------

    fn req_with(headers: Vec<(&str, &str)>, uri: &str) -> Request {
        let mut b = Request::builder().uri(uri);
        for (k, v) in headers {
            b = b.header(k, v);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn credentials_are_read_from_every_carrier_in_order() {
        let req = req_with(
            vec![
                (TOKEN_HEADER, "from-header"),
                ("authorization", "Bearer from-bearer"),
                ("cookie", &format!("{TOKEN_COOKIE}=from-cookie; other=x")),
            ],
            "/api/events?token=from-query",
        );
        assert_eq!(
            presented_credentials(&req),
            vec!["from-header", "from-bearer", "from-query", "from-cookie"],
        );
    }

    #[test]
    fn a_lowercase_bearer_prefix_is_accepted() {
        let req = req_with(vec![("authorization", "bearer x")], "/api/settings");
        assert_eq!(presented_credentials(&req), vec!["x"]);
    }

    #[test]
    fn a_non_bearer_authorization_scheme_is_ignored() {
        let req = req_with(vec![("authorization", "Basic abc")], "/api/settings");
        assert!(presented_credentials(&req).is_empty());
    }

    #[test]
    fn blank_carriers_are_dropped() {
        let req = req_with(
            vec![(TOKEN_HEADER, "   "), ("authorization", "Bearer   ")],
            "/api/settings?token=",
        );
        assert!(presented_credentials(&req).is_empty());
    }

    #[test]
    fn the_query_carrier_is_percent_decoded() {
        let req = req_with(vec![], "/api/events?token=a%2Bb&x=1");
        assert_eq!(presented_credentials(&req), vec!["a+b"]);
    }

    #[test]
    fn the_cookie_is_ignored_for_cross_site_and_same_site_requests() {
        for site in ["cross-site", "same-site"] {
            let req = req_with(
                vec![
                    ("cookie", &format!("{TOKEN_COOKIE}=c")),
                    ("sec-fetch-site", site),
                ],
                "/api/settings",
            );
            assert!(
                presented_credentials(&req).is_empty(),
                "{site} must not use the cookie carrier"
            );
        }
    }

    #[test]
    fn the_cookie_is_used_for_same_origin_none_and_absent_sec_fetch_site() {
        for site in [Some("same-origin"), Some("none"), None] {
            let mut headers = vec![("cookie", format!("{TOKEN_COOKIE}=c"))];
            if let Some(s) = site {
                headers.push(("sec-fetch-site", s.to_string()));
            }
            let req = req_with(
                headers.iter().map(|(k, v)| (*k, v.as_str())).collect(),
                "/api/settings",
            );
            assert_eq!(presented_credentials(&req), vec!["c"], "{site:?}");
        }
    }

    // -- cookie issuance -----------------------------------------------------

    #[test]
    fn loopback_hosts_always_receive_the_cookie() {
        for host in ["localhost", "localhost:1422", "127.0.0.1:1422", "[::1]:80"] {
            assert!(host_allowed(host, &[]), "{host}");
        }
    }

    #[test]
    fn an_unlisted_host_does_not_receive_the_cookie() {
        assert!(!host_allowed("trace.example:1422", &[]));
        assert!(!host_allowed("", &[]));
    }

    #[test]
    fn a_host_matching_an_allowlisted_origin_receives_the_cookie() {
        let origins = vec!["https://trace.example".to_string()];
        assert!(host_allowed("trace.example:1422", &origins));
        assert!(host_allowed("TRACE.EXAMPLE", &origins));
        assert!(!host_allowed("other.example", &origins));
    }

    #[test]
    fn strip_port_handles_ipv6_and_bare_hosts() {
        assert_eq!(strip_port("[::1]:8080"), "::1");
        assert_eq!(strip_port("::1"), "::1");
        assert_eq!(strip_port("host:80"), "host");
        assert_eq!(strip_port("host"), "host");
    }

    #[test]
    fn the_cookie_is_httponly_and_samesite_strict() {
        let value = credential_cookie_header("abc").unwrap();
        let text = value.to_str().unwrap();
        assert!(text.starts_with(&format!("{TOKEN_COOKIE}=abc;")), "{text}");
        assert!(text.contains("HttpOnly"), "{text}");
        assert!(text.contains("SameSite=Strict"), "{text}");
    }

    #[test]
    fn a_credential_with_header_breaking_characters_yields_no_cookie() {
        assert!(credential_cookie_header("bad\nvalue").is_none());
    }

    #[test]
    fn shell_paths_are_recognised_for_a_304() {
        assert!(is_shell_path("/"));
        assert!(is_shell_path("/index.html"));
        assert!(is_shell_path("/sub/"));
        assert!(!is_shell_path("/assets/app.js"));
    }

    #[test]
    fn a_304_on_a_shell_path_still_counts_as_the_shell() {
        let resp = Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(is_shell_response(&resp, true));
        assert!(!is_shell_response(&resp, false), "assets are untouched");
    }

    #[test]
    fn a_200_is_recognised_as_the_shell_by_content_type() {
        let html = Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(is_shell_response(&html, false));

        let js = Response::builder()
            .header(header::CONTENT_TYPE, "text/javascript")
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(!is_shell_response(&js, false));
    }
}
