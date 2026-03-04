mod analyzer;
mod checkpoint_timeout;
mod gpu_oom;
mod gpu_util_low;
mod network_stall;
mod npu_subhealth;
mod process_crash;
mod storage_io_error;
mod storage_slow;
mod types;
mod workload_stalled;

pub use analyzer::{SceneAnalyzer, SceneRegistry};
pub use checkpoint_timeout::CheckpointTimeoutAnalyzer;
pub use gpu_oom::GpuOomAnalyzer;
pub use gpu_util_low::GpuUtilLowAnalyzer;
pub use network_stall::NetworkStallAnalyzer;
pub use npu_subhealth::NpuSubhealthAnalyzer;
pub use process_crash::ProcessCrashAnalyzer;
pub use storage_io_error::StorageIoErrorAnalyzer;
pub use storage_slow::StorageSlowAnalyzer;
pub use types::{AnalysisResult, SceneType, Severity};
pub use workload_stalled::WorkloadStalledAnalyzer;

use ark_core::graph::StateGraph;

/// 场景识别器
pub struct SceneIdentifier {
    registry: SceneRegistry,
}

impl SceneIdentifier {
    pub fn new() -> Self {
        let mut registry = SceneRegistry::new();

        // 注册所有场景分析器（按优先级顺序）
        registry.register(GpuOomAnalyzer);
        registry.register(NpuSubhealthAnalyzer);
        registry.register(WorkloadStalledAnalyzer);
        registry.register(GpuUtilLowAnalyzer);
        registry.register(NetworkStallAnalyzer);
        registry.register(ProcessCrashAnalyzer);
        registry.register(StorageIoErrorAnalyzer);
        registry.register(StorageSlowAnalyzer);
        registry.register(CheckpointTimeoutAnalyzer);

        Self { registry }
    }

    /// 识别场景类型
    pub async fn identify_scene(&self, graph: &StateGraph, pid: u32) -> Option<SceneType> {
        let pid_str = format!("pid-{}", pid);
        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;

        // 检查 GPU/NPU 相关错误
        for edge in &edges {
            if edge.from == pid_str && edge.edge_type == ark_core::graph::EdgeType::BlockedBy {
                if let Some(node) = nodes.get(&edge.to) {
                    // GPU OOM
                    if node.id.starts_with("gpu-") || node.id.contains("gpu") {
                        if let Some(error_type) = node.metadata.get("error_type") {
                            if error_type.contains("OOM") || error_type.contains("out of memory") {
                                return Some(SceneType::GpuOom);
                            }
                            if error_type.contains("error") || error_type.contains("XID") {
                                return Some(SceneType::GpuError);
                            }
                        }
                    }
                    // NPU 亚健康
                    if node.id.starts_with("npu-") || node.id.contains("ascend") {
                        if let Some(temp) = node.metadata.get("temperature") {
                            if let Ok(temp_val) = temp.parse::<f64>() {
                                if temp_val > 85.0 {
                                    return Some(SceneType::NpuSubhealth);
                                }
                            }
                        }
                        if let Some(hccs_status) = node.metadata.get("hccs_lane_status") {
                            if hccs_status == "degraded" || hccs_status.contains("降级") {
                                return Some(SceneType::NpuSubhealth);
                            }
                        }
                    }
                }
            }
        }

        // 检查网络阻塞
        for edge in &edges {
            if edge.from == pid_str && edge.edge_type == ark_core::graph::EdgeType::WaitsOn {
                if edge.to.starts_with("network-") || edge.to.contains("net") {
                    return Some(SceneType::NetworkStall);
                }
            }
        }

        // 检查进程状态和工作负载卡死
        if let Some(node) = nodes.get(&pid_str) {
            if let Some(state) = node.metadata.get("state") {
                if state == "exit" || state == "crash" || state == "failed" {
                    return Some(SceneType::ProcessCrash);
                }
                if state == "blocked" || state == "waiting" {
                    return Some(SceneType::ProcessBlocked);
                }
                // 检查工作负载卡死：running 但资源利用率极低
                if state == "running" {
                    let mut low_util_count = 0;
                    let mut total_resources = 0;
                    let mut has_io_wait = false;

                    for edge in &edges {
                        if edge.from == pid_str
                            && edge.edge_type == ark_core::graph::EdgeType::Consumes
                        {
                            total_resources += 1;
                            if let Some(res_node) = nodes.get(&edge.to) {
                                if let Some(util) = res_node.metadata.get("util") {
                                    if let Ok(util_val) = util.parse::<f64>() {
                                        if util_val < 1.0 {
                                            low_util_count += 1;
                                        }
                                    }
                                }
                            }
                        }
                        if edge.from == pid_str
                            && edge.edge_type == ark_core::graph::EdgeType::WaitsOn
                        {
                            if edge.to.contains("network") || edge.to.contains("storage") {
                                has_io_wait = true;
                            }
                        }
                    }

                    // 所有资源利用率 < 1% 且没有 IO 等待，可能是卡死
                    if total_resources > 0 && low_util_count == total_resources && !has_io_wait {
                        return Some(SceneType::WorkloadStalled);
                    }
                }
            }
        }

        None
    }

    /// 使用场景分析器分析
    pub async fn analyze_scene(
        &self,
        scene: SceneType,
        graph: &StateGraph,
        pid: u32,
    ) -> Option<AnalysisResult> {
        let pid_str = format!("pid-{}", pid);

        if let Some(analyzer) = self.registry.get_analyzer(scene) {
            Some(analyzer.analyze(graph, &pid_str).await)
        } else {
            None
        }
    }
}

impl Default for SceneIdentifier {
    fn default() -> Self {
        Self::new()
    }
}
