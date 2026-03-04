use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType, Severity};
use ark_core::graph::{EdgeType, StateGraph};

/// 存储慢速场景分析器
pub struct StorageSlowAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for StorageSlowAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::StorageSlow
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 查找 WaitsOn 存储的边，并检查 IOPS 和延迟
        let mut slow_storage = Vec::new();

        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::WaitsOn {
                if edge.to.contains("storage")
                    || edge.to.contains("disk")
                    || edge.to.contains("nvme")
                {
                    if let Some(node) = nodes.get(&edge.to) {
                        // 检查 IOPS（如果低于阈值）
                        if let Some(iops) = node.metadata.get("iops") {
                            if let Ok(iops_val) = iops.parse::<f64>() {
                                if iops_val < 100.0 {
                                    slow_storage.push((
                                        edge.to.clone(),
                                        format!("IOPS 过低: {:.0}", iops_val),
                                    ));
                                }
                            }
                        }

                        // 检查 IO 延迟
                        if let Some(latency) = node.metadata.get("latency_ms") {
                            if let Ok(latency_val) = latency.parse::<f64>() {
                                if latency_val > 100.0 {
                                    slow_storage.push((
                                        edge.to.clone(),
                                        format!("IO 延迟过高: {:.1}ms", latency_val),
                                    ));
                                }
                            }
                        }

                        // 检查队列深度
                        if let Some(qdepth) = node.metadata.get("qdepth") {
                            if let Ok(qdepth_val) = qdepth.parse::<f64>() {
                                if qdepth_val > 100.0 {
                                    slow_storage.push((
                                        edge.to.clone(),
                                        format!("队列深度过高: {:.0}", qdepth_val),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        if !slow_storage.is_empty() {
            for (storage_id, reason) in &slow_storage {
                root_causes.push(format!("{}: {}", storage_id, reason));
            }
        } else {
            root_causes.push("存储性能可能偏低".to_string());
        }

        recommendations.push("检查存储设备性能基准".to_string());
        recommendations.push("检查是否有其他进程竞争存储资源".to_string());
        recommendations.push("检查存储设备是否过热".to_string());

        let mut recommended_actions = Vec::new();
        recommended_actions.push("使用 iostat 监控存储性能".to_string());
        recommended_actions.push("考虑使用更快的存储（NVMe SSD）".to_string());
        recommended_actions.push("优化数据加载策略（预取、缓存）".to_string());

        AnalysisResult {
            scene: SceneType::StorageSlow,
            root_causes,
            confidence: if !slow_storage.is_empty() { 0.8 } else { 0.6 },
            recommendations,
            recommended_actions,
            severity: Severity::Warning,
        }
    }
}
