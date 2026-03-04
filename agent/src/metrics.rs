//! Prometheus Metrics 收集模块
//!
//! 暴露 Ark Agent 的指标供 Prometheus 抓取

use ark_core::event::EventType;
use ark_core::graph::StateGraph;
use prometheus::{
    register_counter_vec, register_gauge_vec, register_histogram_vec, CounterVec, GaugeVec,
    HistogramVec, TextEncoder,
};
use std::sync::Arc;

/// Metrics 收集器
pub struct MetricsCollector {
    // 基础指标
    graph_nodes_total: GaugeVec,
    graph_edges_total: GaugeVec,
    events_processed_total: CounterVec,
    probe_errors_total: CounterVec,

    // 详细指标
    process_resource_usage: GaugeVec,
    process_wait_time_seconds: HistogramVec,
    error_count: CounterVec,
    rule_matches_total: CounterVec,
}

impl MetricsCollector {
    /// 创建新的 Metrics 收集器
    pub fn new() -> Result<Self, prometheus::Error> {
        Ok(Self {
            // 基础指标
            graph_nodes_total: register_gauge_vec!(
                "ark_graph_nodes_total",
                "图中节点总数",
                &["node_type"]
            )?,
            graph_edges_total: register_gauge_vec!(
                "ark_graph_edges_total",
                "图中边总数",
                &["edge_type"]
            )?,
            events_processed_total: register_counter_vec!(
                "ark_events_processed_total",
                "已处理事件总数",
                &["event_type"]
            )?,
            probe_errors_total: register_counter_vec!(
                "ark_probe_errors_total",
                "探针错误计数",
                &["probe_name"]
            )?,

            // 详细指标
            process_resource_usage: register_gauge_vec!(
                "ark_process_resource_usage",
                "进程资源使用",
                &["pid", "job_id", "resource_type", "metric"]
            )?,
            process_wait_time_seconds: register_histogram_vec!(
                "ark_process_wait_time_seconds",
                "进程等待时间",
                &["pid", "job_id", "resource_type"],
                vec![0.001, 0.01, 0.1, 1.0, 10.0, 60.0, 300.0]
            )?,
            error_count: register_counter_vec!(
                "ark_error_count",
                "错误计数",
                &["error_type", "node_id"]
            )?,
            rule_matches_total: register_counter_vec!(
                "ark_rule_matches_total",
                "规则匹配次数",
                &["rule_name"]
            )?,
        })
    }

    /// 更新图指标（从 StateGraph 收集）
    pub async fn update_graph_metrics(&self, graph: &Arc<StateGraph>) {
        let nodes = graph.get_nodes_async().await;
        let edges = graph.get_all_edges_async().await;

        // 统计节点类型
        let mut node_counts = std::collections::HashMap::new();
        for node in nodes.values() {
            let node_type = match node.node_type {
                ark_core::graph::NodeType::Process => "process",
                ark_core::graph::NodeType::Resource => "resource",
                ark_core::graph::NodeType::Error => "error",
            };
            *node_counts.entry(node_type).or_insert(0) += 1;
        }

        // 更新节点指标
        for (node_type, count) in node_counts {
            self.graph_nodes_total
                .with_label_values(&[node_type])
                .set(count as f64);
        }

        // 统计边类型
        let mut edge_counts = std::collections::HashMap::new();
        for edge in &edges {
            let edge_type = match edge.edge_type {
                ark_core::graph::EdgeType::Consumes => "consumes",
                ark_core::graph::EdgeType::WaitsOn => "waits_on",
                ark_core::graph::EdgeType::BlockedBy => "blocked_by",
            };
            *edge_counts.entry(edge_type).or_insert(0) += 1;
        }

        // 更新边指标
        for (edge_type, count) in edge_counts {
            self.graph_edges_total
                .with_label_values(&[edge_type])
                .set(count as f64);
        }
    }

    /// 记录事件处理
    pub fn record_event(&self, event_type: &EventType) {
        let event_type_str = match event_type {
            EventType::ComputeUtil => "compute_util",
            EventType::ComputeMem => "compute_mem",
            EventType::TransportBw => "transport_bw",
            EventType::TransportDrop => "transport_drop",
            EventType::StorageIops => "storage_iops",
            EventType::StorageQDepth => "storage_qdepth",
            EventType::ProcessState => "process_state",
            EventType::ErrorHw => "error_hw",
            EventType::ErrorNet => "error_net",
            EventType::TopoLinkDown => "topo_link_down",
            EventType::IntentRun => "intent_run",
            EventType::ActionExec => "action_exec",
        };

        self.events_processed_total
            .with_label_values(&[event_type_str])
            .inc();
    }

    /// 记录探针错误
    pub fn record_probe_error(&self, probe_name: &str) {
        self.probe_errors_total
            .with_label_values(&[probe_name])
            .inc();
    }

    /// 更新进程资源使用指标
    pub fn update_process_resource(
        &self,
        pid: u32,
        job_id: Option<&str>,
        resource_type: &str,
        metric: &str,
        value: f64,
    ) {
        let job_id_str = job_id.unwrap_or("unknown");
        self.process_resource_usage
            .with_label_values(&[&pid.to_string(), job_id_str, resource_type, metric])
            .set(value);
    }

    /// 记录进程等待时间
    pub fn record_process_wait_time(
        &self,
        pid: u32,
        job_id: Option<&str>,
        resource_type: &str,
        seconds: f64,
    ) {
        let job_id_str = job_id.unwrap_or("unknown");
        self.process_wait_time_seconds
            .with_label_values(&[&pid.to_string(), job_id_str, resource_type])
            .observe(seconds);
    }

    /// 记录错误
    pub fn record_error(&self, error_type: &str, node_id: &str) {
        self.error_count
            .with_label_values(&[error_type, node_id])
            .inc();
    }

    /// 记录规则匹配
    pub fn record_rule_match(&self, rule_name: &str) {
        self.rule_matches_total
            .with_label_values(&[rule_name])
            .inc();
    }

    /// 生成 Prometheus 格式的指标输出
    pub fn gather(&self) -> Result<String, prometheus::Error> {
        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        encoder.encode_to_string(&metric_families)
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new().expect("Failed to create MetricsCollector")
    }
}
