//! Hub 事件转发器：将本地重要事件推送到 Hub
//!
//! 实现边缘折叠（Edge Roll-up）逻辑：
//! - 只推送错误事件、进程状态变化、触发规则的事件
//! - 过滤高频波动（如 gpu.util 的微小变化）

use crate::exec::{ActionExecutor, ActionType};
use ark_core::event::{Event, EventType};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
type WsSender =
    futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

const HUB_PROTOCOL_VERSION: &str = "1.0";
const EVENT_SCHEMA_VERSION: &str = "1.0";
const PENDING_EVENT_CAPACITY: usize = 1024;

#[derive(serde::Serialize)]
struct HubEventEnvelope {
    kind: String,
    protocol_version: String,
    schema_version: String,
    agent_id: String,
    feature_flags: Vec<String>,
    event: Event,
}

/// Hub 事件转发器
pub struct HubForwarder {
    hub_url: String,
    node_id: String,
    ws_sender: Option<Arc<RwLock<Option<WsSender>>>>,
    command_listener_handle: Option<tokio::task::JoinHandle<()>>,
    forwarded_bindings: Arc<RwLock<HashSet<(u32, String)>>>,
    last_util_values: Arc<RwLock<std::collections::HashMap<(u32, String), f64>>>,
    pending_events: VecDeque<Event>,
}

impl HubForwarder {
    /// 创建新的 Hub 转发器
    pub fn new(hub_url: String, node_id: String) -> Self {
        Self {
            hub_url,
            node_id,
            ws_sender: None,
            command_listener_handle: None,
            forwarded_bindings: Arc::new(RwLock::new(HashSet::new())),
            last_util_values: Arc::new(RwLock::new(std::collections::HashMap::new())),
            pending_events: VecDeque::with_capacity(PENDING_EVENT_CAPACITY),
        }
    }

    /// 连接到 Hub WebSocket 服务器
    pub async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let url = url::Url::parse(&self.hub_url)?;
        let ws_stream = connect_async(url).await?.0;
        let (write, read) = ws_stream.split();

        // 保存 write 端用于发送事件
        let sender = Arc::new(RwLock::new(Some(write)));
        self.ws_sender = Some(sender);

        // 启动命令监听任务
        let listener_handle = tokio::spawn(async move {
            let mut receiver = read;
            while let Some(msg) = receiver.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // 解析 Hub 下发的命令
                        if let Ok(cmd) = serde_json::from_str::<HubCommand>(&text) {
                            if let Err(e) = Self::handle_command(cmd).await {
                                eprintln!("[hub-forwarder] 执行命令失败: {}", e);
                            }
                        } else {
                            // 不是命令，可能是其他消息，忽略
                        }
                    }
                    Ok(Message::Close(_)) => {
                        println!("[hub-forwarder] Hub 关闭连接");
                        break;
                    }
                    Err(e) => {
                        eprintln!("[hub-forwarder] 接收消息错误: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        self.command_listener_handle = Some(listener_handle);

        println!("[hub-forwarder] 已连接到 Hub: {}", self.hub_url);
        Ok(())
    }

    /// 断开现有连接并重连
    pub async fn reconnect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(handle) = self.command_listener_handle.take() {
            handle.abort();
        }
        self.ws_sender = None;
        self.connect().await
    }

    /// 处理 Hub 下发的命令
    async fn handle_command(
        cmd: HubCommand,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match cmd.intent.as_str() {
            "fix" => {
                println!(
                    "[hub-forwarder] 收到修复命令: PID={}, action={:?}",
                    cmd.target_pid, cmd.action
                );

                // 根据 action 字符串创建 ActionType
                let action = if let Some(action_str) = &cmd.action {
                    Self::action_from_string(action_str)?
                } else {
                    // 默认：优雅降级
                    ActionType::GracefulShutdown {
                        signal: 10, // SIGUSR1
                        wait_seconds: 10,
                        force_kill: true,
                    }
                };

                // 执行动作
                let executor = ActionExecutor::new();
                match executor.execute(&action, cmd.target_pid).await {
                    Ok(msg) => {
                        println!("[hub-forwarder] 命令执行成功: {}", msg);
                    }
                    Err(e) => {
                        eprintln!("[hub-forwarder] 命令执行失败: {}", e);
                        return Err(std::io::Error::other(e).into());
                    }
                }
            }
            _ => {
                eprintln!("[hub-forwarder] 未知命令意图: {}", cmd.intent);
            }
        }
        Ok(())
    }

    /// 从字符串创建 ActionType
    fn action_from_string(action_str: &str) -> Result<ActionType, String> {
        match action_str.to_lowercase().as_str() {
            "gracefulshutdown" | "graceful_shutdown" => Ok(ActionType::GracefulShutdown {
                signal: 10,
                wait_seconds: 10,
                force_kill: true,
            }),
            "killprocess" | "kill_process" | "kill" => Ok(ActionType::KillProcess),
            "signal" | "sigusr1" => Ok(ActionType::Signal { signal: 10 }),
            _ => {
                // 尝试使用 from_recommendation 解析
                ActionType::from_recommendation(action_str)
                    .ok_or_else(|| format!("未知动作类型: {}", action_str))
            }
        }
    }

    /// 判断事件是否应该推送到 Hub（边缘折叠逻辑）
    pub async fn should_forward(&self, event: &Event) -> bool {
        match event.event_type {
            // 错误事件：必须推送
            EventType::ErrorHw | EventType::ErrorNet => true,

            // 进程状态变化：必须推送
            EventType::ProcessState => true,

            // 网络丢包：必须推送（重要阻塞信号）
            EventType::TransportDrop => true,

            // 拓扑降级：必须推送
            EventType::TopoLinkDown => true,

            // 计算资源事件：只在建立新绑定或利用率剧烈变化时推送
            EventType::ComputeUtil | EventType::ComputeMem => {
                if let Some(pid) = event.pid {
                    let binding_key = (pid, event.entity_id.clone());

                    // 检查是否是新的资源绑定（第一次推送）
                    let mut bindings = self.forwarded_bindings.write().await;
                    if !bindings.contains(&binding_key) {
                        // 新绑定，标记为已推送并允许推送
                        bindings.insert(binding_key.clone());
                        // 记录当前利用率
                        if let Ok(util) = event.value.parse::<f64>() {
                            self.last_util_values
                                .write()
                                .await
                                .insert(binding_key, util);
                        }
                        return true;
                    }

                    // 检查利用率是否发生剧烈变化（从 >80% 跌到 <1%，或从 <1% 升到 >80%）
                    if let Ok(current_util) = event.value.parse::<f64>() {
                        let mut last_utils = self.last_util_values.write().await;
                        if let Some(&last_util) = last_utils.get(&binding_key) {
                            // 检测剧烈变化：高->低 或 低->高
                            if (last_util > 80.0 && current_util < 1.0)
                                || (last_util < 1.0 && current_util > 80.0)
                            {
                                last_utils.insert(binding_key, current_util);
                                return true;
                            }
                        }
                    }
                }
                false
            }

            // 存储事件：类似计算资源，只在建立新绑定或剧烈变化时推送
            EventType::StorageIops | EventType::StorageQDepth => {
                if let Some(pid) = event.pid {
                    let binding_key = (pid, event.entity_id.clone());
                    let mut bindings = self.forwarded_bindings.write().await;
                    if !bindings.contains(&binding_key) {
                        bindings.insert(binding_key);
                        return true;
                    }
                }
                false
            }

            // 传输带宽：只在建立新绑定时推送
            EventType::TransportBw => {
                if let Some(pid) = event.pid {
                    let binding_key = (pid, event.entity_id.clone());
                    let mut bindings = self.forwarded_bindings.write().await;
                    if !bindings.contains(&binding_key) {
                        bindings.insert(binding_key);
                        return true;
                    }
                }
                false
            }

            // 其他事件：不推送（高频波动，由 Hub 通过查询获取）
            _ => false,
        }
    }

    /// 推送事件到 Hub
    pub async fn forward_event(
        &self,
        mut event: Event,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // 注入 node_id
        event.node_id = Some(self.node_id.clone());

        // 序列化为 JSON
        let payload = HubEventEnvelope {
            kind: "event".to_string(),
            protocol_version: HUB_PROTOCOL_VERSION.to_string(),
            schema_version: EVENT_SCHEMA_VERSION.to_string(),
            agent_id: self.node_id.clone(),
            feature_flags: vec!["edge_rollup".to_string()],
            event,
        };
        let json = serde_json::to_string(&payload)?;

        // 发送到 WebSocket
        if let Some(ref sender_arc) = self.ws_sender {
            let mut sender = sender_arc.write().await;
            if let Some(ref mut ws_sender) = *sender {
                ws_sender.send(Message::Text(json)).await?;
                return Ok(());
            }
        }

        Err("WebSocket 连接未建立".into())
    }

    /// 推送事件，失败时自动重连并重试一次。
    pub async fn forward_event_with_retry(
        &mut self,
        event: Event,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.pending_events.is_empty()
            && self.flush_pending_events().await.is_err()
            && self.reconnect().await.is_ok()
        {
            let _ = self.flush_pending_events().await;
        }

        if self.forward_event(event.clone()).await.is_ok() {
            return Ok(());
        }

        self.enqueue_pending_event(event);
        self.reconnect().await?;
        self.flush_pending_events().await
    }

    fn enqueue_pending_event(&mut self, event: Event) {
        if self.pending_events.len() >= PENDING_EVENT_CAPACITY {
            self.pending_events.pop_front();
        }
        self.pending_events.push_back(event);
    }

    async fn flush_pending_events(
        &mut self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        while let Some(event) = self.pending_events.pop_front() {
            if let Err(e) = self.forward_event(event.clone()).await {
                self.pending_events.push_front(event);
                return Err(e);
            }
        }
        Ok(())
    }
}

/// Hub 命令结构
#[derive(serde::Deserialize)]
struct HubCommand {
    intent: String,
    target_pid: u32,
    action: Option<String>,
}

/// 获取当前节点 ID（使用 hostname）
pub fn get_node_id() -> String {
    use std::process::Command;

    // 尝试获取 hostname
    if let Ok(output) = Command::new("hostname").output() {
        if let Ok(hostname) = String::from_utf8(output.stdout) {
            return hostname.trim().to_string();
        }
    }

    // 回退到环境变量
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown-node".to_string())
}

#[cfg(test)]
mod tests {
    use super::{HubForwarder, PENDING_EVENT_CAPACITY};
    use ark_core::event::{Event, EventType};
    use futures_util::StreamExt;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    fn test_event(ts: u64) -> Event {
        Event {
            ts,
            event_type: EventType::ErrorNet,
            entity_id: "eth0".to_string(),
            job_id: Some("job-1".to_string()),
            pid: Some(42),
            value: "drop".to_string(),
            node_id: None,
        }
    }

    #[test]
    fn pending_queue_is_bounded() {
        let mut forwarder =
            HubForwarder::new("ws://127.0.0.1:1".to_string(), "node-test".to_string());
        for i in 0..(PENDING_EVENT_CAPACITY + 5) {
            forwarder.enqueue_pending_event(test_event(i as u64));
        }
        assert_eq!(forwarder.pending_events.len(), PENDING_EVENT_CAPACITY);
        assert_eq!(
            forwarder.pending_events.front().map(|e| e.ts),
            Some(5),
            "oldest events should be evicted first"
        );
    }

    #[tokio::test]
    #[ignore = "requires local TCP socket permissions"]
    async fn forward_event_with_retry_reconnects_when_sender_missing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let ws_url = format!("ws://{}", addr);

        let (msg_tx, msg_rx) = oneshot::channel::<String>();
        let delivered = Arc::new(AtomicBool::new(false));
        let delivered_flag = Arc::clone(&delivered);
        let sender_cell = Arc::new(std::sync::Mutex::new(Some(msg_tx)));
        let sender_cell_bg = Arc::clone(&sender_cell);

        tokio::spawn(async move {
            while !delivered_flag.load(Ordering::Relaxed) {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let delivered_conn = Arc::clone(&delivered_flag);
                let sender_cell_conn = Arc::clone(&sender_cell_bg);
                tokio::spawn(async move {
                    let Ok(ws) = accept_async(stream).await else {
                        return;
                    };
                    let (_, mut read) = ws.split();
                    while let Some(msg) = read.next().await {
                        let Ok(Message::Text(text)) = msg else {
                            continue;
                        };
                        if !delivered_conn.swap(true, Ordering::SeqCst) {
                            let tx_opt = sender_cell_conn.lock().ok().and_then(|mut g| g.take());
                            if let Some(tx) = tx_opt {
                                let _ = tx.send(text.to_string());
                            }
                        }
                        break;
                    }
                });
            }
        });

        let mut forwarder = HubForwarder::new(ws_url, "node-test".to_string());
        forwarder.connect().await.expect("connect");

        // 模拟连接状态丢失，触发重连路径。
        forwarder.ws_sender = None;

        let event = test_event(1);

        forwarder
            .forward_event_with_retry(event)
            .await
            .expect("forward with retry");

        let payload = tokio::time::timeout(std::time::Duration::from_secs(3), msg_rx)
            .await
            .expect("receive timeout")
            .expect("receive payload");
        assert!(
            payload.contains("\"kind\":\"event\""),
            "payload={}",
            payload
        );
        assert!(
            payload.contains("\"agent_id\":\"node-test\""),
            "payload={}",
            payload
        );
    }
}
