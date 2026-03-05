use serde::{Deserialize, Serialize};

/// 规则数据结构
#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub id: Option<String>,
    pub name: String,
    pub scene: String,
    pub priority: u32,
    pub reason_codes: Vec<String>,
    pub conditions: Vec<Condition>,
    pub root_cause_pattern: RootCausePattern,
    pub solution_steps: Vec<SolutionStep>,
    pub related_evidences: Vec<String>,
    pub applicability: Applicability,
}

#[derive(Debug, Deserialize)]
pub struct RuleWire {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub scene: String,
    pub priority: u32,
    #[serde(default)]
    pub reason_codes: Vec<String>,
    pub conditions: ConditionsWire,
    pub root_cause_pattern: RootCausePattern,
    pub solution_steps: Vec<SolutionStep>,
    pub related_evidences: Vec<String>,
    pub applicability: Applicability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacySyntaxStatus {
    None,
    Migrated,
}

impl RuleWire {
    pub fn normalize(self) -> Result<(Rule, LegacySyntaxStatus), String> {
        let (conditions, legacy) = match self.conditions {
            ConditionsWire::New(v) => (v, LegacySyntaxStatus::None),
            ConditionsWire::Legacy(legacy) => match (legacy.all, legacy.any) {
                (Some(conditions), None) => (
                    vec![Condition::All { conditions }],
                    LegacySyntaxStatus::Migrated,
                ),
                (None, Some(conditions)) => (
                    vec![Condition::Any { conditions }],
                    LegacySyntaxStatus::Migrated,
                ),
                (Some(_), Some(_)) => {
                    return Err("legacy conditions 不能同时包含 all 和 any".to_string());
                }
                (None, None) => {
                    return Err("legacy conditions 必须包含 all 或 any".to_string());
                }
            },
        };

        Ok((
            Rule {
                id: self.id,
                name: self.name,
                scene: self.scene,
                priority: self.priority,
                reason_codes: self.reason_codes,
                conditions,
                root_cause_pattern: self.root_cause_pattern,
                solution_steps: self.solution_steps,
                related_evidences: self.related_evidences,
                applicability: self.applicability,
            },
            legacy,
        ))
    }
}

/// 值比较操作符
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComparisonOp {
    Gt,       // 大于
    Lt,       // 小于
    Eq,       // 等于
    Gte,      // 大于等于
    Lte,      // 小于等于
    Ne,       // 不等于
    Contains, // 包含（字符串）
}

/// 值类型
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    Numeric, // 数值类型
    String,  // 字符串类型
    Auto,    // 自动检测
}

/// 指标条件（用于节点 metadata 匹配）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MetricCondition {
    pub key: String,
    pub op: ComparisonOp,
    pub target: String,
    #[serde(default = "default_value_type")]
    pub value_type: ValueType,
}

fn default_value_type() -> ValueType {
    ValueType::Auto
}

/// 规则条件
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum Condition {
    /// 事件条件
    #[serde(rename = "event")]
    Event {
        event_type: String,
        entity_id_pattern: Option<String>,
        value_pattern: Option<String>,
        value_threshold: Option<f64>,
    },
    /// 图边条件
    #[serde(rename = "graph")]
    Graph {
        edge_type: String,
        from_pattern: Option<String>,
        to_pattern: Option<String>,
    },
    /// 节点指标条件（新增：支持 metadata 匹配）
    #[serde(rename = "metric")]
    Metric {
        node_type: Option<String>, // Process, Resource, Error
        entity_id_pattern: Option<String>,
        metrics: Vec<MetricCondition>,
    },
    /// 任意条件（OR 逻辑）
    #[serde(rename = "any")]
    Any { conditions: Vec<Condition> },
    /// 所有条件（AND 逻辑）
    #[serde(rename = "all")]
    All { conditions: Vec<Condition> },
}

/// 根因模式
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RootCausePattern {
    pub primary: String,
    pub secondary: Option<Vec<String>>,
}

/// 解决步骤
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SolutionStep {
    pub step: u32,
    pub action: String,
    pub command: Option<String>,
    #[serde(default)]
    pub manual: bool,
}

/// 适用条件
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Applicability {
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
    pub required_events: Option<Vec<String>>,
}

fn default_min_confidence() -> f64 {
    0.8
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ConditionsWire {
    New(Vec<Condition>),
    Legacy(ConditionsLegacyWire),
}

#[derive(Debug, Deserialize)]
pub struct ConditionsLegacyWire {
    #[serde(default)]
    all: Option<Vec<Condition>>,
    #[serde(default)]
    any: Option<Vec<Condition>>,
}
