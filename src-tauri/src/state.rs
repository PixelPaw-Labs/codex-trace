use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::auth::{AuthMode, ClientIdentity, ResolvedAuth};
use crate::clients::{self, Client, ClientRegistry};
use crate::jwt::Claims;
use crate::parser::discover::CodexSessionInfo;
use crate::settings::Settings;
use crate::watcher::WatcherHandle;

/// A Server-Sent Event destined for browser clients.
#[derive(Clone, Debug)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

struct SessionsCache {
    dir: String,
    cached_at: Instant,
    sessions: Vec<CodexSessionInfo>,
}

const SESSIONS_CACHE_TTL: Duration = Duration::from_secs(2);

/// The on-disk registry, when it is safe to adopt at runtime: the file must
/// exist, parse, and still know every built-in client. A missing or emptied
/// `clients.json` (someone "resetting" while the server runs) or a half-edited
/// one must not lock every client out; startup handles those cases instead.
fn load_adoptable_registry(root: &Path) -> Option<ClientRegistry> {
    let path = crate::auth::registry_path(root);
    if !path.exists() {
        return None;
    }
    let on_disk = ClientRegistry::load(&path).ok()?;
    clients::BUILTIN_NAMES
        .iter()
        .all(|name| on_disk.find_by_name(name).is_some())
        .then_some(on_disk)
}

pub struct AppState {
    pub session_watcher: Mutex<Option<WatcherHandle>>,
    pub picker_watcher: Mutex<Option<WatcherHandle>>,
    pub settings: Mutex<Settings>,
    /// Live client-verification mode (see `crate::auth`), read on every request.
    pub auth: RwLock<AuthMode>,
    clients: RwLock<ClientRegistry>,
    web_ui_credential: RwLock<Option<String>>,
    config_root: RwLock<Option<PathBuf>>,
    pub watched_session_ongoing: Mutex<Option<(String, bool)>>,
    pub event_tx: broadcast::Sender<SseEvent>,
    sessions_cache: Mutex<Option<SessionsCache>>,
}

impl AppState {
    /// Everything auth-related is injected (see `crate::auth::resolve_auth`) so
    /// construction itself never touches the filesystem.
    pub fn new(resolved: ResolvedAuth) -> Self {
        let (event_tx, _) = broadcast::channel(64);
        Self {
            session_watcher: Mutex::new(None),
            picker_watcher: Mutex::new(None),
            settings: Mutex::new(crate::settings::load_settings()),
            auth: RwLock::new(resolved.mode),
            clients: RwLock::new(resolved.registry),
            web_ui_credential: RwLock::new(resolved.web_ui_credential),
            config_root: RwLock::new(resolved.config_root),
            watched_session_ongoing: Mutex::new(None),
            event_tx,
            sessions_cache: Mutex::new(None),
        }
    }

    /// Test constructor: the given mode, the built-in clients registered in
    /// memory (with a `web-ui` credential when enabled), and **no** config root
    /// — so nothing a test does can reach a developer's real secrets. Tests that
    /// need persistence point at a temp dir via [`set_config_root`](Self::set_config_root).
    #[cfg(test)]
    pub fn for_tests(mode: AuthMode) -> Self {
        let mut registry = ClientRegistry::default();
        registry.ensure_builtins(clients::now());
        let web_ui_credential = mode.key().and_then(|key| {
            registry
                .find_by_name(clients::WEB_UI)
                .map(|c| crate::auth::issue_credential(c, key, c.issued_at))
        });
        Self::new(ResolvedAuth {
            mode,
            registry,
            web_ui_credential,
            config_root: None,
            warnings: vec![],
        })
    }

    #[cfg(test)]
    pub fn set_config_root(&self, root: Option<PathBuf>) {
        if let Ok(mut g) = self.config_root.write() {
            *g = root;
        }
    }

    /// `Ok(None)` when nothing is persisted (ephemeral key, tests). A poisoned
    /// lock is an error rather than a silent "nothing to persist", so a mutation
    /// can never report success without having been saved.
    fn config_root(&self) -> Result<Option<PathBuf>, String> {
        self.config_root
            .read()
            .map(|g| g.clone())
            .map_err(|_| "config root lock poisoned".to_string())
    }

    /// Clone of the live auth mode, for building `SettingsResponse`.
    pub fn auth_snapshot(&self) -> AuthMode {
        self.auth
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// The accepted clients (never their credentials).
    pub fn clients_snapshot(&self) -> Vec<Client> {
        self.clients
            .read()
            .map(|g| g.clients.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clients.clone())
    }

    pub fn web_ui_credential(&self) -> Option<String> {
        self.web_ui_credential.read().ok().and_then(|g| g.clone())
    }

    fn signing_key(&self) -> Result<Vec<u8>, String> {
        self.auth
            .read()
            .map_err(|e| e.to_string())?
            .key()
            .map(<[u8]>::to_vec)
            .ok_or_else(|| {
                "API client verification is disabled (CODEXTRACE_API_AUTH=off); there are no \
                 client credentials to manage"
                    .to_string()
            })
    }

    /// Replace the in-memory registry with the on-disk one when they differ.
    /// Must be called with the `clients` write guard held, so the compare and
    /// the swap cannot interleave with a mutation (otherwise a concurrent revoke
    /// could be undone by a refresh that loaded the file a moment before it was
    /// saved). Also re-reads the `web-ui` credential file, which follows the
    /// registry. Returns whether anything changed.
    fn adopt_from_disk_locked(&self, guard: &mut ClientRegistry, root: &Path) -> bool {
        let Some(on_disk) = load_adoptable_registry(root) else {
            return false;
        };
        if *guard == on_disk {
            return false;
        }
        *guard = on_disk;
        let web_ui =
            crate::auth::read_trimmed(&crate::auth::builtin_credential_path(root, clients::WEB_UI));
        if let Ok(mut g) = self.web_ui_credential.write() {
            *g = web_ui;
        }
        true
    }

    /// Apply `mutate` to the registry and persist the result before it becomes
    /// visible. Under the write lock the on-disk registry is adopted first, so a
    /// change made by another codex-trace process sharing the config dir (a
    /// revoke, say) is not overwritten by this one's stale copy. If the save
    /// fails, memory is left exactly as it was.
    fn mutate_registry<T>(
        &self,
        mutate: impl FnOnce(&mut ClientRegistry) -> Result<T, String>,
    ) -> Result<T, String> {
        let root = self.config_root()?;
        let mut guard = self.clients.write().map_err(|e| e.to_string())?;
        if let Some(root) = &root {
            self.adopt_from_disk_locked(&mut guard, root);
        }
        let mut next = guard.clone();
        let out = mutate(&mut next)?;
        if let Some(root) = &root {
            next.save(&crate::auth::registry_path(root))?;
        }
        *guard = next;
        Ok(out)
    }

    /// Keep a built-in client's credential file (and the live `web-ui` cookie
    /// value) in step with a reissue or revocation.
    fn sync_builtin_credential(&self, client: &Client, credential: Option<&str>) {
        if !client.builtin {
            return;
        }
        if let Ok(Some(root)) = self.config_root() {
            let path = crate::auth::builtin_credential_path(&root, &client.name);
            match credential {
                Some(c) => {
                    if let Err(e) =
                        crate::auth::write_private_atomic(&path, format!("{c}\n").as_bytes())
                    {
                        eprintln!(
                            "HTTP API: WARNING — could not write {}: {e}",
                            path.display()
                        );
                    }
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        if client.name == clients::WEB_UI {
            if let Ok(mut g) = self.web_ui_credential.write() {
                *g = credential.map(str::to_owned);
            }
        }
    }

    /// Register a new client and mint its credential (returned once).
    pub fn register_client(&self, name: &str) -> Result<(Client, String), String> {
        let key = self.signing_key()?;
        let client = self.mutate_registry(|r| r.register(name, clients::now()))?;
        let credential = crate::auth::issue_credential(&client, &key, client.issued_at);
        Ok((client, credential))
    }

    /// Reissue: mint a new credential and invalidate every older one.
    pub fn reissue_client(&self, id: uuid::Uuid) -> Result<(Client, String), String> {
        let key = self.signing_key()?;
        let client = self.mutate_registry(|r| r.reissue(id, clients::now()))?;
        let credential = crate::auth::issue_credential(&client, &key, client.issued_at);
        self.sync_builtin_credential(&client, Some(&credential));
        Ok((client, credential))
    }

    /// Revoke: every credential of this client stops working immediately.
    pub fn revoke_client(&self, id: uuid::Uuid) -> Result<Client, String> {
        self.signing_key()?;
        let client = self.mutate_registry(|r| r.revoke(id, clients::now()))?;
        self.sync_builtin_credential(&client, None);
        Ok(client)
    }

    fn check_registry(&self, id: uuid::Uuid, iat: i64) -> Option<ClientIdentity> {
        let registry = self.clients.read().ok()?;
        let client = registry.find(id)?;
        if client.is_revoked() || iat < client.issued_at {
            return None;
        }
        Some(ClientIdentity {
            id,
            name: client.name.clone(),
        })
    }

    /// Map verified claims to a live client. On a miss, re-read the registry
    /// from disk once — another codex-trace process sharing the config dir may
    /// have registered or reissued this client — and retry. Called by the auth
    /// middleware, so the happy path never touches the disk.
    pub fn authenticate(&self, claims: &Claims) -> Option<ClientIdentity> {
        let id = uuid::Uuid::parse_str(&claims.sub).ok()?;
        if let Some(identity) = self.check_registry(id, claims.iat) {
            return Some(identity);
        }
        if self.refresh_clients_from_disk() {
            return self.check_registry(id, claims.iat);
        }
        None
    }

    /// Adopt the on-disk registry (and the `web-ui` credential file) when they
    /// differ from memory. Returns whether anything changed. No-op without a
    /// config root, and never adopts a missing, unparsable or built-in-less file
    /// (see [`load_adoptable_registry`]).
    pub fn refresh_clients_from_disk(&self) -> bool {
        let Ok(Some(root)) = self.config_root() else {
            return false;
        };
        let Ok(mut guard) = self.clients.write() else {
            return false;
        };
        self.adopt_from_disk_locked(&mut guard, &root)
    }

    pub fn stop_session_watcher(&self) -> Result<(), String> {
        let mut guard = self.session_watcher.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = guard.take() {
            handle.stop();
        }
        Ok(())
    }

    pub fn set_session_watcher(&self, handle: WatcherHandle) -> Result<(), String> {
        let mut guard = self.session_watcher.lock().map_err(|e| e.to_string())?;
        *guard = Some(handle);
        Ok(())
    }

    pub fn stop_picker_watcher(&self) -> Result<(), String> {
        let mut guard = self.picker_watcher.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = guard.take() {
            handle.stop();
        }
        Ok(())
    }

    pub fn set_picker_watcher(&self, handle: WatcherHandle) -> Result<(), String> {
        let mut guard = self.picker_watcher.lock().map_err(|e| e.to_string())?;
        *guard = Some(handle);
        Ok(())
    }

    pub fn set_watched_ongoing(&self, path: String, ongoing: bool) {
        if let Ok(mut guard) = self.watched_session_ongoing.lock() {
            *guard = Some((path, ongoing));
        }
    }

    pub fn clear_watched_ongoing(&self) {
        if let Ok(mut guard) = self.watched_session_ongoing.lock() {
            *guard = None;
        }
    }

    pub fn apply_watched_ongoing(&self, sessions: &mut [CodexSessionInfo]) {
        let guard = match self.watched_session_ongoing.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some((ref path, ongoing)) = *guard {
            if let Some(s) = sessions.iter_mut().find(|s| s.path == *path) {
                s.is_ongoing = ongoing;
            }
        }
    }

    /// Discover sessions for `dir`, returning a cached result if fresh enough.
    /// Multiple concurrent callers within the TTL window share one disk scan.
    pub fn discover_sessions_cached(&self, dir: &str) -> Result<Vec<CodexSessionInfo>, String> {
        let mut cache = self.sessions_cache.lock().map_err(|e| e.to_string())?;
        if let Some(ref c) = *cache {
            if c.dir == dir && c.cached_at.elapsed() < SESSIONS_CACHE_TTL {
                return Ok(c.sessions.clone());
            }
        }
        let path = std::path::Path::new(dir);
        let sessions = crate::parser::discover::discover_sessions(path)?;
        *cache = Some(SessionsCache {
            dir: dir.to_string(),
            cached_at: Instant::now(),
            sessions: sessions.clone(),
        });
        Ok(sessions)
    }

    pub fn broadcast(&self, event: &str, data: &str) {
        let _ = self.event_tx.send(SseEvent {
            event: event.to_string(),
            data: data.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> AppState {
        AppState::for_tests(AuthMode::Disabled)
    }

    #[test]
    fn discover_sessions_cached_returns_empty_for_nonexistent_dir() {
        // discover_sessions returns Ok(empty) for nonexistent dirs (not an error).
        let state = make_state();
        let result = state.discover_sessions_cached("/nonexistent/path/that/does/not/exist");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn discover_sessions_cached_hits_cache_on_second_call() {
        let state = make_state();
        // Use a real empty temp dir so the first call succeeds and populates cache
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let first = state.discover_sessions_cached(path).unwrap();
        assert!(first.is_empty());

        // Prime the cache with a fake entry by directly writing to the cache lock
        {
            let mut cache = state.sessions_cache.lock().unwrap();
            *cache = Some(SessionsCache {
                dir: path.to_string(),
                cached_at: Instant::now(),
                sessions: vec![CodexSessionInfo {
                    id: "cached-session".to_string(),
                    path: "/fake/path.jsonl".to_string(),
                    cwd: None,
                    git_branch: None,
                    originator: None,
                    model: None,
                    cli_version: None,
                    thread_name: None,
                    turn_count: 0,
                    start_time: String::new(),
                    end_time: None,
                    total_tokens: None,
                    is_ongoing: false,
                    is_external_worker: false,
                    is_inline_worker: false,
                    is_headless: false,
                    is_archived: false,
                    worker_nickname: None,
                    worker_role: None,
                    spawned_worker_ids: vec![],
                    date_group: String::new(),
                    ai_title: None,
                    approval_mode: None,
                    history_base_thread_id: None,
                    forked_from_thread_id: None,
                    mentioned_thread_ids: vec![],
                }],
            });
        }

        // Second call must return the cached fake entry, not re-scan the dir
        let second = state.discover_sessions_cached(path).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, "cached-session");
    }

    #[test]
    fn discover_sessions_cached_invalidates_cache_for_different_dir() {
        let state = make_state();
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        // Populate cache for dir_a with a fake entry
        {
            let mut cache = state.sessions_cache.lock().unwrap();
            *cache = Some(SessionsCache {
                dir: dir_a.path().to_str().unwrap().to_string(),
                cached_at: Instant::now(),
                sessions: vec![CodexSessionInfo {
                    id: "dir-a-session".to_string(),
                    path: "/fake/a.jsonl".to_string(),
                    cwd: None,
                    git_branch: None,
                    originator: None,
                    model: None,
                    cli_version: None,
                    thread_name: None,
                    turn_count: 0,
                    start_time: String::new(),
                    end_time: None,
                    total_tokens: None,
                    is_ongoing: false,
                    is_external_worker: false,
                    is_inline_worker: false,
                    is_headless: false,
                    is_archived: false,
                    worker_nickname: None,
                    worker_role: None,
                    spawned_worker_ids: vec![],
                    date_group: String::new(),
                    ai_title: None,
                    approval_mode: None,
                    history_base_thread_id: None,
                    forked_from_thread_id: None,
                    mentioned_thread_ids: vec![],
                }],
            });
        }

        // Requesting dir_b must bypass the cache and return the real (empty) scan
        let result = state
            .discover_sessions_cached(dir_b.path().to_str().unwrap())
            .unwrap();
        assert!(
            result.is_empty(),
            "different dir must not return dir_a cached data"
        );
    }
}
