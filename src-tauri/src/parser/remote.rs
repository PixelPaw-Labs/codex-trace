//! SSH-backed access to Codex session files on a remote host.
//!
//! Remote sessions are represented by an opaque `ssh://<host>/<path>` string. The
//! host is passed to the user's normal `ssh` client, so aliases, identities,
//! jump-hosts, and agent forwarding continue to come from `~/.ssh/config`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::discover::CodexSessionInfo;
use super::session::{parse_session, CodexSession};

const PREFIX: &str = "ssh://";
const REMOTE_TITLE_CACHE_TTL: Duration = Duration::from_secs(30);

struct RemoteTitleCacheEntry {
    fetched_at: Instant,
    titles: HashMap<String, String>,
}

static REMOTE_TITLE_CACHE: OnceLock<Mutex<HashMap<String, RemoteTitleCacheEntry>>> =
    OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    pub host: String,
    pub path: String,
}

pub fn is_remote_spec(value: &str) -> bool {
    value.starts_with(PREFIX)
}

pub fn parse_spec(value: &str) -> Result<RemoteSpec, String> {
    let rest = value
        .strip_prefix(PREFIX)
        .ok_or_else(|| format!("invalid remote sessions path: {value}"))?;
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| "remote sessions path must be ssh://host/path".to_string())?;
    if !valid_host(host) {
        return Err("remote SSH host must be a non-empty host or SSH config alias".to_string());
    }
    if path.is_empty() {
        return Err("remote sessions directory must not be empty".to_string());
    }
    Ok(RemoteSpec {
        host: host.to_string(),
        path: if path == "~" || path.starts_with("~/") {
            path.to_string()
        } else {
            format!("/{path}")
        },
    })
}

pub fn make_spec(host: &str, path: &str) -> Result<String, String> {
    let host = host.trim();
    let path = path.trim();
    if !valid_host(host) {
        return Err("remote SSH host must be a non-empty host or SSH config alias".to_string());
    }
    if path.is_empty() {
        return Err("remote sessions directory must not be empty".to_string());
    }
    let path = if path.starts_with('/') || path == "~" || path.starts_with("~/") {
        path.to_string()
    } else {
        return Err("remote sessions directory must be absolute or start with ~/".to_string());
    };
    Ok(format!("{PREFIX}{host}/{}", path.trim_start_matches('/')))
}

/// Discover remote rollout files by invoking the user's OpenSSH client. Metadata
/// is computed remotely so large session trees do not have to cross the network.
pub fn discover_remote_sessions(spec_value: &str) -> Result<Vec<CodexSessionInfo>, String> {
    discover_remote_sessions_with_script(spec_value, REMOTE_DISCOVERY_FAST_SCRIPT)
}

/// Perform the legacy full metadata scan. This is intentionally separate from
/// the fast picker path: the watcher can run it in the background and refresh
/// the picker after the first lightweight result is already visible.
pub fn discover_remote_sessions_full(spec_value: &str) -> Result<Vec<CodexSessionInfo>, String> {
    discover_remote_sessions_with_script(spec_value, REMOTE_DISCOVERY_FULL_SCRIPT)
}

fn discover_remote_sessions_with_script(
    spec_value: &str,
    discovery_script: &str,
) -> Result<Vec<CodexSessionInfo>, String> {
    let spec = parse_spec(spec_value)?;
    // Do not copy the remote sessions directory: a normal development host can
    // contain gigabytes of rollout history. Instead, run a small metadata-only
    // scanner remotely and transfer one compact JSON record per session. The
    // selected session itself is fetched lazily by `parse_remote_session`.
    let output = run_ssh(
        &spec.host,
        &remote_discovery_command(&spec, discovery_script),
    )?;
    let mut sessions = Vec::new();
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let mut session: CodexSessionInfo = serde_json::from_slice(line)
            .map_err(|e| format!("invalid remote session metadata: {e}"))?;
        let relative = session.path.clone();
        let remote_path = join_remote_path(&spec.path, &relative);
        session.path = format_remote_file(&spec.host, &remote_path);
        sessions.push(session);
    }
    sessions.sort_by(|a, b| b.start_time.cmp(&a.start_time));
    Ok(sessions)
}

/// Download and parse one remote rollout file. The temporary file is removed
/// after parsing; the returned session retains its `ssh://` path for reloads.
pub fn parse_remote_session(spec_value: &str) -> Result<CodexSession, String> {
    let spec = parse_spec(spec_value)?;
    let remote_path = remote_file_path(&spec.path)?;
    let temp_root = temporary_root("session")?;
    let file_name = Path::new(&remote_path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "remote session path has no file name".to_string())?;
    let local_path = temp_root.join(file_name);
    fs::write(&local_path, read_remote_file(&spec.host, &remote_path)?)
        .map_err(|e| format!("write remote session cache: {e}"))?;

    let parsed = parse_session(&local_path);
    let _ = fs::remove_dir_all(&temp_root);
    let mut session = parsed?;
    // The rollout itself does not contain Codex Desktop's user-facing title.
    // Fetch the matching sibling index entry while the selected session is
    // already being loaded. Discovery performs the same merge in bulk, and
    // this keeps the detail view's InfoBar consistent with the picker.
    if let Some(title) = remote_session_title(&spec, &session.id) {
        session.thread_name = Some(title);
    }
    session.path = spec_value.to_string();
    Ok(session)
}

/// Run a cheap remote command to validate credentials and the sessions path.
pub fn test_connection(spec_value: &str) -> Result<(), String> {
    let spec = parse_spec(spec_value)?;
    let command = format!("test -d {}", shell_path(&spec.path));
    run_ssh(&spec.host, &command).map(|_| ())
}

/// Return a cheap, content-independent fingerprint of the remote rollout tree.
/// Used by the picker watcher so it does not rescan gigabytes of JSONL every few
/// seconds just to detect whether a file was appended.
pub fn remote_sessions_snapshot(spec_value: &str) -> Result<String, String> {
    let spec = parse_spec(spec_value)?;
    let command = format!(
        "python3 - {} <<'PY'\n{}\nPY",
        shell_path(&spec.path),
        REMOTE_SNAPSHOT_SCRIPT
    );
    let output = run_ssh(&spec.host, &command)?;
    String::from_utf8(output).map_err(|e| format!("invalid remote sessions snapshot: {e}"))
}

pub fn remote_file_snapshot(spec_value: &str) -> Result<String, String> {
    let spec = parse_spec(spec_value)?;
    let remote_path = remote_file_path(&spec.path)?;
    let command = format!(
        "python3 - {} <<'PY'\n{}\nPY",
        shell_path(&remote_path),
        REMOTE_FILE_SNAPSHOT_SCRIPT
    );
    let output = run_ssh(&spec.host, &command)?;
    String::from_utf8(output).map_err(|e| format!("invalid remote file snapshot: {e}"))
}

fn remote_discovery_command(spec: &RemoteSpec, discovery_script: &str) -> String {
    format!(
        "python3 - {} <<'PY'\n{}\nPY",
        shell_path(&spec.path),
        discovery_script
    )
}

fn read_remote_file(host: &str, path: &str) -> Result<Vec<u8>, String> {
    run_ssh(host, &format!("cat -- {}", shell_path(path)))
}

fn remote_index_path(root: &str) -> String {
    let path = Path::new(root);
    let index_parent = if root.ends_with(".jsonl") || root.ends_with(".jsonl.zst") {
        // A selected rollout is rooted at sessions/YYYY/MM/DD/file. Walk back
        // through the date components to the sessions directory.
        path.parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("."))
    } else {
        path.parent().unwrap_or_else(|| Path::new("."))
    };
    index_parent
        .join("session_index.jsonl")
        .to_string_lossy()
        .to_string()
}

fn remote_session_title(spec: &RemoteSpec, session_id: &str) -> Option<String> {
    let index_path = remote_index_path(&spec.path);
    let cache_key = format!("{}:{index_path}", spec.host);
    let cache = REMOTE_TITLE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(entry) = guard.get(&cache_key) {
            if entry.fetched_at.elapsed() < REMOTE_TITLE_CACHE_TTL {
                return entry.titles.get(session_id).cloned();
            }
        }
    }

    let output = read_remote_file(&spec.host, &index_path).ok()?;
    let mut titles = HashMap::new();
    for value in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
    {
        let id = value.get("id").and_then(|value| value.as_str());
        let next_title = value
            .get("thread_name")
            .or_else(|| value.get("title"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let (Some(id), Some(title)) = (id, next_title) {
            titles.insert(id.to_string(), title.to_string());
        }
    }
    let title = titles.get(session_id).cloned();
    if let Ok(mut guard) = cache.lock() {
        guard.insert(
            cache_key,
            RemoteTitleCacheEntry {
                fetched_at: Instant::now(),
                titles,
            },
        );
    }
    title
}

fn run_ssh(host: &str, command: &str) -> Result<Vec<u8>, String> {
    let control_path = ssh_control_path();
    let output = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "Compression=yes",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=60",
            "-o",
            &format!("ControlPath={control_path}"),
            host,
            command,
        ])
        .output()
        .map_err(|e| format!("failed to start ssh: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("ssh connection to {host} failed ({})", output.status)
        } else {
            format!("ssh connection to {host} failed: {detail}")
        });
    }
    Ok(output.stdout)
}

fn ssh_control_path() -> String {
    // OpenSSH limits ControlPath to 104 bytes. macOS temp_dir() can already
    // contain a long per-process path, so keep the socket prefix deliberately
    // short while `%C` still isolates hosts/configurations by hash.
    "/tmp/ct-ssh-%C".to_string()
}

fn shell_path(path: &str) -> String {
    if path == "~" {
        return "$HOME".to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("$HOME/{}", shell_quote_fragment(rest));
    }
    shell_quote_fragment(path)
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'@' | b':')
        })
}

fn shell_quote_fragment(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_file_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "~" || trimmed.ends_with("/sessions") {
        return Err("remote session path must point to a rollout JSONL file".to_string());
    }
    if !(trimmed.ends_with(".jsonl") || trimmed.ends_with(".jsonl.zst")) {
        return Err("remote session path must end in .jsonl or .jsonl.zst".to_string());
    }
    Ok(trimmed.to_string())
}

fn join_remote_path(root: &str, relative: &str) -> String {
    format!(
        "{}/{}",
        root.trim_end_matches('/'),
        relative.trim_start_matches('/')
    )
}

#[cfg(test)]
fn relative_remote_path(root: &str, file: &str) -> Option<PathBuf> {
    let root = root.trim_end_matches('/');
    let relative = file.strip_prefix(root)?.trim_start_matches('/');
    if relative.is_empty() || relative.starts_with("../") || relative == ".." {
        None
    } else {
        Some(PathBuf::from(relative))
    }
}

fn format_remote_file(host: &str, path: &str) -> String {
    format!("{PREFIX}{host}/{}", path.trim_start_matches('/'))
}

fn temporary_root(label: &str) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!(
        "codex-trace-remote-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&path).map_err(|e| format!("create remote cache: {e}"))?;
    Ok(path)
}

const REMOTE_DISCOVERY_FULL_SCRIPT: &str = r#"
import json, os, shutil, subprocess, sys, time

root = os.path.expanduser(sys.argv[1])
now = time.time()
zstd_binary = shutil.which('zstd')
try:
    import zstandard
except ImportError:
    zstandard = None

class ZstdStream:
    def __init__(self, path):
        self.compressed = open(path, 'rb')
        self.reader = zstandard.ZstdDecompressor().stream_reader(self.compressed)

    def __iter__(self):
        return self

    def __next__(self):
        raw = self.reader.readline()
        if not raw:
            raise StopIteration
        return raw

    def close(self):
        self.reader.close()
        self.compressed.close()

def stream(path):
    if path.endswith('.zst'):
        if zstd_binary:
            return subprocess.Popen([zstd_binary, '-q', '-dc', path], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL).stdout
        if zstandard:
            return ZstdStream(path)
        raise RuntimeError('compressed rollout requires zstd or Python zstandard')
    return open(path, 'rb')

def text(value):
    return value if isinstance(value, str) else None

# Codex Desktop keeps user-facing titles in the sibling session_index.jsonl,
# not in rollout files. Read it once so discovery returns the same names as the
# local Desktop session picker. Missing/malformed index lines are harmless.
index_titles = {}
index_path = os.path.join(os.path.dirname(root), 'session_index.jsonl')
try:
    with open(index_path, 'r', encoding='utf-8') as index:
        for raw in index:
            try:
                value = json.loads(raw)
            except Exception:
                continue
            session_id = text(value.get('id'))
            title = text(value.get('thread_name') or value.get('title'))
            if session_id and title and title.strip():
                index_titles[session_id] = title.strip()
except OSError:
    pass

for dirpath, _, names in os.walk(root):
    for name in names:
        if not (name.startswith('rollout-') and (name.endswith('.jsonl') or name.endswith('.jsonl.zst'))):
            continue
        path = os.path.join(dirpath, name)
        try:
            fh = stream(path)
            first = None
            turns = 0
            model = None
            thread_name = None
            total_tokens = None
            end_time = None
            ongoing = False
            has_end = False
            spawned = []
            for raw in fh:
                try:
                    value = json.loads(raw)
                except Exception:
                    continue
                if first is None:
                    first = value
                typ = value.get('type')
                payload = value.get('payload') or {}
                if typ == 'session_end':
                    has_end = True
                    ongoing = False
                    end_time = end_time or text(value.get('timestamp'))
                elif typ == 'event_msg':
                    event = payload.get('type')
                    if event == 'task_started':
                        turns += 1
                        ongoing = not has_end
                        end_time = None
                    elif event == 'user_message' and turns == 0:
                        turns = 1
                        ongoing = not has_end
                        end_time = None
                    elif event == 'task_complete':
                        ongoing = False
                        end_time = text(value.get('timestamp'))
                        if total_tokens is None:
                            total_tokens = payload.get('total_tokens')
                    elif event in ('turn_aborted', 'token_budget_abort', 'inference_stream_cancelled'):
                        ongoing = False
                        end_time = text(value.get('timestamp'))
                    elif event == 'token_count':
                        info = payload.get('info') or {}
                        usage = info.get('total_token_usage') or {}
                        total_tokens = usage.get('total_tokens', total_tokens)
                    elif event == 'thread_name_updated':
                        thread_name = text(payload.get('thread_name'))
                    elif event == 'collab_agent_spawn_end':
                        new_id = payload.get('new_session_id') or payload.get('new_thread_id')
                        if isinstance(new_id, str) and new_id and new_id not in spawned:
                            spawned.append(new_id)
                elif typ == 'turn_context':
                    model = text(payload.get('model')) or model
            try:
                fh.close()
            except Exception:
                pass
            if not first:
                continue
            meta = first.get('payload') or first
            session_id = text(meta.get('id')) or text(meta.get('session_id')) or text((meta.get('thread') or {}).get('sessionId'))
            if not session_id:
                continue
            thread_name = index_titles.get(session_id) or thread_name
            start_time = text(meta.get('timestamp')) or text(first.get('timestamp')) or ''
            try:
                ongoing = ongoing and (now - os.path.getmtime(path) <= 60) and not has_end and turns > 0
            except Exception:
                ongoing = False
            source = meta.get('source')
            source_subagent = source.get('subagent') if isinstance(source, dict) else None
            print(json.dumps({
                'id': session_id,
                'path': os.path.relpath(path, root),
                'cwd': text(meta.get('cwd')),
                'git_branch': text((meta.get('git') or {}).get('branch')),
                'originator': text(meta.get('originator')),
                'model': model,
                'cli_version': text(meta.get('cli_version')),
                'thread_name': thread_name,
                'turn_count': turns,
                'start_time': start_time,
                'end_time': end_time,
                'total_tokens': total_tokens,
                'is_ongoing': ongoing,
                'is_external_worker': bool(source_subagent),
                'is_inline_worker': False,
                'worker_nickname': None,
                'worker_role': None,
                'spawned_worker_ids': spawned,
                'date_group': '/'.join(os.path.relpath(os.path.dirname(path), root).split(os.sep)[-3:]),
                'ai_title': text(meta.get('ai-title')),
                'is_headless': meta.get('originator') == 'remote-control' or source == 'remote-control',
                'is_archived': bool(meta.get('archived', False)),
                'approval_mode': text(meta.get('ask_for_approval')),
                'history_base_thread_id': text((meta.get('history_base') or {}).get('thread_id')),
                'forked_from_thread_id': text(meta.get('forked_from_id')),
                'mentioned_thread_ids': [],
            }, separators=(',', ':')), flush=True)
        except Exception:
            continue
"#;

/// Fast remote picker scan. Unlike the legacy scanner above, this reads only
/// the first JSONL record from each rollout (and never counts every turn or
/// token). A remote directory can contain gigabytes of history; full parsing
/// is deferred until the user selects one session.
const REMOTE_DISCOVERY_FAST_SCRIPT: &str = r#"
import json, os, shutil, subprocess, sys

root = os.path.expanduser(sys.argv[1])
zstd_binary = shutil.which('zstd')
try:
    import zstandard
except ImportError:
    zstandard = None

def text(value):
    return value if isinstance(value, str) else None

def first_record(path):
    try:
        if path.endswith('.zst'):
            if zstd_binary:
                process = subprocess.Popen(
                    [zstd_binary, '-q', '-dc', path],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                )
                raw = process.stdout.readline()
                process.kill()
                process.wait()
            elif zstandard:
                with open(path, 'rb') as compressed:
                    reader = zstandard.ZstdDecompressor().stream_reader(compressed)
                    raw = reader.readline()
                    reader.close()
            else:
                return None
        else:
            with open(path, 'rb') as stream:
                raw = stream.readline()
        return json.loads(raw) if raw else None
    except Exception:
        return None

index_titles = {}
index_path = os.path.join(os.path.dirname(root), 'session_index.jsonl')
try:
    with open(index_path, 'r', encoding='utf-8') as index:
        for raw in index:
            try:
                value = json.loads(raw)
            except Exception:
                continue
            session_id = text(value.get('id'))
            title = text(value.get('thread_name') or value.get('title'))
            if session_id and title and title.strip():
                index_titles[session_id] = title.strip()
except OSError:
    pass

for dirpath, _, names in os.walk(root):
    for name in names:
        if not (name.startswith('rollout-') and (name.endswith('.jsonl') or name.endswith('.jsonl.zst'))):
            continue
        path = os.path.join(dirpath, name)
        first = first_record(path)
        if not first:
            continue
        meta = first.get('payload') or first
        session_id = text(meta.get('id')) or text(meta.get('session_id')) or text((meta.get('thread') or {}).get('sessionId'))
        if not session_id:
            continue
        source = meta.get('source')
        source_subagent = source.get('subagent') if isinstance(source, dict) else None
        relative_dir = os.path.relpath(os.path.dirname(path), root)
        print(json.dumps({
            'id': session_id,
            'path': os.path.relpath(path, root),
            'cwd': text(meta.get('cwd')),
            'git_branch': text((meta.get('git') or {}).get('branch')),
            'originator': text(meta.get('originator')),
            'model': None,
            'cli_version': text(meta.get('cli_version')),
            'thread_name': index_titles.get(session_id),
            'turn_count': 0,
            'start_time': text(meta.get('timestamp')) or text(first.get('timestamp')) or '',
            'end_time': None,
            'total_tokens': None,
            'is_ongoing': False,
            'is_external_worker': bool(source_subagent),
            'is_inline_worker': False,
            'worker_nickname': None,
            'worker_role': None,
            'spawned_worker_ids': [],
            'date_group': '/'.join(relative_dir.split(os.sep)[-3:]),
            'ai_title': text(meta.get('ai-title')),
            'is_headless': meta.get('originator') == 'remote-control' or source == 'remote-control',
            'is_archived': bool(meta.get('archived', False)),
            'approval_mode': text(meta.get('ask_for_approval')),
            'history_base_thread_id': text((meta.get('history_base') or {}).get('thread_id')),
            'forked_from_thread_id': text(meta.get('forked_from_id')),
            'mentioned_thread_ids': [],
        }, separators=(',', ':')), flush=True)
"#;

const REMOTE_SNAPSHOT_SCRIPT: &str = r#"
import os, sys
root = os.path.expanduser(sys.argv[1])
rows = []
for dirpath, _, names in os.walk(root):
    for name in names:
        if name.startswith('rollout-') and (name.endswith('.jsonl') or name.endswith('.jsonl.zst')):
            path = os.path.join(dirpath, name)
            try:
                stat = os.stat(path)
                rows.append(f'{os.path.relpath(path, root)}:{stat.st_size}:{stat.st_mtime_ns}:{stat.st_ino}')
            except OSError:
                pass
index_path = os.path.join(os.path.dirname(root), 'session_index.jsonl')
try:
    stat = os.stat(index_path)
    rows.append(f'../session_index.jsonl:{stat.st_size}:{stat.st_mtime_ns}:{stat.st_ino}')
except OSError:
    pass
print('\n'.join(sorted(rows)))
"#;

const REMOTE_FILE_SNAPSHOT_SCRIPT: &str = r#"
import os, sys
path = os.path.expanduser(sys.argv[1])
stat = os.stat(path)
print(f'{stat.st_size}:{stat.st_mtime_ns}:{stat.st_ino}')
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_alias_and_absolute_path() {
        assert_eq!(
            parse_spec("ssh://dev/home/user/.codex/sessions").unwrap(),
            RemoteSpec {
                host: "dev".to_string(),
                path: "/home/user/.codex/sessions".to_string(),
            }
        );
    }

    #[test]
    fn makes_remote_spec_from_alias_and_tilde_path() {
        assert_eq!(
            make_spec("dev", "~/.codex/sessions").unwrap(),
            "ssh://dev/~/.codex/sessions"
        );
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_remote_specs() {
        assert!(make_spec("", "/tmp/sessions").is_err());
        assert!(make_spec("dev host", "/tmp/sessions").is_err());
        assert!(make_spec("dev", "relative/sessions").is_err());
        assert!(parse_spec("ssh://dev").is_err());
    }

    #[test]
    fn shell_path_expands_tilde_without_exposing_shell_input() {
        assert_eq!(shell_path("~/.codex/sessions"), "$HOME/'.codex/sessions'");
        assert_eq!(shell_path("/tmp/my sessions"), "'/tmp/my sessions'");
    }

    #[test]
    fn ssh_control_path_is_short_enough_for_openssh() {
        let path = ssh_control_path();
        assert_eq!(path, "/tmp/ct-ssh-%C");
        assert!(path.len() < 104);
    }

    #[test]
    fn derives_remote_session_index_next_to_sessions_directory() {
        assert_eq!(
            remote_index_path("~/.codex/sessions"),
            "~/.codex/session_index.jsonl"
        );
        assert_eq!(
            remote_index_path("/home/user/.codex/sessions"),
            "/home/user/.codex/session_index.jsonl"
        );
        assert_eq!(
            remote_index_path("~/.codex/sessions/2026/09/07/rollout-a.jsonl"),
            "~/.codex/session_index.jsonl"
        );
    }

    #[test]
    fn maps_only_files_inside_remote_root() {
        assert_eq!(
            relative_remote_path(
                "/home/user/.codex/sessions",
                "/home/user/.codex/sessions/2026/09/07/rollout-a.jsonl"
            ),
            Some(PathBuf::from("2026/09/07/rollout-a.jsonl"))
        );
        assert!(relative_remote_path("/home/user/.codex/sessions", "/tmp/other.jsonl").is_none());
    }
}
