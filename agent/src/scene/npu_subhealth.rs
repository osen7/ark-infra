use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType};
use ark_core::graph::{EdgeType, StateGraph};

/// NPU 亚健康场景分析器
/// 检测 SOC 过温、HCCS 降级等亚健康状态
pub struct NpuSubhealthAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for NpuSubhealthAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::NpuSubhealth
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 查找进程消耗的 NPU 资源
        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::Consumes {
                if edge.to.starts_with("npu-") || edge.to.contains("ascend") {
                    if let Some(node) = nodes.get(&edge.to) {
                        // 检查温度
                        if let Some(temp) = node.metadata.get("temperature") {
                            if let Ok(temp_val) = temp.parse::<f64>() {
                                if temp_val > 85.0 {
                                    root_causes.push(format!(
                                        "NPU {} SOC 过温: {:.1}°C",
                                        edge.to, temp_val
                                    ));
                                    recommendations
                                        .push(format!("检查 NPU {} 的散热系统", edge.to));
                                }
                            }
                        }

                        // 检查 HCCS 降级
                        if let Some(hccs_status) = node.metadata.get("hccs_lane_status") {
                            if hccs_status == "degraded" || hccs_status.contains("降级") {
                                root_causes.push(format!("NPU {} HCCS 链路降级", edge.to));
                                recommendations.push(format!("检查 NPU {} 的 HCCS 连接", edge.to));
                            }
                        }

                        // 检查性能降频
                        if let Some(freq) = node.metadata.get("frequency") {
                            if let (Ok(freq_val), Some(max_freq)) =
                                (freq.parse::<f64>(), node.metadata.get("max_frequency"))
                            {
                                if let Ok(max_freq_val) = max_freq.parse::<f64>() {
                                    if freq_val < max_freq_val * 0.9 {
                                        root_causes.push(format!(
                                            "NPU {} 频率降频: {:.0}MHz (最大: {:.0}MHz)",
                                            edge.to, freq_val, max_freq_val
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if root_causes.is_empty() {
            root_causes.push("NPU 可能处于亚健康状态".to_string());
        }

        recommendations.push("检查机器散热和风扇状态".to_string());
        recommendations.push("检查 NPU 固件版本和驱动".to_string());
        recommendations.push("监控 NPU 温度趋势".to_string());

        let mut recommended_actions = Vec::new();
        recommended_actions.push("隔离亚健康节点，避免新任务调度到此节点".to_string());
        recommended_actions.push("联系硬件维护团队检查 NPU 硬件状态".to_string());

        let confidence = if root_causes.len() > 1 { 0.85 } else { 0.7 };
        AnalysisResult {
            scene: SceneType::NpuSubhealth,
            root_causes,
            confidence,
            recommendations,
            recommended_actions,
            severity: crate::scene::types::Severity::Warning, // 亚健康是警告级别
        }
    }
}
