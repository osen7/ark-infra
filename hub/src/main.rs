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
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use warp::{Filter, Reply};
mod k8s_controller;
mod metrics;
use k8s_controller::{IrreversibleFault, K8sController};
use metrics::HubMetricsCollector;

const DEFAULT_RULES_DIR: &str = "rules";
const DEFAULT_EVENT_BUFFER_SIZE: usize = 20_000;
const DEFAULT_WAL_PATH: &str = "tmp/hub-events.wal.jsonl";

struct WalState {
    file: File,
    path: PathBuf,
    max_bytes: u64,
}

type WalWriter = Arc<Mutex<WalState>>;

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

    // 启动时回放历史 WAL，恢复内存状态图与窗口缓存
    let replayed = replay_wal(
        &wal_path,
        Arc::clone(&global_graph),
        Arc::clone(&event_buffer),
        event_buffer_size,
    )
    .await?;
    if replayed > 0 {
        println!("♻️  已从 WAL 回放事件: {} 条", replayed);
    }

    // 打开 WAL 追加写句柄
    let wal_writer = Some(open_wal_writer(&wal_path, cli.wal_max_mb).await?);

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

    // 启动 WebSocket 服务器
    let ws_listen = cli.ws_listen.clone();
    let ws_handle = {
        let graph = Arc::clone(&global_graph);
        let conns = Arc::clone(&connections);
        let k8s_ctrl = k8s_controller.clone();
        let metrics = Arc::clone(&metrics);
        let event_buffer = Arc::clone(&event_buffer);
        let wal_writer = wal_writer.clone();
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
    let http_handle = {
        let graph = Arc::clone(&global_graph);
        let conns = Arc::clone(&connections);
        let metrics = Arc::clone(&metrics);
        let rules = Arc::clone(&rule_engine);
        let event_buffer = Arc::clone(&event_buffer);
        let k8s_ctrl = k8s_controller.clone();
        tokio::spawn(async move {
            // 创建 API 路由（包含 metrics 端点）
            let api = create_api_routes(
                graph,
                conns,
                metrics,
                rules,
                event_buffer,
                k8s_ctrl,
                cli.allow_execute,
            );
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

async fn open_wal_writer(
    path: &str,
    wal_max_mb: u64,
) -> Result<WalWriter, Box<dyn std::error::Error>> {
    ensure_parent_dir(path).await?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let max_bytes = wal_max_mb.saturating_mul(1024 * 1024);
    Ok(Arc::new(Mutex::new(WalState {
        file,
        path: PathBuf::from(path),
        max_bytes,
    })))
}

async fn replay_wal(
    path: &str,
    graph: Arc<StateGraph>,
    event_buffer: Arc<RwLock<VecDeque<Event>>>,
    event_buffer_size: usize,
) -> Result<usize, Box<dyn std::error::Error>> {
    if !Path::new(path).exists() {
        return Ok(0);
    }

    let file = File::open(path).await?;
    let mut reader = BufReader::new(file).lines();
    let mut replayed = 0usize;

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: Event = match serde_json::from_str(trimmed) {
            Ok(event) => event,
            Err(e) => {
                eprintln!("[hub] 跳过损坏 WAL 行: {}", e);
                continue;
            }
        };

        if let Err(e) = graph.process_event(&event).await {
            eprintln!("[hub] 回放事件失败: {}", e);
            continue;
        }

        {
            let mut buf = event_buffer.write().await;
            if buf.len() >= event_buffer_size {
                buf.pop_front();
            }
            buf.push_back(event);
        }
        replayed += 1;
    }

    Ok(replayed)
}

async fn append_wal_event(writer: &WalWriter, event: &Event) -> Result<(), std::io::Error> {
    let mut state = writer.lock().await;
    let mut line = serde_json::to_vec(event)
        .map_err(|e| std::io::Error::other(format!("serialize wal event: {}", e)))?;
    line.push(b'\n');
    let current_size = state.file.metadata().await?.len();
    if state.max_bytes > 0 && current_size.saturating_add(line.len() as u64) > state.max_bytes {
        rotate_wal(&mut state).await?;
    }
    state.file.write_all(&line).await
}

async fn ensure_parent_dir(path: &str) -> Result<(), std::io::Error> {
    let p = PathBuf::from(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }
    Ok(())
}

async fn rotate_wal(state: &mut WalState) -> Result<(), std::io::Error> {
    let active_path = state.path.clone();
    let rotated_path = PathBuf::from(format!("{}.1", active_path.display()));

    // 关闭旧句柄并轮转文件
    state.file.flush().await?;
    if rotated_path.exists() {
        fs::remove_file(&rotated_path).await?;
    }
    if active_path.exists() {
        fs::rename(&active_path, &rotated_path).await?;
    }

    state.file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&active_path)
        .await?;
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
                                if let Err(e) = append_wal_event(writer, &event).await {
                                    eprintln!("[hub] 写入 WAL 失败: {}", e);
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

/// 创建 HTTP API 路由
fn create_api_routes(
    graph: Arc<StateGraph>,
    connections: Arc<DashMap<String, mpsc::UnboundedSender<Message>>>,
    metrics: Arc<HubMetricsCollector>,
    rules: Arc<RuleEngine>,
    event_buffer: Arc<RwLock<VecDeque<Event>>>,
    k8s_controller: Option<Arc<K8sController>>,
    allow_execute: bool,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let graph_filter = with_graph(graph.clone());
    let conns_filter = with_connections(connections.clone());
    let metrics_filter = with_metrics(metrics.clone());
    let rules_filter = with_rules(rules.clone());
    let event_buffer_filter = with_event_buffer(event_buffer.clone());
    let k8s_filter = with_k8s_controller(k8s_controller.clone());
    let allow_execute_filter = with_allow_execute(allow_execute);

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

    // GET /api/v1/diagnose?job_id=xxx&window_s=60&execute=false
    let diagnose_route = warp::path!("api" / "v1" / "diagnose")
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(graph_filter.clone())
        .and(conns_filter.clone())
        .and(rules_filter)
        .and(event_buffer_filter)
        .and(k8s_filter)
        .and(allow_execute_filter)
        .and_then(
            |params: std::collections::HashMap<String, String>,
             graph: Arc<StateGraph>,
             conns: Arc<DashMap<String, mpsc::UnboundedSender<Message>>>,
             rules: Arc<RuleEngine>,
             event_buffer: Arc<RwLock<VecDeque<Event>>>,
             k8s_controller: Option<Arc<K8sController>>,
             allow_execute: bool| async move {
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

                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let cutoff = now_ms.saturating_sub(window_s * 1000);

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

                let mut execution_results = Vec::new();
                if execute {
                    for action in &policy {
                        let result = send_fix_command(
                            &conns,
                            &action.node_id,
                            action.target_pid,
                            &action.action,
                        );
                        execution_results.push(json!({
                            "node_id": action.node_id,
                            "target_pid": action.target_pid,
                            "action": action.action,
                            "success": result.is_ok(),
                            "error": result.err()
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
                    "policy": policy,
                    "dry_run": dry_run,
                    "execute_requested": execute_requested,
                    "execute_enabled": allow_execute,
                    "execution": execution_results
                })))
            },
        );

    // GET /api/v1/preflight?node_id=node-a&window_s=120
    let preflight_route = warp::path!("api" / "v1" / "preflight")
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(with_event_buffer(event_buffer.clone()))
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
        .and(with_event_buffer(event_buffer.clone()))
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
        .or(why_route)
        .or(ps_route)
        .or(diagnose_route)
        .or(preflight_route)
        .or(training_slow_route)
        .or(fix_route)
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
    use super::{build_policy_actions, classify_training_slow};
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
}
