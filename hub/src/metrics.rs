//! Prometheus Metrics 收集模块（Hub 端）
//!
//! 暴露 Ark Hub 的指标供 Prometheus 抓取
#![allow(dead_code)]

use ark_core::graph::StateGraph;
use ark_core::rules::RuleLoadStats;
use prometheus::{
    register_counter, register_counter_vec, register_gauge, register_gauge_vec,
    register_histogram_vec, Counter, CounterVec, Gauge, GaugeVec, HistogramVec, TextEncoder,
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
    rules_loaded_total: Gauge,
    rules_skipped_total: Gauge,
    rules_invalid_total: Gauge,
    rules_skipped_reason_total: CounterVec,
    rules_legacy_total: Gauge,
    rules_legacy_migratable_total: Gauge,
    rules_legacy_unsupported_total: Gauge,
    wal_replayed_total: Counter,
    wal_replay_corrupted_lines_total: Counter,
    wal_replay_dedup_dropped_total: Counter,
    wal_replay_process_failed_total: Counter,
    wal_append_errors_total: Counter,
    wal_rotations_total: Counter,
    wal_size_bytes: Gauge,
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
            rules_loaded_total: register_gauge!("ark_rules_loaded_total", "规则加载成功总数")?,
            rules_skipped_total: register_gauge!("ark_rules_skipped_total", "规则加载跳过总数")?,
            rules_invalid_total: register_gauge!(
                "ark_rules_invalid_total",
                "规则加载无效总数（当前等于 skipped）"
            )?,
            rules_skipped_reason_total: register_counter_vec!(
                "ark_rules_skipped_reason_total",
                "按原因统计规则跳过次数",
                &["reason"]
            )?,
            rules_legacy_total: register_gauge!("ark_rules_legacy_total", "legacy 语法规则总数")?,
            rules_legacy_migratable_total: register_gauge!(
                "ark_rules_legacy_migratable_total",
                "legacy 语法中可迁移规则数"
            )?,
            rules_legacy_unsupported_total: register_gauge!(
                "ark_rules_legacy_unsupported_total",
                "legacy 语法中不可迁移规则数"
            )?,
            wal_replayed_total: register_counter!(
                "ark_hub_wal_replayed_total",
                "Hub 启动时从 WAL 成功回放的事件总数"
            )?,
            wal_replay_corrupted_lines_total: register_counter!(
                "ark_hub_wal_replay_corrupted_lines_total",
                "Hub 启动回放 WAL 时遇到的损坏行总数"
            )?,
            wal_replay_dedup_dropped_total: register_counter!(
                "ark_hub_wal_replay_dedup_dropped_total",
                "Hub 启动回放 WAL 时被去重窗口丢弃的事件总数"
            )?,
            wal_replay_process_failed_total: register_counter!(
                "ark_hub_wal_replay_process_failed_total",
                "Hub 启动回放 WAL 时处理失败事件总数"
            )?,
            wal_append_errors_total: register_counter!(
                "ark_hub_wal_append_errors_total",
                "Hub 追加写 WAL 失败总数"
            )?,
            wal_rotations_total: register_counter!(
                "ark_hub_wal_rotations_total",
                "Hub WAL 轮转总次数"
            )?,
            wal_size_bytes: register_gauge!(
                "ark_hub_wal_size_bytes",
                "Hub 当前 WAL 文件大小（字节）"
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

    /// 记录规则加载统计
    pub fn record_rule_load_stats(&self, stats: &RuleLoadStats) {
        self.rules_loaded_total.set(stats.loaded_rules as f64);
        self.rules_skipped_total.set(stats.skipped_rules as f64);
        self.rules_invalid_total.set(stats.skipped_rules as f64);
        self.rules_legacy_total.set(stats.legacy_total as f64);
        self.rules_legacy_migratable_total
            .set(stats.legacy_migratable_total as f64);
        self.rules_legacy_unsupported_total
            .set(stats.legacy_unsupported_total as f64);
        for (reason, count) in &stats.skipped_by_reason {
            self.rules_skipped_reason_total
                .with_label_values(&[reason])
                .inc_by(*count as f64);
        }
    }

    pub fn record_wal_replayed(&self, count: usize) {
        if count > 0 {
            self.wal_replayed_total.inc_by(count as f64);
        }
    }

    pub fn record_wal_replay_corrupted_lines(&self, count: usize) {
        if count > 0 {
            self.wal_replay_corrupted_lines_total.inc_by(count as f64);
        }
    }

    pub fn record_wal_replay_dedup_dropped(&self, count: usize) {
        if count > 0 {
            self.wal_replay_dedup_dropped_total.inc_by(count as f64);
        }
    }

    pub fn record_wal_replay_process_failed(&self, count: usize) {
        if count > 0 {
            self.wal_replay_process_failed_total.inc_by(count as f64);
        }
    }

    pub fn record_wal_append_error(&self) {
        self.wal_append_errors_total.inc();
    }

    pub fn record_wal_rotation(&self) {
        self.wal_rotations_total.inc();
    }

    pub fn update_wal_size_bytes(&self, size_bytes: u64) {
        self.wal_size_bytes.set(size_bytes as f64);
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
