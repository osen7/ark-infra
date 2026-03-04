use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// 八大原子事件类型
/// 使用蛇形小写加点格式，便于跨语言协作
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    // 1. 计算域
    #[serde(rename = "compute.util")]
    ComputeUtil, // 算力利用率 (如: gpu.util)
    #[serde(rename = "compute.mem")]
    ComputeMem, // 显存/内存使用率 (如: gpu.mem)
    // 2. 传输域
    #[serde(rename = "transport.bw")]
    TransportBw, // 网络吞吐 (如: rdma.bw)
    #[serde(rename = "transport.drop")]
    TransportDrop, // 丢包/重传 (如: rdma.drop)
    // 3. 存储域
    #[serde(rename = "storage.iops")]
    StorageIops, // 存储 IO (如: nvme.iops)
    #[serde(rename = "storage.qdepth")]
    StorageQDepth, // 队列深度 (如: nvme.qdepth)
    // 4. 进程域
    #[serde(rename = "process.state")]
    ProcessState, // 进程状态 (start/exit/zombie)
    // 5. 错误域
    #[serde(rename = "error.hw")]
    ErrorHw, // 硬件级报错 (如 XID/ECC)
    #[serde(rename = "error.net")]
    ErrorNet, // 网络阻塞报错 (如 PFC Storm)
    // 6. 拓扑域
    #[serde(rename = "topo.link_down")]
    TopoLinkDown, // NVLink/PCIe 降级或断开
    // 7. 意图域
    #[serde(rename = "intent.run")]
    IntentRun, // 调度器元数据 (如 Job 分配)
    // 8. 动作域
    #[serde(rename = "action.exec")]
    ActionExec, // 系统干预动作 (如 kill/reset)
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            EventType::ComputeUtil => "compute.util",
            EventType::ComputeMem => "compute.mem",
            EventType::TransportBw => "transport.bw",
            EventType::TransportDrop => "transport.drop",
            EventType::StorageIops => "storage.iops",
            EventType::StorageQDepth => "storage.qdepth",
            EventType::ProcessState => "process.state",
            EventType::ErrorHw => "error.hw",
            EventType::ErrorNet => "error.net",
            EventType::TopoLinkDown => "topo.link_down",
            EventType::IntentRun => "intent.run",
            EventType::ActionExec => "action.exec",
        };
        write!(f, "{s}")
    }
}

/// 统一的事件载体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: u64,                // 毫秒级时间戳
    pub event_type: EventType,  // 事件类型
    pub entity_id: String,      // 物理资源抽象ID (如 "gpu-03", "mlx5_0")
    pub job_id: Option<String>, // 关联的任务ID (如果有)
    pub pid: Option<u32>,       // 关联的进程PID (如果有)
    pub value: String,          // 具体载荷 (如 "85", "XID_79")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>, // 节点ID（用于 Hub 命名空间隔离，如 "node-a", "node-b"）
}

impl Event {
    /// 创建新事件，自动填充当前时间戳
    pub fn new(
        event_type: EventType,
        entity_id: String,
        value: String,
        job_id: Option<String>,
        pid: Option<u32>,
    ) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            ts,
            event_type,
            entity_id,
            job_id,
            pid,
            value,
            node_id: None, // 默认无节点ID，由 Agent 在推送时注入
        }
    }
}

/// 事件总线：基于有界通道 (Bounded Channel) 的事件分发器
pub struct EventBus {
    tx: mpsc::Sender<Event>,
    rx: Option<mpsc::Receiver<Event>>,
}

impl EventBus {
    /// 创建新的事件总线，指定通道容量
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        Self { tx, rx: Some(rx) }
    }

    /// 获取发送端（用于探针推送事件）
    pub fn sender(&self) -> mpsc::Sender<Event> {
        self.tx.clone()
    }

    /// 获取接收端（用于事件消费循环）
    pub fn receiver(&mut self) -> Option<mpsc::Receiver<Event>> {
        self.rx.take()
    }
}

/// 模拟探针：每秒随机生成几条事件用于测试
pub async fn dummy_probe(tx: mpsc::Sender<Event>) -> Result<(), Box<dyn std::error::Error>> {
    use rand::{Rng, SeedableRng};
    use tokio::time::{sleep, Duration};

    let mut rng = rand::rngs::StdRng::from_entropy();
    let mut pid_counter = 1000u32;

    loop {
        // 每秒生成 1-3 个随机事件
        let event_count = rng.gen_range(1..=3);

        for _ in 0..event_count {
            let event = if rng.gen_bool(0.5) {
                // 生成 gpu.util 事件
                let gpu_id = format!("gpu-{:02}", rng.gen_range(0..=7));
                let util = rng.gen_range(0..=100);
                let pid = Some(pid_counter + rng.gen_range(0..=10));
                Event::new(EventType::ComputeUtil, gpu_id, util.to_string(), None, pid)
            } else {
                // 生成 process.start 事件
                pid_counter += 1;
                Event::new(
                    EventType::ProcessState,
                    format!("proc-{}", pid_counter),
                    "start".to_string(),
                    Some(format!("job-{}", rng.gen_range(1000..=9999))),
                    Some(pid_counter),
                )
            };

            // 发送事件（如果通道已满，则等待）
            if let Err(e) = tx.send(event).await {
                eprintln!("[dummy_probe] 发送事件失败: {}", e);
                return Err(Box::new(e));
            }
        }

        sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, EventType};
    use serde_json::json;

    #[test]
    fn event_type_serialization_uses_dot_notation() {
        let event = Event {
            ts: 1_741_234_567_890,
            event_type: EventType::TransportDrop,
            entity_id: "roce-mlx5_0".to_string(),
            job_id: Some("job-42".to_string()),
            pid: Some(1234),
            value: "12.5".to_string(),
            node_id: None,
        };

        let serialized = serde_json::to_value(&event).expect("serialize event");
        assert_eq!(serialized["event_type"], "transport.drop");
        assert!(serialized.get("node_id").is_none());
    }

    #[test]
    fn event_round_trip_preserves_optional_node_id() {
        let original = Event {
            ts: 1_741_234_567_891,
            event_type: EventType::ErrorNet,
            entity_id: "roce-mlx5_0".to_string(),
            job_id: Some("job-rdma".to_string()),
            pid: Some(2345),
            value: "pfc_storm".to_string(),
            node_id: Some("node-a".to_string()),
        };

        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: Event = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(decoded.ts, original.ts);
        assert_eq!(decoded.event_type, original.event_type);
        assert_eq!(decoded.entity_id, original.entity_id);
        assert_eq!(decoded.job_id, original.job_id);
        assert_eq!(decoded.pid, original.pid);
        assert_eq!(decoded.value, original.value);
        assert_eq!(decoded.node_id, original.node_id);
    }

    #[test]
    fn event_deserialization_accepts_legacy_payload_without_node_id() {
        let legacy_payload = json!({
            "ts": 1741234567892u64,
            "event_type": "transport.bw",
            "entity_id": "roce-mlx5_0",
            "job_id": "job-legacy",
            "pid": 3456,
            "value": "89.7"
        });

        let event: Event = serde_json::from_value(legacy_payload).expect("deserialize legacy");
        assert_eq!(event.event_type, EventType::TransportBw);
        assert_eq!(event.node_id, None);
        assert_eq!(event.pid, Some(3456));
    }
}
