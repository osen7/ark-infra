use crate::dedup::{accept_event_with_dedup, DedupStore};
use ark_core::event::Event;
use ark_core::graph::StateGraph;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock};

pub struct WalState {
    pub file: File,
    pub path: PathBuf,
    pub max_bytes: u64,
}

pub type WalWriter = Arc<Mutex<WalState>>;

pub struct WalAppendResult {
    pub rotated: bool,
    pub size_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WalReplayStats {
    pub replayed: usize,
    pub corrupted_lines: usize,
    pub dedup_dropped: usize,
    pub process_failed: usize,
}

pub async fn open_wal_writer(
    path: &str,
    wal_max_mb: u64,
) -> Result<WalWriter, Box<dyn std::error::Error>> {
    ensure_parent_dir(path).await?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let max_bytes = wal_max_mb.saturating_mul(1024 * 1024);
    Ok(Arc::new(Mutex::new(WalState {
        file,
        path: PathBuf::from(path),
        max_bytes,
    })))
}

pub async fn replay_wal(
    path: &str,
    graph: Arc<StateGraph>,
    event_buffer: Arc<RwLock<VecDeque<Event>>>,
    event_buffer_size: usize,
    dedup_store: DedupStore,
    dedup_window_ms: u64,
) -> Result<WalReplayStats, Box<dyn std::error::Error>> {
    let active_path = PathBuf::from(path);
    let rotated_path = PathBuf::from(format!("{}.1", path));
    if !active_path.exists() && !rotated_path.exists() {
        return Ok(WalReplayStats::default());
    }

    let mut total = WalReplayStats::default();
    if rotated_path.exists() {
        let stats = replay_wal_file(
            &rotated_path,
            graph.clone(),
            event_buffer.clone(),
            event_buffer_size,
            dedup_store.clone(),
            dedup_window_ms,
        )
        .await?;
        merge_replay_stats(&mut total, stats);
    }
    if active_path.exists() {
        let stats = replay_wal_file(
            &active_path,
            graph,
            event_buffer,
            event_buffer_size,
            dedup_store,
            dedup_window_ms,
        )
        .await?;
        merge_replay_stats(&mut total, stats);
    }

    Ok(total)
}

async fn replay_wal_file(
    path: &Path,
    graph: Arc<StateGraph>,
    event_buffer: Arc<RwLock<VecDeque<Event>>>,
    event_buffer_size: usize,
    dedup_store: DedupStore,
    dedup_window_ms: u64,
) -> Result<WalReplayStats, Box<dyn std::error::Error>> {
    let file = File::open(path).await?;
    let mut reader = BufReader::new(file).lines();
    let mut stats = WalReplayStats::default();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: Event = match serde_json::from_str(trimmed) {
            Ok(event) => event,
            Err(e) => {
                stats.corrupted_lines += 1;
                eprintln!("[hub] 跳过损坏 WAL 行: {}", e);
                continue;
            }
        };

        if !accept_event_with_dedup(&event, &dedup_store, dedup_window_ms).await {
            stats.dedup_dropped += 1;
            continue;
        }

        if let Err(e) = graph.process_event(&event).await {
            stats.process_failed += 1;
            eprintln!("[hub] 回放事件失败: {}", e);
            continue;
        }

        {
            let mut buf = event_buffer.write().await;
            if buf.len() >= event_buffer_size {
                buf.pop_front();
            }
            buf.push_back(event);
        }
        stats.replayed += 1;
    }

    Ok(stats)
}

fn merge_replay_stats(total: &mut WalReplayStats, delta: WalReplayStats) {
    total.replayed += delta.replayed;
    total.corrupted_lines += delta.corrupted_lines;
    total.dedup_dropped += delta.dedup_dropped;
    total.process_failed += delta.process_failed;
}

pub async fn append_wal_event(
    writer: &WalWriter,
    event: &Event,
) -> Result<WalAppendResult, std::io::Error> {
    let mut state = writer.lock().await;
    let mut line = serde_json::to_vec(event)
        .map_err(|e| std::io::Error::other(format!("serialize wal event: {}", e)))?;
    line.push(b'\n');
    let mut rotated = false;
    let current_size = state.file.metadata().await?.len();
    if state.max_bytes > 0 && current_size.saturating_add(line.len() as u64) > state.max_bytes {
        rotate_wal(&mut state).await?;
        rotated = true;
    }
    state.file.write_all(&line).await?;
    let size_bytes = state.file.metadata().await?.len();
    Ok(WalAppendResult {
        rotated,
        size_bytes,
    })
}

async fn ensure_parent_dir(path: &str) -> Result<(), std::io::Error> {
    let p = PathBuf::from(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }
    Ok(())
}

async fn rotate_wal(state: &mut WalState) -> Result<(), std::io::Error> {
    let active_path = state.path.clone();
    let rotated_path = PathBuf::from(format!("{}.1", active_path.display()));

    state.file.flush().await?;
    if rotated_path.exists() {
        fs::remove_file(&rotated_path).await?;
    }
    if active_path.exists() {
        fs::rename(&active_path, &rotated_path).await?;
    }

    state.file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&active_path)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{replay_wal, WalReplayStats};
    use crate::dedup::DedupStore;
    use ark_core::event::{Event, EventType};
    use ark_core::graph::StateGraph;
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::{Mutex, RwLock};

    #[tokio::test]
    async fn replay_wal_tracks_corrupted_and_replayed_lines() {
        let test_file = unique_temp_file("wal-replay-stats");
        let valid = Event {
            ts: 1_741_234_000_000,
            event_type: EventType::TransportDrop,
            entity_id: "mlx5_0".to_string(),
            job_id: Some("job-1".to_string()),
            pid: Some(1234),
            value: "1".to_string(),
            node_id: Some("node-a".to_string()),
        };
        let mut payload = serde_json::to_string(&valid).expect("serialize valid event");
        payload.push('\n');
        payload.push_str("{{bad json line}}\n");
        tokio::fs::write(&test_file, payload)
            .await
            .expect("write wal test file");

        let graph = Arc::new(StateGraph::new());
        let event_buffer = Arc::new(RwLock::new(VecDeque::with_capacity(16)));
        let dedup_store: DedupStore = Arc::new(Mutex::new(HashMap::new()));
        let stats: WalReplayStats = replay_wal(
            test_file.to_str().expect("wal path string"),
            Arc::clone(&graph),
            Arc::clone(&event_buffer),
            16,
            dedup_store,
            30_000,
        )
        .await
        .expect("replay wal");

        assert_eq!(stats.replayed, 1);
        assert_eq!(stats.corrupted_lines, 1);
        assert_eq!(stats.dedup_dropped, 0);
        assert_eq!(stats.process_failed, 0);

        tokio::fs::remove_file(&test_file)
            .await
            .expect("remove wal test file");
    }

    #[tokio::test]
    async fn replay_wal_replays_rotated_file_first() {
        let base = unique_temp_file("wal-replay-rotated");
        let active_path = base.clone();
        let rotated_path = PathBuf::from(format!("{}.1", active_path.display()));

        let old = Event {
            ts: 1_741_233_000_000,
            event_type: EventType::TransportDrop,
            entity_id: "mlx5_0".to_string(),
            job_id: Some("job-old".to_string()),
            pid: Some(1111),
            value: "1".to_string(),
            node_id: Some("node-a".to_string()),
        };
        let new = Event {
            ts: 1_741_234_000_000,
            event_type: EventType::TransportDrop,
            entity_id: "mlx5_0".to_string(),
            job_id: Some("job-new".to_string()),
            pid: Some(2222),
            value: "2".to_string(),
            node_id: Some("node-a".to_string()),
        };

        let mut old_line = serde_json::to_string(&old).expect("serialize old");
        old_line.push('\n');
        tokio::fs::write(&rotated_path, old_line)
            .await
            .expect("write rotated wal file");

        let mut new_line = serde_json::to_string(&new).expect("serialize new");
        new_line.push('\n');
        tokio::fs::write(&active_path, new_line)
            .await
            .expect("write active wal file");

        let graph = Arc::new(StateGraph::new());
        let event_buffer = Arc::new(RwLock::new(VecDeque::with_capacity(16)));
        let dedup_store: DedupStore = Arc::new(Mutex::new(HashMap::new()));
        let stats: WalReplayStats = replay_wal(
            active_path.to_str().expect("active wal path"),
            Arc::clone(&graph),
            Arc::clone(&event_buffer),
            16,
            dedup_store,
            30_000,
        )
        .await
        .expect("replay wal");

        assert_eq!(stats.replayed, 2);
        assert_eq!(stats.corrupted_lines, 0);
        assert_eq!(stats.dedup_dropped, 0);
        assert_eq!(stats.process_failed, 0);

        tokio::fs::remove_file(&active_path)
            .await
            .expect("remove active wal file");
        tokio::fs::remove_file(&rotated_path)
            .await
            .expect("remove rotated wal file");
    }

    fn unique_temp_file(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}.jsonl", prefix, nanos))
    }
}
