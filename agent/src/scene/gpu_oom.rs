use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType};
use ark_core::graph::{EdgeType, StateGraph};

/// GPU OOM 场景分析器
pub struct GpuOomAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for GpuOomAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::GpuOom
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        // 检查是否有 GPU 相关的错误节点
        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 查找与目标进程相关的 GPU 错误
        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::BlockedBy {
                if let Some(node) = nodes.get(&edge.to) {
                    if node.id.starts_with("gpu-") || node.id.contains("gpu") {
                        if let Some(error_type) = node.metadata.get("error_type") {
                            if error_type.contains("OOM") || error_type.contains("out of memory") {
                                root_causes.push(format!("GPU {} 显存不足", node.id));
                            }
                        }
                    }
                }
            }
        }

        // 查找进程消耗的 GPU 资源
        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::Consumes {
                if edge.to.starts_with("gpu-") {
                    if let Some(node) = nodes.get(&edge.to) {
                        if let Some(mem_usage) = node.metadata.get("mem_usage") {
                            if let Ok(usage) = mem_usage.parse::<f64>() {
                                if usage > 95.0 {
                                    root_causes.push(format!(
                                        "GPU {} 显存使用率过高: {:.1}%",
                                        edge.to, usage
                                    ));
                                    recommendations
                                        .push(format!("检查 GPU {} 上的进程显存使用", edge.to));
                                }
                            }
                        }
                    }
                }
            }
        }

        if root_causes.is_empty() {
            root_causes.push("GPU 显存可能不足".to_string());
        }

        recommendations.push("使用 nvidia-smi 检查显存使用情况".to_string());
        recommendations.push("考虑降低批处理大小或模型精度".to_string());
        recommendations.push("检查是否有显存泄漏".to_string());

        // 推荐的操作（为 ark fix 铺路）
        let mut recommended_actions = Vec::new();
        recommended_actions.push("尝试触发框架层的 Checkpoint Dump 信号 (SIGUSR1)".to_string());
        recommended_actions.push("隔离该节点，执行 ark zap 清理僵尸进程".to_string());
        recommended_actions.push("修改批量大小 (Batch Size) 后重提任务".to_string());

        let confidence = if root_causes.len() > 1 { 0.9 } else { 0.7 };
        AnalysisResult {
            scene: SceneType::GpuOom,
            root_causes,
            confidence,
            recommendations,
            recommended_actions,
            severity: crate::scene::types::Severity::Critical,
        }
    }
}
