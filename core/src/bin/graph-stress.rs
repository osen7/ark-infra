use ark_core::event::{Event, EventType};
use ark_core::graph::StateGraph;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut events: usize = 100_000;
    let mut resources: usize = 8;
    let mut pids: usize = 1024;

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--events" if i + 1 < args.len() => {
                events = args[i + 1].parse().unwrap_or(events);
                i += 2;
            }
            "--resources" if i + 1 < args.len() => {
                resources = args[i + 1].parse().unwrap_or(resources);
                i += 2;
            }
            "--pids" if i + 1 < args.len() => {
                pids = args[i + 1].parse().unwrap_or(pids);
                i += 2;
            }
            "--help" | "-h" => {
                println!("Usage: graph-stress [--events N] [--resources N] [--pids N]");
                return Ok(());
            }
            _ => {
                i += 1;
            }
        }
    }

    let graph = StateGraph::new();
    let start = Instant::now();
    let base_ts = now_ms();

    for n in 0..events {
        let pid = (1000 + (n % pids)) as u32;
        let resource_id = format!("gpu-{}", n % resources);
        let node_id = format!("node-{}", n % 16);
        let event_type = if n % 10 == 0 {
            EventType::TransportDrop
        } else {
            EventType::ComputeUtil
        };
        let event = Event {
            ts: base_ts + n as u64,
            event_type,
            entity_id: resource_id,
            job_id: Some(format!("job-{}", n % 64)),
            pid: Some(pid),
            value: ((n % 100) as u32).to_string(),
            node_id: Some(node_id),
        };
        graph.process_event(&event).await?;
    }

    let elapsed = start.elapsed();
    let metrics = graph.metrics_snapshot().await;
    let eps = events as f64 / elapsed.as_secs_f64().max(0.0001);

    println!("graph_stress.events={}", events);
    println!("graph_stress.elapsed_ms={}", elapsed.as_millis());
    println!("graph_stress.events_per_sec={:.2}", eps);
    println!("graph_stress.nodes_total={}", metrics.nodes_total);
    println!("graph_stress.edges_total={}", metrics.edges_total);
    for (edge_type, count) in metrics.edges_by_type {
        println!("graph_stress.edges_by_type.{}={}", edge_type, count);
    }

    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
