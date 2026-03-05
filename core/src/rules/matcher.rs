use crate::event::Event;
use crate::graph::{EdgeType, NodeType, StateGraph};
use crate::rules::rule::{ComparisonOp, Condition, MetricCondition, ValueType};

/// 规则匹配器
pub struct RuleMatcher;

impl RuleMatcher {
    /// 检查规则条件是否满足（异步版本）
    pub async fn match_condition(
        condition: &Condition,
        events: &[Event],
        graph: &StateGraph,
    ) -> bool {
        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;
        Self::match_condition_with_snapshot(condition, events, &edges, &nodes)
    }

    fn match_condition_with_snapshot(
        condition: &Condition,
        events: &[Event],
        edges: &[crate::graph::Edge],
        nodes: &std::collections::HashMap<String, crate::graph::Node>,
    ) -> bool {
        match condition {
            Condition::Event {
                event_type,
                entity_id_pattern,
                value_pattern,
                value_threshold,
            } => {
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

                    // 匹配值模式
                    if let Some(pattern) = value_pattern {
                        if !matches_value_pattern(&event.value, pattern) {
                            return false;
                        }
                    }

                    // 匹配值阈值（改进：更安全的数值解析）
                    if let Some(threshold) = value_threshold {
                        match event.value.parse::<f64>() {
                            Ok(value) => {
                                if value < *threshold {
                                    return false;
                                }
                            }
                            Err(_) => {
                                // 如果无法解析为数值，且阈值存在，则不匹配
                                // 这避免了将 "D" (Disk Sleep) 误解析为 0.0
                                return false;
                            }
                        }
                    }

                    true
                })
            }
            Condition::Graph {
                edge_type,
                from_pattern,
                to_pattern,
            } => {
                edges.iter().any(|edge| {
                    // 匹配边类型
                    let edge_type_str = match edge.edge_type {
                        EdgeType::Consumes => "consumes",
                        EdgeType::WaitsOn => "waits_on",
                        EdgeType::BlockedBy => "blocked_by",
                    };

                    if edge_type_str != edge_type.as_str() {
                        return false;
                    }

                    // 匹配源节点模式
                    if let Some(pattern) = from_pattern {
                        if !matches_pattern(&edge.from, pattern) {
                            return false;
                        }
                    }

                    // 匹配目标节点模式
                    if let Some(pattern) = to_pattern {
                        if !matches_pattern(&edge.to, pattern) {
                            return false;
                        }
                    }

                    true
                })
            }
            Condition::Metric {
                node_type,
                entity_id_pattern,
                metrics,
            } => {
                nodes.values().any(|node| {
                    // 匹配节点类型
                    if let Some(ref nt) = node_type {
                        let node_type_str = match node.node_type {
                            NodeType::Process => "process",
                            NodeType::Resource => "resource",
                            NodeType::Error => "error",
                        };
                        if node_type_str != nt.as_str() {
                            return false;
                        }
                    }

                    // 匹配实体 ID 模式
                    if let Some(ref pattern) = entity_id_pattern {
                        if !matches_pattern(&node.id, pattern) {
                            return false;
                        }
                    }

                    // 匹配所有指标条件
                    metrics
                        .iter()
                        .all(|metric| match_metric_condition(metric, &node.metadata))
                })
            }
            Condition::Any { conditions } => {
                // OR 逻辑：任意一个条件满足即可
                for condition in conditions {
                    if Self::match_condition_with_snapshot(condition, events, edges, nodes) {
                        return true;
                    }
                }
                false
            }
            Condition::All { conditions } => {
                // AND 逻辑：所有条件都必须满足
                for condition in conditions {
                    if !Self::match_condition_with_snapshot(condition, events, edges, nodes) {
                        return false;
                    }
                }
                true
            }
        }
    }

    /// 检查所有条件是否满足（异步版本）
    pub async fn match_all_conditions(
        conditions: &[Condition],
        events: &[Event],
        graph: &StateGraph,
    ) -> bool {
        let edges = graph.get_all_edges_async().await;
        let nodes = graph.get_nodes_async().await;
        for condition in conditions {
            if !Self::match_condition_with_snapshot(condition, events, &edges, &nodes) {
                return false;
            }
        }
        true
    }
}

/// 匹配指标条件（支持数值和字符串比较）
fn match_metric_condition(
    metric: &MetricCondition,
    metadata: &std::collections::HashMap<String, String>,
) -> bool {
    let actual_str = match metadata.get(&metric.key) {
        Some(v) => v,
        None => return false,
    };

    match metric.value_type {
        ValueType::Numeric => {
            // 数值比较
            let actual_val = match actual_str.parse::<f64>() {
                Ok(v) => v,
                Err(_) => return false, // 无法解析为数值，不匹配
            };

            let target_val = match metric.target.parse::<f64>() {
                Ok(v) => v,
                Err(_) => return false,
            };

            match metric.op {
                ComparisonOp::Gt => actual_val > target_val,
                ComparisonOp::Lt => actual_val < target_val,
                ComparisonOp::Eq => (actual_val - target_val).abs() < 0.001, // 浮点数比较
                ComparisonOp::Gte => actual_val >= target_val,
                ComparisonOp::Lte => actual_val <= target_val,
                ComparisonOp::Ne => (actual_val - target_val).abs() >= 0.001,
                ComparisonOp::Contains => actual_str.contains(&metric.target),
            }
        }
        ValueType::String => {
            // 字符串比较
            match metric.op {
                ComparisonOp::Eq => actual_str == metric.target.as_str(),
                ComparisonOp::Ne => actual_str != metric.target.as_str(),
                ComparisonOp::Contains => actual_str.contains(&metric.target),
                _ => false, // 其他操作符对字符串无效
            }
        }
        ValueType::Auto => {
            // 自动检测：先尝试数值，失败则用字符串
            if let (Ok(actual_val), Ok(target_val)) =
                (actual_str.parse::<f64>(), metric.target.parse::<f64>())
            {
                // 数值比较
                match metric.op {
                    ComparisonOp::Gt => actual_val > target_val,
                    ComparisonOp::Lt => actual_val < target_val,
                    ComparisonOp::Eq => (actual_val - target_val).abs() < 0.001,
                    ComparisonOp::Gte => actual_val >= target_val,
                    ComparisonOp::Lte => actual_val <= target_val,
                    ComparisonOp::Ne => (actual_val - target_val).abs() >= 0.001,
                    ComparisonOp::Contains => actual_str.contains(&metric.target),
                }
            } else {
                // 字符串比较
                match metric.op {
                    ComparisonOp::Eq => actual_str == metric.target.as_str(),
                    ComparisonOp::Ne => actual_str != metric.target.as_str(),
                    ComparisonOp::Contains => actual_str.contains(&metric.target),
                    _ => false,
                }
            }
        }
    }
}

/// 简单的通配符模式匹配
/// 支持 * 通配符（如 "gpu-*"）
fn matches_pattern(text: &str, pattern: &str) -> bool {
    pattern
        .split('|')
        .any(|p| matches_single_pattern(text, p.trim()))
}

fn matches_value_pattern(text: &str, pattern: &str) -> bool {
    pattern
        .split('|')
        .map(str::trim)
        .any(|p| !p.is_empty() && text.contains(p))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_pattern() {
        assert!(matches_pattern("gpu-0", "gpu-*"));
        assert!(matches_pattern("gpu-1", "gpu-*"));
        assert!(!matches_pattern("cpu-0", "gpu-*"));
        assert!(matches_pattern("mlx5_0", "mlx5_*"));
        assert!(matches_pattern("node-a::roce-mlx5_0", "*roce-*"));
        assert!(matches_pattern("eth0", "roce-*|eth*"));
        assert!(!matches_pattern("ib0", "roce-*|eth*"));
    }
}
