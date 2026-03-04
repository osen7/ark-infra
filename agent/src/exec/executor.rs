use crate::exec::action::ActionType;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

/// 动作执行器
///
/// 负责执行各种类型的动作
pub struct ActionExecutor;

impl ActionExecutor {
    pub fn new() -> Self {
        Self
    }

    /// 执行动作
    pub async fn execute(&self, action: &ActionType, pid: u32) -> Result<String, String> {
        match action {
            ActionType::Signal { signal } => self.send_signal(*signal, pid).await,
            ActionType::CgroupThrottle {
                cpu_quota,
                memory_limit,
                io_limit,
            } => {
                self.apply_cgroup_throttle(pid, *cpu_quota, *memory_limit, *io_limit)
                    .await
            }
            ActionType::NetworkRestart { interface } => {
                self.restart_network_interface(interface).await
            }
            ActionType::GracefulShutdown {
                signal,
                wait_seconds,
                force_kill,
            } => {
                self.graceful_shutdown(*signal, *wait_seconds, *force_kill, pid)
                    .await
            }
            ActionType::KillProcess => self.kill_process(pid).await,
            ActionType::IsolateNode { reason } => self.isolate_node(reason).await,
            ActionType::CheckCheckpoint { checkpoint_dir } => {
                self.check_checkpoint(checkpoint_dir).await
            }
            ActionType::Custom { command, args } => {
                self.execute_custom_command(command, args).await
            }
        }
    }

    /// 发送信号
    async fn send_signal(&self, signal: i32, pid: u32) -> Result<String, String> {
        #[cfg(unix)]
        {
            let output = Command::new("kill")
                .arg(format!("-{}", signal))
                .arg(pid.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| format!("执行 kill 失败: {}", e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("发送信号失败: {}", stderr));
            }

            Ok(format!("成功发送信号 {} 到进程 {}", signal, pid))
        }

        #[cfg(windows)]
        {
            Err("Windows 不支持信号发送".to_string())
        }
    }

    /// 应用 Cgroup 限流
    async fn apply_cgroup_throttle(
        &self,
        pid: u32,
        cpu_quota: Option<u64>,
        memory_limit: Option<u64>,
        _io_limit: Option<u64>,
    ) -> Result<String, String> {
        #[cfg(unix)]
        {
            // 创建临时 cgroup
            let cgroup_name = format!("ark-{}", pid);
            let cgroup_path = format!("/sys/fs/cgroup/ark/{}", cgroup_name);

            // 创建 cgroup 目录
            let _ = Command::new("mkdir")
                .arg("-p")
                .arg(&cgroup_path)
                .output()
                .await;

            let mut results = Vec::new();

            // 设置 CPU 限制
            if let Some(quota) = cpu_quota {
                let cpu_quota_file = format!("{}/cpu.cfs_quota_us", cgroup_path);
                if let Err(e) = tokio::fs::write(&cpu_quota_file, quota.to_string()).await {
                    results.push(format!("CPU 限流失败: {}", e));
                } else {
                    results.push(format!("CPU 限流: {}%", quota / 1000));
                }
            }

            // 设置内存限制
            if let Some(limit) = memory_limit {
                let memory_limit_file = format!("{}/memory.limit_in_bytes", cgroup_path);
                if let Err(e) = tokio::fs::write(&memory_limit_file, limit.to_string()).await {
                    results.push(format!("内存限流失败: {}", e));
                } else {
                    results.push(format!("内存限流: {}MB", limit / 1024 / 1024));
                }
            }

            // 将进程加入 cgroup
            let tasks_file = format!("{}/tasks", cgroup_path);
            if let Err(e) = tokio::fs::write(&tasks_file, pid.to_string()).await {
                return Err(format!("将进程加入 cgroup 失败: {}", e));
            }

            Ok(format!("Cgroup 限流应用成功: {}", results.join(", ")))
        }

        #[cfg(windows)]
        {
            Err("Windows 不支持 Cgroup".to_string())
        }
    }

    /// 重启网络接口
    async fn restart_network_interface(&self, interface: &str) -> Result<String, String> {
        #[cfg(unix)]
        {
            // 先 down
            let down_output = Command::new("ip")
                .arg("link")
                .arg("set")
                .arg("down")
                .arg(interface)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| format!("执行 ip link set down 失败: {}", e))?;

            if !down_output.status.success() {
                let stderr = String::from_utf8_lossy(&down_output.stderr);
                return Err(format!("关闭接口失败: {}", stderr));
            }

            // 等待 1 秒
            sleep(Duration::from_secs(1)).await;

            // 再 up
            let up_output = Command::new("ip")
                .arg("link")
                .arg("set")
                .arg("up")
                .arg(interface)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| format!("执行 ip link set up 失败: {}", e))?;

            if !up_output.status.success() {
                let stderr = String::from_utf8_lossy(&up_output.stderr);
                return Err(format!("启动接口失败: {}", stderr));
            }

            Ok(format!("成功重启网络接口: {}", interface))
        }

        #[cfg(windows)]
        {
            Err("Windows 网络接口重启需要管理员权限".to_string())
        }
    }

    /// 优雅降级
    async fn graceful_shutdown(
        &self,
        signal: i32,
        wait_seconds: u64,
        force_kill: bool,
        pid: u32,
    ) -> Result<String, String> {
        // 1. 先发信号
        let signal_result = self.send_signal(signal, pid).await;
        if let Err(e) = signal_result {
            if force_kill {
                // 如果发信号失败但需要强制终止，继续执行
            } else {
                return Err(e);
            }
        }

        // 2. 等待
        sleep(Duration::from_secs(wait_seconds)).await;

        // 3. 如果设置了强制终止，执行 kill
        if force_kill {
            match self.kill_process(pid).await {
                Ok(msg) => Ok(format!(
                    "优雅降级完成: 已发送信号 {}，等待 {} 秒后{}",
                    signal, wait_seconds, msg
                )),
                Err(e) => Err(format!("优雅降级失败: {}", e)),
            }
        } else {
            Ok(format!("已发送信号 {}，等待 {} 秒", signal, wait_seconds))
        }
    }

    /// 终止进程
    async fn kill_process(&self, pid: u32) -> Result<String, String> {
        #[cfg(unix)]
        {
            let output = Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| format!("执行 kill 失败: {}", e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("No such process") {
                    return Ok("进程已不存在".to_string());
                }
                return Err(format!("kill 失败: {}", stderr));
            }

            Ok(format!("成功终止进程 {}", pid))
        }

        #[cfg(windows)]
        {
            let output = Command::new("taskkill")
                .args(&["/F", "/PID", &pid.to_string()])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| format!("执行 taskkill 失败: {}", e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("找不到") || stderr.contains("not found") {
                    return Ok("进程已不存在".to_string());
                }
                return Err(format!("taskkill 失败: {}", stderr));
            }

            Ok(format!("成功终止进程 {}", pid))
        }
    }

    /// 隔离节点
    async fn isolate_node(&self, reason: &str) -> Result<String, String> {
        // 这里可以实现节点隔离逻辑
        // 例如：标记节点为不可调度、更新集群状态等
        Ok(format!("节点已隔离: {}", reason))
    }

    /// 检查 Checkpoint
    async fn check_checkpoint(&self, checkpoint_dir: &str) -> Result<String, String> {
        use tokio::fs;

        match fs::metadata(checkpoint_dir).await {
            Ok(metadata) => {
                if metadata.is_dir() {
                    let entries = fs::read_dir(checkpoint_dir)
                        .await
                        .map_err(|e| format!("读取目录失败: {}", e))?;

                    let mut count = 0;
                    let mut latest = None;
                    let mut latest_time = 0u64;

                    let mut entries = entries;
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        count += 1;
                        if let Ok(modified) = entry.metadata().await.and_then(|m| m.modified()) {
                            let time = modified
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            if time > latest_time {
                                latest_time = time;
                                latest = Some(entry.file_name().to_string_lossy().to_string());
                            }
                        }
                    }

                    Ok(format!(
                        "Checkpoint 检查完成: 找到 {} 个文件，最新: {:?}",
                        count, latest
                    ))
                } else {
                    Err(format!("{} 不是目录", checkpoint_dir))
                }
            }
            Err(e) => Err(format!("检查 Checkpoint 失败: {}", e)),
        }
    }

    /// 执行自定义命令
    async fn execute_custom_command(
        &self,
        command: &str,
        args: &[String],
    ) -> Result<String, String> {
        let output = Command::new(command)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("执行命令失败: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("命令执行失败: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(format!("命令执行成功: {}", stdout.trim()))
    }
}

impl Default for ActionExecutor {
    fn default() -> Self {
        Self::new()
    }
}
