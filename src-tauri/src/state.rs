use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::auth::{AuthMode, ClientIdentity, ResolvedAuth};
use crate::clients::{self, Client, ClientRegistry};
use crate::jwt::Claims;
use crate::parser::discover::CodexSessionInfo;
use crate::parser::session::CodexSession;
use crate::parser::summary::SessionIndex;
use crate::parser::turn::CodexTurn;
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

/// Prefix of the error returned when a turn index is past the end of the
/// session. `http_api` matches on it to answer 404 rather than 500.
pub const NO_TURN_AT_INDEX: &str = "no turn at index";

const SESSIONS_CACHE_TTL: Duration = Duration::from_secs(2);

/// The most recently parsed session, so opening turn after turn in the detail
/// view re-reads the file only when it has actually changed on disk. One entry:
/// the viewer shows one session at a time, and holding more would defeat the
/// point of not shipping every turn body to the frontend.
struct ParsedSession {
    path: String,
    modified: Option<std::time::SystemTime>,
    session: CodexSession,
}

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
    parsed_session: Mutex<Option<ParsedSession>>,
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
            parsed_session: Mutex::new(None),
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

    /// Mtime of `path`, or `None` when it cannot be read. A `None` on either
    /// side of the comparison counts as "changed", so an unreadable mtime
    /// re-parses rather than serving a possibly stale turn.
    fn modified_at(path: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(path).ok()?.modified().ok()
    }

    /// Run `f` over the parsed session at `path`, parsing it only when the
    /// cached copy is for a different file or the file has changed on disk.
    ///
    /// The closure borrows the session rather than returning a clone, so the
    /// caller can pull out one turn without copying every other one.
    fn with_parsed_session<T>(
        &self,
        path: &str,
        f: impl FnOnce(&CodexSession) -> T,
    ) -> Result<T, String> {
        self.with_parsed_session_mut(path, |s| f(s))
    }

    /// As [`Self::with_parsed_session`], but the closure may borrow the cached
    /// session mutably — used to lift its turns out and put them back without
    /// copying them.
    fn with_parsed_session_mut<T>(
        &self,
        path: &str,
        f: impl FnOnce(&mut CodexSession) -> T,
    ) -> Result<T, String> {
        let modified = Self::modified_at(path);
        let mut guard = self.parsed_session.lock().map_err(|e| e.to_string())?;
        let fresh = guard
            .as_ref()
            .is_some_and(|c| c.path == path && modified.is_some() && c.modified == modified);
        if !fresh {
            let session = crate::commands::session::load_session_from_path(path)?;
            *guard = Some(ParsedSession {
                path: path.to_string(),
                modified,
                session,
            });
        }
        let cached = guard.as_mut().expect("just populated");
        Ok(f(&mut cached.session))
    }

    /// The session's metadata and its lightweight turn index — no turn bodies.
    pub fn load_session_index(&self, path: &str) -> Result<SessionIndex, String> {
        self.with_parsed_session_mut(path, SessionIndex::of_cached)
    }

    /// One turn, with its bodies, by position in the turn index.
    pub fn load_turn(&self, path: &str, index: usize) -> Result<CodexTurn, String> {
        let turn = self.with_parsed_session(path, |s| s.turns.get(index).cloned())?;
        turn.ok_or_else(|| format!("{NO_TURN_AT_INDEX} {index}"))
    }

    /// Drop the parsed session, so the next read re-parses. Called when a
    /// watched session changes and when the view moves off it, so a large
    /// transcript is not held after it stops being looked at.
    pub fn clear_parsed_session(&self) {
        if let Ok(mut guard) = self.parsed_session.lock() {
            *guard = None;
        }
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

    /// A two-turn session whose second turn carries a large tool output, so a
    /// test can tell the index apart from the bodies by size alone.
    fn write_session(dir: &std::path::Path) -> String {
        let path = dir.join("rollout-2026-05-07T00-00-00-paged.jsonl");
        let big = "x".repeat(50_000);
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{"session_id":"paged","timestamp":"2026-05-07T00:00:00Z","cwd":"/tmp"}}"#.to_string(),
                r#"{"timestamp":"2026-05-07T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#.to_string(),
                r#"{"timestamp":"2026-05-07T00:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746576002.0}}"#.to_string(),
                r#"{"timestamp":"2026-05-07T00:00:03Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#.to_string(),
                r#"{"timestamp":"2026-05-07T00:00:04Z","type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"c1","arguments":"{\"command\":[\"ls\"]}"}}"#.to_string(),
                format!(
                    r#"{{"timestamp":"2026-05-07T00:00:05Z","type":"event_msg","payload":{{"type":"exec_command_end","call_id":"c1","exit_code":0,"aggregated_output":"{big}"}}}}"#
                ),
                r#"{"timestamp":"2026-05-07T00:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1746576006.0}}"#.to_string(),
            ]
            .join("\n"),
        )
        .unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn the_session_index_carries_summaries_but_no_turn_bodies() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path());
        let state = make_state();

        let index = state.load_session_index(&path).unwrap();

        assert_eq!(index.summaries.len(), 2);
        assert!(index.session.turns.is_empty());
        let json = serde_json::to_string(&index).unwrap();
        assert!(
            !json.contains(&"x".repeat(1_000)),
            "the index must not carry tool output",
        );
    }

    #[test]
    fn load_turn_returns_the_bodies_the_index_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path());
        let state = make_state();

        let turn = state.load_turn(&path, 1).unwrap();

        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].output.as_deref().unwrap().len(), 50_000);
    }

    /// The index read must not copy the turn bodies out of the cache. A clone
    /// of the whole session would duplicate every tool output in the
    /// transcript, once per index read — and the watcher triggers one of those
    /// per append while a session is live.
    #[test]
    fn reading_the_index_leaves_the_cached_bodies_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path());
        let state = make_state();

        let index = state.load_session_index(&path).unwrap();
        assert_eq!(index.summaries.len(), 2);
        assert!(
            index.session.turns.is_empty(),
            "the index carries no bodies"
        );

        // Straight after the index read, the cache must still have the bodies:
        // this is served from the same parse, not a re-parse.
        let turn = state.load_turn(&path, 1).unwrap();
        assert_eq!(turn.tool_calls[0].output.as_deref().unwrap().len(), 50_000);

        // And the turns are still there for a second index read.
        assert_eq!(state.load_session_index(&path).unwrap().summaries.len(), 2);
    }

    #[test]
    fn load_turn_reports_an_index_past_the_end() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path());
        let state = make_state();

        let err = state.load_turn(&path, 99).unwrap_err();
        assert!(err.starts_with(NO_TURN_AT_INDEX), "{err}");
    }

    #[test]
    fn the_parsed_session_is_reused_while_the_file_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path());
        let state = make_state();
        let mtime =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(&path).unwrap());
        assert_eq!(state.load_session_index(&path).unwrap().summaries.len(), 2);

        // Append a third turn but restore the original mtime, so only a cache
        // hit can still report two.
        let grown = [
            std::fs::read_to_string(&path).unwrap(),
            r#"{"timestamp":"2026-05-07T00:00:07Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-3"}}"#.to_string(),
        ]
        .join("\n");
        std::fs::write(&path, grown).unwrap();
        filetime::set_file_mtime(&path, mtime).unwrap();

        assert_eq!(
            state.load_session_index(&path).unwrap().summaries.len(),
            2,
            "served from the cache rather than re-parsed",
        );
    }

    #[test]
    fn an_unreadable_mtime_re_parses_rather_than_serving_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path());
        let state = make_state();
        state.load_session_index(&path).unwrap();

        // No mtime to compare against: fall back to reading, and report the
        // read failure instead of quietly handing back the old parse.
        std::fs::remove_file(&path).unwrap();
        assert!(state.load_turn(&path, 0).is_err());
    }

    #[test]
    fn a_changed_file_is_reparsed_rather_than_served_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path());
        let state = make_state();
        assert_eq!(state.load_session_index(&path).unwrap().summaries.len(), 2);

        // Rewrite with a third turn, forcing a new mtime.
        let extra = [
            std::fs::read_to_string(&path).unwrap(),
            r#"{"timestamp":"2026-05-07T00:00:07Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-3"}}"#.to_string(),
            r#"{"timestamp":"2026-05-07T00:00:08Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-3","completed_at":1746576008.0}}"#.to_string(),
        ]
        .join("\n");
        std::fs::write(&path, extra).unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(2_000_000_000, 0))
            .unwrap();

        assert_eq!(state.load_session_index(&path).unwrap().summaries.len(), 3);
    }

    #[test]
    fn clearing_the_parsed_session_forces_a_reread() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path());
        let state = make_state();
        state.load_session_index(&path).unwrap();

        state.clear_parsed_session();
        std::fs::remove_file(&path).unwrap();

        assert!(state.load_turn(&path, 0).is_err(), "no longer cached");
    }

    #[test]
    fn a_second_session_replaces_the_first_in_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let first = write_session(tmp.path());
        let second_dir = tempfile::tempdir().unwrap();
        let second = write_session(second_dir.path());
        let state = make_state();

        state.load_session_index(&first).unwrap();
        state.load_session_index(&second).unwrap();
        std::fs::remove_file(&first).unwrap();

        assert!(state.load_turn(&first, 0).is_err(), "first was evicted");
        assert!(state.load_turn(&second, 0).is_ok());
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
