use ark_core::graph::StateGraph;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(windows)]
use tokio::net::{TcpListener, TcpStream};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

/// RPC 请求类型
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum RpcRequest {
    #[serde(rename = "list_processes")]
    ListProcesses,
    #[serde(rename = "why_process")]
    WhyProcess { pid: u32 },
    #[serde(rename = "ping")]
    Ping,
}

/// RPC 响应
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcResponse {
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

impl RpcResponse {
    pub fn success(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(msg: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg),
        }
    }
}

/// 获取默认的 IPC Socket 路径
#[cfg(unix)]
pub fn default_socket_path() -> PathBuf {
    // 优先使用 /var/run/ark.sock（需要 root 权限）
    // 如果不可写，则使用用户目录
    let system_path = PathBuf::from("/var/run/ark.sock");
    if std::fs::metadata("/var/run").is_ok() {
        system_path
    } else {
        // 回退到用户目录
        let mut home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        home.push(".ark");
        home.push("ark.sock");
        home
    }
}

#[cfg(windows)]
pub fn default_socket_path() -> PathBuf {
    // Windows 不支持 UDS，返回空路径（将使用 TCP）
    PathBuf::new()
}

/// IPC 服务器：提供对 StateGraph 的远程查询接口
pub struct IpcServer {
    graph: Arc<StateGraph>,
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(windows)]
    port: u16,
}

impl IpcServer {
    #[cfg(unix)]
    pub fn new(graph: Arc<StateGraph>, socket_path: Option<PathBuf>) -> Self {
        Self {
            graph,
            socket_path: socket_path.unwrap_or_else(default_socket_path),
        }
    }

    #[cfg(windows)]
    pub fn new(graph: Arc<StateGraph>, port: u16) -> Self {
        Self { graph, port }
    }

    /// 启动 IPC 服务器（阻塞运行）
    #[cfg(unix)]
    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error>> {
        // 如果 Socket 文件已存在，先删除（可能是上次异常退出留下的）
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        // 确保父目录存在
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;

        // 设置 Socket 文件权限：rw-rw---- (660)
        // 只允许 owner 和 group 读写
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o660);
            std::fs::set_permissions(&self.socket_path, perms)?;
        }

        println!(
            "[ark] IPC 服务器已启动，监听 Unix Socket: {}",
            self.socket_path.display()
        );

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let graph = Arc::clone(&self.graph);
                    tokio::spawn(async move {
                        if let Err(e) = handle_client_unix(stream, graph).await {
                            eprintln!("[ark] 处理客户端请求失败: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[ark] 接受连接失败: {}", e);
                }
            }
        }
    }

    #[cfg(windows)]
    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = TcpListener::bind(&addr).await?;

        println!("[ark] IPC 服务器已启动，监听 TCP: {}", addr);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let graph = Arc::clone(&self.graph);
                    tokio::spawn(async move {
                        if let Err(e) = handle_client_tcp(stream, graph).await {
                            eprintln!("[ark] 处理客户端 {} 请求失败: {}", addr, e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[ark] 接受连接失败: {}", e);
                }
            }
        }
    }

    /// 获取 Socket 路径（Unix）或端口（Windows）
    #[cfg(unix)]
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    #[cfg(windows)]
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// 处理单个客户端连接（Unix Domain Socket）
#[cfg(unix)]
async fn handle_client_unix(
    mut stream: UnixStream,
    graph: Arc<StateGraph>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 最大请求体大小：10MB（防止 OOM 攻击）
    const MAX_REQUEST_SIZE: u32 = 10 * 1024 * 1024;

    loop {
        // 读取请求长度（4字节）
        let n = stream.read_u32().await?;
        if n == 0 {
            break; // 客户端关闭连接
        }

        // 安全检查：防止恶意客户端发送超大请求导致 OOM
        if n > MAX_REQUEST_SIZE {
            let response = RpcResponse::error(format!(
                "请求体过大: {} 字节（最大允许: {} 字节）",
                n, MAX_REQUEST_SIZE
            ));
            send_response_unix(&mut stream, &response).await?;
            continue;
        }

        // 读取请求体
        let mut request_buf = vec![0u8; n as usize];
        stream.read_exact(&mut request_buf).await?;

        // 解析 JSON 请求
        let request: RpcRequest = match serde_json::from_slice(&request_buf) {
            Ok(req) => req,
            Err(e) => {
                let response = RpcResponse::error(format!("解析请求失败: {}", e));
                send_response_unix(&mut stream, &response).await?;
                continue;
            }
        };

        // 处理请求
        let response = match handle_request(request, Arc::clone(&graph)).await {
            Ok(data) => RpcResponse::success(data),
            Err(e) => RpcResponse::error(e),
        };

        // 发送响应
        send_response_unix(&mut stream, &response).await?;
    }

    Ok(())
}

/// 处理单个客户端连接（TCP Socket，Windows）
#[cfg(windows)]
async fn handle_client_tcp(
    mut stream: TcpStream,
    graph: Arc<StateGraph>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = vec![0u8; 4096];

    // 最大请求体大小：10MB（防止 OOM 攻击）
    const MAX_REQUEST_SIZE: u32 = 10 * 1024 * 1024;

    loop {
        // 读取请求长度（4字节）
        let n = stream.read_u32().await?;
        if n == 0 {
            break; // 客户端关闭连接
        }

        // 安全检查：防止恶意客户端发送超大请求导致 OOM
        if n > MAX_REQUEST_SIZE {
            let response = RpcResponse::error(format!(
                "请求体过大: {} 字节（最大允许: {} 字节）",
                n, MAX_REQUEST_SIZE
            ));
            send_response_tcp(&mut stream, &response).await?;
            continue;
        }

        // 读取请求体
        let mut request_buf = vec![0u8; n as usize];
        stream.read_exact(&mut request_buf).await?;

        // 解析 JSON 请求
        let request: RpcRequest = match serde_json::from_slice(&request_buf) {
            Ok(req) => req,
            Err(e) => {
                let response = RpcResponse::error(format!("解析请求失败: {}", e));
                send_response_tcp(&mut stream, &response).await?;
                continue;
            }
        };

        // 处理请求
        let response = match handle_request(request, Arc::clone(&graph)).await {
            Ok(data) => RpcResponse::success(data),
            Err(e) => RpcResponse::error(e),
        };

        // 发送响应
        send_response_tcp(&mut stream, &response).await?;
    }

    Ok(())
}

/// 处理 RPC 请求
async fn handle_request(
    request: RpcRequest,
    graph: Arc<StateGraph>,
) -> Result<serde_json::Value, String> {
    match request {
        RpcRequest::ListProcesses => {
            let processes = graph.get_active_processes().await;
            let mut processes_json = Vec::new();

            for node in processes {
                let pid = node
                    .id
                    .strip_prefix("pid-")
                    .unwrap_or(&node.id)
                    .parse::<u32>()
                    .unwrap_or(0);

                // 获取进程消耗的资源
                let resources = graph.get_process_resources(pid).await;

                processes_json.push(json!({
                    "pid": pid,
                    "id": node.id,
                    "job_id": node.metadata.get("job_id").cloned(),
                    "state": node.metadata.get("state").cloned().unwrap_or_else(|| "unknown".to_string()),
                    "resources": resources,
                    "last_update": node.last_update,
                }));
            }

            Ok(json!(processes_json))
        }
        RpcRequest::WhyProcess { pid } => {
            let causes = graph.find_root_cause(pid).await;
            Ok(json!({
                "pid": pid,
                "causes": causes,
            }))
        }
        RpcRequest::Ping => Ok(json!({"status": "ok"})),
    }
}

/// 发送响应到客户端（Unix Domain Socket）
#[cfg(unix)]
async fn send_response_unix(
    stream: &mut UnixStream,
    response: &RpcResponse,
) -> Result<(), Box<dyn std::error::Error>> {
    let response_json = serde_json::to_vec(response)?;
    let len = response_json.len() as u32;

    // 先发送长度（4字节）
    stream.write_u32(len).await?;
    // 再发送响应体
    stream.write_all(&response_json).await?;
    stream.flush().await?;

    Ok(())
}

/// 发送响应到客户端（TCP Socket）
#[cfg(windows)]
async fn send_response_tcp(
    stream: &mut TcpStream,
    response: &RpcResponse,
) -> Result<(), Box<dyn std::error::Error>> {
    let response_json = serde_json::to_vec(response)?;
    let len = response_json.len() as u32;

    // 先发送长度（4字节）
    stream.write_u32(len).await?;
    // 再发送响应体
    stream.write_all(&response_json).await?;
    stream.flush().await?;

    Ok(())
}

/// IPC 客户端：用于 CLI 命令查询 daemon 状态
pub struct IpcClient {
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(windows)]
    port: u16,
}

impl IpcClient {
    #[cfg(unix)]
    pub fn new(socket_path: Option<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.unwrap_or_else(default_socket_path),
        }
    }

    #[cfg(windows)]
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    /// 连接到 daemon
    #[cfg(unix)]
    async fn connect(&self) -> Result<UnixStream, String> {
        UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| format!("无法连接到 daemon ({}): {}", self.socket_path.display(), e))
    }

    #[cfg(windows)]
    async fn connect(&self) -> Result<TcpStream, String> {
        let addr = format!("127.0.0.1:{}", self.port);
        TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("无法连接到 daemon ({}): {}", addr, e))
    }

    /// 发送 RPC 请求并接收响应
    async fn call(&self, request: RpcRequest) -> Result<RpcResponse, String> {
        let mut stream = self.connect().await?;

        // 序列化请求
        let request_json =
            serde_json::to_vec(&request).map_err(|e| format!("序列化请求失败: {}", e))?;

        // 发送请求长度和内容
        stream
            .write_u32(request_json.len() as u32)
            .await
            .map_err(|e| format!("发送请求长度失败: {}", e))?;
        stream
            .write_all(&request_json)
            .await
            .map_err(|e| format!("发送请求内容失败: {}", e))?;
        stream
            .flush()
            .await
            .map_err(|e| format!("刷新流失败: {}", e))?;

        // 读取响应长度
        let response_len = stream
            .read_u32()
            .await
            .map_err(|e| format!("读取响应长度失败: {}", e))?;

        // 安全检查：防止恶意服务器发送超大响应导致 OOM
        const MAX_RESPONSE_SIZE: u32 = 100 * 1024 * 1024; // 100MB（响应可能包含大量进程数据）
        if response_len > MAX_RESPONSE_SIZE {
            return Err(format!(
                "响应体过大: {} 字节（最大允许: {} 字节）",
                response_len, MAX_RESPONSE_SIZE
            ));
        }

        // 读取响应内容
        let mut response_buf = vec![0u8; response_len as usize];
        stream
            .read_exact(&mut response_buf)
            .await
            .map_err(|e| format!("读取响应内容失败: {}", e))?;

        // 解析响应
        let response: RpcResponse =
            serde_json::from_slice(&response_buf).map_err(|e| format!("解析响应失败: {}", e))?;

        Ok(response)
    }

    /// 查询进程列表
    pub async fn list_processes(&self) -> Result<Vec<serde_json::Value>, String> {
        let response = self.call(RpcRequest::ListProcesses).await?;

        if !response.success {
            return Err(response.error.unwrap_or_else(|| "未知错误".to_string()));
        }

        let processes: Vec<serde_json::Value> =
            serde_json::from_value(response.data.ok_or_else(|| "响应数据为空".to_string())?)
                .map_err(|e| format!("解析进程列表失败: {}", e))?;

        Ok(processes)
    }

    /// 查询进程阻塞根因
    pub async fn why_process(&self, pid: u32) -> Result<Vec<String>, String> {
        let response = self.call(RpcRequest::WhyProcess { pid }).await?;

        if !response.success {
            return Err(response.error.unwrap_or_else(|| "未知错误".to_string()));
        }

        let data = response.data.ok_or_else(|| "响应数据为空".to_string())?;
        let causes: Vec<String> = data["causes"]
            .as_array()
            .ok_or_else(|| "causes 字段格式错误".to_string())?
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        Ok(causes)
    }

    /// 检查 daemon 是否运行
    pub async fn ping(&self) -> Result<bool, String> {
        match self.call(RpcRequest::Ping).await {
            Ok(response) => Ok(response.success),
            Err(_) => Ok(false),
        }
    }
}
