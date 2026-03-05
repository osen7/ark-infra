# Ark 规则引擎设计（声明式知识库）

## 设计原则

**核心原则**：保持 Daemon 的 stateless 和极简，知识库以声明式规则文件形式存在。

## 架构设计

### 规则文件结构

```
ark-infra/
├── rules/
│   ├── gpu-oom.yaml          # GPU OOM 场景规则
│   ├── network-stall.yaml    # 网络阻塞场景规则
│   ├── process-crash.yaml    # 进程崩溃场景规则
│   └── gpu-error.yaml        # GPU 硬件错误规则
```

### 规则文件格式（YAML）

```yaml
# rules/gpu-oom.yaml
name: "GPU OOM 场景"
scene: "gpu_oom"
priority: 100

# 场景特征（匹配条件）
conditions:
  - type: "event"
    event_type: "error.hw"
    entity_id_pattern: "gpu-*"
    value_pattern: "OOM|out of memory"
  
  - type: "event"
    event_type: "compute.mem"
    value_threshold: 95  # 显存使用率 > 95%

# 根因模式
root_cause_pattern:
  primary: "GPU 显存不足"
  secondary:
    - "进程显存泄漏"
    - "批处理大小过大"

# 解决步骤
solution_steps:
  - step: 1
    action: "检查进程显存使用"
    command: "nvidia-smi --query-compute-apps=pid,used_memory --format=csv"
  
  - step: 2
    action: "终止占用显存最大的进程"
    command: "ark zap <pid>"
  
  - step: 3
    action: "降低批处理大小或模型精度"
    manual: true

# 证据类型
related_evidences:
  - "compute.mem"
  - "error.hw"
  - "process.state"

# 适用条件
applicability:
  min_confidence: 0.8
  required_events: ["compute.mem", "error.hw"]
```

### 规则引擎实现

```rust
// core/src/rules/mod.rs

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct Rule {
    pub name: String,
    pub scene: String,
    pub priority: u32,
    pub conditions: Vec<Condition>,
    pub root_cause_pattern: RootCausePattern,
    pub solution_steps: Vec<SolutionStep>,
    pub related_evidences: Vec<String>,
    pub applicability: Applicability,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Condition {
    Event {
        event_type: String,
        entity_id_pattern: Option<String>,
        value_pattern: Option<String>,
        value_threshold: Option<f64>,
    },
    Graph {
        edge_type: String,
        from_pattern: Option<String>,
        to_pattern: Option<String>,
    },
}

pub struct RuleEngine {
    rules: Vec<Rule>,
}

impl RuleEngine {
    /// 从 rules/ 目录加载所有规则
    pub fn load_from_dir<P: AsRef<Path>>(dir: P) -> Result<Self, String> {
        let mut rules = Vec::new();
        
        for entry in fs::read_dir(dir).map_err(|e| format!("读取规则目录失败: {}", e))? {
            let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
            let path = entry.path();
            
            if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                let content = fs::read_to_string(&path)
                    .map_err(|e| format!("读取规则文件失败: {}", e))?;
                let rule: Rule = serde_yaml::from_str(&content)
                    .map_err(|e| format!("解析规则文件失败: {}", e))?;
                rules.push(rule);
            }
        }
        
        // 按优先级排序
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        
        Ok(Self { rules })
    }
    
    /// 匹配规则
    pub fn match_rules(&self, graph: &StateGraph, events: &[Event]) -> Vec<&Rule> {
        self.rules
            .iter()
            .filter(|rule| self.check_conditions(rule, graph, events))
            .collect()
    }
    
    fn check_conditions(&self, rule: &Rule, graph: &StateGraph, events: &[Event]) -> bool {
        // 检查所有条件是否满足
        rule.conditions.iter().all(|condition| {
            match condition {
                Condition::Event { event_type, entity_id_pattern, value_pattern, value_threshold } => {
                    events.iter().any(|event| {
                        // 匹配事件类型
                        if event.event_type.to_string() != *event_type {
                            return false;
                        }
                        
                        // 匹配实体 ID 模式
                        if let Some(pattern) = entity_id_pattern {
                            if !matches_pattern(&event.entity_id, pattern) {
                                return false;
                            }
                        }
                        
                        // 匹配值模式或阈值
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
                            }
                        }
                        
                        true
                    })
                }
                Condition::Graph { edge_type, from_pattern, to_pattern } => {
                    // 检查图中的边
                    // 实现图匹配逻辑
                    false // TODO
                }
            }
        })
    }
}
```

## 使用方式

### 1. 规则文件管理

规则文件由用户或运维团队维护，放在 `rules/` 目录下。

### 2. 在诊断中使用

```rust
// agent/src/diag.rs

pub async fn run_diagnosis_with_rules(
    pid: u32,
    port: u16,
) -> Result<Diagnosis, Box<dyn std::error::Error>> {
    // 1. 加载规则引擎
    let rule_engine = RuleEngine::load_from_dir("rules")?;
    
    // 2. 获取图状态和事件
    let client = IpcClient::new(port);
    let causes = client.why_process(pid).await?;
    let processes = client.list_processes().await?;
    
    // 3. 匹配规则
    let matched_rules = rule_engine.match_rules(&graph, &events);
    
    // 4. 如果匹配到规则，直接返回规则中的解决方案
    if let Some(rule) = matched_rules.first() {
        return Ok(Diagnosis {
            pid,
            causes: vec![rule.root_cause_pattern.primary.clone()],
            recommendation: format_solution_steps(&rule.solution_steps),
            confidence: 0.9, // 规则匹配置信度较高
        });
    }
    
    // 5. 未匹配到规则，调用大模型
    let llm_client = LlmClient::from_env()?;
    llm_client.diagnose(pid, causes, processes).await
}
```

## 优势

1. **保持极简**：Daemon 只加载规则到内存，无数据库依赖
2. **易于维护**：规则文件是纯文本，易于版本控制和协作
3. **灵活扩展**：用户可以自定义规则，无需修改代码
4. **性能优秀**：内存匹配，无 I/O 开销
5. **Stateless**：Daemon 重启后重新加载规则，无状态依赖

## 与知识库系统的关系

- **规则引擎**：Daemon 端，轻量级，声明式
- **知识库系统**：CLI 端（可选），复杂匹配时调用远端向量数据库或 LLM

两者互补，规则引擎处理常见场景，知识库处理复杂场景。
