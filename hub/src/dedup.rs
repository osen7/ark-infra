use ark_core::event::Event;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::Mutex;

pub type DedupStore = Arc<Mutex<HashMap<u64, u64>>>;

pub async fn accept_event_with_dedup(
    event: &Event,
    dedup_store: &DedupStore,
    window_ms: u64,
) -> bool {
    let key = event_fingerprint(event);
    let mut store = dedup_store.lock().await;
    let cutoff = event.ts.saturating_sub(window_ms);
    store.retain(|_, ts| *ts >= cutoff);
    if store.contains_key(&key) {
        return false;
    }
    store.insert(key, event.ts);
    true
}

fn event_fingerprint(event: &Event) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    event.ts.hash(&mut hasher);
    event.event_type.to_string().hash(&mut hasher);
    event.entity_id.hash(&mut hasher);
    event.job_id.hash(&mut hasher);
    event.pid.hash(&mut hasher);
    event.value.hash(&mut hasher);
    event.node_id.hash(&mut hasher);
    hasher.finish()
}
