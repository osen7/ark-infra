//! 审计日志模块
//!
//! 记录所有 ark fix 执行的系统级动作，满足企业合规要求

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 审计日志条目
#[derive(Debug, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub timestamp: String,
    pub user: String,
    pub action: String,
    pub target_pid: u32,
    pub target_job_id: Option<String>,
    pub result: String,
    pub details: String,
}

/// 审计日志记录器
pub struct AuditLogger {
    log_file: Arc<RwLock<BufWriter<File>>>,
    log_path: PathBuf,
    max_size: u64, // 最大文件大小（字节）
    current_size: Arc<RwLock<u64>>,
}

impl AuditLogger {
    /// 创建新的审计日志记录器
    pub fn new(log_path: PathBuf, max_size_mb: u64) -> Result<Self, std::io::Error> {
        let max_size = max_size_mb * 1024 * 1024; // 转换为字节

        // 确保日志目录存在
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // 打开或创建日志文件（追加模式）
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        let current_size = file.metadata()?.len();

        Ok(Self {
            log_file: Arc::new(RwLock::new(BufWriter::new(file))),
            log_path,
            max_size,
            current_size: Arc::new(RwLock::new(current_size)),
        })
    }

    /// 记录审计日志
    pub async fn log(&self, entry: AuditLogEntry) -> Result<(), std::io::Error> {
        // 序列化为 JSON
        let json = serde_json::to_string(&entry)?;
        let line = format!("{}\n", json);
        let line_bytes = line.len() as u64;

        // 检查文件大小，如果超过限制则轮转
        let mut current_size = self.current_size.write().await;
        if *current_size + line_bytes > self.max_size {
            self.rotate_log().await?;
            *current_size = 0;
        }

        // 写入日志
        {
            let mut writer = self.log_file.write().await;
            writer.write_all(line.as_bytes())?;
            writer.flush()?;
        }

        *current_size += line_bytes;

        Ok(())
    }

    /// 轮转日志文件
    async fn rotate_log(&self) -> Result<(), std::io::Error> {
        // 关闭当前文件
        {
            let mut writer = self.log_file.write().await;
            writer.flush()?;
        }

        // 生成带时间戳的新文件名
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let rotated_path = self.log_path.with_extension(format!("{}.log", timestamp));

        // 重命名当前文件
        std::fs::rename(&self.log_path, &rotated_path)?;

        // 创建新文件
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        *self.log_file.write().await = BufWriter::new(file);

        println!("[audit] 日志文件已轮转: {}", rotated_path.display());

        Ok(())
    }

    /// 获取当前用户（从环境变量或系统）
    fn get_current_user() -> String {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

/// 创建审计日志条目
pub fn create_audit_entry(
    action: &str,
    target_pid: u32,
    target_job_id: Option<&str>,
    result: &str,
    details: &str,
) -> AuditLogEntry {
    AuditLogEntry {
        timestamp: Utc::now().to_rfc3339(),
        user: AuditLogger::get_current_user(),
        action: action.to_string(),
        target_pid,
        target_job_id: target_job_id.map(|s| s.to_string()),
        result: result.to_string(),
        details: details.to_string(),
    }
}
