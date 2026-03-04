mod audit;
mod diag;
#[allow(dead_code)]
mod exec;
#[allow(dead_code)]
mod hub_forwarder;
#[allow(dead_code)]
mod ipc;
#[allow(dead_code)]
mod metrics;
#[allow(dead_code)]
mod plugin;
#[allow(
    dead_code,
    clippy::collapsible_if,
    clippy::vec_init_then_push,
    clippy::derivable_impls
)]
mod scene;

use ark_core::event::EventBus;
use ark_core::graph::StateGraph;
use clap::{Parser, Subcommand};
use diag::run_diagnosis;
use exec::{FixEngine, SystemActuator};
use hub_forwarder::{get_node_id, HubForwarder};
use ipc::{default_socket_path, IpcClient, IpcServer};
use metrics::MetricsCollector;
use plugin::{Actuator, EventSource, SubprocessProbe};
use scene::SceneType;
use std::path::PathBuf;
use std::sync::Arc;
use warp::{Filter, Reply};

#[cfg(windows)]
const DEFAULT_IPC_PORT: u16 = 9090;

#[derive(Parser)]
#[command(name = "ark")]
#[command(about = "极简主义异构 AI 算力集群管控底座", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动后台 Daemon 模式（运行事件总线和探针）
    Run {
        #[cfg(unix)]
        /// Unix Domain Socket 路径（默认: /var/run/ark.sock 或 ~/.ark/ark.sock）
        #[arg(long)]
        socket_path: Option<PathBuf>,
        #[cfg(windows)]
        /// IPC 服务端口（默认: 9090）
        #[arg(long, default_value_t = DEFAULT_IPC_PORT)]
        port: u16,
        /// 探针脚本路径（可选，默认使用内置 dummy_probe）
        #[arg(long)]
        probe: Option<PathBuf>,
        /// Hub WebSocket 地址（可选，如 ws://hub.example.com:8080）
        #[arg(long)]
        hub_url: Option<String>,
    },
    /// 查询当前活跃进程列表
    Ps {
        #[cfg(unix)]
        /// Unix Domain Socket 路径（默认: /var/run/ark.sock 或 ~/.ark/ark.sock）
        #[arg(long)]
        socket_path: Option<PathBuf>,
        #[cfg(windows)]
        /// IPC 服务端口（默认: 9090）
        #[arg(long, default_value_t = DEFAULT_IPC_PORT)]
        port: u16,
    },
    /// 分析进程阻塞根因
    Why {
        /// 目标进程 PID
        pid: u32,
        #[cfg(unix)]
        /// Unix Domain Socket 路径（默认: /var/run/ark.sock 或 ~/.ark/ark.sock）
        #[arg(long)]
        socket_path: Option<PathBuf>,
        #[cfg(windows)]
        /// IPC 服务端口（默认: 9090）
        #[arg(long, default_value_t = DEFAULT_IPC_PORT)]
        port: u16,
    },
    /// 强制终止进程（包括进程树）
    Zap {
        /// 目标进程 PID
        pid: u32,
    },
    /// AI 诊断：使用大模型分析进程阻塞根因并提供修复建议
    Diag {
        /// 目标进程 PID
        pid: u32,
        #[cfg(unix)]
        /// Unix Domain Socket 路径（默认: /var/run/ark.sock 或 ~/.ark/ark.sock）
        #[arg(long)]
        socket_path: Option<PathBuf>,
        #[cfg(windows)]
        /// IPC 服务端口（默认: 9090）
        #[arg(long, default_value_t = DEFAULT_IPC_PORT)]
        port: u16,
        /// 大模型提供商（openai/claude/local，默认从环境变量读取）
        #[arg(long)]
        provider: Option<String>,
        /// 规则文件目录（默认: ./rules）
        #[arg(long)]
        rules_dir: Option<PathBuf>,
    },
    /// 自动修复：根据诊断结果执行推荐动作（优雅降级、发信号、限流等）
    Fix {
        /// 目标进程 PID
        pid: u32,
        /// 审计日志文件路径（可选，如 /var/log/ark/audit.log）
        #[arg(long)]
        audit_log: Option<PathBuf>,
        #[cfg(unix)]
        /// Unix Domain Socket 路径（默认: /var/run/ark.sock 或 ~/.ark/ark.sock）
        #[arg(long)]
        socket_path: Option<PathBuf>,
        #[cfg(windows)]
        /// IPC 服务端口（默认: 9090）
        #[arg(long, default_value_t = DEFAULT_IPC_PORT)]
        port: u16,
        /// 规则文件目录（默认: ./rules）
        #[arg(long)]
        rules_dir: Option<PathBuf>,
        /// 是否自动执行（不询问确认）
        #[arg(long)]
        yes: bool,
    },
    /// 集群级命令：查询全局状态和根因分析
    Cluster {
        #[command(subcommand)]
        command: ClusterCommands,
        /// Hub HTTP API 地址（如 http://hub.example.com:8081）
        #[arg(long, default_value = "http://localhost:8081")]
        hub: String,
    },
}

#[derive(Subcommand)]
enum ClusterCommands {
    /// 查询集群中所有活跃进程
    Ps,
    /// 分析集群中某个 job 的根因
    Why {
        /// 目标 job_id
        job_id: String,
    },
    /// 修复集群中某个 job 的问题（自动诊断并下发修复命令）
    Fix {
        /// 目标 job_id
        job_id: String,
        /// 是否自动确认（跳过交互式确认）
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        #[cfg(unix)]
        Commands::Run {
            socket_path,
            probe,
            hub_url,
        } => {
            run_daemon(socket_path, probe, hub_url).await?;
        }
        #[cfg(windows)]
        Commands::Run {
            port,
            probe,
            hub_url,
        } => {
            run_daemon(port, probe, hub_url).await?;
        }
        #[cfg(unix)]
        Commands::Ps { socket_path } => {
            query_processes(socket_path).await?;
        }
        #[cfg(windows)]
        Commands::Ps { port } => {
            query_processes(port).await?;
        }
        #[cfg(unix)]
        Commands::Why { pid, socket_path } => {
            query_why(pid, socket_path).await?;
        }
        #[cfg(windows)]
        Commands::Why { pid, port } => {
            query_why(pid, port).await?;
        }
        Commands::Zap { pid } => {
            zap_process(pid).await?;
        }
        #[cfg(unix)]
        Commands::Diag {
            pid,
            socket_path,
            provider,
            rules_dir,
        } => {
            diagnose_process(pid, socket_path, provider, rules_dir).await?;
        }
        #[cfg(windows)]
        Commands::Diag {
            pid,
            port,
            provider,
            rules_dir,
        } => {
            diagnose_process(pid, port, provider, rules_dir).await?;
        }
        #[cfg(unix)]
        Commands::Fix {
            pid,
            socket_path,
            rules_dir,
            yes,
            audit_log,
        } => {
            fix_process(pid, socket_path, rules_dir, yes, audit_log).await?;
        }
        #[cfg(windows)]
        Commands::Fix {
            pid,
            port,
            rules_dir,
            yes,
            audit_log,
        } => {
            fix_process(pid, port, rules_dir, yes, audit_log).await?;
        }
        Commands::Cluster { command, hub } => match command {
            ClusterCommands::Ps => {
                cluster_ps(&hub).await?;
            }
            ClusterCommands::Why { job_id } => {
                cluster_why(&hub, &job_id).await?;
            }
            ClusterCommands::Fix { job_id, yes } => {
                cluster_fix(&hub, &job_id, yes).await?;
            }
        },
    }

    Ok(())
}

/// Daemon 模式：启动事件总线、状态图、IPC 服务和探针
#[cfg(unix)]
async fn run_daemon(
    socket_path: Option<PathBuf>,
    probe_path: Option<PathBuf>,
    hub_url: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("[ark] 启动事件总线...");

    // 创建事件总线
    let mut bus = EventBus::new(1000);
    let tx = bus.sender();

    // 创建状态图
    let graph = Arc::new(StateGraph::new());

    // 创建 Metrics 收集器
    let metrics = Arc::new(MetricsCollector::new()?);

    // 启动 Prometheus Metrics HTTP 服务器
    let metrics_server_handle = {
        let metrics = Arc::clone(&metrics);
        tokio::spawn(async move {
            let routes = warp::path("metrics").and(warp::get()).and_then(move || {
                let metrics = Arc::clone(&metrics);
                async move {
                    match metrics.gather() {
                        Ok(body) => Ok::<_, warp::Rejection>(
                            warp::reply::with_header(
                                body,
                                "content-type",
                                "text/plain; version=0.0.4",
                            )
                            .into_response(),
                        ),
                        Err(e) => {
                            eprintln!("[metrics] 收集指标失败: {}", e);
                            Ok(warp::reply::with_status(
                                format!("Error: {}", e),
                                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                            )
                            .into_response())
                        }
                    }
                }
            });

            println!("[ark] Prometheus Metrics 端点: http://0.0.0.0:9091/metrics");
            warp::serve(routes).run(([0, 0, 0, 0], 9091)).await;
        })
    };

    // 启动指标更新任务（每 5 秒更新一次）
    let metrics_update_handle = {
        let graph = Arc::clone(&graph);
        let metrics = Arc::clone(&metrics);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                metrics.update_graph_metrics(&graph).await;
            }
        })
    };

    // 启动探针
    let probe_handle = {
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Some(ref path) = probe_path {
                // 使用外部探针脚本
                // 尝试 python3，如果失败则尝试 python（Windows 兼容）
                let python_cmd = if cfg!(windows) { "python" } else { "python3" };

                let probe = SubprocessProbe::new(
                    python_cmd.to_string(),
                    vec![path.to_string_lossy().to_string()],
                );

                if let Err(e) = probe.start_stream(tx).await {
                    eprintln!("[ark] 外部探针异常退出: {}", e);
                }
            } else {
                // 使用内置 dummy_probe（向后兼容）
                eprintln!("[ark] 警告：使用内置 dummy_probe，建议使用 --probe 指定外部探针脚本");
                if let Err(e) = ark_core::event::dummy_probe(tx).await {
                    eprintln!("[ark] 内置探针异常退出: {}", e);
                }
            }
        })
    };

    // 初始化 Hub 转发器（如果配置了 hub_url）
    let mut hub_forwarder: Option<HubForwarder> = None;
    if let Some(ref url) = hub_url {
        let node_id = get_node_id();
        let mut forwarder = HubForwarder::new(url.clone(), node_id.clone());
        if let Err(e) = forwarder.connect().await {
            eprintln!(
                "[ark] 警告：无法连接到 Hub {}: {}，将继续运行但不推送事件",
                url, e
            );
        } else {
            hub_forwarder = Some(forwarder);
            println!("[ark] Hub 转发器已启动，节点ID: {}", node_id);
        }
    }

    // 启动事件消费和图形更新任务（同时推送到 Hub）
    let graph_handle = {
        let graph = Arc::clone(&graph);
        let metrics = Arc::clone(&metrics);
        let hub_forwarder = hub_forwarder.map(|f| Arc::new(tokio::sync::RwLock::new(f)));
        let mut rx = bus
            .receiver()
            .ok_or_else(|| std::io::Error::other("event receiver already taken"))?;
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Some(event) => {
                        // 记录事件处理指标
                        metrics.record_event(&event.event_type);

                        if let Err(e) = graph.process_event(&event).await {
                            eprintln!("[ark] 处理事件失败: {}", e);
                        }

                        // 推送到 Hub（如果配置了且事件需要推送）
                        if let Some(ref forwarder_arc) = hub_forwarder {
                            let forwarder = forwarder_arc.read().await;
                            if forwarder.should_forward(&event).await {
                                if let Err(e) = forwarder.forward_event(event.clone()).await {
                                    eprintln!("[ark] 推送事件到 Hub 失败: {}", e);
                                }
                            }
                        }
                    }
                    None => {
                        eprintln!("[ark] 事件通道已关闭");
                        break;
                    }
                }
            }
        })
    };

    // 启动 IPC 服务器（在后台任务中运行）
    let socket_path = socket_path.unwrap_or_else(default_socket_path);
    let socket_path_clone = socket_path.clone();

    let ipc_handle = {
        let graph = Arc::clone(&graph);
        tokio::spawn(async move {
            let server = IpcServer::new(graph, Some(socket_path_clone));
            if let Err(e) = server.serve().await {
                eprintln!("[ark] IPC 服务器异常退出: {}", e);
            }
        })
    };

    println!("[ark] 探针已启动，状态图已初始化");
    println!(
        "[ark] IPC 服务器已启动，监听 Unix Socket: {}",
        socket_path.display()
    );
    println!("[ark] 按 Ctrl+C 退出\n");

    // 等待退出信号
    tokio::signal::ctrl_c().await?;
    println!("\n[ark] 收到退出信号，正在关闭...");

    probe_handle.abort();
    graph_handle.abort();
    ipc_handle.abort();
    metrics_server_handle.abort();
    metrics_update_handle.abort();

    // 清理 Socket 文件
    if socket_path.exists() {
        if let Err(e) = std::fs::remove_file(&socket_path) {
            eprintln!("[ark] 警告：删除 Socket 文件失败: {}", e);
        }
    }

    println!("[ark] 退出完成");
    Ok(())
}

#[cfg(windows)]
async fn run_daemon(
    port: u16,
    probe_path: Option<PathBuf>,
    hub_url: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("[ark] 启动事件总线...");

    // 创建事件总线
    let mut bus = EventBus::new(1000);
    let tx = bus.sender();

    // 创建状态图
    let graph = Arc::new(StateGraph::new());

    // 启动探针
    let probe_handle = {
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Some(ref path) = probe_path {
                let probe = SubprocessProbe::new(
                    "python".to_string(),
                    vec![path.to_string_lossy().to_string()],
                );

                if let Err(e) = probe.start_stream(tx).await {
                    eprintln!("[ark] 外部探针异常退出: {}", e);
                }
            } else {
                eprintln!("[ark] 警告：使用内置 dummy_probe，建议使用 --probe 指定外部探针脚本");
                if let Err(e) = ark_core::event::dummy_probe(tx).await {
                    eprintln!("[ark] 内置探针异常退出: {}", e);
                }
            }
        })
    };

    // 初始化 Hub 转发器（如果配置了 hub_url）
    let mut hub_forwarder: Option<HubForwarder> = None;
    if let Some(ref url) = hub_url {
        let node_id = get_node_id();
        let mut forwarder = HubForwarder::new(url.clone(), node_id.clone());
        if let Err(e) = forwarder.connect().await {
            eprintln!(
                "[ark] 警告：无法连接到 Hub {}: {}，将继续运行但不推送事件",
                url, e
            );
        } else {
            hub_forwarder = Some(forwarder);
            println!("[ark] Hub 转发器已启动，节点ID: {}", node_id);
        }
    }

    // 启动事件消费和图形更新任务（同时推送到 Hub）
    let graph_handle = {
        let graph = Arc::clone(&graph);
        let hub_forwarder = hub_forwarder.map(|f| Arc::new(tokio::sync::RwLock::new(f)));
        let mut rx = bus
            .receiver()
            .ok_or_else(|| std::io::Error::other("event receiver already taken"))?;
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Some(event) => {
                        // 更新本地图
                        if let Err(e) = graph.process_event(&event).await {
                            eprintln!("[ark] 处理事件失败: {}", e);
                        }

                        // 推送到 Hub（如果配置了且事件需要推送）
                        if let Some(ref forwarder_arc) = hub_forwarder {
                            let forwarder = forwarder_arc.read().await;
                            if forwarder.should_forward(&event).await {
                                if let Err(e) = forwarder.forward_event(event.clone()).await {
                                    eprintln!("[ark] 推送事件到 Hub 失败: {}", e);
                                }
                            }
                        }
                    }
                    None => {
                        eprintln!("[ark] 事件通道已关闭");
                        break;
                    }
                }
            }
        })
    };

    // 启动 IPC 服务器（在后台任务中运行）
    let ipc_handle = {
        let graph = Arc::clone(&graph);
        tokio::spawn(async move {
            let server = IpcServer::new(graph, port);
            if let Err(e) = server.serve().await {
                eprintln!("[ark] IPC 服务器异常退出: {}", e);
            }
        })
    };

    println!("[ark] 探针已启动，状态图已初始化");
    println!("[ark] IPC 服务器已启动，监听端口 {}", port);
    println!("[ark] 按 Ctrl+C 退出\n");

    // 等待退出信号
    tokio::signal::ctrl_c().await?;
    println!("\n[ark] 收到退出信号，正在关闭...");

    probe_handle.abort();
    graph_handle.abort();
    ipc_handle.abort();

    println!("[ark] 退出完成");
    Ok(())
}

/// 查询进程列表（通过 IPC）
#[cfg(unix)]
async fn query_processes(socket_path: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let client = IpcClient::new(socket_path);

    // 检查 daemon 是否运行
    if !client.ping().await? {
        eprintln!("[ark] 错误：无法连接到 daemon");
        eprintln!("[ark] 请先运行: ark run");
        return Err("daemon 未运行".into());
    }

    // 查询进程列表
    let processes = client.list_processes().await?;

    if processes.is_empty() {
        println!("没有活跃进程");
        return Ok(());
    }

    // 打印表头
    use colored::*;
    println!(
        "{:>8} | {:>12} | {:>20} | {}",
        "PID".bright_cyan(),
        "JOB_ID".bright_cyan(),
        "RESOURCES".bright_cyan(),
        "STATE".bright_cyan()
    );
    println!("{}", "-".repeat(80));

    // 打印每个进程
    for proc in processes {
        let pid = proc["pid"].as_u64().unwrap_or(0) as u32;
        let job_id = proc["job_id"].as_str().unwrap_or("-").to_string();
        let state = proc["state"].as_str().unwrap_or("unknown").to_string();

        // 从 IPC 响应中获取资源列表
        let resources: Vec<String> = proc["resources"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let resources_str = if resources.is_empty() {
            "-".to_string()
        } else {
            resources.join(", ")
        };

        println!(
            "{:>8} | {:>12} | {:>20} | {}",
            pid.to_string().bright_green(),
            job_id.bright_yellow(),
            resources_str.bright_white(),
            state.bright_blue()
        );
    }

    Ok(())
}

#[cfg(windows)]
async fn query_processes(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let client = IpcClient::new(port);

    // 检查 daemon 是否运行
    if !client.ping().await? {
        eprintln!("[ark] 错误：无法连接到 daemon (端口 {})", port);
        eprintln!("[ark] 请先运行: ark run");
        return Err("daemon 未运行".into());
    }

    // 查询进程列表
    let processes = client.list_processes().await?;

    if processes.is_empty() {
        println!("没有活跃进程");
        return Ok(());
    }

    // 打印表头
    use colored::*;
    println!(
        "{:>8} | {:>12} | {:>20} | {}",
        "PID".bright_cyan(),
        "JOB_ID".bright_cyan(),
        "RESOURCES".bright_cyan(),
        "STATE".bright_cyan()
    );
    println!("{}", "-".repeat(80));

    // 打印每个进程
    for proc in processes {
        let pid = proc["pid"].as_u64().unwrap_or(0) as u32;
        let job_id = proc["job_id"].as_str().unwrap_or("-").to_string();
        let state = proc["state"].as_str().unwrap_or("unknown").to_string();

        // 从 IPC 响应中获取资源列表
        let resources: Vec<String> = proc["resources"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let resources_str = if resources.is_empty() {
            "-".to_string()
        } else {
            resources.join(", ")
        };

        println!(
            "{:>8} | {:>12} | {:>20} | {}",
            pid.to_string().bright_green(),
            job_id.bright_yellow(),
            resources_str.bright_white(),
            state.bright_blue()
        );
    }

    Ok(())
}

/// 查询进程阻塞根因（通过 IPC）
#[cfg(unix)]
async fn query_why(
    pid: u32,
    socket_path: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::ipc::IpcClient;
    use colored::*;

    let client = IpcClient::new(socket_path);

    // 检查 daemon 是否运行
    if !client.ping().await? {
        eprintln!("[ark] 错误：无法连接到 daemon");
        eprintln!("[ark] 请先运行: ark run");
        return Err("daemon 未运行".into());
    }

    // 查询根因
    let causes = client.why_process(pid).await?;

    // 尝试场景识别和分析（需要访问图状态，当前通过 IPC 无法直接访问）
    // 这里先使用基本的根因分析，场景分析功能可以在未来扩展 IPC 接口后启用

    if causes.is_empty() {
        println!("进程 {} 未发现阻塞问题", pid.to_string().bright_green());
        return Ok(());
    }

    println!("进程 {} 的阻塞根因分析:", pid.to_string().bright_green());
    println!("{}", "-".repeat(60));

    // 尝试识别场景类型（基于根因文本）
    let scene_hint = if causes
        .iter()
        .any(|c| c.contains("GPU") || c.contains("OOM") || c.contains("显存"))
    {
        Some("GPU OOM")
    } else if causes
        .iter()
        .any(|c| c.contains("网络") || c.contains("network") || c.contains("等待资源"))
    {
        Some("网络阻塞")
    } else if causes
        .iter()
        .any(|c| c.contains("exit") || c.contains("crash") || c.contains("failed"))
    {
        Some("进程崩溃")
    } else {
        None
    };

    if let Some(scene) = scene_hint {
        println!("  [场景识别] {}", scene.bright_cyan());
        println!();
    }

    for (idx, cause) in causes.iter().enumerate() {
        if cause.starts_with("等待资源") {
            println!("  {}. {}", idx + 1, cause.bright_yellow());
        } else if cause.contains("error") {
            println!("  {}. {}", idx + 1, cause.bright_red());
        } else {
            println!("  {}. {}", idx + 1, cause);
        }
    }

    Ok(())
}

/// 强制终止进程
async fn zap_process(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    println!("[ark] 正在终止进程 {}...", pid);

    let actuator = SystemActuator::new();
    match actuator.execute(pid, "zap").await {
        Ok(_) => {
            println!("[ark] 进程 {} 已成功终止", pid);
        }
        Err(e) => {
            eprintln!("[ark] 终止进程失败: {}", e);
            return Err(std::io::Error::other(e).into());
        }
    }

    Ok(())
}

#[cfg(windows)]
async fn query_why(pid: u32, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    use crate::ipc::IpcClient;
    use colored::*;

    let client = IpcClient::new(port);

    // 检查 daemon 是否运行
    if !client.ping().await? {
        eprintln!("[ark] 错误：无法连接到 daemon (端口 {})", port);
        eprintln!("[ark] 请先运行: ark run");
        return Err("daemon 未运行".into());
    }

    // 查询根因
    let causes = client.why_process(pid).await?;

    if causes.is_empty() {
        println!("进程 {} 未发现阻塞问题", pid.to_string().bright_green());
        return Ok(());
    }

    println!("进程 {} 的阻塞根因分析:", pid.to_string().bright_green());
    println!("{}", "-".repeat(60));

    // 尝试识别场景类型（基于根因文本）
    let scene_hint = if causes
        .iter()
        .any(|c| c.contains("GPU") || c.contains("OOM") || c.contains("显存"))
    {
        Some("GPU OOM")
    } else if causes
        .iter()
        .any(|c| c.contains("网络") || c.contains("network") || c.contains("等待资源"))
    {
        Some("网络阻塞")
    } else if causes
        .iter()
        .any(|c| c.contains("exit") || c.contains("crash") || c.contains("failed"))
    {
        Some("进程崩溃")
    } else {
        None
    };

    if let Some(scene) = scene_hint {
        println!("  [场景识别] {}", scene.bright_cyan());
        println!();
    }

    for (idx, cause) in causes.iter().enumerate() {
        if cause.starts_with("等待资源") {
            println!("  {}. {}", idx + 1, cause.bright_yellow());
        } else if cause.contains("error") {
            println!("  {}. {}", idx + 1, cause.bright_red());
        } else {
            println!("  {}. {}", idx + 1, cause);
        }
    }

    Ok(())
}

/// AI 诊断：使用大模型分析进程问题
#[cfg(unix)]
async fn diagnose_process(
    pid: u32,
    socket_path: Option<PathBuf>,
    provider: Option<String>,
    rules_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::*;

    println!("[ark] 正在诊断进程 {}...", pid.to_string().bright_green());
    println!("[ark] 收集诊断信息...\n");

    // 如果没有指定规则目录，尝试使用默认的 ./rules
    let rules_path = rules_dir.or_else(|| {
        let default = PathBuf::from("rules");
        if default.exists() {
            Some(default)
        } else {
            None
        }
    });

    // 执行诊断
    let diagnosis = match run_diagnosis(pid, socket_path, provider, rules_path).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[ark] 诊断失败: {}", e);
            eprintln!("\n提示:");
            eprintln!("  1. 确保 daemon 正在运行: ark run");
            eprintln!("  2. 设置 API Key:");
            eprintln!("     export OPENAI_API_KEY=your_key");
            eprintln!("     或");
            eprintln!("     export ANTHROPIC_API_KEY=your_key");
            eprintln!("  3. 检查网络连接");
            return Err(e);
        }
    };

    // 显示诊断结果
    println!("{}", "=".repeat(70).bright_cyan());
    println!("{}", "AI 诊断报告".bright_cyan().bold());
    println!("{}", "=".repeat(70).bright_cyan());
    println!();

    // 阻塞根因
    if !diagnosis.causes.is_empty() {
        println!("{}", "阻塞根因:".bright_yellow().bold());
        for (idx, cause) in diagnosis.causes.iter().enumerate() {
            if cause.starts_with("等待资源") {
                println!("  {}. {}", idx + 1, cause.bright_yellow());
            } else if cause.contains("error") {
                println!("  {}. {}", idx + 1, cause.bright_red());
            } else {
                println!("  {}. {}", idx + 1, cause);
            }
        }
        println!();
    }

    // AI 建议
    println!("{}", "AI 诊断建议:".bright_green().bold());
    println!("{}", "-".repeat(70));

    // 格式化输出建议（按段落分割）
    let lines: Vec<&str> = diagnosis.recommendation.lines().collect();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            println!();
        } else if trimmed.starts_with('#') || trimmed.starts_with("##") {
            // 标题
            println!("{}", trimmed.bright_cyan());
        } else if trimmed
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            // 编号列表
            println!("  {}", trimmed);
        } else {
            // 普通文本
            println!("  {}", trimmed);
        }
    }

    println!();
    println!(
        "{}",
        format!("置信度: {:.0}%", diagnosis.confidence * 100.0).bright_white()
    );
    println!();

    Ok(())
}

/// 自动修复进程：根据诊断结果执行推荐动作
#[cfg(unix)]
async fn fix_process(
    pid: u32,
    socket_path: Option<PathBuf>,
    _rules_dir: Option<PathBuf>,
    auto_yes: bool,
    audit_log: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;

    println!("[ark] 正在修复进程 {}...", pid.to_string().bright_green());

    // 连接到 daemon
    let client = IpcClient::new(socket_path);
    if !client.ping().await? {
        return Err("无法连接到 daemon，请先运行: ark run".into());
    }

    // 获取根因分析（用于场景识别）
    let causes = client.why_process(pid).await?;

    // 识别场景（简化版：基于根因文本）
    let scene = identify_scene_from_causes(&causes);

    if scene.is_none() {
        println!("{}", "[ark] 未识别到问题场景，无法自动修复".bright_yellow());
        println!("提示: 可以尝试手动执行: ark zap {}", pid);
        return Ok(());
    }

    let scene = scene.unwrap();
    println!("[ark] 识别到场景: {:?}", scene);

    // 创建分析结果（基于根因）
    let analysis = create_analysis_from_causes(scene, &causes);

    // 显示推荐动作
    if !analysis.recommended_actions.is_empty() {
        println!("\n{}", "推荐动作:".bright_cyan().bold());
        for (idx, action) in analysis.recommended_actions.iter().enumerate() {
            println!("  {}. {}", idx + 1, action);
        }
        println!();
    }

    // 确认执行
    if !auto_yes {
        use std::io::{self, Write};
        print!("{}", "是否执行修复? [y/N]: ".bright_yellow());
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") && !input.trim().eq_ignore_ascii_case("yes") {
            println!("{}", "已取消".bright_yellow());
            return Ok(());
        }
    }

    // 初始化审计日志（如果指定了路径）
    let audit_logger = if let Some(ref log_path) = audit_log {
        Some(Arc::new(audit::AuditLogger::new(log_path.clone(), 100)?)) // 100MB 最大大小
    } else {
        None
    };

    // 执行修复
    let fix_engine = FixEngine::new();
    let result = fix_engine.fix_from_analysis(&analysis, pid).await?;

    // 记录审计日志
    if let Some(ref logger) = audit_logger {
        // 尝试从分析结果中获取 job_id（如果有）
        let action_str = if !result.executed_actions.is_empty() {
            result.executed_actions[0].action.clone()
        } else if !analysis.recommended_actions.is_empty() {
            analysis.recommended_actions[0].clone()
        } else {
            "Unknown".to_string()
        };

        let details = format!(
            "执行动作: {}; 成功: {}; 失败: {}; 场景: {:?}",
            action_str,
            result.executed_actions.len(),
            result.failed_actions.len(),
            analysis.scene
        );

        let entry = audit::create_audit_entry(
            &action_str,
            pid,
            None, // job_id 暂时为 None
            if result.success {
                "success"
            } else {
                "partial_failure"
            },
            &details,
        );

        if let Err(e) = logger.log(entry).await {
            eprintln!("[audit] 记录审计日志失败: {}", e);
        }
    }

    // 显示结果
    println!("\n{}", "=".repeat(70).bright_cyan());
    println!("{}", "修复结果".bright_cyan().bold());
    println!("{}", "=".repeat(70).bright_cyan());
    println!();

    if result.success {
        println!("{}", format!("✅ {}", result.message).bright_green());
    } else {
        println!("{}", format!("⚠️  {}", result.message).bright_yellow());
    }

    if !result.executed_actions.is_empty() {
        println!("\n{}", "已执行的动作:".bright_green().bold());
        for action in &result.executed_actions {
            println!("  ✅ {}: {}", action.action, action.result);
        }
    }

    if !result.failed_actions.is_empty() {
        println!("\n{}", "失败的动作:".bright_red().bold());
        for action in &result.failed_actions {
            println!("  ❌ {}: {}", action.action, action.error);
        }
    }

    Ok(())
}

#[cfg(windows)]
async fn fix_process(
    pid: u32,
    port: u16,
    _rules_dir: Option<PathBuf>,
    _auto_yes: bool,
    audit_log: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;

    println!("[ark] 正在修复进程 {}...", pid.to_string().bright_green());

    // 连接到 daemon
    let client = IpcClient::new(port);
    if !client.ping().await? {
        return Err("无法连接到 daemon，请先运行: ark run".into());
    }

    // 获取根因分析
    let causes = client.why_process(pid).await?;

    // 识别场景
    let scene = identify_scene_from_causes(&causes);

    if scene.is_none() {
        println!("{}", "[ark] 未识别到问题场景，无法自动修复".bright_yellow());
        return Ok(());
    }

    let scene = scene.unwrap();

    // 创建分析结果
    let analysis = create_analysis_from_causes(scene, &causes);

    // 初始化审计日志（如果指定了路径）
    let audit_logger = if let Some(ref log_path) = audit_log {
        Some(Arc::new(audit::AuditLogger::new(log_path.clone(), 100)?)) // 100MB 最大大小
    } else {
        None
    };

    // 执行修复
    let fix_engine = FixEngine::new();
    let result = fix_engine.fix_from_analysis(&analysis, pid).await?;

    // 记录审计日志
    if let Some(ref logger) = audit_logger {
        let action_str = if !result.executed_actions.is_empty() {
            result.executed_actions[0].action.clone()
        } else if !analysis.recommended_actions.is_empty() {
            analysis.recommended_actions[0].clone()
        } else {
            "Unknown".to_string()
        };

        let details = format!(
            "执行动作: {}; 成功: {}; 失败: {}; 场景: {:?}",
            action_str,
            result.executed_actions.len(),
            result.failed_actions.len(),
            analysis.scene
        );

        let entry = audit::create_audit_entry(
            &action_str,
            pid,
            None, // job_id 暂时为 None
            if result.success {
                "success"
            } else {
                "partial_failure"
            },
            &details,
        );

        if let Err(e) = logger.log(entry).await {
            eprintln!("[audit] 记录审计日志失败: {}", e);
        }
    }

    println!("修复结果: {}", result.message);

    Ok(())
}

/// 从根因识别场景（简化版）
fn identify_scene_from_causes(causes: &[String]) -> Option<SceneType> {
    for cause in causes {
        let cause_lower = cause.to_lowercase();
        if cause_lower.contains("gpu") && cause_lower.contains("oom") {
            return Some(SceneType::GpuOom);
        }
        if cause_lower.contains("network") || cause_lower.contains("网络") {
            return Some(SceneType::NetworkStall);
        }
        if cause_lower.contains("storage") || cause_lower.contains("存储") {
            return Some(SceneType::StorageIoError);
        }
        if cause_lower.contains("crash") || cause_lower.contains("崩溃") {
            return Some(SceneType::ProcessCrash);
        }
    }
    Some(SceneType::WorkloadStalled) // 默认场景
}

/// 从根因创建分析结果（简化版）
fn create_analysis_from_causes(scene: SceneType, causes: &[String]) -> scene::AnalysisResult {
    let mut recommended_actions = Vec::new();

    // 根据场景类型添加推荐动作
    match scene {
        SceneType::GpuOom => {
            recommended_actions.push("尝试触发框架层的 Checkpoint Dump 信号 (SIGUSR1)".to_string());
            recommended_actions.push("隔离该节点，执行 ark zap 清理僵尸进程".to_string());
        }
        SceneType::NetworkStall => {
            recommended_actions.push("检查交换机 PFC 配置".to_string());
            recommended_actions.push("检查 RoCE/HCCS 连接状态".to_string());
        }
        SceneType::WorkloadStalled => {
            recommended_actions.push("如果确认卡死，执行 ark zap 终止进程".to_string());
            recommended_actions.push("检查是否有 Checkpoint 可以恢复".to_string());
        }
        _ => {
            recommended_actions.push("执行 ark zap 终止进程".to_string());
        }
    }

    scene::AnalysisResult {
        scene,
        root_causes: causes.to_vec(),
        confidence: 0.7,
        recommendations: vec!["根据根因分析执行修复".to_string()],
        recommended_actions,
        severity: scene::Severity::Warning,
    }
}

/// 集群级进程列表查询
async fn cluster_ps(hub_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    use colored::*;

    let url = format!("{}/api/v1/ps", hub_url.trim_end_matches('/'));
    let response = reqwest::get(&url).await?;
    let json: serde_json::Value = response.json().await?;

    if let Some(processes) = json.get("processes").and_then(|p| p.as_array()) {
        if processes.is_empty() {
            println!("集群中没有活跃进程");
            return Ok(());
        }

        println!(
            "{:>20} | {:>12} | {:>15} | {}",
            "NODE_ID".bright_cyan(),
            "JOB_ID".bright_cyan(),
            "PID".bright_cyan(),
            "STATE".bright_cyan()
        );
        println!("{}", "-".repeat(80));

        for proc in processes {
            let id = proc["id"].as_str().unwrap_or("-");
            let job_id = proc["job_id"].as_str().unwrap_or("-");
            let state = proc["state"].as_str().unwrap_or("unknown");

            // 从 id 中提取节点和 PID
            let (node_id, pid): (&str, &str) = if id.contains("::") {
                let parts: Vec<&str> = id.split("::").collect();
                (parts[0], parts.get(1).copied().unwrap_or("-"))
            } else {
                ("local", id)
            };

            println!("{:>20} | {:>12} | {:>15} | {}", node_id, job_id, pid, state);
        }
    } else {
        eprintln!("错误：无法解析 Hub 响应");
    }

    Ok(())
}

/// 集群级根因分析
async fn cluster_why(hub_url: &str, job_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    use colored::*;

    let url = format!(
        "{}/api/v1/why?job_id={}",
        hub_url.trim_end_matches('/'),
        job_id
    );
    let response = reqwest::get(&url).await?;
    let json: serde_json::Value = response.json().await?;

    if let Some(error) = json.get("error") {
        eprintln!("错误: {}", error.as_str().unwrap_or("unknown"));
        return Ok(());
    }

    println!("🔍 集群级根因分析：job_id = {}", job_id.bright_green());
    println!();

    if let Some(causes) = json.get("causes").and_then(|c| c.as_array()) {
        if causes.is_empty() {
            println!("未发现阻塞根因");
        } else {
            println!("发现的根因：");
            for (i, cause) in causes.iter().enumerate() {
                if let Some(cause_str) = cause.as_str() {
                    println!("  {}. {}", i + 1, cause_str.bright_red());
                }
            }
        }
    }

    Ok(())
}

/// 集群级修复：自动诊断并下发修复命令
async fn cluster_fix(
    hub_url: &str,
    job_id: &str,
    auto_confirm: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::*;
    use std::io::{self, Write};

    println!("🔧 集群级修复：job_id = {}", job_id.bright_green());
    println!();

    // 步骤 1：调用 why 接口获取根因和涉及的节点/PID
    let url = format!(
        "{}/api/v1/why?job_id={}",
        hub_url.trim_end_matches('/'),
        job_id
    );
    let response = reqwest::get(&url).await?;
    let json: serde_json::Value = response.json().await?;

    if let Some(error) = json.get("error") {
        eprintln!("错误: {}", error.as_str().unwrap_or("unknown"));
        return Ok(());
    }

    // 步骤 2：显示根因
    if let Some(causes) = json.get("causes").and_then(|c| c.as_array()) {
        if causes.is_empty() {
            println!("未发现阻塞根因，无需修复");
            return Ok(());
        }

        println!("发现的根因：");
        for (i, cause) in causes.iter().enumerate() {
            if let Some(cause_str) = cause.as_str() {
                println!("  {}. {}", i + 1, cause_str.bright_red());
            }
        }
    }

    // 步骤 3：从进程列表中提取节点和 PID
    let mut target_nodes: Vec<(String, u32)> = Vec::new(); // (node_id, pid)

    if let Some(processes) = json.get("processes").and_then(|p| p.as_array()) {
        for process in processes {
            if let (Some(node_id), Some(pid)) = (
                process.get("node_id").and_then(|n| n.as_str()),
                process.get("pid").and_then(|p| p.as_u64()),
            ) {
                target_nodes.push((node_id.to_string(), pid as u32));
            }
        }
    }

    // 如果进程列表为空，尝试从根因字符串中解析（向后兼容）
    if target_nodes.is_empty() {
        if let Some(causes) = json.get("causes").and_then(|c| c.as_array()) {
            for cause in causes {
                if let Some(cause_str) = cause.as_str() {
                    if let Some((node_id, pid)) = extract_node_and_pid(cause_str) {
                        target_nodes.push((node_id, pid));
                    }
                }
            }
        }
    }

    if target_nodes.is_empty() {
        println!("⚠️  无法从响应中提取节点和 PID 信息，请手动指定");
        return Ok(());
    }

    // 步骤 4：显示将要执行的操作并确认
    println!();
    println!("将执行以下修复操作：");
    for (node_id, pid) in &target_nodes {
        println!(
            "  • 节点 {} 上的 PID {}: 优雅降级 (GracefulShutdown)",
            node_id.bright_cyan(),
            pid.to_string().bright_yellow()
        );
    }
    println!();

    // 步骤 5：用户确认
    if !auto_confirm {
        print!("是否确认执行？[y/N]: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if input.trim().to_lowercase() != "y" {
            println!("已取消");
            return Ok(());
        }
    }

    // 步骤 6：调用 fix API 下发命令
    println!();
    println!("正在下发修复命令...");

    let client = reqwest::Client::new();
    let mut success_count = 0;
    let mut fail_count = 0;

    for (node_id, pid) in target_nodes {
        let fix_url = format!("{}/api/v1/fix", hub_url.trim_end_matches('/'));
        let fix_request = serde_json::json!({
            "node_id": node_id,
            "target_pid": pid,
            "action": "GracefulShutdown"
        });

        match client.post(&fix_url).json(&fix_request).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    println!(
                        "  ✅ 节点 {} PID {}: 命令已发送",
                        node_id.bright_cyan(),
                        pid.to_string().bright_yellow()
                    );
                    success_count += 1;
                } else {
                    let error_text = response.text().await.unwrap_or_default();
                    eprintln!(
                        "  ❌ 节点 {} PID {}: 发送失败 - {}",
                        node_id.bright_red(),
                        pid.to_string().bright_yellow(),
                        error_text
                    );
                    fail_count += 1;
                }
            }
            Err(e) => {
                eprintln!(
                    "  ❌ 节点 {} PID {}: 请求失败 - {}",
                    node_id.bright_red(),
                    pid.to_string().bright_yellow(),
                    e
                );
                fail_count += 1;
            }
        }
    }

    println!();
    if success_count > 0 {
        println!(
            "✅ 成功发送 {} 个修复命令",
            success_count.to_string().bright_green()
        );
    }
    if fail_count > 0 {
        println!("❌ 失败 {} 个命令", fail_count.to_string().bright_red());
    }

    Ok(())
}

/// 从根因字符串中提取节点 ID 和 PID
/// 支持格式：
/// - "node-a: pid-1234 WaitsOn network"
/// - "node-a::pid-1234: ..."
/// - "node-a::pid-1234 WaitsOn ..."
fn extract_node_and_pid(cause_str: &str) -> Option<(String, u32)> {
    // 尝试匹配 "node-xxx::pid-yyy" 格式
    if let Some(pos) = cause_str.find("::pid-") {
        let node_part = &cause_str[..pos];
        if let Some(pid_start) = cause_str[pos + 6..].find(|c: char| c.is_ascii_digit()) {
            let pid_str = &cause_str[pos + 6 + pid_start..];
            let pid_end = pid_str
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(pid_str.len());
            if let Ok(pid) = pid_str[..pid_end].parse::<u32>() {
                return Some((node_part.to_string(), pid));
            }
        }
    }

    // 尝试匹配 "node-xxx: pid-yyy" 格式（单冒号）
    if let Some(node_end) = cause_str.find(": pid-") {
        let node_part = cause_str[..node_end].trim();
        let pid_start = node_end + 6;
        let pid_str = &cause_str[pid_start..];
        let pid_end = pid_str
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(pid_str.len());
        if let Ok(pid) = pid_str[..pid_end].parse::<u32>() {
            return Some((node_part.to_string(), pid));
        }
    }

    // 尝试匹配 "node-xxx pid-yyy" 格式（空格分隔）
    if let Some(node_end) = cause_str.find(" pid-") {
        let node_part = cause_str[..node_end].trim();
        let pid_start = node_end + 5;
        let pid_str = &cause_str[pid_start..];
        let pid_end = pid_str
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(pid_str.len());
        if let Ok(pid) = pid_str[..pid_end].parse::<u32>() {
            return Some((node_part.to_string(), pid));
        }
    }

    None
}
