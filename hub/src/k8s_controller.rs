//! Kubernetes 控制器模块
//!
//! 当 Hub 诊断出不可逆硬件故障时，自动调用 K8s API：
//! 1. 给 Node 打上 NoSchedule 污点
//! 2. 执行 Pod Eviction（驱逐）
//!
//! 让 Ark 从被动监控工具升维成 AI 集群自动驾驶控制面

use ark_core::event::{Event, EventType};
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::api::{ApiResource, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams};
use kube::{Api, Client, Config};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration, Instant};

/// 不可逆故障类型
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrreversibleFault {
    /// 持续 XID 错误（GPU 硬件故障）
    PersistentXidError {
        node_id: String,
        gpu_id: String,
        xid_code: String,
    },
    /// RDMA 物理链路断开
    RdmaLinkDown { node_id: String, interface: String },
    /// 存储设备故障
    StorageDeviceFailure { node_id: String, device: String },
    /// 其他不可逆硬件故障
    OtherHardwareFailure { node_id: String, reason: String },
}

/// Kubernetes 控制器
pub struct K8sController {
    client: Client,
    node_api: Api<Node>,
    pod_api: Api<Pod>,
    /// 已处理的故障节点（避免重复操作）
    processed_nodes: Arc<RwLock<HashMap<String, Instant>>>,
    /// 故障冷却时间（默认 5 分钟，避免频繁操作）
    cooldown_duration: Duration,
    /// 是否启用自动操作（默认 false，需要显式启用）
    enabled: bool,
}

impl K8sController {
    /// 创建新的 K8s 控制器
    pub async fn new(enabled: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let config = Config::infer().await?;
        let client = Client::try_from(config)?;

        let node_api: Api<Node> = Api::all(client.clone());
        let pod_api: Api<Pod> = Api::all(client.clone());

        Ok(Self {
            client,
            node_api,
            pod_api,
            processed_nodes: Arc::new(RwLock::new(HashMap::new())),
            cooldown_duration: Duration::from_secs(300), // 5 分钟冷却
            enabled,
        })
    }

    /// 检查事件是否表示不可逆故障
    pub fn detect_irreversible_fault(&self, event: &Event) -> Option<IrreversibleFault> {
        // 只处理错误事件
        match event.event_type {
            EventType::ErrorHw => {
                // 检查是否为持续 XID 错误
                if event.value.contains("XID") || event.value.contains("xid") {
                    // 检查错误频率（简化：如果短时间内多次出现，认为是持续故障）
                    // TODO: 实际应该查询图引擎，统计该节点的 XID 错误频率
                    let node_id = event
                        .node_id
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    return Some(IrreversibleFault::PersistentXidError {
                        node_id,
                        gpu_id: event.entity_id.clone(),
                        xid_code: event.value.clone(),
                    });
                }

                // 其他硬件错误
                let node_id = event
                    .node_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                Some(IrreversibleFault::OtherHardwareFailure {
                    node_id,
                    reason: format!("{}: {}", event.entity_id, event.value),
                })
            }
            EventType::ErrorNet => {
                // 检查是否为 RDMA 链路断开
                if event.value.contains("link_down") || event.value.contains("LINK_DOWN") {
                    let node_id = event
                        .node_id
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    return Some(IrreversibleFault::RdmaLinkDown {
                        node_id,
                        interface: event.entity_id.clone(),
                    });
                }
                None
            }
            EventType::TopoLinkDown => {
                // 拓扑链路断开（可能是 PCIe/NVLink）
                let node_id = event
                    .node_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                Some(IrreversibleFault::OtherHardwareFailure {
                    node_id,
                    reason: format!("Topology link down: {} - {}", event.entity_id, event.value),
                })
            }
            _ => None,
        }
    }

    /// 处理不可逆故障：打污点 + 驱逐 Pod
    pub async fn handle_irreversible_fault(
        &self,
        fault: &IrreversibleFault,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.enabled {
            eprintln!("[k8s-controller] 控制器未启用，跳过操作");
            return Ok(());
        }

        let node_id = match fault {
            IrreversibleFault::PersistentXidError { node_id, .. }
            | IrreversibleFault::RdmaLinkDown { node_id, .. }
            | IrreversibleFault::StorageDeviceFailure { node_id, .. }
            | IrreversibleFault::OtherHardwareFailure { node_id, .. } => node_id,
        };

        // 检查冷却时间
        {
            let processed = self.processed_nodes.read().await;
            if let Some(last_time) = processed.get(node_id) {
                if last_time.elapsed() < self.cooldown_duration {
                    eprintln!(
                        "[k8s-controller] 节点 {} 在冷却期内，跳过操作（距离上次操作: {:?}）",
                        node_id,
                        last_time.elapsed()
                    );
                    return Ok(());
                }
            }
        }

        println!("🚨 [k8s-controller] 检测到不可逆故障: {:?}", fault);
        println!("🔧 [k8s-controller] 开始处理节点: {}", node_id);

        // 1. 给 Node 打上 NoSchedule 污点
        match self.taint_node(node_id, fault).await {
            Ok(_) => {
                println!(
                    "✅ [k8s-controller] 节点 {} 已打上 NoSchedule 污点",
                    node_id
                );
            }
            Err(e) => {
                eprintln!("❌ [k8s-controller] 打污点失败: {}", e);
                return Err(e);
            }
        }

        // 2. 驱逐该节点上的所有 Pod
        match self.evict_pods_on_node(node_id).await {
            Ok(count) => {
                println!(
                    "✅ [k8s-controller] 已驱逐节点 {} 上的 {} 个 Pod",
                    node_id, count
                );
            }
            Err(e) => {
                eprintln!("⚠️  [k8s-controller] 驱逐 Pod 时出错: {}", e);
                // 不返回错误，因为污点已经打上，Pod 调度器会自动处理
            }
        }

        // 记录处理时间
        {
            let mut processed = self.processed_nodes.write().await;
            processed.insert(node_id.clone(), Instant::now());
        }

        Ok(())
    }

    /// 运行 Ark Remediation Operator 循环（CRD 驱动）
    ///
    /// 监听 `arkremediationrequests.ark.io/v1alpha1`：
    /// - status.phase 为空 / Pending => 执行处置
    /// - 执行中写入 Running
    /// - 完成后写入 Succeeded / Failed
    pub async fn run_operator_loop(
        self: Arc<Self>,
        namespace: String,
        poll_interval_s: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.enabled {
            eprintln!("[k8s-operator] 控制器未启用，跳过 operator loop");
            return Ok(());
        }
        let gvk = GroupVersionKind::gvk("ark.io", "v1alpha1", "ArkRemediationRequest");
        let ar = ApiResource::from_gvk(&gvk);
        let req_api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), &namespace, &ar);
        let poll = Duration::from_secs(poll_interval_s.max(5));

        println!(
            "[k8s-operator] 启动 CRD 循环: namespace={}, poll={}s",
            namespace,
            poll.as_secs()
        );

        loop {
            let lp = ListParams::default().limit(200);
            let listed = req_api.list(&lp).await;
            match listed {
                Ok(list) => {
                    for obj in list {
                        let name = obj.metadata.name.clone().unwrap_or_default();
                        if name.is_empty() {
                            continue;
                        }
                        let phase = obj
                            .data
                            .get("status")
                            .and_then(|v| v.get("phase"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("Pending");
                        if matches!(phase, "Succeeded" | "Failed" | "Running") {
                            continue;
                        }
                        let Some(spec) = obj.data.get("spec") else {
                            let _ = self
                                .patch_request_status(
                                    &req_api,
                                    &name,
                                    "Failed",
                                    Some("missing spec".to_string()),
                                )
                                .await;
                            continue;
                        };
                        let node_id = spec
                            .get("nodeId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let action = spec
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("taint_evict")
                            .to_string();
                        let reason = spec
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("operator request")
                            .to_string();
                        let dry_run = spec
                            .get("dryRun")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if node_id.is_empty() {
                            let _ = self
                                .patch_request_status(
                                    &req_api,
                                    &name,
                                    "Failed",
                                    Some("spec.nodeId is empty".to_string()),
                                )
                                .await;
                            continue;
                        }
                        let _ = self
                            .patch_request_status(&req_api, &name, "Running", None)
                            .await;

                        let (succeeded, message) = match self
                            .execute_operator_action(&node_id, &action, &reason, dry_run)
                            .await
                        {
                            Ok(msg) => (true, msg),
                            Err(e) => (false, e.to_string()),
                        };
                        let _ = if succeeded {
                            self.patch_request_status(&req_api, &name, "Succeeded", Some(message))
                                .await
                        } else {
                            self.patch_request_status(&req_api, &name, "Failed", Some(message))
                                .await
                        };
                    }
                }
                Err(e) => {
                    eprintln!("[k8s-operator] 列举 ArkRemediationRequest 失败: {}", e);
                }
            }
            sleep(poll).await;
        }
    }

    /// 给 Node 打上 NoSchedule 污点
    async fn taint_node(
        &self,
        node_id: &str,
        fault: &IrreversibleFault,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 查找节点（通过 node_id 匹配 K8s Node 名称或标签）
        // 注意：node_id 可能是 "node-a" 格式，需要映射到实际的 K8s Node 名称
        let k8s_node_name = self.map_node_id_to_k8s_name(node_id).await?;

        // 获取当前节点
        let node = self.node_api.get(&k8s_node_name).await?;

        // 构建污点
        let taint_key = "ark.io/hardware-failure";
        let taint_value = match fault {
            IrreversibleFault::PersistentXidError { xid_code, .. } => {
                format!("xid-error:{}", xid_code)
            }
            IrreversibleFault::RdmaLinkDown { interface, .. } => {
                format!("rdma-link-down:{}", interface)
            }
            IrreversibleFault::StorageDeviceFailure { device, .. } => {
                format!("storage-failure:{}", device)
            }
            IrreversibleFault::OtherHardwareFailure { reason, .. } => {
                format!("hardware-failure:{}", reason.replace(" ", "-"))
            }
        };

        // 检查污点是否已存在
        let mut taints = node
            .spec
            .as_ref()
            .and_then(|s| s.taints.clone())
            .unwrap_or_default();

        // 检查是否已有相同 key 的污点
        if !taints.iter().any(|t| t.key == taint_key) {
            // 添加新污点
            taints.push(k8s_openapi::api::core::v1::Taint {
                key: taint_key.to_string(),
                value: Some(taint_value),
                effect: "NoSchedule".to_string(),
                time_added: None,
            });

            // 使用 JSON Patch 更新节点
            let patch = json!({
                "spec": {
                    "taints": taints
                }
            });

            let params = PatchParams::apply("ark-controller");
            self.node_api
                .patch(&k8s_node_name, &params, &Patch::Apply(patch))
                .await?;
        } else {
            println!("[k8s-controller] 节点 {} 已有污点，跳过", k8s_node_name);
        }

        Ok(())
    }

    async fn execute_operator_action(
        &self,
        node_id: &str,
        action: &str,
        reason: &str,
        dry_run: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
        if dry_run {
            println!(
                "[k8s-operator] dry-run: node_id={}, action={}, reason={}",
                node_id, action, reason
            );
            return Ok("dry-run".to_string());
        }

        let fault = IrreversibleFault::OtherHardwareFailure {
            node_id: node_id.to_string(),
            reason: reason.to_string(),
        };
        match action {
            "taint_only" => {
                self.taint_node(node_id, &fault).await?;
                Ok("taint_only applied".to_string())
            }
            "taint_evict" => {
                self.taint_node(node_id, &fault).await?;
                let evicted = self.evict_pods_on_node(node_id).await?;
                Ok(format!("taint+evict applied, evicted_pods={}", evicted))
            }
            other => Err(format!("unsupported action: {}", other).into()),
        }
    }

    /// 驱逐节点上的所有 Pod
    async fn evict_pods_on_node(&self, node_id: &str) -> Result<usize, Box<dyn std::error::Error>> {
        let k8s_node_name = self.map_node_id_to_k8s_name(node_id).await?;

        // 列出所有 Pod
        let pods = self.pod_api.list(&Default::default()).await?;

        // 筛选出在该节点上的 Pod
        let pods_on_node: Vec<_> = pods
            .iter()
            .filter(|pod| {
                pod.spec.as_ref().and_then(|s| s.node_name.as_deref())
                    == Some(k8s_node_name.as_str())
            })
            .collect();

        let mut evicted_count = 0;

        for pod in pods_on_node {
            // 跳过 DaemonSet Pod（系统 Pod）
            if let Some(owner_refs) = &pod.metadata.owner_references {
                if owner_refs
                    .iter()
                    .any(|ref_| ref_.kind == "DaemonSet" || ref_.kind == "Node")
                {
                    continue;
                }
            }

            // 执行优雅驱逐（使用 Eviction API，尊重 PDB）
            let namespace = pod.metadata.namespace.as_deref().unwrap_or("default");
            let pod_name = pod.metadata.name.as_deref().ok_or("Pod name is missing")?;

            // 使用 Pod Eviction Subresource API
            // 这是生产级实现：尊重 PodDisruptionBudget，优雅处理退出信号
            let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);

            // 构建 Eviction 请求体
            let eviction_body = serde_json::json!({
                "apiVersion": "policy/v1",
                "kind": "Eviction",
                "metadata": {
                    "name": pod_name,
                    "namespace": namespace
                }
            });
            let eviction_bytes = serde_json::to_vec(&eviction_body)?;

            // 使用 kube 的 create_subresource 调用 Eviction API
            // 这会触发 Pod 的优雅关闭流程，并尊重 PDB 限制
            match pod_api
                .create_subresource::<serde_json::Value>(
                    "eviction",
                    pod_name,
                    &Default::default(),
                    eviction_bytes,
                )
                .await
            {
                Ok(_) => {
                    evicted_count += 1;
                    println!(
                        "[k8s-controller] ✅ 已优雅驱逐 Pod: {}/{} (尊重 PDB)",
                        namespace, pod_name
                    );
                }
                Err(e) => {
                    // Eviction API 可能因为 PDB 限制而失败，这是正常行为
                    // 我们记录警告但不中断流程（因为污点已经打上，调度器会处理）
                    eprintln!(
                        "[k8s-controller] ⚠️  驱逐 Pod {}/{} 失败（可能受 PDB 限制）: {}",
                        namespace, pod_name, e
                    );
                    eprintln!(
                        "[k8s-controller]   提示：节点已打上 NoSchedule 污点，调度器将自动处理新 Pod 的调度"
                    );
                }
            }
        }

        Ok(evicted_count)
    }

    /// 将 Ark node_id 映射到 K8s Node 名称
    ///
    /// 策略：
    /// 1. 如果 node_id 就是 K8s Node 名称，直接返回
    /// 2. 如果 node_id 是 "node-<ip>" 格式，尝试通过 IP 或标签查找
    /// 3. 默认假设 node_id 就是 Node 名称
    async fn map_node_id_to_k8s_name(
        &self,
        node_id: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        // 首先尝试直接使用 node_id 作为 Node 名称
        if self.node_api.get(node_id).await.is_ok() {
            return Ok(node_id.to_string());
        }

        // 如果失败，尝试通过标签查找
        // 假设 Agent 在启动时会给 Node 打上标签 ark.io/node-id=<node_id>
        let nodes = self.node_api.list(&Default::default()).await?;

        for node in nodes {
            if let Some(labels) = &node.metadata.labels {
                if let Some(label_value) = labels.get("ark.io/node-id") {
                    if label_value == node_id {
                        if let Some(name) = &node.metadata.name {
                            return Ok(name.clone());
                        }
                    }
                }
            }

            // 也尝试匹配节点名称（如果 node_id 是 "node-<ip>" 格式）
            if let Some(name) = &node.metadata.name {
                if name == node_id || name.contains(node_id) {
                    return Ok(name.clone());
                }
            }
        }

        // 如果都找不到，返回 node_id（让 K8s API 返回错误，而不是静默失败）
        Ok(node_id.to_string())
    }

    async fn patch_request_status(
        &self,
        req_api: &Api<DynamicObject>,
        name: &str,
        phase: &str,
        message: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let patch = json!({
            "status": {
                "phase": phase,
                "message": message.unwrap_or_default(),
                "lastTransitionTime": chrono::Utc::now().to_rfc3339(),
            }
        });
        let pp = PatchParams::apply("ark-operator").force();
        req_api
            .patch_status(name, &pp, &Patch::Merge(&patch))
            .await?;
        Ok(())
    }
}
