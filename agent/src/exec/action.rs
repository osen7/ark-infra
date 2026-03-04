/// 执行动作类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionType {
    /// 发送信号（如 SIGUSR1 触发 Checkpoint）
    Signal { signal: i32 },
    /// Cgroup 限流（限制 CPU/内存/IO）
    CgroupThrottle {
        cpu_quota: Option<u64>,    // CPU 配额（微秒）
        memory_limit: Option<u64>, // 内存限制（字节）
        io_limit: Option<u64>,     // IO 限制（字节/秒）
    },
    /// 重启网络接口
    NetworkRestart { interface: String },
    /// 优雅降级（先发信号，等待后 kill）
    GracefulShutdown {
        signal: i32,
        wait_seconds: u64,
        force_kill: bool,
    },
    /// 清理进程（kill -9）
    KillProcess,
    /// 隔离节点（标记为不可调度）
    IsolateNode { reason: String },
    /// 检查 Checkpoint 文件
    CheckCheckpoint { checkpoint_dir: String },
    /// 其他自定义动作
    Custom { command: String, args: Vec<String> },
}

/// 从 recommended_actions 文本解析动作类型
impl ActionType {
    pub fn from_recommendation(text: &str) -> Option<Self> {
        let text_lower = text.to_lowercase();

        // 信号相关
        if text_lower.contains("sigusr1")
            || text_lower.contains("checkpoint dump")
            || text_lower.contains("触发 checkpoint")
            || text_lower.contains("保存 checkpoint")
        {
            return Some(ActionType::Signal { signal: 10 }); // SIGUSR1 = 10
        }

        if text_lower.contains("signal") && text_lower.contains("dump") {
            return Some(ActionType::Signal { signal: 10 });
        }

        // Kill/Zap 相关
        if text_lower.contains("ark zap")
            || text_lower.contains("kill")
            || text_lower.contains("终止进程")
            || text_lower.contains("清理")
        {
            return Some(ActionType::KillProcess);
        }

        // 优雅降级
        if text_lower.contains("优雅")
            || text_lower.contains("graceful")
            || (text_lower.contains("信号") && text_lower.contains("等待"))
        {
            return Some(ActionType::GracefulShutdown {
                signal: 10, // SIGUSR1
                wait_seconds: 10,
                force_kill: true,
            });
        }

        // Cgroup 限流
        if text_lower.contains("cgroup")
            || text_lower.contains("限流")
            || text_lower.contains("限制")
            || text_lower.contains("throttle")
        {
            return Some(ActionType::CgroupThrottle {
                cpu_quota: Some(50000), // 50% CPU
                memory_limit: None,
                io_limit: None,
            });
        }

        // 网络重启
        if text_lower.contains("重启网卡")
            || text_lower.contains("restart network")
            || text_lower.contains("网络接口")
        {
            // 尝试提取接口名，默认 eth0
            let interface = if text_lower.contains("eth") {
                "eth0".to_string()
            } else if text_lower.contains("eno") {
                "eno1".to_string()
            } else {
                "eth0".to_string()
            };
            return Some(ActionType::NetworkRestart { interface });
        }

        // 隔离节点
        if text_lower.contains("隔离") || text_lower.contains("isolate") {
            return Some(ActionType::IsolateNode {
                reason: text.to_string(),
            });
        }

        // Checkpoint 检查
        if text_lower.contains("checkpoint")
            && (text_lower.contains("检查") || text_lower.contains("check"))
        {
            return Some(ActionType::CheckCheckpoint {
                checkpoint_dir: "/tmp/checkpoints".to_string(),
            });
        }

        None
    }

    /// 获取动作的描述
    pub fn description(&self) -> String {
        match self {
            ActionType::Signal { signal } => {
                format!("发送信号 {} ({})", signal, signal_name(*signal))
            }
            ActionType::CgroupThrottle {
                cpu_quota,
                memory_limit,
                io_limit,
            } => {
                let mut parts = Vec::new();
                if let Some(cpu) = cpu_quota {
                    parts.push(format!("CPU: {}%", cpu / 1000));
                }
                if let Some(mem) = memory_limit {
                    parts.push(format!("内存: {}MB", mem / 1024 / 1024));
                }
                if let Some(io) = io_limit {
                    parts.push(format!("IO: {}MB/s", io / 1024 / 1024));
                }
                format!("Cgroup 限流: {}", parts.join(", "))
            }
            ActionType::NetworkRestart { interface } => {
                format!("重启网络接口: {}", interface)
            }
            ActionType::GracefulShutdown {
                signal,
                wait_seconds,
                force_kill,
            } => {
                format!(
                    "优雅降级: 发送信号 {}，等待 {} 秒{}",
                    signal,
                    wait_seconds,
                    if *force_kill {
                        "，然后强制终止"
                    } else {
                        ""
                    }
                )
            }
            ActionType::KillProcess => "强制终止进程".to_string(),
            ActionType::IsolateNode { reason } => {
                format!("隔离节点: {}", reason)
            }
            ActionType::CheckCheckpoint { checkpoint_dir } => {
                format!("检查 Checkpoint: {}", checkpoint_dir)
            }
            ActionType::Custom { command, args } => {
                format!("执行命令: {} {}", command, args.join(" "))
            }
        }
    }
}

/// 获取信号名称
fn signal_name(sig: i32) -> &'static str {
    match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        12 => "SIGUSR2",
        15 => "SIGTERM",
        _ => "UNKNOWN",
    }
}
