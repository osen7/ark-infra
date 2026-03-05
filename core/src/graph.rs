use crate::event::{Event, EventType};
use crate::signals::{SignalEngine, SignalRegistry, SignalValue};
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

const EDGE_TTL_MS: u64 = 60 * 60 * 1000;
const MAX_EDGES: usize = 100_000;
const DEFAULT_CLEANUP_INTERVAL_MS: u64 = 30_000;

/// 三大推导边类型
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EdgeType {
    Consumes,  // 进程 PID 消耗某物理资源
    WaitsOn,   // 进程 PID 正在等待某网络/存储资源完成
    BlockedBy, // 资源/进程被某个 Error 彻底阻塞（根因）
}

/// 图中的边
#[derive(Debug, Clone)]
pub struct Edge {
    pub edge_type: EdgeType,
    pub from: String, // 源节点ID
    pub to: String,   // 目标节点ID
    pub ts: u64,      // 事件时间戳
}

/// 节点状态
#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub node_type: NodeType,
    pub last_update: u64,
    pub metadata: HashMap<String, String>, // 存储额外信息（如利用率、状态等）
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeType {
    Process,  // 进程节点
    Resource, // 资源节点（GPU、网络、存储等）
    Error,    // 错误节点
}

/// 状态图：基于事件流构建的实时因果图
pub struct StateGraph {
    nodes: RwLock<HashMap<String, Node>>,
    edges: RwLock<Vec<Edge>>,
    edge_index: RwLock<HashSet<EdgeKey>>,
    signals: RwLock<SignalEngine>,
    error_window_ms: u64, // 错误窗口时间（默认5分钟）
    cleanup_interval_ms: u64,
    last_cleanup_ts: RwLock<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EdgeKey {
    edge_type: EdgeType,
    from: String,
    to: String,
}

#[derive(Debug, Clone)]
pub struct GraphMetrics {
    pub nodes_total: usize,
    pub edges_total: usize,
    pub edges_by_type: HashMap<String, usize>,
}

impl StateGraph {
    /// 创建新的状态图
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            edges: RwLock::new(Vec::new()),
            edge_index: RwLock::new(HashSet::new()),
            signals: RwLock::new(SignalEngine::new(SignalRegistry::default_mvp())),
            error_window_ms: 5 * 60 * 1000, // 5分钟
            cleanup_interval_ms: DEFAULT_CLEANUP_INTERVAL_MS,
            last_cleanup_ts: RwLock::new(0),
        }
    }

    /// 根据 event.node_id 为节点 ID 添加命名空间前缀
    /// 如果 event.node_id 存在，返回 "{node_id}::{node_id}"，否则返回原 ID
    fn namespace_node_id(&self, event: &Event, node_id: &str) -> String {
        if let Some(ref node_id_prefix) = event.node_id {
            format!("{}::{}", node_id_prefix, node_id)
        } else {
            node_id.to_string()
        }
    }

    /// 处理事件，更新图状态
    pub async fn process_event(&self, event: &Event) -> Result<(), String> {
        match event.event_type {
            EventType::ProcessState => {
                self.handle_process_state(event).await?;
            }
            EventType::ComputeUtil | EventType::ComputeMem => {
                self.handle_compute_event(event).await?;
            }
            EventType::TransportBw | EventType::TransportDrop => {
                self.handle_transport_event(event).await?;
            }
            EventType::StorageIops | EventType::StorageQDepth => {
                self.handle_storage_event(event).await?;
            }
            EventType::ErrorHw | EventType::ErrorNet => {
                self.handle_error_event(event).await?;
            }
            EventType::TopoLinkDown => {
                self.handle_topo_event(event).await?;
            }
            _ => {
                // IntentRun, ActionExec 等其他事件类型暂不处理
            }
        }

        // 周期性清理，避免每条事件都触发重清理
        let should_cleanup = {
            let last_cleanup = *self.last_cleanup_ts.read().await;
            last_cleanup == 0 || event.ts.saturating_sub(last_cleanup) >= self.cleanup_interval_ms
        };
        if should_cleanup {
            self.cleanup_old_errors(event.ts).await;
            *self.last_cleanup_ts.write().await = event.ts;
        }
        self.update_signals(event).await;

        Ok(())
    }

    async fn update_signals(&self, event: &Event) {
        let points = {
            let mut engine = self.signals.write().await;
            engine.on_event(event)
        };
        if points.is_empty() {
            return;
        }

        let mut nodes = self.nodes.write().await;
        for point in points {
            let signal_node_id = format!("signal::{}::{}", point.name, point.entity.id);
            let mut metadata = HashMap::new();
            if let SignalValue::Number(v) = point.value {
                metadata.insert("value".to_string(), v.to_string());
            }
            metadata.insert("window_ms".to_string(), point.window_ms.to_string());
            metadata.insert("unit".to_string(), point.unit);

            nodes.insert(
                signal_node_id.clone(),
                Node {
                    id: signal_node_id,
                    node_type: NodeType::Resource,
                    last_update: point.ts,
                    metadata,
                },
            );
        }
    }

    /// 处理进程状态事件
    async fn handle_process_state(&self, event: &Event) -> Result<(), String> {
        if let Some(pid) = event.pid {
            let pid_str = format!("pid-{}", pid);
            let pid_str = self.namespace_node_id(event, &pid_str);
            let mut nodes = self.nodes.write().await;

            if event.value == "start" {
                // 创建进程节点
                let mut metadata = HashMap::new();
                if let Some(ref job_id) = event.job_id {
                    metadata.insert("job_id".to_string(), job_id.clone());
                }
                metadata.insert("state".to_string(), "running".to_string());

                nodes.insert(
                    pid_str.clone(),
                    Node {
                        id: pid_str.clone(),
                        node_type: NodeType::Process,
                        last_update: event.ts,
                        metadata,
                    },
                );
            } else if event.value == "exit" || event.value == "zombie" {
                // 移除进程节点（或标记为已退出）
                if let Some(node) = nodes.get_mut(&pid_str) {
                    node.metadata
                        .insert("state".to_string(), event.value.clone());
                    node.last_update = event.ts;
                }
            }
        }
        Ok(())
    }

    /// 处理计算资源事件（GPU利用率等）
    async fn handle_compute_event(&self, event: &Event) -> Result<(), String> {
        let mut nodes = self.nodes.write().await;
        let mut edges = self.edges.write().await;
        let mut edge_index = self.edge_index.write().await;

        // 确保资源节点存在（应用命名空间）
        let resource_id = self.namespace_node_id(event, &event.entity_id);
        if !nodes.contains_key(&resource_id) {
            nodes.insert(
                resource_id.clone(),
                Node {
                    id: resource_id.clone(),
                    node_type: NodeType::Resource,
                    last_update: event.ts,
                    metadata: HashMap::new(),
                },
            );
        }

        // 更新资源状态
        if let Some(node) = nodes.get_mut(&resource_id) {
            node.metadata
                .insert("util".to_string(), event.value.clone());
            node.last_update = event.ts;
        }

        // 如果有 PID，建立 Consumes 边
        if let Some(pid) = event.pid {
            let pid_str = format!("pid-{}", pid);
            let pid_str = self.namespace_node_id(event, &pid_str);

            // 确保进程节点存在
            if !nodes.contains_key(&pid_str) {
                nodes.insert(
                    pid_str.clone(),
                    Node {
                        id: pid_str.clone(),
                        node_type: NodeType::Process,
                        last_update: event.ts,
                        metadata: HashMap::new(),
                    },
                );
            }

            upsert_edge(
                &mut edges,
                &mut edge_index,
                Edge {
                    edge_type: EdgeType::Consumes,
                    from: pid_str,
                    to: resource_id.clone(),
                    ts: event.ts,
                },
            );
        }

        Ok(())
    }

    /// 处理传输事件（网络等）
    async fn handle_transport_event(&self, event: &Event) -> Result<(), String> {
        let mut nodes = self.nodes.write().await;
        let mut edges = self.edges.write().await;
        let mut edge_index = self.edge_index.write().await;

        // 确保资源节点存在
        // 对于 transport.drop 事件，entity_id 格式可能是 "network-pid-<PID>" 或 "eth0" 等
        let resource_id_base = if event.entity_id.starts_with("network-") {
            // eBPF 探针输出的格式：network-pid-<PID>
            // 我们提取网络资源标识（可以是网卡名或通用网络资源）
            if let Some(_pid_from_entity) = event.entity_id.strip_prefix("network-pid-") {
                // 如果有 PID，使用通用网络资源标识
                "network".to_string()
            } else {
                event.entity_id.clone()
            }
        } else {
            event.entity_id.clone()
        };
        let resource_id = self.namespace_node_id(event, &resource_id_base);

        if !nodes.contains_key(&resource_id) {
            nodes.insert(
                resource_id.clone(),
                Node {
                    id: resource_id.clone(),
                    node_type: NodeType::Resource,
                    last_update: event.ts,
                    metadata: HashMap::new(),
                },
            );
        }

        // 更新资源状态
        if let Some(node) = nodes.get_mut(&resource_id) {
            let key = match event.event_type {
                EventType::TransportBw => "bw",
                EventType::TransportDrop => "drop",
                _ => "unknown",
            };
            node.metadata.insert(key.to_string(), event.value.clone());
            node.last_update = event.ts;
        }

        // 处理 transport.drop 事件：建立 WaitsOn 边
        // 这是诊断闭环的关键：网络重传 -> 进程阻塞
        if event.event_type == EventType::TransportDrop {
            // 从事件中提取 PID
            let pid = if let Some(pid) = event.pid {
                pid
            } else if let Some(pid_str) = event.entity_id.strip_prefix("network-pid-") {
                // 如果 entity_id 是 "network-pid-<PID>" 格式，提取 PID
                pid_str.parse::<u32>().unwrap_or(0)
            } else {
                0
            };

            if pid > 0 {
                let pid_str = format!("pid-{}", pid);
                let pid_str = self.namespace_node_id(event, &pid_str);

                // 确保进程节点存在
                if !nodes.contains_key(&pid_str) {
                    nodes.insert(
                        pid_str.clone(),
                        Node {
                            id: pid_str.clone(),
                            node_type: NodeType::Process,
                            last_update: event.ts,
                            metadata: {
                                let mut m = HashMap::new();
                                m.insert("state".to_string(), "running".to_string());
                                m
                            },
                        },
                    );
                }

                let inserted = upsert_edge(
                    &mut edges,
                    &mut edge_index,
                    Edge {
                        edge_type: EdgeType::WaitsOn,
                        from: pid_str.clone(),
                        to: resource_id.clone(),
                        ts: event.ts,
                    },
                );
                if inserted {
                    // 日志输出（用于调试）
                    eprintln!(
                        "🔗 [图引擎] 建立阻塞关联: {} WaitsOn {} (transport.drop)",
                        pid_str, resource_id
                    );
                }
            }
        }

        // 处理 TransportBw 事件（带宽低时也可能阻塞）
        if event.event_type == EventType::TransportBw {
            if let Some(pid) = event.pid {
                let should_create_waitson = event.value.contains("IO_WAIT")
                    || event.value.parse::<f64>().unwrap_or(1000.0) < 1.0;

                if should_create_waitson {
                    let pid_str = format!("pid-{}", pid);
                    let pid_str = self.namespace_node_id(event, &pid_str);

                    if !nodes.contains_key(&pid_str) {
                        nodes.insert(
                            pid_str.clone(),
                            Node {
                                id: pid_str.clone(),
                                node_type: NodeType::Process,
                                last_update: event.ts,
                                metadata: HashMap::new(),
                            },
                        );
                    }

                    upsert_edge(
                        &mut edges,
                        &mut edge_index,
                        Edge {
                            edge_type: EdgeType::WaitsOn,
                            from: pid_str,
                            to: resource_id.clone(),
                            ts: event.ts,
                        },
                    );
                }
            }
        }

        Ok(())
    }

    /// 处理存储事件
    async fn handle_storage_event(&self, event: &Event) -> Result<(), String> {
        // 类似 handle_transport_event 的逻辑
        self.handle_transport_event(event).await
    }

    /// 处理错误事件
    async fn handle_error_event(&self, event: &Event) -> Result<(), String> {
        let mut nodes = self.nodes.write().await;
        let mut edges = self.edges.write().await;
        let mut edge_index = self.edge_index.write().await;

        let error_id_base = format!("error-{}", event.entity_id);
        let error_id = self.namespace_node_id(event, &error_id_base);

        // 创建错误节点
        if !nodes.contains_key(&error_id) {
            nodes.insert(
                error_id.clone(),
                Node {
                    id: error_id.clone(),
                    node_type: NodeType::Error,
                    last_update: event.ts,
                    metadata: {
                        let mut m = HashMap::new();
                        m.insert("error_type".to_string(), event.value.clone());
                        m
                    },
                },
            );
        }

        // 找到所有使用该资源的进程，建立 BlockedBy 边
        let resource_id_base = event.entity_id.clone();
        let resource_id = self.namespace_node_id(event, &resource_id_base);
        let affected_pids: Vec<String> = {
            edges
                .iter()
                .filter(|e| e.edge_type == EdgeType::Consumes && e.to == resource_id)
                .map(|e| e.from.clone())
                .collect()
        };

        for pid_str in affected_pids {
            upsert_edge(
                &mut edges,
                &mut edge_index,
                Edge {
                    edge_type: EdgeType::BlockedBy,
                    from: pid_str,
                    to: error_id.clone(),
                    ts: event.ts,
                },
            );
        }

        Ok(())
    }

    /// 处理拓扑事件
    async fn handle_topo_event(&self, event: &Event) -> Result<(), String> {
        // 拓扑降级事件可以视为错误的一种
        self.handle_error_event(event).await
    }

    /// 清理过期的错误节点和边（只保留近 error_window_ms 的错误）
    async fn cleanup_old_errors(&self, current_ts: u64) {
        let mut nodes = self.nodes.write().await;
        let mut edges = self.edges.write().await;
        let mut edge_index = self.edge_index.write().await;

        let cutoff_ts = current_ts.saturating_sub(self.error_window_ms);

        // 移除过期的错误节点
        let error_ids: Vec<String> = nodes
            .iter()
            .filter(|(_, node)| node.node_type == NodeType::Error && node.last_update < cutoff_ts)
            .map(|(id, _)| id.clone())
            .collect();

        for error_id in &error_ids {
            nodes.remove(error_id);
        }

        // 移除相关的 BlockedBy 边
        edges.retain(|e| !(e.edge_type == EdgeType::BlockedBy && error_ids.contains(&e.to)));

        // 清理非活跃进程（超过10分钟未更新）
        // 重要：只清理明确标记为 exit/zombie 的进程，不清理稳态运行的进程
        // 即使长时间没有事件更新，只要状态是 running，就保留（可能是稳态工作负载）
        let process_cutoff = current_ts.saturating_sub(10 * 60 * 1000);
        let dead_pids: Vec<String> = nodes
            .iter()
            .filter(|(_, node)| {
                if node.node_type != NodeType::Process {
                    return false;
                }

                // 只清理明确退出的进程，或者长时间未更新且状态不是 running 的进程
                let state = node.metadata.get("state");
                let is_explicitly_dead =
                    state == Some(&"exit".to_string()) || state == Some(&"zombie".to_string());

                let is_stale_non_running =
                    node.last_update < process_cutoff && state != Some(&"running".to_string());

                is_explicitly_dead || is_stale_non_running
            })
            .map(|(id, _)| id.clone())
            .collect();

        for pid in &dead_pids {
            nodes.remove(pid);
        }

        // 清理相关的边
        edges.retain(|e| !dead_pids.contains(&e.from) && !dead_pids.contains(&e.to));

        // 全局 TTL 清理，避免边无界增长
        let edge_cutoff = current_ts.saturating_sub(EDGE_TTL_MS);
        edges.retain(|e| e.ts >= edge_cutoff);

        // 容量保护，保留最新边防止异常流量导致 OOM
        if edges.len() > MAX_EDGES {
            edges.sort_by_key(|e| std::cmp::Reverse(e.ts));
            edges.truncate(MAX_EDGES);
        }
        rebuild_edge_index(&edges, &mut edge_index);

        // 注意：资源节点（Resource）不会被清理，即使长时间没有更新
        // 因为资源可能处于稳态（如 GPU 利用率保持 100%），需要探针发送心跳事件来维持
    }

    pub async fn metrics_snapshot(&self) -> GraphMetrics {
        let nodes = self.nodes.read().await;
        let edges = self.edges.read().await;
        let mut edges_by_type = HashMap::new();

        for edge in edges.iter() {
            let ty = match edge.edge_type {
                EdgeType::Consumes => "consumes",
                EdgeType::WaitsOn => "waits_on",
                EdgeType::BlockedBy => "blocked_by",
            };
            *edges_by_type.entry(ty.to_string()).or_insert(0) += 1;
        }

        GraphMetrics {
            nodes_total: nodes.len(),
            edges_total: edges.len(),
            edges_by_type,
        }
    }

    /// 获取所有活跃进程
    pub async fn get_active_processes(&self) -> Vec<Node> {
        let nodes = self.nodes.read().await;
        nodes
            .values()
            .filter(|node| {
                node.node_type == NodeType::Process
                    && node.metadata.get("state") != Some(&"exit".to_string())
                    && node.metadata.get("state") != Some(&"zombie".to_string())
            })
            .cloned()
            .collect()
    }

    /// 获取进程消耗的资源
    pub async fn get_process_resources(&self, pid: u32) -> Vec<String> {
        let pid_str = format!("pid-{}", pid);
        let edges = self.edges.read().await;
        edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Consumes && e.from == pid_str)
            .map(|e| e.to.clone())
            .collect()
    }

    /// 逆向深度优先搜索：查找进程阻塞的根因（通过 PID）
    pub async fn find_root_cause(&self, pid: u32) -> Vec<String> {
        let pid_str = format!("pid-{}", pid);
        self.find_root_cause_by_id(&pid_str).await
    }

    /// 逆向深度优先搜索：查找进程阻塞的根因（通过完整节点 ID，支持命名空间）
    /// 这是集群模式下的标准方法，可以直接处理 "node-a::pid-1234" 格式的节点 ID
    pub async fn find_root_cause_by_id(&self, node_id: &str) -> Vec<String> {
        let edges = self.edges.read().await;
        let nodes = self.nodes.read().await;
        let mut visited = HashSet::new();
        let mut causes = Vec::new();

        self.dfs_backward(node_id, &edges, &nodes, &mut visited, &mut causes);

        causes
    }

    fn dfs_backward(
        &self,
        node_id: &str,
        edges: &[Edge],
        nodes: &HashMap<String, Node>,
        visited: &mut HashSet<String>,
        causes: &mut Vec<String>,
    ) {
        if visited.contains(node_id) {
            return;
        }
        visited.insert(node_id.to_string());

        // 查找指向当前节点的 BlockedBy 边
        for edge in edges.iter() {
            if edge.edge_type == EdgeType::BlockedBy && edge.from == node_id {
                if let Some(node) = nodes.get(&edge.to) {
                    if node.node_type == NodeType::Error {
                        let error_desc = format!(
                            "{}: {}",
                            edge.to,
                            node.metadata
                                .get("error_type")
                                .unwrap_or(&"未知错误".to_string())
                        );
                        causes.push(error_desc);
                    }
                    // 继续递归查找
                    self.dfs_backward(&edge.to, edges, nodes, visited, causes);
                }
            }
        }

        // 查找 WaitsOn 边
        for edge in edges.iter() {
            if edge.edge_type == EdgeType::WaitsOn && edge.from == node_id {
                causes.push(format!("等待资源: {}", edge.to));
            }
        }
    }

    /// 异步获取所有边（用于规则匹配）
    pub async fn get_all_edges_async(&self) -> Vec<Edge> {
        self.edges.read().await.clone()
    }

    /// 异步获取所有节点（用于场景分析）
    pub async fn get_nodes_async(&self) -> HashMap<String, Node> {
        self.nodes.read().await.clone()
    }
}

impl Default for StateGraph {
    fn default() -> Self {
        Self::new()
    }
}

fn edge_key(edge: &Edge) -> EdgeKey {
    EdgeKey {
        edge_type: edge.edge_type.clone(),
        from: edge.from.clone(),
        to: edge.to.clone(),
    }
}

fn upsert_edge(edges: &mut Vec<Edge>, edge_index: &mut HashSet<EdgeKey>, edge: Edge) -> bool {
    let key = edge_key(&edge);
    if edge_index.insert(key) {
        edges.push(edge);
        true
    } else {
        false
    }
}

fn rebuild_edge_index(edges: &[Edge], edge_index: &mut HashSet<EdgeKey>) {
    edge_index.clear();
    for edge in edges {
        edge_index.insert(edge_key(edge));
    }
}

#[cfg(test)]
mod tests {
    use super::StateGraph;
    use crate::event::{Event, EventType};

    #[tokio::test]
    async fn cleanup_applies_edge_ttl() {
        let graph = StateGraph::new();

        let old_event = Event {
            ts: 1_000,
            event_type: EventType::TransportDrop,
            entity_id: "network-pid-1001".to_string(),
            job_id: Some("job-1".to_string()),
            pid: Some(1001),
            value: "retransmit".to_string(),
            node_id: Some("node-a".to_string()),
        };
        graph
            .process_event(&old_event)
            .await
            .expect("process old event");

        let new_event = Event {
            ts: 1_000 + super::EDGE_TTL_MS + 1,
            event_type: EventType::TransportDrop,
            entity_id: "network-pid-1002".to_string(),
            job_id: Some("job-2".to_string()),
            pid: Some(1002),
            value: "retransmit".to_string(),
            node_id: Some("node-a".to_string()),
        };
        graph
            .process_event(&new_event)
            .await
            .expect("process new event");

        let metrics = graph.metrics_snapshot().await;
        assert!(
            metrics.edges_total <= 2,
            "old edges should be cleaned by TTL, edges_total={}",
            metrics.edges_total
        );
    }

    #[tokio::test]
    async fn cleanup_runs_on_interval() {
        let graph = StateGraph::new();

        let start_event = Event {
            ts: 1_000,
            event_type: EventType::ProcessState,
            entity_id: "proc".to_string(),
            job_id: Some("job-1".to_string()),
            pid: Some(2001),
            value: "start".to_string(),
            node_id: Some("node-a".to_string()),
        };
        graph
            .process_event(&start_event)
            .await
            .expect("process start event");

        let exit_event = Event {
            ts: 2_000, // within cleanup interval, should not trigger cleanup
            event_type: EventType::ProcessState,
            entity_id: "proc".to_string(),
            job_id: Some("job-1".to_string()),
            pid: Some(2001),
            value: "exit".to_string(),
            node_id: Some("node-a".to_string()),
        };
        graph
            .process_event(&exit_event)
            .await
            .expect("process exit event");

        let nodes_after_exit = graph.get_nodes_async().await;
        assert!(
            nodes_after_exit.contains_key("node-a::pid-2001"),
            "cleanup should not run before interval"
        );

        let trigger_cleanup_event = Event {
            ts: 32_000, // exceed default 30s cleanup interval
            event_type: EventType::ComputeUtil,
            entity_id: "gpu-0".to_string(),
            job_id: Some("job-2".to_string()),
            pid: Some(3001),
            value: "80".to_string(),
            node_id: Some("node-a".to_string()),
        };
        graph
            .process_event(&trigger_cleanup_event)
            .await
            .expect("process cleanup trigger event");

        let nodes_after_cleanup = graph.get_nodes_async().await;
        assert!(
            !nodes_after_cleanup.contains_key("node-a::pid-2001"),
            "cleanup should remove exited process after interval"
        );
    }
}
