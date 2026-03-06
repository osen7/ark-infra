mod action;
mod executor;
mod fix_engine;

pub use action::ActionType;
pub use executor::ActionExecutor;
pub use fix_engine::FixEngine;

use crate::plugin::Actuator;
use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

/// 系统执行器：执行进程清理等系统级操作
pub struct SystemActuator;

impl SystemActuator {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemActuator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actuator for SystemActuator {
    fn name(&self) -> &str {
        "system"
    }

    async fn execute(&self, target_pid: u32, action: &str) -> Result<(), String> {
        match action {
            "kill" | "zap" => self.kill_process_tree(target_pid).await,
            "reset" => {
                // 重置操作（未来可扩展）
                Err("重置操作尚未实现".to_string())
            }
            _ => Err(format!("未知动作: {}", action)),
        }
    }
}

impl SystemActuator {
    /// 彻底清理进程树（包括所有子进程）
    async fn kill_process_tree(&self, pid: u32) -> Result<(), String> {
        // Linux/Unix: 使用 kill -9 和进程组
        let pgid_result = self.get_process_group(pid).await;

        if let Ok(pgid) = pgid_result {
            // 使用 kill 命令终止整个进程组
            let output = Command::new("kill")
                .args(["-9", &format!("-{}", pgid)])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| format!("执行 kill 失败: {}", e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("No such process") {
                    return Ok(());
                }
                return Err(format!("kill 失败: {}", stderr));
            }
        } else {
            // 如果无法获取进程组，直接 kill 主进程
            let output = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| format!("执行 kill 失败: {}", e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("No such process") {
                    return Ok(());
                }
                return Err(format!("kill 失败: {}", stderr));
            }
        }

        Ok(())
    }

    #[cfg(unix)]
    async fn get_process_group(&self, pid: u32) -> Result<u32, String> {
        use std::fs;

        // 读取 /proc/{pid}/stat 获取进程组 ID
        let stat_path = format!("/proc/{}/stat", pid);
        let stat_content = fs::read_to_string(&stat_path)
            .map_err(|e| format!("读取 {} 失败: {}", stat_path, e))?;

        // /proc/{pid}/stat 格式：pid comm state ppid pgrp ...
        let fields: Vec<&str> = stat_content.split_whitespace().collect();
        if fields.len() < 5 {
            return Err("stat 文件格式错误".to_string());
        }

        let pgid = fields[4]
            .parse::<u32>()
            .map_err(|e| format!("解析进程组 ID 失败: {}", e))?;

        Ok(pgid)
    }
}
