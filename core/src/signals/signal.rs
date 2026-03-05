use crate::event::{Event, EventType};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone)]
pub enum SignalValue {
    Number(f64),
    Bool(bool),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityKind {
    Node,
}

#[derive(Debug, Clone)]
pub struct EntityRef {
    pub kind: EntityKind,
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct SignalPoint {
    pub name: String,
    pub ts: u64,
    pub entity: EntityRef,
    pub value: SignalValue,
    pub window_ms: u64,
    pub unit: String,
}

#[derive(Debug, Clone, Copy)]
pub struct WindowSpec {
    pub window_ms: u64,
    pub step_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum Aggregator {
    CounterRate,
    GaugeAvg,
}

#[derive(Debug, Clone)]
pub struct SignalSpec {
    pub name: &'static str,
    pub window: WindowSpec,
    pub aggregator: Aggregator,
    pub unit: &'static str,
}

#[derive(Debug)]
enum SignalState {
    CounterTimestamps { events: VecDeque<u64> },
    GaugeSamples { samples: VecDeque<(u64, f64)> },
}

impl SignalState {
    fn for_aggregator(aggregator: Aggregator) -> Self {
        match aggregator {
            Aggregator::CounterRate => SignalState::CounterTimestamps {
                events: VecDeque::new(),
            },
            Aggregator::GaugeAvg => SignalState::GaugeSamples {
                samples: VecDeque::new(),
            },
        }
    }
}

pub struct SignalEngine {
    registry: super::SignalRegistry,
    // key = <signal_name>::<entity_id>
    states: HashMap<String, SignalState>,
    last_emit_ts: HashMap<String, u64>,
}

impl SignalEngine {
    pub fn new(registry: super::SignalRegistry) -> Self {
        Self {
            registry,
            states: HashMap::new(),
            last_emit_ts: HashMap::new(),
        }
    }

    pub fn on_event(&mut self, event: &Event) -> Vec<SignalPoint> {
        let mut out = Vec::new();
        for spec in self.registry.specs().iter().cloned() {
            let sample = extract_sample(&spec, event);
            let Some(sample) = sample else {
                continue;
            };

            let entity_id = entity_from_event(event);
            let key = format!("{}::{}", spec.name, entity_id);
            let state = self
                .states
                .entry(key.clone())
                .or_insert_with(|| SignalState::for_aggregator(spec.aggregator));

            match spec.aggregator {
                Aggregator::CounterRate => {
                    if let SignalState::CounterTimestamps { events } = state {
                        events.push_back(event.ts);
                        trim_counter(events, event.ts, spec.window.window_ms);
                    }
                }
                Aggregator::GaugeAvg => {
                    if let SignalState::GaugeSamples { samples } = state {
                        samples.push_back((event.ts, sample));
                        trim_gauge(samples, event.ts, spec.window.window_ms);
                    }
                }
            }

            let should_emit = match self.last_emit_ts.get(&key) {
                None => true,
                Some(last) => event.ts.saturating_sub(*last) >= spec.window.step_ms,
            };
            if !should_emit {
                continue;
            }

            if let Some(value) = compute_value(state, spec.aggregator, spec.window.window_ms) {
                out.push(SignalPoint {
                    name: spec.name.to_string(),
                    ts: event.ts,
                    entity: EntityRef {
                        kind: EntityKind::Node,
                        id: entity_id.clone(),
                    },
                    value: SignalValue::Number(value),
                    window_ms: spec.window.window_ms,
                    unit: spec.unit.to_string(),
                });
                self.last_emit_ts.insert(key, event.ts);
            }
        }

        out
    }
}

fn extract_sample(spec: &SignalSpec, event: &Event) -> Option<f64> {
    match spec.name {
        "network.tcp.retransmit_rate_1m" => {
            if event.event_type != EventType::TransportDrop {
                return None;
            }
            let lower = event.value.to_ascii_lowercase();
            if lower.contains("retransmit") || lower.contains("drop") {
                Some(1.0)
            } else {
                None
            }
        }
        "hardware.gpu.util_avg_1m" => {
            if event.event_type != EventType::ComputeUtil {
                return None;
            }
            event.value.parse::<f64>().ok()
        }
        _ => None,
    }
}

fn entity_from_event(event: &Event) -> String {
    if let Some(node_id) = &event.node_id {
        return node_id.clone();
    }
    "local".to_string()
}

fn trim_counter(events: &mut VecDeque<u64>, now_ts: u64, window_ms: u64) {
    let cutoff = now_ts.saturating_sub(window_ms);
    while let Some(ts) = events.front().copied() {
        if ts < cutoff {
            events.pop_front();
        } else {
            break;
        }
    }
}

fn trim_gauge(samples: &mut VecDeque<(u64, f64)>, now_ts: u64, window_ms: u64) {
    let cutoff = now_ts.saturating_sub(window_ms);
    while let Some((ts, _)) = samples.front().copied() {
        if ts < cutoff {
            samples.pop_front();
        } else {
            break;
        }
    }
}

fn compute_value(state: &SignalState, aggregator: Aggregator, window_ms: u64) -> Option<f64> {
    match (state, aggregator) {
        (SignalState::CounterTimestamps { events }, Aggregator::CounterRate) => {
            let per_sec = events.len() as f64 / (window_ms as f64 / 1000.0);
            Some(per_sec)
        }
        (SignalState::GaugeSamples { samples }, Aggregator::GaugeAvg) => {
            if samples.is_empty() {
                return None;
            }
            let sum: f64 = samples.iter().map(|(_, v)| v).sum();
            Some(sum / samples.len() as f64)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_rate_signal_emits() {
        let registry = super::super::SignalRegistry::default_mvp();
        let mut engine = SignalEngine::new(registry);

        let mut last = Vec::new();
        for i in 0..3_u64 {
            let event = Event {
                ts: i * 1000,
                event_type: EventType::TransportDrop,
                entity_id: "eth0".to_string(),
                job_id: None,
                pid: Some(1),
                value: "retransmit".to_string(),
                node_id: Some("node-a".to_string()),
            };
            last = engine.on_event(&event);
        }

        assert!(!last.is_empty());
        let point = last
            .iter()
            .find(|p| p.name == "network.tcp.retransmit_rate_1m")
            .expect("signal exists");
        match point.value {
            SignalValue::Number(v) => assert!(v > 0.0),
            _ => panic!("expected number"),
        }
    }
}
