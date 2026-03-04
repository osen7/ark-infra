use crate::exec::action::ActionType;
use crate::exec::executor::ActionExecutor;
use crate::scene::AnalysisResult;

/// ark fix 执行引擎
///
/// 这是 OODA 循环中的 Act 层，负责执行诊断结果中的 recommended_actions
pub struct FixEngine {
    executor: ActionExecutor,
}

impl FixEngine {
    pub fn new() -> Self {
        Self {
            executor: ActionExecutor::new(),
        }
    }

    /// 从 AnalysisResult 解析并执行推荐动作
    pub async fn fix_from_analysis(
        &self,
        result: &AnalysisResult,
        pid: u32,
    ) -> Result<FixResult, String> {
        let mut executed_actions = Vec::new();
        let mut failed_actions = Vec::new();

        // 解析 recommended_actions
        let actions = self.parse_recommendations(&result.recommended_actions);

        if actions.is_empty() {
            return Ok(FixResult {
                success: false,
                message: "没有可执行的动作".to_string(),
                executed_actions,
                failed_actions,
            });
        }

        // 按优先级执行动作
        for (action, priority) in actions {
            match self.executor.execute(&action, pid).await {
                Ok(msg) => {
                    executed_actions.push(ExecutedAction {
                        action: action.description(),
                        result: msg,
                        priority,
                    });
                }
                Err(e) => {
                    failed_actions.push(FailedAction {
                        action: action.description(),
                        error: e,
                        priority,
                    });
                }
            }
        }

        let success = failed_actions.is_empty();
        let message = if success {
            format!("成功执行 {} 个动作", executed_actions.len())
        } else {
            format!(
                "执行完成：{} 成功，{} 失败",
                executed_actions.len(),
                failed_actions.len()
            )
        };

        Ok(FixResult {
            success,
            message,
            executed_actions,
            failed_actions,
        })
    }

    /// 解析 recommended_actions 文本为 ActionType 列表
    fn parse_recommendations(&self, recommendations: &[String]) -> Vec<(ActionType, u8)> {
        let mut actions = Vec::new();

        for rec in recommendations {
            if let Some(action) = ActionType::from_recommendation(rec) {
                // 根据动作类型设置优先级
                let priority = match &action {
                    ActionType::Signal { .. } => 1, // 最高优先级：先发信号
                    ActionType::GracefulShutdown { .. } => 2,
                    ActionType::CgroupThrottle { .. } => 3,
                    ActionType::CheckCheckpoint { .. } => 4,
                    ActionType::NetworkRestart { .. } => 5,
                    ActionType::IsolateNode { .. } => 6,
                    ActionType::KillProcess => 10, // 最低优先级：最后才 kill
                    ActionType::Custom { .. } => 7,
                };
                actions.push((action, priority));
            }
        }

        // 按优先级排序
        actions.sort_by_key(|(_, p)| *p);
        actions
    }
}

impl Default for FixEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// 执行结果
#[derive(Debug, Clone)]
pub struct FixResult {
    pub success: bool,
    pub message: String,
    pub executed_actions: Vec<ExecutedAction>,
    pub failed_actions: Vec<FailedAction>,
}

/// 已执行的动作
#[derive(Debug, Clone)]
pub struct ExecutedAction {
    pub action: String,
    pub result: String,
    pub priority: u8,
}

/// 失败的动作
#[derive(Debug, Clone)]
pub struct FailedAction {
    pub action: String,
    pub error: String,
    pub priority: u8,
}
