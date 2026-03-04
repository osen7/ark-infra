use crate::scene::analyzer::SceneAnalyzer;
use crate::scene::types::{AnalysisResult, SceneType};
use ark_core::graph::{EdgeType, StateGraph};

/// 工作负载卡死场景分析器
/// 智能判断：进程 running 但资源利用率极低，且没有等待 IO
pub struct WorkloadStalledAnalyzer;

#[async_trait::async_trait]
impl SceneAnalyzer for WorkloadStalledAnalyzer {
    fn scene_type(&self) -> SceneType {
        SceneType::WorkloadStalled
    }

    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult {
        let mut root_causes = Vec::new();
        let mut recommendations = Vec::new();

        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 检查进程节点状态
        let process_node = nodes.get(target);
        let is_running = process_node
            .and_then(|n| n.metadata.get("state"))
            .map(|s| s == "running")
            .unwrap_or(false);

        if !is_running {
            // 不是 running 状态，不适用此分析器
            return AnalysisResult {
                scene: SceneType::WorkloadStalled,
                root_causes: vec!["进程不在运行状态".to_string()],
                confidence: 0.0,
                recommendations: vec![],
                recommended_actions: vec![],
                severity: crate::scene::types::Severity::Info,
            };
        }

        // 检查消耗的资源利用率
        let mut low_util_count = 0;
        let mut total_resources = 0;
        let mut has_io_wait = false;

        for edge in &edges {
            if edge.from == target && edge.edge_type == EdgeType::Consumes {
                total_resources += 1;
                if let Some(node) = nodes.get(&edge.to) {
                    // 检查 GPU/NPU 利用率
                    if let Some(util) = node.metadata.get("util") {
                        if let Ok(util_val) = util.parse::<f64>() {
                            if util_val < 1.0 {
                                low_util_count += 1;
                            }
                        }
                    }
                }
            }

            // 检查是否有 WaitsOn IO
            if edge.from == target && edge.edge_type == EdgeType::WaitsOn {
                if edge.to.contains("network")
                    || edge.to.contains("storage")
                    || edge.to.contains("disk")
                {
                    has_io_wait = true;
                }
            }
        }

        // 判断是否真正卡死
        // 条件：所有资源利用率 < 1%，且没有等待 IO，且进程状态为 running
        if total_resources > 0 && low_util_count == total_resources && !has_io_wait {
            root_causes.push("进程处于死锁/卡死状态".to_string());
            root_causes.push(format!("所有 {} 个资源利用率均 < 1%", total_resources));
            root_causes.push("未检测到网络或存储 IO 等待".to_string());

            recommendations.push("检查进程是否在等待锁或信号量".to_string());
            recommendations.push("检查进程是否在等待其他进程".to_string());
            recommendations.push("检查应用日志中的死锁信息".to_string());
        } else if has_io_wait {
            root_causes.push("进程可能在等待 IO 操作完成".to_string());
            recommendations.push("检查网络或存储性能".to_string());
        } else {
            root_causes.push("进程可能处于正常的数据预处理阶段".to_string());
            recommendations.push("继续观察，如果超过预期时间再处理".to_string());
        }

        let mut recommended_actions = Vec::new();
        recommended_actions.push("如果确认卡死，执行 ark zap 终止进程".to_string());
        recommended_actions.push("检查是否有 Checkpoint 可以恢复".to_string());

        AnalysisResult {
            scene: SceneType::WorkloadStalled,
            root_causes,
            confidence: if low_util_count == total_resources && !has_io_wait {
                0.9
            } else {
                0.6
            },
            recommendations,
            recommended_actions,
            severity: crate::scene::types::Severity::Warning,
        }
    }
}
