use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType};
use ark_core::graph::{EdgeType, StateGraph};

/// 进程崩溃场景分析器
pub struct ProcessCrashAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for ProcessCrashAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::ProcessCrash
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        let nodes = graph.get_nodes_async().await;
        let edges = graph.get_all_edges_async().await;

        // 检查进程节点状态
        if let Some(node) = nodes.get(target) {
            if let Some(state) = node.metadata.get("state") {
                if state == "exit" || state == "crash" || state == "failed" {
                    root_causes.push(format!("进程状态: {}", state));
                }
            }
        }

        // 查找导致进程崩溃的错误
        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::BlockedBy {
                if let Some(node) = nodes.get(&edge.to) {
                    if node.id.contains("error") {
                        if let Some(error_type) = node.metadata.get("error_type") {
                            root_causes.push(format!("错误: {}", error_type));
                        } else {
                            root_causes.push(format!("错误节点: {}", edge.to));
                        }
                    }
                }
            }
        }

        if root_causes.is_empty() {
            root_causes.push("进程可能异常退出".to_string());
        }

        recommendations.push("检查进程退出码".to_string());
        recommendations.push("检查系统日志".to_string());
        recommendations.push("检查资源使用情况（内存、CPU）".to_string());
        recommendations.push("检查依赖服务状态".to_string());

        let mut recommended_actions = Vec::new();
        recommended_actions.push("检查 Checkpoint 文件是否完整".to_string());
        recommended_actions.push("如果支持，尝试从 Checkpoint 恢复训练".to_string());
        recommended_actions.push("修复根因后重新提交任务".to_string());

        AnalysisResult {
            scene: SceneType::ProcessCrash,
            root_causes,
            confidence: 0.75,
            recommendations,
            recommended_actions,
            severity: crate::scene::types::Severity::Critical,
        }
    }
}
