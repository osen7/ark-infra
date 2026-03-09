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
) -> Result<usize, Box<dyn std::error::Error>> {
    if !Path::new(path).exists() {
        return Ok(0);
    }

    let file = File::open(path).await?;
    let mut reader = BufReader::new(file).lines();
    let mut replayed = 0usize;

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: Event = match serde_json::from_str(trimmed) {
            Ok(event) => event,
            Err(e) => {
                eprintln!("[hub] 跳过损坏 WAL 行: {}", e);
                continue;
            }
        };

        if !accept_event_with_dedup(&event, &dedup_store, dedup_window_ms).await {
            continue;
        }

        if let Err(e) = graph.process_event(&event).await {
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
        replayed += 1;
    }

    Ok(replayed)
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
