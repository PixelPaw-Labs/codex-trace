//! Reading the sessions directory in the background.
//!
//! Discovery has to open every session file: the turn count, token totals and whether a
//! session is still running are only knowable from its last line. On a real directory
//! that is thousands of files and tens of gigabytes, and one of those files can be 22GB
//! on its own — minutes of solid reading before the picker had a single row to draw.
//!
//! So the walk runs here, on its own thread, and the app reads whatever it has got so
//! far. Three things follow from that:
//!
//! - **It reports as it goes**, in bytes as well as files, from inside the read of a
//!   single file as well as between files. A progress bar that only moved between files
//!   would sit still for minutes on the 22GB one.
//! - **It holds itself back.** Reading flat out takes a whole core for the duration and
//!   makes the machine — including this app's own window — feel slow, so the walk rests
//!   in proportion to the reading it just did.
//! - **It saves as it goes.** The scan cache is written every few seconds, so closing
//!   the window part-way through keeps the work rather than starting over next launch.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

use crate::parser::discover::{self, CodexSessionInfo, IndexProgress};
use crate::parser::scan_cache;
use crate::state::SseEvent;

/// How long a finished index stays good before a walk is started to refresh it. Short:
/// a warm re-walk is stat-only and costs almost nothing.
const REFRESH_AFTER: Duration = Duration::from_secs(2);

/// How often a running walk says how far it has got.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// How often a running walk publishes the sessions it has read, for the picker to draw.
/// Longer than the progress tick: each publish copies and re-sorts everything read so
/// far, and costs the frontend a re-fetch of every row on screen.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(3);

/// How often a running walk writes what it has read to the on-disk scan cache.
const PERSIST_INTERVAL: Duration = Duration::from_secs(10);

/// The share of one core a walk may use. Reading a whole sessions directory is minutes
/// of solid work; taking a core for it is what made the app feel like it had hung.
const DUTY_CYCLE: f64 = 0.2;

/// How much reading to do between pauses. Short enough that the pauses are invisible,
/// long enough not to spend them all on the cost of sleeping.
const WORK_SLICE: Duration = Duration::from_millis(40);

/// A pause is never longer than this, however slow the preceding read was, so a walk
/// cannot disappear for seconds at a time.
const MAX_PAUSE: Duration = Duration::from_millis(400);

/// How long to rest after `worked` spent reading, to hold a walk to [`DUTY_CYCLE`] of a
/// core.
fn throttle_pause(worked: Duration) -> Duration {
    worked
        .mul_f64((1.0 - DUTY_CYCLE) / DUTY_CYCLE)
        .min(MAX_PAUSE)
}

/// What a walk has read for one directory.
struct Indexed {
    dir: String,
    at: Instant,
    sessions: Vec<CodexSessionInfo>,
    progress: IndexProgress,
}

/// The walk currently running, if any.
struct Run {
    dir: String,
    /// Bumped every time a walk starts. A walk whose generation is no longer the current
    /// one has been superseded — the sessions directory changed under it — and stops at
    /// its next report rather than spending minutes on a directory nobody is looking at.
    generation: u64,
}

pub struct Indexer {
    indexed: Mutex<Option<Indexed>>,
    run: Mutex<Option<Run>>,
    events: broadcast::Sender<SseEvent>,
}

impl Indexer {
    pub fn new(events: broadcast::Sender<SseEvent>) -> Self {
        Self {
            indexed: Mutex::new(None),
            run: Mutex::new(None),
            events,
        }
    }

    /// The sessions known for `dir` right now, and how far the walk has got.
    ///
    /// Never waits for a walk. The first call starts one and returns an empty list; each
    /// later call returns more of the directory, newest day first, until `progress.done`.
    pub fn snapshot(
        indexer: &Arc<Self>,
        dir: &str,
        app: Option<AppHandle>,
    ) -> (Vec<CodexSessionInfo>, IndexProgress) {
        let held = match indexer.indexed.lock() {
            Ok(indexed) => indexed
                .as_ref()
                .filter(|i| i.dir == dir)
                .map(|i| (i.sessions.clone(), i.progress.clone(), i.at.elapsed())),
            Err(_) => None,
        };

        match held {
            // In hand and recent enough to serve as is.
            Some((sessions, progress, age)) if progress.done && age < REFRESH_AFTER => {
                (sessions, progress)
            }
            // Either a walk is still running, or what we hold has gone stale and one
            // should start. Serve what there is either way: a stale list beats a blank
            // one, and a warm re-walk lands in a moment.
            Some((sessions, progress, _)) => {
                Self::start(indexer, dir.to_string(), app);
                (sessions, progress)
            }
            None => {
                Self::start(indexer, dir.to_string(), app);
                (Vec::new(), IndexProgress::default())
            }
        }
    }

    /// Start walking `dir` in the background, unless that is already happening.
    pub fn start(indexer: &Arc<Self>, dir: String, app: Option<AppHandle>) {
        let generation = {
            let Ok(mut run) = indexer.run.lock() else {
                return;
            };
            if run.as_ref().is_some_and(|current| current.dir == dir) {
                return;
            }
            let generation = run.as_ref().map(|r| r.generation).unwrap_or(0) + 1;
            *run = Some(Run {
                dir: dir.clone(),
                generation,
            });
            generation
        };

        let indexer = Arc::clone(indexer);
        std::thread::spawn(move || indexer.walk(dir, generation, app));
    }

    /// Whether this walk is still the one the app wants.
    fn is_current(&self, generation: u64) -> bool {
        self.run
            .lock()
            .map(|run| run.as_ref().is_some_and(|r| r.generation == generation))
            .unwrap_or(false)
    }

    /// Read `dir` end to end, publishing as it goes. Runs on its own thread.
    fn walk(&self, dir: String, generation: u64, app: Option<AppHandle>) {
        let path = std::path::PathBuf::from(&dir);
        let started = Instant::now();
        let (total_files, total_bytes) = discover::measure_session_files(&path);
        self.publish(
            &dir,
            None,
            IndexProgress {
                total_files,
                total_bytes,
                ..IndexProgress::default()
            },
            &app,
        );

        let mut last_progress = Instant::now();
        let mut last_publish = Instant::now();
        let mut last_persist = Instant::now();
        let mut worked = Duration::ZERO;
        let mut slice_started = Instant::now();

        let scanned = discover::discover_sessions_streaming(&path, |update| {
            if !self.is_current(generation) {
                return false;
            }
            worked += slice_started.elapsed();

            let progress = IndexProgress {
                files_read: update.files_read,
                // A directory written while it is walked can hold more than the count
                // found, and a bar reading past its own end looks broken.
                total_files: total_files.max(update.files_read),
                bytes_read: update.bytes_read,
                total_bytes: total_bytes.max(update.bytes_read),
                done: false,
            };
            if last_publish.elapsed() >= PUBLISH_INTERVAL {
                last_publish = Instant::now();
                last_progress = Instant::now();
                self.publish(
                    &dir,
                    Some(discover::finalize(update.sessions)),
                    progress,
                    &app,
                );
            } else if last_progress.elapsed() >= PROGRESS_INTERVAL {
                last_progress = Instant::now();
                self.publish(&dir, None, progress, &app);
            }

            if last_persist.elapsed() >= PERSIST_INTERVAL {
                last_persist = Instant::now();
                scan_cache::persist_progress();
            }

            // Rest in proportion to the reading just done. Files served from the scan
            // cache cost microseconds, so a warm re-walk barely pauses; it is the cold
            // reads that get held back.
            if worked >= WORK_SLICE {
                std::thread::sleep(throttle_pause(worked));
                worked = Duration::ZERO;
            }
            slice_started = Instant::now();
            true
        });

        let Ok(sessions) = scanned else { return };
        if !self.is_current(generation) {
            return;
        }
        eprintln!(
            "Indexed {} sessions ({:.1} GB) in {:.0}s",
            sessions.len(),
            total_bytes as f64 / 1e9,
            started.elapsed().as_secs_f64()
        );
        self.publish(
            &dir,
            Some(sessions),
            IndexProgress {
                files_read: total_files,
                total_files,
                bytes_read: total_bytes,
                total_bytes,
                done: true,
            },
            &app,
        );
        if let Ok(mut run) = self.run.lock() {
            if run.as_ref().is_some_and(|r| r.generation == generation) {
                *run = None;
            }
        }
    }

    /// Keep a walk's latest results and tell the frontend about them.
    ///
    /// `sessions` is `None` for a progress-only tick: the bar moves several times
    /// between publishes, and re-sending the list each time would put the whole picker
    /// on the wire four times a second.
    fn publish(
        &self,
        dir: &str,
        sessions: Option<Vec<CodexSessionInfo>>,
        progress: IndexProgress,
        app: &Option<AppHandle>,
    ) {
        let published_sessions = sessions.is_some();
        if let Ok(mut indexed) = self.indexed.lock() {
            match indexed.as_mut() {
                Some(existing) if existing.dir == dir => {
                    if let Some(sessions) = sessions {
                        existing.sessions = sessions;
                        existing.at = Instant::now();
                    }
                    existing.progress = progress.clone();
                }
                _ => {
                    *indexed = Some(Indexed {
                        dir: dir.to_string(),
                        at: Instant::now(),
                        sessions: sessions.unwrap_or_default(),
                        progress: progress.clone(),
                    })
                }
            }
        }

        let payload = serde_json::to_string(&progress).unwrap_or_else(|_| "{}".to_string());
        self.send("index-progress", &payload, app, &progress);

        // Only when there is more of the directory to draw. The picker answers this by
        // re-fetching every row it is showing, far too expensive to do on each tick.
        if published_sessions {
            self.send("picker-refresh", "{}", app, &serde_json::json!({}));
        }
    }

    /// Both ways the frontend can be listening: SSE for a browser, Tauri's own event
    /// bridge for the desktop window.
    fn send<T: serde::Serialize + Clone>(
        &self,
        event: &str,
        sse_payload: &str,
        app: &Option<AppHandle>,
        payload: &T,
    ) {
        let _ = self.events.send(SseEvent {
            event: event.to_string(),
            data: sse_payload.to_string(),
        });
        if let Some(app) = app {
            let _ = app.emit(event, payload.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexer() -> Arc<Indexer> {
        Arc::new(Indexer::new(broadcast::channel(16).0))
    }

    /// A directory of `count` one-line sessions, all on the same day.
    fn sessions_dir(count: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("2026/09/18");
        std::fs::create_dir_all(&day).unwrap();
        for i in 0..count {
            std::fs::write(
                day.join(format!("rollout-2026-09-18T09-00-{i:02}-s{i}.jsonl")),
                format!(
                    r#"{{"timestamp":"2026-09-18T09:00:00Z","type":"session_meta","payload":{{"id":"s{i}","timestamp":"2026-09-18T09:00:00Z","cwd":"/tmp"}}}}"#
                ),
            )
            .unwrap();
        }
        dir
    }

    /// Poll until the walk reports itself finished, or give up.
    fn wait_for_index(indexer: &Arc<Indexer>, dir: &str) -> IndexProgress {
        for _ in 0..300 {
            let (_, progress) = Indexer::snapshot(indexer, dir, None);
            if progress.done {
                return progress;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the background walk never finished");
    }

    #[test]
    fn a_snapshot_answers_before_the_directory_has_been_read() {
        // The whole point: the first call comes back at once with a walk running behind
        // it, rather than sitting on the caller until every file has been read.
        let indexer = indexer();
        let dir = sessions_dir(5);
        let path = dir.path().to_string_lossy().to_string();

        let (sessions, progress) = Indexer::snapshot(&indexer, &path, None);
        assert!(sessions.is_empty(), "nothing read yet");
        assert!(!progress.done);

        let progress = wait_for_index(&indexer, &path);
        let (sessions, _) = Indexer::snapshot(&indexer, &path, None);
        assert_eq!(sessions.len(), 5);
        assert_eq!(progress.files_read, 5);
        assert_eq!(progress.total_files, 5);
        assert!(progress.bytes_read > 0);
        assert_eq!(progress.bytes_read, progress.total_bytes);
    }

    #[test]
    fn a_directory_with_nothing_in_it_finishes_rather_than_running_forever() {
        let indexer = indexer();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();

        Indexer::snapshot(&indexer, &path, None);

        let progress = wait_for_index(&indexer, &path);
        assert_eq!(progress.files_read, 0);
        assert!(progress.done);
    }

    #[test]
    fn a_second_ask_for_the_same_directory_joins_the_walk_already_running() {
        let indexer = indexer();
        let dir = sessions_dir(3);
        let path = dir.path().to_string_lossy().to_string();

        Indexer::snapshot(&indexer, &path, None);
        let generation = indexer.run.lock().unwrap().as_ref().unwrap().generation;
        Indexer::snapshot(&indexer, &path, None);

        assert_eq!(
            indexer
                .run
                .lock()
                .unwrap()
                .as_ref()
                .map(|r| r.generation)
                .unwrap_or(generation),
            generation,
            "a rival walk was started over the same directory"
        );
        wait_for_index(&indexer, &path);
    }

    #[test]
    fn a_throttled_walk_rests_longer_than_it_works() {
        // At a fifth of a core, 40ms of reading buys 160ms of rest.
        assert_eq!(
            throttle_pause(Duration::from_millis(40)),
            Duration::from_millis(160)
        );
    }

    #[test]
    fn a_walk_that_did_no_reading_does_not_rest() {
        // Files served from the scan cache cost microseconds, so a warm re-walk must not
        // sit there sleeping between them.
        assert_eq!(throttle_pause(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn no_single_pause_stalls_the_walk() {
        // One 22GB file can take minutes; its pause must not be minutes long too.
        assert_eq!(throttle_pause(Duration::from_secs(60)), MAX_PAUSE);
    }
}
