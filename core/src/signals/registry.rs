use super::signal::{Aggregator, SignalSpec, WindowSpec};

#[derive(Debug, Clone)]
pub struct SignalRegistry {
    specs: Vec<SignalSpec>,
}

impl SignalRegistry {
    pub fn default_mvp() -> Self {
        Self {
            specs: vec![
                SignalSpec {
                    name: "network.tcp.retransmit_rate_1m",
                    window: WindowSpec {
                        window_ms: 60_000,
                        step_ms: 1_000,
                    },
                    aggregator: Aggregator::CounterRate,
                    unit: "count/s",
                },
                SignalSpec {
                    name: "hardware.gpu.util_avg_1m",
                    window: WindowSpec {
                        window_ms: 60_000,
                        step_ms: 1_000,
                    },
                    aggregator: Aggregator::GaugeAvg,
                    unit: "percent",
                },
            ],
        }
    }

    pub fn specs(&self) -> &[SignalSpec] {
        &self.specs
    }
}
