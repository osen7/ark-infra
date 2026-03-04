mod r#trait;

pub use r#trait::{Actuator, EventSource};

use ark_core::event::Event;
use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// 子进程探针：通过启动外部脚本并读取其 stdout 来获取事件
pub struct SubprocessProbe {
    command: String,
    args: Vec<String>,
}

impl SubprocessProbe {
    /// 创建新的子进程探针
    pub fn new(command: String, args: Vec<String>) -> Self {
        Self { command, args }
    }
}

#[async_trait]
impl EventSource for SubprocessProbe {
    fn name(&self) -> &str {
        "subprocess"
    }

    async fn start_stream(&self, tx: mpsc::Sender<Event>) -> Result<(), String> {
        loop {
            // 启动子进程
            let mut child = Command::new(&self.command)
                .args(&self.args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("启动探针进程失败: {}", e))?;

            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| "无法获取子进程 stdout".to_string())?;

            // 按行读取 stdout
            let mut reader = BufReader::new(stdout);
            let mut line_buf = String::new();

            loop {
                line_buf.clear();
                match reader.read_line(&mut line_buf).await {
                    Ok(0) => {
                        // EOF，子进程已退出
                        break;
                    }
                    Ok(_) => {
                        // 解析 JSON 行
                        let line = line_buf.trim();
                        if line.is_empty() {
                            continue;
                        }

                        match serde_json::from_str::<Event>(line) {
                            Ok(event) => {
                                // 发送事件
                                if let Err(e) = tx.send(event).await {
                                    return Err(format!("发送事件失败（通道已关闭）: {}", e));
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "[SubprocessProbe] 解析 JSON 失败: {} | 行内容: {}",
                                    e, line
                                );
                                // 继续处理下一行，不中断探针
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[SubprocessProbe] 读取 stdout 失败: {}", e);
                        break;
                    }
                }
            }

            // 等待子进程退出
            match child.wait().await {
                Ok(status) => {
                    if !status.success() {
                        eprintln!(
                            "[SubprocessProbe] 子进程异常退出，状态码: {:?}",
                            status.code()
                        );
                    }
                }
                Err(e) => {
                    eprintln!("[SubprocessProbe] 等待子进程失败: {}", e);
                }
            }

            // 如果子进程崩溃，等待一秒后重启（避免快速重启循环）
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }
}
