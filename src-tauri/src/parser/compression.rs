use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

// zstd frame magic: 0xFD2FB528 stored little-endian => bytes [0x28, 0xB5, 0x2F, 0xFD]
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Open a session file for streaming, line-by-line reads, transparently
/// decompressing zstd-compressed files.
///
/// Unlike [`read_session_file`], this never loads the whole file into memory.
/// The discovery scan only needs the first line plus a light pass over event
/// types, so streaming keeps peak memory bounded to a single line even for
/// multi-hundred-megabyte session files.
pub fn open_session_reader(path: &Path) -> Result<Box<dyn BufRead>, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;

    // Peek the first 4 bytes to detect the zstd frame magic, then rewind.
    let mut magic = [0u8; 4];
    let read = read_up_to(&mut file, &mut magic).map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;

    if read == 4 && magic == ZSTD_MAGIC {
        let decoder = zstd::stream::read::Decoder::new(file).map_err(|e| format!("zstd: {e}"))?;
        Ok(Box::new(BufReader::new(decoder)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

/// Fill `buf` from `reader`, tolerating short reads. Returns the number of
/// bytes actually read (which may be less than `buf.len()` at EOF).
fn read_up_to<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match reader.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// Resolve the actual on-disk path for a rollout session file.
///
/// Codex's background compression worker can replace a cold plain `rollout-*.jsonl`
/// file with a `rollout-*.jsonl.zst` sibling and remove the original (see
/// `codex-rs/rollout/src/compression.rs` upstream). A watcher holding the original
/// plain path would otherwise find it gone and silently stop updating. Returns
/// `None` if neither representation exists.
pub fn resolve_rollout_path(path: &Path) -> Option<PathBuf> {
    if path.exists() {
        return Some(path.to_path_buf());
    }
    let name = path.file_name()?.to_str()?;
    if !name.ends_with(".jsonl.zst") {
        let compressed = path.with_file_name(format!("{name}.zst"));
        if compressed.exists() {
            return Some(compressed);
        }
    }
    None
}

/// Read a session file that may be plain text or zstd-compressed.
/// Detects the zstd frame magic and decompresses transparently.
pub fn read_session_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.starts_with(&ZSTD_MAGIC) {
        let decompressed = zstd::decode_all(bytes.as_slice()).map_err(|e| format!("zstd: {e}"))?;
        String::from_utf8(decompressed).map_err(|e| format!("zstd utf-8: {e}"))
    } else {
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn compress_zstd(data: &[u8]) -> Vec<u8> {
        zstd::encode_all(data, 3).expect("zstd compress failed")
    }

    #[test]
    fn read_plain_text_unchanged() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("plain.jsonl");
        let content = r#"{"type":"session_meta","payload":{"id":"abc"}}"#;
        std::fs::write(&path, content).unwrap();
        let result = read_session_file(&path).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn read_zstd_compressed_decompresses() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("compressed.jsonl");
        let content = r#"{"type":"session_meta","payload":{"id":"compressed-session"}}"#;
        let compressed = compress_zstd(content.as_bytes());
        std::fs::write(&path, &compressed).unwrap();
        let result = read_session_file(&path).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn read_zstd_multiline_jsonl() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("multi.jsonl");
        let content = [
            r#"{"timestamp":"2026-06-04T00:00:00Z","type":"session_meta","payload":{"id":"zstd-session","timestamp":"2026-06-04T00:00:00Z"}}"#,
            r#"{"timestamp":"2026-06-04T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            r#"{"timestamp":"2026-06-04T00:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","completed_at":1748995202.0}}"#,
        ]
        .join("\n");
        let compressed = compress_zstd(content.as_bytes());
        std::fs::write(&path, &compressed).unwrap();
        let result = read_session_file(&path).unwrap();
        assert_eq!(result, content);
    }

    fn read_lines_via_stream(path: &Path) -> Vec<String> {
        open_session_reader(path)
            .unwrap()
            .lines()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn stream_plain_text_lines() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("plain.jsonl");
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"abc"}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        assert_eq!(read_lines_via_stream(&path), lines.to_vec());
    }

    #[test]
    fn stream_zstd_compressed_lines() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("compressed.jsonl");
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"compressed-session"}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#,
        ];
        let compressed = compress_zstd(lines.join("\n").as_bytes());
        std::fs::write(&path, &compressed).unwrap();
        assert_eq!(read_lines_via_stream(&path), lines.to_vec());
    }

    #[test]
    fn stream_matches_full_read_for_crlf_and_trailing_newline() {
        // BufRead::lines strips trailing \r and the final newline, matching
        // str::lines used elsewhere. Verify both readers agree.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("crlf.jsonl");
        let raw = "{\"a\":1}\r\n{\"b\":2}\n";
        std::fs::write(&path, raw).unwrap();

        let streamed = read_lines_via_stream(&path);
        let slurped: Vec<String> = read_session_file(&path)
            .unwrap()
            .lines()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(streamed, slurped);
        assert_eq!(
            streamed,
            vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]
        );
    }

    #[test]
    fn stream_handles_empty_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("empty.jsonl");
        std::fs::write(&path, b"").unwrap();
        assert!(read_lines_via_stream(&path).is_empty());
    }

    #[test]
    fn open_session_reader_errors_on_missing_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("does-not-exist.jsonl");
        assert!(open_session_reader(&path).is_err());
    }

    #[test]
    fn resolve_rollout_path_returns_plain_path_when_it_exists() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-plain.jsonl");
        std::fs::write(&path, "content").unwrap();
        assert_eq!(resolve_rollout_path(&path), Some(path));
    }

    #[test]
    fn resolve_rollout_path_falls_back_to_compressed_sibling() {
        // Issue #209: a watched session that goes cold can be replaced by Codex's
        // compression worker with a `.jsonl.zst` sibling, removing the plain file.
        let tmp = tempdir().unwrap();
        let plain_path = tmp.path().join("rollout-cold.jsonl");
        let compressed_path = tmp.path().join("rollout-cold.jsonl.zst");
        std::fs::write(&compressed_path, compress_zstd(b"content")).unwrap();
        assert_eq!(resolve_rollout_path(&plain_path), Some(compressed_path));
    }

    // Codex v0.171.0 (issue #407) cites four upstream PRs — #61222 (store rollout trace
    // payloads in compressed shards), #61253 (move rollout trace recording to a background
    // worker), #61896 (bound rollout trace capture with configurable storage limits), #62092
    // (skip recent rollouts before reading headers during compression) — as changing how
    // Codex persists "rollout/session trace data" into compressed shards.
    //
    // Diffing each PR's changed files against openai/codex shows the first three (#61222,
    // #61253, #61896) touch only the `codex-rs/rollout-trace` crate: the same opt-in,
    // same-binary-replay-only diagnostic bundle feature already audited for issue #356 (see
    // `v0166_rollout_trace_crate_changes_do_not_affect_session_jsonl_parsing` in `turn.rs`).
    // It is inactive unless `CODEX_ROLLOUT_TRACE_ROOT` is set and writes its own separate
    // bundle (`manifest.json`/`trace.jsonl`/`payloads/*.zst`/`state.json`), not the
    // `~/.codex/sessions/*.jsonl` file codex-trace reads.
    //
    // The fourth PR, #62092, does touch the real `codex-rs/rollout/src/compression.rs` that
    // backs this file's `resolve_rollout_path`/`open_session_reader`. Reading its diff shows
    // the change only skips reading a rollout's header during the background compression scan
    // when the file is younger than seven days, to avoid bumping its access time on relatime
    // mounts — it does not alter the on-disk format, file naming, or directory layout. Rollout
    // files are still either plain `.jsonl` or single-file `.jsonl.zst`, exactly as already
    // handled here (see `resolve_rollout_path_falls_back_to_compressed_sibling`, added for
    // issue #209).
    //
    // codex-trace is unaffected. This test documents that a plain-text rollout file — standing
    // in for one younger than seven days, which #62092 now leaves unscanned for longer — still
    // resolves and reads correctly.
    #[test]
    fn v0171_compression_skip_recent_rollouts_does_not_affect_resolution() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-recent.jsonl");
        let content = r#"{"type":"session_meta","payload":{"id":"recent-session"}}"#;
        std::fs::write(&path, content).unwrap();

        assert_eq!(resolve_rollout_path(&path), Some(path.clone()));
        assert_eq!(read_session_file(&path).unwrap(), content);
    }

    #[test]
    fn resolve_rollout_path_returns_none_when_neither_representation_exists() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-gone.jsonl");
        assert_eq!(resolve_rollout_path(&path), None);
    }
}
