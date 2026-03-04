//! Hub 事件转发器：将本地重要事件推送到 Hub
//!
//! 实现边缘折叠（Edge Roll-up）逻辑：
//! - 只推送错误事件、进程状态变化、触发规则的事件
//! - 过滤高频波动（如 gpu.util 的微小变化）

use crate::exec::{ActionExecutor, ActionType};
use ark_core::event::{Event, EventType};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
type WsSender =
    futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

/// Hub 事件转发器
pub struct HubForwarder {
    hub_url: String,
    node_id: String,
    ws_sender: Option<Arc<RwLock<Option<WsSender>>>>,
    command_listener_handle: Option<tokio::task::JoinHandle<()>>,
    forwarded_bindings: Arc<RwLock<HashSet<(u32, String)>>>,
    last_util_values: Arc<RwLock<std::collections::HashMap<(u32, String), f64>>>,
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
        }
    }

    /// 连接到 Hub WebSocket 服务器
    pub async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error>> {
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

    /// 处理 Hub 下发的命令
    async fn handle_command(cmd: HubCommand) -> Result<(), Box<dyn std::error::Error>> {
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
    pub async fn forward_event(&self, mut event: Event) -> Result<(), Box<dyn std::error::Error>> {
        // 注入 node_id
        event.node_id = Some(self.node_id.clone());

        // 序列化为 JSON
        let json = serde_json::to_string(&event)?;

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
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown-node".to_string())
}
