//! Prometheus Metrics 收集模块（Hub 端）
//!
//! 暴露 Ark Hub 的指标供 Prometheus 抓取
#![allow(dead_code)]

use ark_core::graph::StateGraph;
use prometheus::{
    register_counter_vec, register_gauge_vec, register_histogram_vec, CounterVec, GaugeVec,
    HistogramVec, TextEncoder,
};
use std::sync::Arc;

/// Hub Metrics 收集器
pub struct HubMetricsCollector {
    // 基础指标
    global_graph_nodes_total: GaugeVec,
    global_graph_edges_total: GaugeVec,
    events_received_total: CounterVec,
    websocket_connections: GaugeVec,

    // 详细指标
    cluster_query_duration_seconds: HistogramVec,
    cluster_fix_actions_total: CounterVec,
    agent_events_received_total: CounterVec,
}

impl HubMetricsCollector {
    /// 创建新的 Hub Metrics 收集器
    pub fn new() -> Result<Self, prometheus::Error> {
        Ok(Self {
            // 基础指标
            global_graph_nodes_total: register_gauge_vec!(
                "ark_hub_graph_nodes_total",
                "全局图中节点总数",
                &["node_type"]
            )?,
            global_graph_edges_total: register_gauge_vec!(
                "ark_hub_graph_edges_total",
                "全局图中边总数",
                &["edge_type"]
            )?,
            events_received_total: register_counter_vec!(
                "ark_hub_events_received_total",
                "Hub 接收的事件总数",
                &["event_type", "node_id"]
            )?,
            websocket_connections: register_gauge_vec!(
                "ark_hub_websocket_connections",
                "当前 WebSocket 连接数",
                &["status"]
            )?,

            // 详细指标
            cluster_query_duration_seconds: register_histogram_vec!(
                "ark_hub_cluster_query_duration_seconds",
                "集群查询耗时",
                &["query_type"],
                vec![0.001, 0.01, 0.1, 1.0, 5.0, 10.0]
            )?,
            cluster_fix_actions_total: register_counter_vec!(
                "ark_hub_cluster_fix_actions_total",
                "集群修复动作总数",
                &["action_type", "node_id", "result"]
            )?,
            agent_events_received_total: register_counter_vec!(
                "ark_hub_agent_events_received_total",
                "从各 Agent 接收的事件数",
                &["node_id", "event_type"]
            )?,
        })
    }

    /// 更新全局图指标
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
            self.global_graph_nodes_total
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
            self.global_graph_edges_total
                .with_label_values(&[edge_type])
                .set(count as f64);
        }
    }

    /// 记录接收的事件
    pub fn record_event_received(&self, event_type: &str, node_id: &str) {
        self.events_received_total
            .with_label_values(&[event_type, node_id])
            .inc();

        self.agent_events_received_total
            .with_label_values(&[node_id, event_type])
            .inc();
    }

    /// 更新 WebSocket 连接数
    pub fn update_websocket_connections(&self, connected: usize, disconnected: usize) {
        self.websocket_connections
            .with_label_values(&["connected"])
            .set(connected as f64);
        self.websocket_connections
            .with_label_values(&["disconnected"])
            .set(disconnected as f64);
    }

    /// 记录集群查询耗时
    pub fn record_query_duration(&self, query_type: &str, duration_seconds: f64) {
        self.cluster_query_duration_seconds
            .with_label_values(&[query_type])
            .observe(duration_seconds);
    }

    /// 记录集群修复动作
    pub fn record_fix_action(&self, action_type: &str, node_id: &str, result: &str) {
        self.cluster_fix_actions_total
            .with_label_values(&[action_type, node_id, result])
            .inc();
    }

    /// 生成 Prometheus 格式的指标输出
    pub fn gather(&self) -> Result<String, prometheus::Error> {
        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        encoder.encode_to_string(&metric_families)
    }
}

impl Default for HubMetricsCollector {
    fn default() -> Self {
        Self::new().expect("Failed to create HubMetricsCollector")
    }
}
