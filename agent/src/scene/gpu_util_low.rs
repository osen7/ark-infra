use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType, Severity};
use ark_core::graph::{EdgeType, StateGraph};

/// GPU 利用率低场景分析器
/// 检测 GPU 空闲或利用率极低的情况
pub struct GpuUtilLowAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for GpuUtilLowAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::GpuUtilLow
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 查找进程消耗的 GPU 资源
        let mut low_util_gpus = Vec::new();
        let mut has_waits_on = false;

        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::Consumes {
                if edge.to.starts_with("gpu-") || edge.to.starts_with("npu-") {
                    if let Some(node) = nodes.get(&edge.to) {
                        if let Some(util) = node.metadata.get("util") {
                            if let Ok(util_val) = util.parse::<f64>() {
                                if util_val < 10.0 {
                                    low_util_gpus.push((edge.to.clone(), util_val));
                                }
                            }
                        }
                    }
                }
            }

            // 检查是否有 WaitsOn（可能等待数据）
            if edge.from == target && edge.edge_type == EdgeType::WaitsOn {
                has_waits_on = true;
            }
        }

        if !low_util_gpus.is_empty() {
            for (gpu_id, util) in &low_util_gpus {
                root_causes.push(format!("{} 利用率极低: {:.1}%", gpu_id, util));
            }

            if has_waits_on {
                root_causes.push("进程可能在等待数据加载或网络传输".to_string());
                recommendations.push("检查数据加载速度".to_string());
                recommendations.push("检查网络带宽".to_string());
            } else {
                root_causes.push("GPU 可能处于空闲状态".to_string());
                recommendations.push("检查训练循环是否正常".to_string());
                recommendations.push("检查是否有死锁或阻塞".to_string());
            }
        } else {
            root_causes.push("GPU 利用率可能偏低".to_string());
        }

        recommendations.push("使用 nvidia-smi 或 ascend-toolkit 检查 GPU/NPU 状态".to_string());
        recommendations.push("检查训练代码中的同步点".to_string());
        recommendations.push("检查数据预处理是否成为瓶颈".to_string());

        let mut recommended_actions = Vec::new();
        recommended_actions.push("优化数据加载管道（增加 DataLoader workers）".to_string());
        recommended_actions.push("检查是否有不必要的同步操作".to_string());
        recommended_actions.push("考虑使用混合精度训练提升吞吐".to_string());

        AnalysisResult {
            scene: SceneType::GpuUtilLow,
            root_causes,
            confidence: if !low_util_gpus.is_empty() { 0.8 } else { 0.6 },
            recommendations,
            recommended_actions,
            severity: Severity::Warning,
        }
    }
}
