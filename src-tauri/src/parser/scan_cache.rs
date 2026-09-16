//! Remembers what a discovery scan found in each session file.
//!
//! Discovery has to read every session file end to end, because the metadata it
//! reports — turn count, token totals, whether the session is still running —
//! is only knowable from the last line. On a real sessions directory that is
//! tens of gigabytes: a cold scan of 3,300 files totalling 33GB takes about
//! seventy seconds. The picker's in-memory result cache expires after two
//! seconds, so without this the whole directory is re-read every few seconds
//! for as long as anything is watching.
//!
//! A session file is append-only and is never rewritten in place, so its
//! modified time and length together identify its contents. Anything that still
//! matches what was scanned before is served from here and never opened.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use super::discover::CodexSessionInfo;

/// What a file looked like when it was scanned. A session file only ever grows,
/// so an unchanged pair means unchanged contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FileStamp {
    /// Whole seconds since the epoch. Whole seconds is deliberate: it is the
    /// coarsest resolution any supported filesystem reports, and pairing it with
    /// the length covers a write that lands inside the same second.
    modified_secs: u64,
    len: u64,
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        let modified_secs = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(Self {
            modified_secs,
            len: meta.len(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    stamp: FileStamp,
    info: CodexSessionInfo,
}

/// Bumped whenever the scanner starts reporting a field differently, so entries
/// written by an older build are discarded rather than served as though the new
/// field had been absent from the file.
const FORMAT_VERSION: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    version: u32,
    entries: HashMap<PathBuf, Entry>,
}

static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    CACHE.get_or_init(|| Mutex::new(load_from_disk()))
}

fn cache_path() -> Option<PathBuf> {
    Some(crate::settings::config_root()?.join("scan-cache.json"))
}

fn load_from_disk() -> HashMap<PathBuf, Entry> {
    // Tests share this one process-wide map, so starting them from the
    // developer's real cache would make what they see depend on what is in the
    // developer's sessions directory. `retain_and_persist` skips its write for
    // the same reason: nothing here touches the real file during a test run.
    if cfg!(test) {
        return HashMap::new();
    }
    cache_path().map(|p| load_from(&p)).unwrap_or_default()
}

fn load_from(path: &Path) -> HashMap<PathBuf, Entry> {
    let Ok(raw) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    match serde_json::from_str::<Persisted>(&raw) {
        Ok(p) if p.version == FORMAT_VERSION => p.entries,
        // A cache written by an older build, or a truncated file. Starting over
        // costs one slow scan; trusting it could show wrong metadata forever.
        _ => HashMap::new(),
    }
}

/// The scan result for `path`, if the file is byte-for-byte what was scanned
/// before.
pub fn get(path: &Path) -> Option<CodexSessionInfo> {
    let stamp = FileStamp::of(path)?;
    let guard = cache().lock().ok()?;
    let entry = guard.get(path)?;
    (entry.stamp == stamp).then(|| entry.info.clone())
}

/// Record what scanning `path` produced.
pub fn put(path: &Path, info: &CodexSessionInfo) {
    let Some(stamp) = FileStamp::of(path) else {
        return;
    };
    if let Ok(mut guard) = cache().lock() {
        guard.insert(
            path.to_path_buf(),
            Entry {
                stamp,
                info: info.clone(),
            },
        );
    }
}

/// Drop entries for files under `root` that the walk no longer found, then write
/// the cache out. Called once per completed discovery so a deleted session does
/// not keep an entry — and its successor does not inherit one — forever.
///
/// Only entries under `root` are considered. The picker can be pointed at a
/// different sessions directory, and pruning everything a given walk did not see
/// would throw away the other directory's entries every time the user switched.
pub fn retain_and_persist(root: &Path, seen: &[PathBuf]) {
    let snapshot = {
        let Ok(mut guard) = cache().lock() else {
            return;
        };
        let keep: std::collections::HashSet<&PathBuf> = seen.iter().collect();
        guard.retain(|path, _| !path.starts_with(root) || keep.contains(path));
        guard.clone()
    };
    if !cfg!(test) {
        if let Some(path) = cache_path() {
            persist_to(&path, &snapshot);
        }
    }
}

fn persist_to(path: &Path, entries: &HashMap<PathBuf, Entry>) {
    let payload = Persisted {
        version: FORMAT_VERSION,
        entries: entries.clone(),
    };
    let Ok(json) = serde_json::to_vec(&payload) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    // Write beside the target and rename, so a reader never sees a half-written
    // cache. A failure here is not worth reporting: the next scan just runs cold.
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, &json).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every test below works in its own temp directory, so the paths it puts in
    // the shared map are its alone. Nothing clears the map between tests: a
    // clear would race tests running in parallel and delete their entries.

    fn info_for(path: &Path, turns: u32) -> CodexSessionInfo {
        CodexSessionInfo {
            path: path.to_string_lossy().to_string(),
            turn_count: turns,
            ..Default::default()
        }
    }

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
    }

    #[test]
    fn an_unchanged_file_is_served_from_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("rollout-a.jsonl");
        write(&file, "one\n");

        put(&file, &info_for(&file, 7));

        assert_eq!(get(&file).map(|i| i.turn_count), Some(7));
    }

    #[test]
    fn appending_to_a_file_invalidates_its_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("rollout-b.jsonl");
        write(&file, "one\n");
        put(&file, &info_for(&file, 1));

        // A longer file is a different file as far as the stamp is concerned,
        // even inside the same filesystem second.
        write(&file, "one\ntwo\n");

        assert!(get(&file).is_none());
    }

    #[test]
    fn a_file_that_has_never_been_scanned_is_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("rollout-c.jsonl");
        write(&file, "one\n");

        assert!(get(&file).is_none());
    }

    #[test]
    fn a_missing_file_is_a_miss_rather_than_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("gone.jsonl");

        assert!(get(&file).is_none());
        put(&file, &info_for(&file, 1));
        assert!(get(&file).is_none());
    }

    #[test]
    fn a_persisted_cache_survives_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("rollout-d.jsonl");
        write(&file, "one\n");
        let stamp = FileStamp::of(&file).unwrap();
        let on_disk = tmp.path().join("scan-cache.json");

        let mut entries = HashMap::new();
        entries.insert(
            file.clone(),
            Entry {
                stamp,
                info: info_for(&file, 11),
            },
        );
        persist_to(&on_disk, &entries);

        // What a fresh process would read back.
        let reloaded = load_from(&on_disk);
        assert_eq!(reloaded.get(&file).map(|e| e.info.turn_count), Some(11));
    }

    #[test]
    fn a_cache_written_by_an_older_build_is_discarded() {
        let tmp = tempfile::tempdir().unwrap();
        let on_disk = tmp.path().join("scan-cache.json");
        fs::write(
            &on_disk,
            r#"{"version":0,"entries":{"/a":{"stamp":{"modified_secs":1,"len":2},"info":{}}}}"#,
        )
        .unwrap();

        assert!(load_from(&on_disk).is_empty());
    }

    #[test]
    fn a_corrupt_cache_file_is_discarded_rather_than_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let on_disk = tmp.path().join("scan-cache.json");
        fs::write(&on_disk, "{not json").unwrap();

        assert!(load_from(&on_disk).is_empty());
    }

    #[test]
    fn retaining_drops_entries_for_files_the_walk_did_not_see() {
        let tmp = tempfile::tempdir().unwrap();
        let kept = tmp.path().join("rollout-kept.jsonl");
        let dropped = tmp.path().join("rollout-dropped.jsonl");
        write(&kept, "one\n");
        write(&dropped, "one\n");
        put(&kept, &info_for(&kept, 1));
        put(&dropped, &info_for(&dropped, 2));

        retain_and_persist(tmp.path(), &[kept.clone()]);

        assert!(get(&kept).is_some());
        assert!(get(&dropped).is_none());
    }

    #[test]
    fn retaining_leaves_another_directorys_entries_alone() {
        let walked = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let here = walked.path().join("rollout-here.jsonl");
        let there = elsewhere.path().join("rollout-there.jsonl");
        write(&here, "one\n");
        write(&there, "one\n");
        put(&here, &info_for(&here, 1));
        put(&there, &info_for(&there, 2));

        // Walking one sessions directory must not cost the user a cold re-scan
        // of the other one next time they switch back to it.
        retain_and_persist(walked.path(), &[here.clone()]);

        assert!(get(&there).is_some());
    }
}
