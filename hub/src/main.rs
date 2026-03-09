//! ark-hub: 全局中控
//!
//! 接收各节点的 WebSocket 连接，维护全局状态图
//! 提供跨节点的根因分析和集群级修复能力

use ark_core::event::{Event, EventType};
use ark_core::graph::{NodeType, StateGraph};
use ark_core::rules::RuleEngine;
use clap::Parser;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex, OwnedSemaphorePermit, RwLock, Semaphore};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use warp::{Filter, Reply};
mod dedup;
mod k8s_controller;
mod metrics;
mod wal;
use dedup::{accept_event_with_dedup, DedupStore};
use k8s_controller::{IrreversibleFault, K8sController};
use metrics::HubMetricsCollector;
use wal::{append_wal_event, open_wal_writer, replay_wal, WalWriter};

const DEFAULT_RULES_DIR: &str = "rules";
const DEFAULT_EVENT_BUFFER_SIZE: usize = 20_000;
const DEFAULT_WAL_PATH: &str = "tmp/hub-events.wal.jsonl";
const DEFAULT_DEDUP_WINDOW_S: u64 = 300;
const DEFAULT_EXECUTE_MAX_ACTIONS: usize = 20;
const DEFAULT_EXECUTE_MAX_CONCURRENCY: usize = 16;
const DEFAULT_EXECUTE_COOLDOWN_S: u64 = 60;
const DEFAULT_POLICY_VERSION: &str = "v1";

#[derive(Parser)]
#[command(name = "ark-hub")]
#[command(about = "Ark 全局中控：集群级状态图和根因分析")]
struct Cli {
    /// WebSocket 监听地址
    #[arg(long, default_value = "0.0.0.0:8080")]
    ws_listen: String,
    /// HTTP API 监听地址
    #[arg(long, default_value = "0.0.0.0:8081")]
    http_listen: String,
    /// 启用 Kubernetes 控制器（自动打污点和驱逐 Pod）
    #[arg(long)]
    enable_k8s_controller: bool,
    /// 规则目录
    #[arg(long, default_value = DEFAULT_RULES_DIR)]
    rules_dir: String,
    /// Hub 事件窗口缓存大小
    #[arg(long, default_value_t = DEFAULT_EVENT_BUFFER_SIZE)]
    event_buffer_size: usize,
    /// 事件 WAL 文件路径（JSONL，启动时自动回放）
    #[arg(long, default_value = DEFAULT_WAL_PATH)]
    wal_path: String,
    /// 事件 WAL 最大大小（MB），达到后轮转为 .1
    #[arg(long, default_value_t = 256)]
    wal_max_mb: u64,
    /// 允许 /api/v1/diagnose?execute=true 真正执行动作（默认关闭，仅 dry-run）
    #[arg(long, default_value_t = false)]
    allow_execute: bool,
    /// 事件去重窗口（秒），用于抑制补发/回放重复事件
    #[arg(long, default_value_t = DEFAULT_DEDUP_WINDOW_S)]
    dedup_window_s: u64,
    /// 每次 execute 请求最多执行的动作数量
    #[arg(long, default_value_t = DEFAULT_EXECUTE_MAX_ACTIONS)]
    execute_max_actions: usize,
    /// execute 请求全局并发上限
    #[arg(long, default_value_t = DEFAULT_EXECUTE_MAX_CONCURRENCY)]
    execute_max_concurrency: usize,
    /// 同一节点同一动作冷却窗口（秒）
    #[arg(long, default_value_t = DEFAULT_EXECUTE_COOLDOWN_S)]
    execute_cooldown_s: u64,
    /// 执行策略版本（用于审计和回放）
    #[arg(long, default_value = DEFAULT_POLICY_VERSION)]
    policy_version: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    println!("🚀 ark-hub 启动中...");
    println!("📡 WebSocket 监听地址: ws://{}", cli.ws_listen);
    println!("🌐 HTTP API 监听地址: http://{}", cli.http_listen);
    println!(
        "🛡️ 动作执行开关: {}",
        if cli.allow_execute {
            "enable (允许 execute=true)"
        } else {
            "disable (强制 dry-run)"
        }
    );

    // 创建全局状态图
    let global_graph = Arc::new(StateGraph::new());

    // 创建 Metrics 收集器
    let metrics = Arc::new(HubMetricsCollector::new()?);
    // 加载规则引擎
    let rule_engine = Arc::new(
        RuleEngine::load_from_dir(&cli.rules_dir)
            .map_err(|e| format!("加载规则目录失败 ({}): {}", cli.rules_dir, e))?,
    );
    metrics.record_rule_load_stats(rule_engine.load_stats());
    println!("📚 已加载规则数量: {}", rule_engine.rule_count());
    // 事件窗口缓存（用于诊断接口）
    let event_buffer = Arc::new(RwLock::new(VecDeque::with_capacity(cli.event_buffer_size)));
    let event_buffer_size = cli.event_buffer_size;
    let wal_path = cli.wal_path.clone();
    let dedup_window_ms = cli.dedup_window_s.saturating_mul(1000);
    let dedup_store: DedupStore = Arc::new(Mutex::new(HashMap::new()));

    // 启动时回放历史 WAL，恢复内存状态图与窗口缓存
    let replay_stats = replay_wal(
        &wal_path,
        Arc::clone(&global_graph),
        Arc::clone(&event_buffer),
        event_buffer_size,
        Arc::clone(&dedup_store),
        dedup_window_ms,
    )
    .await?;
    if replay_stats.replayed > 0 {
        println!("♻️  已从 WAL 回放事件: {} 条", replay_stats.replayed);
    }
    if replay_stats.corrupted_lines > 0
        || replay_stats.dedup_dropped > 0
        || replay_stats.process_failed > 0
    {
        eprintln!(
            "[hub] WAL 回放统计: corrupted_lines={}, dedup_dropped={}, process_failed={}",
            replay_stats.corrupted_lines, replay_stats.dedup_dropped, replay_stats.process_failed
        );
    }
    metrics.record_wal_replayed(replay_stats.replayed);
    metrics.record_wal_replay_corrupted_lines(replay_stats.corrupted_lines);
    metrics.record_wal_replay_dedup_dropped(replay_stats.dedup_dropped);
    metrics.record_wal_replay_process_failed(replay_stats.process_failed);

    // 打开 WAL 追加写句柄
    let wal_writer = Some(open_wal_writer(&wal_path, cli.wal_max_mb).await?);
    if let Ok(meta) = tokio::fs::metadata(&wal_path).await {
        metrics.update_wal_size_bytes(meta.len());
    }

    // 创建 K8s 控制器（如果启用）
    let k8s_controller = if cli.enable_k8s_controller {
        match K8sController::new(true).await {
            Ok(controller) => {
                println!("✅ Kubernetes 控制器已启用");
                Some(Arc::new(controller))
            }
            Err(e) => {
                eprintln!("⚠️  无法初始化 Kubernetes 控制器: {}", e);
                eprintln!("   继续运行，但不会执行自动节点隔离操作");
                None
            }
        }
    } else {
        println!("ℹ️  Kubernetes 控制器未启用（使用 --enable-k8s-controller 启用）");
        None
    };

    // 创建 WebSocket 连接管理器（node_id -> sender）
    let connections: Arc<DashMap<String, mpsc::UnboundedSender<Message>>> =
        Arc::new(DashMap::new());
    let execute_guard = Arc::new(ExecuteGuard::new(
        cli.execute_max_concurrency,
        cli.execute_cooldown_s,
        cli.execute_max_actions,
    ));

    // 启动 WebSocket 服务器
    let ws_listen = cli.ws_listen.clone();
    let ws_handle = {
        let graph = Arc::clone(&global_graph);
        let conns = Arc::clone(&connections);
        let k8s_ctrl = k8s_controller.clone();
        let metrics = Arc::clone(&metrics);
        let event_buffer = Arc::clone(&event_buffer);
        let wal_writer = wal_writer.clone();
        let dedup_store = Arc::clone(&dedup_store);
        tokio::spawn(async move {
            let listener = TcpListener::bind(&ws_listen).await?;
            println!("✅ WebSocket 服务器已启动，等待节点连接...");

            while let Ok((stream, addr)) = listener.accept().await {
                let graph = Arc::clone(&graph);
                let conns = Arc::clone(&conns);
                let k8s_ctrl = k8s_ctrl.clone();
                let metrics = Arc::clone(&metrics);
                let event_buffer = Arc::clone(&event_buffer);
                let wal_writer = wal_writer.clone();
                let dedup_store = Arc::clone(&dedup_store);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(
                        stream,
                        addr,
                        graph,
                        conns,
                        k8s_ctrl,
                        metrics,
                        event_buffer,
                        event_buffer_size,
                        wal_writer,
                        dedup_store,
                        dedup_window_ms,
                    )
                    .await
                    {
                        eprintln!("[hub] 处理连接 {} 时出错: {}", addr, e);
                    }
                });
            }

            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        })
    };

    // 启动指标更新任务（每 5 秒更新一次）
    let _metrics_update_handle = {
        let graph = Arc::clone(&global_graph);
        let metrics = Arc::clone(&metrics);
        let connections = Arc::clone(&connections);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                metrics.update_graph_metrics(&graph).await;
                // 更新 WebSocket 连接数
                let connected = connections.len();
                metrics.update_websocket_connections(connected, 0);
            }
        })
    };

    // 启动 HTTP API 服务器
    let http_listen = cli.http_listen.clone();
    let http_wal_path = wal_path.clone();
    let http_handle = {
        let graph = Arc::clone(&global_graph);
        let conns = Arc::clone(&connections);
        let metrics = Arc::clone(&metrics);
        let rules = Arc::clone(&rule_engine);
        let event_buffer = Arc::clone(&event_buffer);
        let k8s_ctrl = k8s_controller.clone();
        let execute_guard = Arc::clone(&execute_guard);
        let policy_version = cli.policy_version.clone();
        tokio::spawn(async move {
            // 创建 API 路由（包含 metrics 端点）
            let api = create_api_routes(ApiContext {
                graph,
                connections: conns,
                metrics,
                rules,
                event_buffer,
                k8s_controller: k8s_ctrl,
                allow_execute: cli.allow_execute,
                wal_path: http_wal_path,
                execute_guard,
                policy_version,
            });
            let bind_addr: SocketAddr = match http_listen.parse() {
                Ok(addr) => addr,
                Err(e) => {
                    eprintln!(
                        "[hub] 无法解析 HTTP 监听地址 '{}': {}，回退到 0.0.0.0:8081",
                        http_listen, e
                    );
                    "0.0.0.0:8081".parse().expect("fallback addr must be valid")
                }
            };
            println!("✅ HTTP API 服务器已启动");
            println!("📊 Prometheus Metrics 端点: http://{}/metrics", bind_addr);
            warp::serve(api).run(bind_addr).await;
        })
    };

    // 等待任一服务器退出
    tokio::select! {
        result = ws_handle => {
            if let Err(e) = result {
                eprintln!("[hub] WebSocket 服务器错误: {:?}", e);
            }
        }
        _ = http_handle => {
            println!("[hub] HTTP 服务器已关闭");
        }
    }

    Ok(())
}

/// 处理单个 WebSocket 连接
#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    stream: TcpStream,
    addr: std::net::SocketAddr,
    graph: Arc<StateGraph>,
    connections: Arc<DashMap<String, mpsc::UnboundedSender<Message>>>,
    k8s_controller: Option<Arc<K8sController>>,
    metrics: Arc<HubMetricsCollector>,
    event_buffer: Arc<RwLock<VecDeque<Event>>>,
    event_buffer_size: usize,
    wal_writer: Option<WalWriter>,
    dedup_store: DedupStore,
    dedup_window_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("[hub] 新节点连接: {}", addr);

    let ws_stream = accept_async(stream).await?;
    let (mut write, mut read) = ws_stream.split();

    // 创建用于发送消息的通道
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // 从连接地址生成默认 node_id（Agent 会在第一个事件中提供真实的 node_id）
    let mut node_id = format!("node-{}", addr.ip());

    // 立即注册连接（使用默认 node_id，后续可能被事件中的 node_id 更新）
    connections.insert(node_id.clone(), tx.clone());
    println!("[hub] 注册节点连接: {} (临时)", node_id);

    // 启动消息转发任务（从通道转发到 WebSocket write 端）
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = write.send(msg).await {
                eprintln!("[hub] 发送消息失败: {}", e);
                break;
            }
        }
    });

    // 读取事件并更新全局图
    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(text) => {
                // 解析事件
                match parse_hub_event_message(&text) {
                    Ok(mut event) => {
                        // 如果事件中包含 node_id，使用它并更新连接表
                        if let Some(event_node_id) = &event.node_id {
                            if *event_node_id != node_id {
                                // node_id 发生变化，更新连接表
                                connections.remove(&node_id);
                                node_id = event_node_id.clone();
                                connections.insert(node_id.clone(), tx.clone());
                                println!("[hub] 更新节点连接: {}", node_id);
                            }
                        } else {
                            // 事件中没有 node_id，使用默认值
                            event.node_id = Some(node_id.clone());
                        }

                        if !accept_event_with_dedup(&event, &dedup_store, dedup_window_ms).await {
                            continue;
                        }

                        // 更新全局图
                        if let Err(e) = graph.process_event(&event).await {
                            eprintln!("[hub] 处理事件失败: {}", e);
                        } else {
                            println!("[hub] 收到事件: {:?} from {}", event.event_type, node_id);
                            metrics.record_event_received(&event.event_type.to_string(), &node_id);
                            {
                                let mut buf = event_buffer.write().await;
                                if buf.len() >= event_buffer_size {
                                    buf.pop_front();
                                }
                                buf.push_back(event.clone());
                            }
                            if let Some(writer) = wal_writer.as_ref() {
                                match append_wal_event(writer, &event).await {
                                    Ok(append_result) => {
                                        if append_result.rotated {
                                            metrics.record_wal_rotation();
                                        }
                                        metrics.update_wal_size_bytes(append_result.size_bytes);
                                    }
                                    Err(e) => {
                                        eprintln!("[hub] 写入 WAL 失败: {}", e);
                                        metrics.record_wal_append_error();
                                    }
                                }
                            }

                            // 检测不可逆故障并触发 K8s 操作
                            if let Some(ref controller) = k8s_controller {
                                if let Some(fault) = controller.detect_irreversible_fault(&event) {
                                    // 在后台任务中处理故障（避免阻塞事件处理）
                                    let controller_clone = Arc::clone(controller);
                                    tokio::spawn(async move {
                                        if let Err(e) =
                                            controller_clone.handle_irreversible_fault(&fault).await
                                        {
                                            eprintln!("[k8s-controller] 处理故障失败: {}", e);
                                        }
                                    });
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[hub] 解析事件失败: {}", e);
                    }
                }
            }
            Message::Close(_) => {
                println!("[hub] 节点 {} 断开连接", node_id);
                break;
            }
            _ => {}
        }
    }

    // 从连接表中移除
    connections.remove(&node_id);
    println!("[hub] 节点 {} 已从连接表移除", node_id);

    // 等待写任务结束
    write_task.abort();

    Ok(())
}

#[derive(serde::Deserialize)]
struct HubEventEnvelope {
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default = "default_protocol_version")]
    protocol_version: String,
    #[serde(default = "default_schema_version")]
    schema_version: String,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    feature_flags: Vec<String>,
    event: Event,
}

fn default_kind() -> String {
    "event".to_string()
}

fn default_protocol_version() -> String {
    "1.0".to_string()
}

fn default_schema_version() -> String {
    "1.0".to_string()
}

fn parse_hub_event_message(raw: &str) -> Result<Event, String> {
    // Backward compatible:
    // 1) New envelope: { protocol_version, feature_flags, event }
    // 2) Legacy payload: Event
    if let Ok(envelope) = serde_json::from_str::<HubEventEnvelope>(raw) {
        if envelope.protocol_version.is_empty() {
            return Err("protocol_version 不能为空".to_string());
        }
        if envelope.kind != "event" {
            return Err(format!("当前仅支持 kind=event，收到: {}", envelope.kind));
        }
        let _ = envelope.feature_flags;
        let _ = envelope.schema_version;
        let mut event = envelope.event;
        if event.node_id.is_none() {
            event.node_id = envelope.agent_id;
        }
        return Ok(event);
    }

    serde_json::from_str::<Event>(raw).map_err(|e| format!("无法解析消息: {}", e))
}

/// Warp Filter：注入 StateGraph 状态
fn with_graph(
    graph: Arc<StateGraph>,
) -> impl Filter<Extract = (Arc<StateGraph>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || graph.clone())
}

/// Warp Filter：注入连接管理器
fn with_connections(
    connections: Arc<DashMap<String, mpsc::UnboundedSender<Message>>>,
) -> impl Filter<
    Extract = (Arc<DashMap<String, mpsc::UnboundedSender<Message>>>,),
    Error = std::convert::Infallible,
> + Clone {
    warp::any().map(move || connections.clone())
}

/// Fix 请求结构
#[derive(serde::Deserialize)]
struct FixRequest {
    node_id: String,
    target_pid: u32,
    action: Option<String>, // 可选，默认 "GracefulShutdown"
}

#[derive(serde::Serialize, Clone)]
struct PolicyAction {
    #[serde(rename = "type")]
    action_type: String,
    node_id: String,
    target_pid: u32,
    action: String,
    dry_run: bool,
}

fn build_policy_actions(
    scenes: &[String],
    processes: &[serde_json::Value],
    dry_run: bool,
) -> Vec<PolicyAction> {
    let action = if scenes.iter().any(|s| s.contains("physical_degradation")) {
        "KillProcess"
    } else {
        "GracefulShutdown"
    };

    processes
        .iter()
        .filter_map(|p| {
            let node_id = p.get("node_id").and_then(|v| v.as_str())?;
            let pid = p.get("pid").and_then(|v| v.as_u64())?;
            Some(PolicyAction {
                action_type: "send_fix_command".to_string(),
                node_id: node_id.to_string(),
                target_pid: pid as u32,
                action: action.to_string(),
                dry_run,
            })
        })
        .collect()
}

fn send_fix_command(
    conns: &DashMap<String, mpsc::UnboundedSender<Message>>,
    node_id: &str,
    target_pid: u32,
    action: &str,
) -> Result<(), String> {
    let sender = conns
        .get(node_id)
        .ok_or_else(|| format!("节点 {} 未连接", node_id))?;
    let command = json!({
        "intent": "fix",
        "target_pid": target_pid,
        "action": action
    });
    let json_str = serde_json::to_string(&command).map_err(|e| format!("序列化命令失败: {}", e))?;
    sender
        .send(Message::Text(json_str))
        .map_err(|_| "发送命令失败：连接已关闭".to_string())
}

fn classify_training_slow(events: &[Event]) -> (&'static str, f64) {
    let mut bw_values = Vec::new();
    let mut drop_values = Vec::new();
    let mut storage_values = Vec::new();

    for e in events {
        if let Ok(v) = e.value.parse::<f64>() {
            match e.event_type {
                EventType::TransportBw => bw_values.push(v),
                EventType::TransportDrop => drop_values.push(v),
                EventType::StorageIops => storage_values.push(v),
                _ => {}
            }
        }
    }

    let avg = |vals: &[f64]| -> f64 {
        if vals.is_empty() {
            0.0
        } else {
            vals.iter().sum::<f64>() / vals.len() as f64
        }
    };

    let avg_bw = avg(&bw_values);
    let avg_drop = avg(&drop_values);
    let avg_storage_iops = avg(&storage_values);

    if avg_drop > 10.0 || avg_bw < 30.0 {
        ("comm_bound", (avg_drop * 2.0).clamp(0.0, 100.0))
    } else if avg_storage_iops < 20.0 {
        ("io_bound", (100.0 - avg_storage_iops).clamp(0.0, 100.0))
    } else {
        ("cpu_bound", 50.0)
    }
}

/// Warp Filter：注入 Metrics 收集器
fn with_metrics(
    metrics: Arc<HubMetricsCollector>,
) -> impl Filter<Extract = (Arc<HubMetricsCollector>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || metrics.clone())
}

fn with_rules(
    rules: Arc<RuleEngine>,
) -> impl Filter<Extract = (Arc<RuleEngine>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || rules.clone())
}

fn with_event_buffer(
    event_buffer: Arc<RwLock<VecDeque<Event>>>,
) -> impl Filter<Extract = (Arc<RwLock<VecDeque<Event>>>,), Error = std::convert::Infallible> + Clone
{
    warp::any().map(move || event_buffer.clone())
}

fn with_k8s_controller(
    k8s_controller: Option<Arc<K8sController>>,
) -> impl Filter<Extract = (Option<Arc<K8sController>>,), Error = std::convert::Infallible> + Clone
{
    warp::any().map(move || k8s_controller.clone())
}

fn with_allow_execute(
    allow_execute: bool,
) -> impl Filter<Extract = (bool,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || allow_execute)
}

#[derive(Clone)]
struct ApiContext {
    graph: Arc<StateGraph>,
    connections: Arc<DashMap<String, mpsc::UnboundedSender<Message>>>,
    metrics: Arc<HubMetricsCollector>,
    rules: Arc<RuleEngine>,
    event_buffer: Arc<RwLock<VecDeque<Event>>>,
    k8s_controller: Option<Arc<K8sController>>,
    allow_execute: bool,
    wal_path: String,
    execute_guard: Arc<ExecuteGuard>,
    policy_version: String,
}

struct ExecuteGuard {
    semaphore: Arc<Semaphore>,
    cooldown_ms: u64,
    max_actions: usize,
    last_action_ms: Mutex<HashMap<String, u64>>,
    request_seq: AtomicU64,
}

impl ExecuteGuard {
    fn new(max_concurrency: usize, cooldown_s: u64, max_actions: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrency.max(1))),
            cooldown_ms: cooldown_s.saturating_mul(1000),
            max_actions: max_actions.max(1),
            last_action_ms: Mutex::new(HashMap::new()),
            request_seq: AtomicU64::new(1),
        }
    }

    fn next_request_id(&self) -> String {
        let seq = self.request_seq.fetch_add(1, Ordering::Relaxed);
        format!("req-{}-{}", now_ms(), seq)
    }

    fn max_actions(&self) -> usize {
        self.max_actions
    }

    fn cooldown_s(&self) -> u64 {
        self.cooldown_ms / 1000
    }

    fn try_acquire(&self) -> Result<OwnedSemaphorePermit, String> {
        self.semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| "concurrency_limit_reached".to_string())
    }

    async fn check_and_record_cooldown(
        &self,
        node_id: &str,
        action: &str,
        now_ms: u64,
    ) -> Result<(), String> {
        let key = format!("{}::{}", node_id, action);
        let mut guard = self.last_action_ms.lock().await;
        if let Some(prev) = guard.get(&key) {
            if now_ms.saturating_sub(*prev) < self.cooldown_ms {
                return Err(format!("cooldown_active:{}s", self.cooldown_s()));
            }
        }
        guard.insert(key, now_ms);
        Ok(())
    }
}

/// 创建 HTTP API 路由
fn create_api_routes(
    ctx: ApiContext,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let graph_filter = with_graph(ctx.graph.clone());
    let conns_filter = with_connections(ctx.connections.clone());
    let metrics_filter = with_metrics(ctx.metrics.clone());
    let rules_filter = with_rules(ctx.rules.clone());
    let event_buffer_filter = with_event_buffer(ctx.event_buffer.clone());
    let k8s_filter = with_k8s_controller(ctx.k8s_controller.clone());
    let allow_execute_filter = with_allow_execute(ctx.allow_execute);
    let wal_path_filter = warp::any().map(move || ctx.wal_path.clone());
    let execute_guard_filter = warp::any().map(move || ctx.execute_guard.clone());
    let policy_version_filter = warp::any().map(move || ctx.policy_version.clone());

    // GET /metrics - Prometheus Metrics 端点
    let metrics_route = warp::path("metrics")
        .and(warp::get())
        .and(metrics_filter.clone())
        .and_then(|metrics: Arc<HubMetricsCollector>| async move {
            match metrics.gather() {
                Ok(body) => Ok::<_, warp::Rejection>(
                    warp::reply::with_header(body, "content-type", "text/plain; version=0.0.4")
                        .into_response(),
                ),
                Err(e) => {
                    eprintln!("[hub-metrics] 收集指标失败: {}", e);
                    Ok(warp::reply::with_status(
                        format!("Error: {}", e),
                        warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    )
                    .into_response())
                }
            }
        });

    // GET /api/v1/why?job_id=xxx
    let why_route = warp::path!("api" / "v1" / "why")
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(graph_filter.clone())
        .and_then(
            |params: std::collections::HashMap<String, String>, graph: Arc<StateGraph>| async move {
                if let Some(job_id) = params.get("job_id") {
                    match cluster_why(graph, job_id).await {
                        Ok((causes, processes)) => {
                            Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                                "job_id": job_id,
                                "causes": causes,
                                "processes": processes
                            })))
                        }
                        Err(e) => Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                            "error": e.to_string()
                        }))),
                    }
                } else {
                    Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                        "error": "missing job_id parameter"
                    })))
                }
            },
        );

    // GET /api/v1/ps
    let ps_route = warp::path!("api" / "v1" / "ps")
        .and(graph_filter.clone())
        .and_then(|graph: Arc<StateGraph>| async move {
            let processes = graph.get_active_processes().await;
            let result: Vec<serde_json::Value> = processes
                .iter()
                .map(|node| {
                    json!({
                        "id": node.id,
                        "job_id": node.metadata.get("job_id").unwrap_or(&"-".to_string()),
                        "state": node.metadata.get("state").unwrap_or(&"unknown".to_string()),
                    })
                })
                .collect();
            Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                "processes": result
            })))
        });

    // GET /api/v1/health
    let health_route = warp::path!("api" / "v1" / "health")
        .and(warp::get())
        .and(graph_filter.clone())
        .and(conns_filter.clone())
        .and(wal_path_filter.clone())
        .and(rules_filter.clone())
        .and_then(
            |graph: Arc<StateGraph>,
             conns: Arc<DashMap<String, mpsc::UnboundedSender<Message>>>,
             wal_path: String,
             rules: Arc<RuleEngine>| async move {
                let nodes = graph.get_nodes_async().await;
                let edges = graph.get_all_edges_async().await;
                let active_meta = tokio::fs::metadata(&wal_path).await.ok();
                let rotated_path = format!("{}.1", wal_path);
                let rotated_meta = tokio::fs::metadata(&rotated_path).await.ok();
                let active_mtime_ms = active_meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64);

                let stats = rules.load_stats();
                Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                    "status": "ok",
                    "timestamp_ms": now_ms(),
                    "graph": {
                        "nodes_total": nodes.len(),
                        "edges_total": edges.len(),
                    },
                    "connections": {
                        "agents_connected": conns.len(),
                    },
                    "rules": {
                        "loaded": stats.loaded_rules,
                        "skipped": stats.skipped_rules,
                        "legacy": stats.legacy_total,
                    },
                    "wal": {
                        "path": wal_path,
                        "active_exists": active_meta.is_some(),
                        "active_size_bytes": active_meta.as_ref().map(|m| m.len()).unwrap_or(0),
                        "active_last_modified_ms": active_mtime_ms,
                        "rotated_path": rotated_path,
                        "rotated_exists": rotated_meta.is_some(),
                        "rotated_size_bytes": rotated_meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    }
                })))
            },
        );

    // GET /api/v1/diagnose?job_id=xxx&window_s=60&execute=false
    let diagnose_route = warp::path!("api" / "v1" / "diagnose")
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(graph_filter.clone())
        .and(conns_filter.clone())
        .and(rules_filter.clone())
        .and(event_buffer_filter.clone())
        .and(k8s_filter.clone())
        .and(allow_execute_filter.clone())
        .and(execute_guard_filter.clone())
        .and(policy_version_filter.clone())
        .and_then(
            |params: std::collections::HashMap<String, String>,
             graph: Arc<StateGraph>,
             conns: Arc<DashMap<String, mpsc::UnboundedSender<Message>>>,
             rules: Arc<RuleEngine>,
             event_buffer: Arc<RwLock<VecDeque<Event>>>,
             k8s_controller: Option<Arc<K8sController>>,
             allow_execute: bool,
             execute_guard: Arc<ExecuteGuard>,
             policy_version: String| async move {
                let job_id = if let Some(job_id) = params.get("job_id") {
                    job_id.to_string()
                } else {
                    return Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                        "error": "missing job_id parameter"
                    })));
                };

                let window_s = params
                    .get("window_s")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(60)
                    .clamp(10, 3600);
                let execute_requested = params
                    .get("execute")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                let execute = allow_execute && execute_requested;
                let dry_run = !execute;

                let now_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let cutoff = now_ts.saturating_sub(window_s * 1000);

                let candidate_events: Vec<Event> = {
                    let buf = event_buffer.read().await;
                    buf.iter()
                        .filter(|e| e.ts >= cutoff)
                        .filter(|e| e.job_id.as_deref() == Some(job_id.as_str()))
                        .cloned()
                        .collect()
                };

                let matched_rules = rules.match_rules(&graph, &candidate_events).await;
                let matched_rules_json: Vec<serde_json::Value> = matched_rules
                    .iter()
                    .map(|r| {
                        json!({
                            "name": r.name,
                            "scene": r.scene,
                            "priority": r.priority,
                            "root_cause": r.root_cause_pattern.primary,
                            "solution_steps": r.solution_steps,
                            "related_evidences": r.related_evidences
                        })
                    })
                    .collect();
                let scenes: Vec<String> = matched_rules.iter().map(|r| r.scene.clone()).collect();

                let (_, processes) = cluster_why(graph.clone(), &job_id)
                    .await
                    .map_err(|_| warp::reject::reject())?;
                let policy = build_policy_actions(&scenes, &processes, dry_run);
                let request_id = execute_guard.next_request_id();
                let max_actions = execute_guard.max_actions();
                let trimmed_policy: Vec<PolicyAction> =
                    policy.iter().take(max_actions).cloned().collect();
                let truncated = policy.len() > trimmed_policy.len();

                let mut execution_results = Vec::new();
                let mut concurrency_limited = false;
                if execute {
                    let permit = execute_guard.try_acquire();
                    if let Ok(_permit) = permit {
                        for action in &trimmed_policy {
                            match execute_guard
                                .check_and_record_cooldown(&action.node_id, &action.action, now_ts)
                                .await
                            {
                                Ok(_) => {
                                    let result = send_fix_command(
                                        &conns,
                                        &action.node_id,
                                        action.target_pid,
                                        &action.action,
                                    );
                                    let success = result.is_ok();
                                    let error = result.err();
                                    execution_results.push(json!({
                                        "node_id": action.node_id,
                                        "target_pid": action.target_pid,
                                        "action": action.action,
                                        "success": success,
                                        "error": error
                                    }));
                                    println!(
                                        "{}",
                                        json!({
                                            "type": "execution_audit",
                                            "request_id": request_id,
                                            "policy_version": policy_version,
                                            "job_id": job_id,
                                            "node_id": action.node_id,
                                            "target_pid": action.target_pid,
                                            "action": action.action,
                                            "success": success,
                                            "ts_ms": now_ms()
                                        })
                                    );
                                }
                                Err(e) => {
                                    execution_results.push(json!({
                                        "node_id": action.node_id,
                                        "target_pid": action.target_pid,
                                        "action": action.action,
                                        "success": false,
                                        "error": e
                                    }));
                                }
                            }
                        }
                    } else {
                        concurrency_limited = true;
                        execution_results.push(json!({
                            "success": false,
                            "error": "concurrency_limit_reached"
                        }));
                    }

                    // 针对物理层退化场景，额外触发节点隔离（如果控制器启用）
                    if scenes.iter().any(|s| s.contains("physical_degradation")) {
                        if let Some(controller) = k8s_controller {
                            for p in &processes {
                                if let Some(node_id) = p.get("node_id").and_then(|v| v.as_str()) {
                                    let fault = IrreversibleFault::OtherHardwareFailure {
                                        node_id: node_id.to_string(),
                                        reason: "rdma physical degradation".to_string(),
                                    };
                                    if let Err(e) =
                                        controller.handle_irreversible_fault(&fault).await
                                    {
                                        execution_results.push(json!({
                                            "node_id": node_id,
                                            "action": "taint_evict",
                                            "success": false,
                                            "error": e.to_string()
                                        }));
                                    }
                                }
                            }
                        }
                    }
                }

                Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                    "job_id": job_id,
                    "window_s": window_s,
                    "event_count": candidate_events.len(),
                    "matched_rules": matched_rules_json,
                    "processes": processes,
                    "policy": trimmed_policy,
                    "dry_run": dry_run,
                    "execute_requested": execute_requested,
                    "execute_enabled": allow_execute,
                    "request_id": request_id,
                    "policy_version": policy_version,
                    "execution_guard": {
                        "max_actions": max_actions,
                        "cooldown_s": execute_guard.cooldown_s(),
                        "truncated": truncated,
                        "concurrency_limited": concurrency_limited,
                    },
                    "execution": execution_results
                })))
            },
        );

    // GET /api/v1/incidents?window_s=300&limit=50
    let incidents_route = warp::path!("api" / "v1" / "incidents")
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(graph_filter.clone())
        .and(rules_filter.clone())
        .and(event_buffer_filter.clone())
        .and_then(
            |params: std::collections::HashMap<String, String>,
             graph: Arc<StateGraph>,
             rules: Arc<RuleEngine>,
             event_buffer: Arc<RwLock<VecDeque<Event>>>| async move {
                let window_s = params
                    .get("window_s")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(300)
                    .clamp(30, 7200);
                let limit = params
                    .get("limit")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(50)
                    .clamp(1, 500);
                let now = now_ms();
                let cutoff = now.saturating_sub(window_s * 1000);

                let candidate_events: Vec<Event> = {
                    let buf = event_buffer.read().await;
                    buf.iter().filter(|e| e.ts >= cutoff).cloned().collect()
                };

                let incidents = aggregate_incidents(&graph, &rules, &candidate_events, limit).await;
                Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                    "status": "ok",
                    "timestamp_ms": now,
                    "window_s": window_s,
                    "total_events": candidate_events.len(),
                    "incidents": incidents
                })))
            },
        );

    // GET /api/v1/preflight?node_id=node-a&window_s=120
    let preflight_route = warp::path!("api" / "v1" / "preflight")
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(event_buffer_filter.clone())
        .and_then(
            |params: std::collections::HashMap<String, String>,
             event_buffer: Arc<RwLock<VecDeque<Event>>>| async move {
                let node_id = if let Some(v) = params.get("node_id") {
                    v.to_string()
                } else {
                    return Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                        "error": "missing node_id parameter"
                    })));
                };
                let window_s = params
                    .get("window_s")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(120)
                    .clamp(30, 3600);

                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let cutoff = now_ms.saturating_sub(window_s * 1000);

                let node_events: Vec<Event> = {
                    let buf = event_buffer.read().await;
                    buf.iter()
                        .filter(|e| e.ts >= cutoff)
                        .filter(|e| e.node_id.as_deref() == Some(node_id.as_str()))
                        .cloned()
                        .collect()
                };

                let has_critical_fault = node_events.iter().any(|e| {
                    matches!(e.event_type, EventType::ErrorHw | EventType::TopoLinkDown)
                        || (e.event_type == EventType::ErrorNet
                            && (e.value.contains("pfc_storm")
                                || e.value.contains("phy_degradation")
                                || e.value.contains("link_down")))
                });

                Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                    "node_id": node_id,
                    "window_s": window_s,
                    "healthy": !has_critical_fault,
                    "event_count": node_events.len(),
                    "reason": if has_critical_fault { "critical_fault_detected" } else { "ok" }
                })))
            },
        );

    // GET /api/v1/training_slow?job_id=job-xx&window_s=120
    let training_slow_route = warp::path!("api" / "v1" / "training_slow")
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(event_buffer_filter.clone())
        .and_then(
            |params: std::collections::HashMap<String, String>,
             event_buffer: Arc<RwLock<VecDeque<Event>>>| async move {
                let job_id = if let Some(v) = params.get("job_id") {
                    v.to_string()
                } else {
                    return Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                        "error": "missing job_id parameter"
                    })));
                };
                let window_s = params
                    .get("window_s")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(120)
                    .clamp(30, 3600);

                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let cutoff = now_ms.saturating_sub(window_s * 1000);
                let job_events: Vec<Event> = {
                    let buf = event_buffer.read().await;
                    buf.iter()
                        .filter(|e| e.ts >= cutoff)
                        .filter(|e| e.job_id.as_deref() == Some(job_id.as_str()))
                        .cloned()
                        .collect()
                };

                let (rca, score) = classify_training_slow(&job_events);

                Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                    "job_id": job_id,
                    "window_s": window_s,
                    "event_count": job_events.len(),
                    "rca": rca,
                    "score": score
                })))
            },
        );

    // POST /api/v1/fix
    let fix_route = warp::path!("api" / "v1" / "fix")
        .and(warp::post())
        .and(warp::body::json())
        .and(conns_filter)
        .and_then(|req: FixRequest, conns: Arc<DashMap<String, mpsc::UnboundedSender<Message>>>| async move {
            let action = req.action.as_deref().unwrap_or("GracefulShutdown");
            if send_fix_command(&conns, &req.node_id, req.target_pid, action).is_ok() {
                Ok::<_, warp::Rejection>(warp::reply::json(&json!({
                    "success": true,
                    "message": format!("命令已发送到节点 {}", req.node_id)
                })).into_response())
            } else {
                Ok(warp::reply::with_status(
                    warp::reply::json(&json!({
                        "error": format!("节点 {} 未连接", req.node_id)
                    })),
                    warp::http::StatusCode::NOT_FOUND
                ).into_response())
            }
        });

    metrics_route
        .or(health_route)
        .or(why_route)
        .or(ps_route)
        .or(diagnose_route)
        .or(incidents_route)
        .or(preflight_route)
        .or(training_slow_route)
        .or(fix_route)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

async fn aggregate_incidents(
    graph: &Arc<StateGraph>,
    rules: &Arc<RuleEngine>,
    events: &[Event],
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut grouped: HashMap<String, Vec<Event>> = HashMap::new();
    for event in events {
        let key = event
            .job_id
            .clone()
            .unwrap_or_else(|| "__no_job__".to_string());
        grouped.entry(key).or_default().push(event.clone());
    }

    let mut incidents = Vec::new();
    for (job_key, job_events) in grouped {
        let matches = rules.match_rules(graph, &job_events).await;
        if matches.is_empty() {
            continue;
        }
        let mut nodes: Vec<String> = job_events
            .iter()
            .filter_map(|e| e.node_id.clone())
            .collect();
        nodes.sort();
        nodes.dedup();

        for matched in matches {
            incidents.push(json!({
                "scene": matched.scene,
                "rule": matched.name,
                "job_id": if job_key == "__no_job__" { serde_json::Value::Null } else { json!(job_key) },
                "nodes": nodes,
                "priority": matched.priority,
                "severity": severity_from_priority(matched.priority),
                "event_count": job_events.len(),
                "root_cause": matched.root_cause_pattern.primary,
            }));
        }
    }

    incidents.sort_by(|a, b| {
        let ap = a.get("priority").and_then(|v| v.as_u64()).unwrap_or(0);
        let bp = b.get("priority").and_then(|v| v.as_u64()).unwrap_or(0);
        let ac = a.get("event_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let bc = b.get("event_count").and_then(|v| v.as_u64()).unwrap_or(0);
        bp.cmp(&ap).then(bc.cmp(&ac))
    });
    incidents.truncate(limit);
    incidents
}

fn severity_from_priority(priority: u32) -> &'static str {
    if priority >= 80 {
        "critical"
    } else if priority >= 50 {
        "high"
    } else if priority >= 20 {
        "medium"
    } else {
        "low"
    }
}

/// 集群级根因分析：根据 job_id 查找所有相关进程并分析根因
async fn cluster_why(
    graph: Arc<StateGraph>,
    target_job_id: &str,
) -> Result<(Vec<String>, Vec<serde_json::Value>), Box<dyn std::error::Error>> {
    let nodes = graph.get_nodes_async().await;
    let mut global_causes = Vec::new();

    // 1. 在全局图中找出所有属于这个 job_id 的进程节点
    let job_pids: Vec<String> = nodes
        .iter()
        .filter(|(_, n)| {
            n.node_type == NodeType::Process
                && n.metadata.get("job_id") == Some(&target_job_id.to_string())
        })
        .map(|(id, _)| id.clone())
        .collect();

    if job_pids.is_empty() {
        return Ok((
            vec![format!("未找到 job_id={} 的进程", target_job_id)],
            Vec::new(),
        ));
    }

    // 2. 构建进程列表（用于 CLI 提取节点和 PID）
    let mut process_list = Vec::new();

    // 3. 对每个进程节点，在全局图中发起根因分析
    // 直接使用完整的节点 ID（包含命名空间），避免命名空间丢失
    for pid_id in &job_pids {
        // 提取节点 ID 和 PID 并添加到进程列表
        if pid_id.contains("::") {
            let parts: Vec<&str> = pid_id.split("::").collect();
            let node_id = parts[0].to_string();
            if let Some(pid_part) = parts.get(1) {
                if let Some(pid_str) = pid_part.strip_prefix("pid-") {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        process_list.push(json!({
                            "node_id": node_id,
                            "pid": pid,
                            "node_id_full": pid_id
                        }));
                    }
                }
            }
        }

        let causes = graph.find_root_cause_by_id(pid_id).await;
        for cause in causes {
            // 添加节点信息到根因描述中
            let node_info = if pid_id.contains("::") {
                let node_name = pid_id.split("::").next().unwrap_or("unknown");
                format!("{}: {}", node_name, cause)
            } else {
                cause
            };
            global_causes.push(node_info);
        }
    }

    // 4. 去重并返回全局根因和进程列表
    global_causes.sort();
    global_causes.dedup();

    Ok((global_causes, process_list))
}

#[cfg(test)]
mod tests {
    use super::{build_policy_actions, classify_training_slow, severity_from_priority};
    use ark_core::event::{Event, EventType};
    use serde_json::json;

    #[test]
    fn classify_training_slow_detects_comm_bound() {
        let events = vec![
            Event {
                ts: 1,
                event_type: EventType::TransportDrop,
                entity_id: "roce-mlx5_0".to_string(),
                job_id: Some("job-1".to_string()),
                pid: Some(10),
                value: "25".to_string(),
                node_id: Some("node-a".to_string()),
            },
            Event {
                ts: 2,
                event_type: EventType::TransportBw,
                entity_id: "roce-mlx5_0".to_string(),
                job_id: Some("job-1".to_string()),
                pid: Some(10),
                value: "20".to_string(),
                node_id: Some("node-a".to_string()),
            },
        ];
        let (rca, score) = classify_training_slow(&events);
        assert_eq!(rca, "comm_bound");
        assert!(score > 0.0);
    }

    #[test]
    fn build_policy_actions_generates_fix_commands() {
        let scenes = vec!["rdma_pfc_storm".to_string()];
        let processes = vec![json!({
            "node_id": "node-a",
            "pid": 1234
        })];

        let actions = build_policy_actions(&scenes, &processes, true);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].node_id, "node-a");
        assert_eq!(actions[0].target_pid, 1234);
        assert_eq!(actions[0].action, "GracefulShutdown");
        assert!(actions[0].dry_run);
    }

    #[test]
    fn severity_from_priority_maps_levels() {
        assert_eq!(severity_from_priority(90), "critical");
        assert_eq!(severity_from_priority(60), "high");
        assert_eq!(severity_from_priority(30), "medium");
        assert_eq!(severity_from_priority(10), "low");
    }
}
