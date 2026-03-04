mod matcher;
mod rule;

pub use matcher::RuleMatcher;
pub use rule::{Applicability, Condition, RootCausePattern, Rule, SolutionStep};

use crate::event::Event;
use crate::graph::StateGraph;
use std::fs;
use std::path::Path;

/// 规则引擎
pub struct RuleEngine {
    rules: Vec<Rule>,
}

impl RuleEngine {
    /// 从目录加载所有规则文件
    pub fn load_from_dir<P: AsRef<Path>>(dir: P) -> Result<Self, String> {
        let mut rules = Vec::new();
        let dir_path = dir.as_ref();

        if !dir_path.exists() {
            // 如果目录不存在，返回空规则引擎（不报错，允许无规则运行）
            return Ok(Self { rules });
        }

        let entries = fs::read_dir(dir_path).map_err(|e| format!("读取规则目录失败: {}", e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("yaml")
                || path.extension().and_then(|s| s.to_str()) == Some("yml")
            {
                let content = fs::read_to_string(&path)
                    .map_err(|e| format!("读取规则文件失败 {}: {}", path.display(), e))?;

                match serde_yaml::from_str::<Rule>(&content) {
                    Ok(rule) => rules.push(rule),
                    Err(e) => {
                        eprintln!("[rule-engine] 跳过无效规则文件 {}: {}", path.display(), e);
                    }
                }
            }
        }

        // 按优先级排序（优先级高的在前）
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));

        Ok(Self { rules })
    }

    /// 匹配规则
    /// 返回匹配的规则列表（按优先级排序）
    pub async fn match_rules(&self, graph: &StateGraph, events: &[Event]) -> Vec<&Rule> {
        let mut matched = Vec::new();

        for rule in &self.rules {
            if RuleMatcher::match_all_conditions(&rule.conditions, events, graph).await {
                matched.push(rule);
            }
        }

        matched
    }

    /// 获取第一个匹配的规则
    pub async fn match_first(&self, graph: &StateGraph, events: &[Event]) -> Option<&Rule> {
        for rule in &self.rules {
            if RuleMatcher::match_all_conditions(&rule.conditions, events, graph).await {
                return Some(rule);
            }
        }
        None
    }

    /// 简化版规则匹配（只匹配事件条件，不匹配图条件）
    /// 用于在无法访问完整图状态时的快速匹配
    pub async fn match_first_simple(&self, events: &[Event]) -> Option<&Rule> {
        for rule in &self.rules {
            // 只检查事件条件
            let event_conditions: Vec<_> = rule
                .conditions
                .iter()
                .filter(|c| matches!(c, Condition::Event { .. }))
                .collect();

            if event_conditions.is_empty() {
                continue; // 如果没有事件条件，跳过
            }

            // 创建一个假的图用于匹配（实际上不会使用）
            // 这里我们需要一个更好的设计，但为了简化先这样
            // 实际上，简化版只匹配事件条件，不匹配图条件
            let all_event_conditions_match = event_conditions.iter().all(|condition| {
                if let Condition::Event {
                    event_type,
                    entity_id_pattern,
                    value_pattern,
                    value_threshold,
                } = condition
                {
                    events.iter().any(|event| {
                        if event.event_type.to_string() != *event_type {
                            return false;
                        }
                        if let Some(pattern) = entity_id_pattern {
                            if !matches_pattern(&event.entity_id, pattern) {
                                return false;
                            }
                        }
                        if let Some(pattern) = value_pattern {
                            if !event.value.contains(pattern) {
                                return false;
                            }
                        }
                        if let Some(threshold) = value_threshold {
                            if let Ok(value) = event.value.parse::<f64>() {
                                if value < *threshold {
                                    return false;
                                }
                            } else {
                                return false;
                            }
                        }
                        true
                    })
                } else {
                    false
                }
            });

            if all_event_conditions_match {
                return Some(rule);
            }
        }
        None
    }
}

/// 简单的通配符模式匹配（从 matcher.rs 复制）
fn matches_pattern(text: &str, pattern: &str) -> bool {
    pattern
        .split('|')
        .any(|p| matches_single_pattern(text, p.trim()))
}

fn matches_single_pattern(text: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return text == pattern;
    }

    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let parts: Vec<&str> = pattern.split('*').filter(|p| !p.is_empty()).collect();

    if parts.is_empty() {
        return true;
    }

    let mut cursor = 0usize;
    for (idx, part) in parts.iter().enumerate() {
        if idx == 0 && anchored_start {
            if !text[cursor..].starts_with(part) {
                return false;
            }
            cursor += part.len();
            continue;
        }
        if let Some(found) = text[cursor..].find(part) {
            cursor += found + part.len();
        } else {
            return false;
        }
    }

    if anchored_end {
        if let Some(last) = parts.last() {
            return text.ends_with(last);
        }
    }
    true
}

impl RuleEngine {
    /// 获取规则数量
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

#[cfg(test)]
mod tests {
    use super::RuleEngine;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn load_from_dir_skips_invalid_rule_files() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ark-rule-test-{}", uniq));
        fs::create_dir_all(&dir).expect("create temp rule dir");

        let valid = r#"
name: "valid"
scene: "network_stall"
priority: 1
conditions:
  - type: "event"
    event_type: "transport.drop"
root_cause_pattern:
  primary: "test"
solution_steps:
  - step: 1
    action: "noop"
    manual: true
related_evidences: []
applicability:
  min_confidence: 0.5
"#;
        let invalid = "conditions:\n  all: bad";

        fs::write(dir.join("valid.yaml"), valid).expect("write valid rule");
        fs::write(dir.join("invalid.yaml"), invalid).expect("write invalid rule");

        let engine = RuleEngine::load_from_dir(&dir).expect("load rules");
        assert_eq!(engine.rule_count(), 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
