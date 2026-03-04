use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType, Severity};
use ark_core::graph::{EdgeType, StateGraph};

/// 存储 IO 错误场景分析器
pub struct StorageIoErrorAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for StorageIoErrorAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::StorageIoError
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 查找存储相关的错误
        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::BlockedBy {
                if edge.to.contains("storage")
                    || edge.to.contains("disk")
                    || edge.to.contains("nvme")
                {
                    if let Some(node) = nodes.get(&edge.to) {
                        if let Some(error_type) = node.metadata.get("error_type") {
                            root_causes.push(format!("存储错误: {}", error_type));
                        } else {
                            root_causes.push(format!("存储设备 {} 异常", edge.to));
                        }
                    }
                }
            }

            // 查找 WaitsOn 存储的边
            if edge.from == target && edge.edge_type == EdgeType::WaitsOn {
                if edge.to.contains("storage") || edge.to.contains("disk") {
                    if let Some(node) = nodes.get(&edge.to) {
                        if let Some(io_error) = node.metadata.get("io_error") {
                            root_causes.push(format!("存储 IO 错误: {}", io_error));
                        }
                    }
                }
            }
        }

        if root_causes.is_empty() {
            root_causes.push("存储 IO 可能存在问题".to_string());
        }

        recommendations.push("检查存储设备健康状态".to_string());
        recommendations.push("检查文件系统错误".to_string());
        recommendations.push("检查磁盘空间".to_string());
        recommendations.push("检查存储设备 I/O 统计".to_string());

        let mut recommended_actions = Vec::new();
        recommended_actions.push("检查 dmesg 中的存储错误日志".to_string());
        recommended_actions.push("运行 fsck 检查文件系统".to_string());
        recommended_actions.push("检查存储设备 SMART 状态".to_string());

        AnalysisResult {
            scene: SceneType::StorageIoError,
            root_causes,
            confidence: 0.75,
            recommendations,
            recommended_actions,
            severity: Severity::Critical,
        }
    }
}
