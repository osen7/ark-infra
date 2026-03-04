use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType, Severity};
use ark_core::graph::{EdgeType, StateGraph};

/// Checkpoint 超时场景分析器
/// 检测 Checkpoint 保存或加载超时的情况
pub struct CheckpointTimeoutAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for CheckpointTimeoutAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::ProcessBlocked // 复用 ProcessBlocked，或未来新增 CheckpointTimeout
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 检查进程是否在等待存储（可能是 Checkpoint 操作）
        let mut checkpoint_wait = false;
        let mut storage_slow = false;

        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::WaitsOn {
                if edge.to.contains("storage") || edge.to.contains("disk") {
                    checkpoint_wait = true;

                    // 检查存储是否慢
                    if let Some(node) = nodes.get(&edge.to) {
                        if let Some(iops) = node.metadata.get("iops") {
                            if let Ok(iops_val) = iops.parse::<f64>() {
                                if iops_val < 50.0 {
                                    storage_slow = true;
                                    root_causes.push(format!(
                                        "存储 {} IOPS 过低: {:.0}",
                                        edge.to, iops_val
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        // 检查进程 metadata 中是否有 checkpoint 相关信息
        if let Some(node) = nodes.get(target) {
            if let Some(state) = node.metadata.get("state") {
                if state.contains("checkpoint") || state.contains("saving") {
                    checkpoint_wait = true;
                }
            }
        }

        if checkpoint_wait {
            if storage_slow {
                root_causes.push("Checkpoint 操作因存储性能问题而超时".to_string());
            } else {
                root_causes.push("Checkpoint 操作可能超时".to_string());
            }

            recommendations.push("检查 Checkpoint 文件大小和存储性能".to_string());
            recommendations.push("考虑使用异步 Checkpoint 保存".to_string());
            recommendations.push("检查存储设备健康状态".to_string());
        } else {
            root_causes.push("可能不是 Checkpoint 相关的问题".to_string());
        }

        let mut recommended_actions = Vec::new();
        recommended_actions.push("尝试触发 Checkpoint Dump 信号 (SIGUSR1)".to_string());
        recommended_actions.push("如果 Checkpoint 损坏，从上一个 Checkpoint 恢复".to_string());
        recommended_actions.push("优化 Checkpoint 保存策略（减少频率或使用增量保存）".to_string());
        recommended_actions.push("检查 Checkpoint 目录的磁盘空间".to_string());

        AnalysisResult {
            scene: SceneType::ProcessBlocked,
            root_causes,
            confidence: if checkpoint_wait { 0.8 } else { 0.5 },
            recommendations,
            recommended_actions,
            severity: Severity::Warning,
        }
    }
}
