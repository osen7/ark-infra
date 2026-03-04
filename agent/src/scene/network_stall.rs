use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType};
use ark_core::graph::{EdgeType, StateGraph};

/// 网络阻塞场景分析器
pub struct NetworkStallAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for NetworkStallAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::NetworkStall
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 查找 WaitsOn 网络资源的边
        let mut network_wait_count = 0;
        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::WaitsOn {
                if edge.to.starts_with("network-") || edge.to.contains("net") {
                    network_wait_count += 1;
                    root_causes.push(format!("等待网络资源: {}", edge.to));

                    if let Some(node) = nodes.get(&edge.to) {
                        if let Some(drop_rate) = node.metadata.get("drop_rate") {
                            if let Ok(rate) = drop_rate.parse::<f64>() {
                                if rate > 10.0 {
                                    root_causes
                                        .push(format!("网络 {} 丢包率过高: {:.1}%", edge.to, rate));
                                }
                            }
                        }
                    }
                }
            }
        }

        // 查找网络错误
        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::BlockedBy {
                if edge.to.starts_with("network-") || edge.to.contains("net") {
                    if let Some(node) = nodes.get(&edge.to) {
                        if node.id.contains("error") {
                            root_causes.push(format!("网络错误: {}", node.id));
                        }
                    }
                }
            }
        }

        if root_causes.is_empty() {
            root_causes.push("网络可能阻塞".to_string());
        }

        recommendations.push("检查网络带宽使用情况".to_string());
        recommendations.push("检查网络丢包统计".to_string());
        recommendations.push("检查 RDMA 连接状态（如果使用）".to_string());

        let mut recommended_actions = Vec::new();
        recommended_actions.push("检查交换机 PFC 配置".to_string());
        recommended_actions.push("检查 RoCE/HCCS 连接状态".to_string());

        AnalysisResult {
            scene: SceneType::NetworkStall,
            root_causes,
            confidence: if network_wait_count > 0 { 0.85 } else { 0.6 },
            recommendations,
            recommended_actions,
            severity: crate::scene::types::Severity::Warning,
        }
    }
}
