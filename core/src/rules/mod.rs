mod matcher;
mod rule;

pub use matcher::RuleMatcher;
pub use rule::{
    Applicability, Condition, LegacySyntaxStatus, RootCausePattern, Rule, RuleWire, SolutionStep,
};

use crate::event::Event;
use crate::graph::StateGraph;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// 规则引擎
pub struct RuleEngine {
    rules: Vec<Rule>,
    load_stats: RuleLoadStats,
}

#[derive(Debug, Clone, Default)]
pub struct RuleLoadStats {
    pub loaded_rules: usize,
    pub legacy_total: usize,
    pub legacy_migratable_total: usize,
    pub legacy_unsupported_total: usize,
    pub skipped_rules: usize,
    pub skipped_by_reason: HashMap<String, usize>,
    pub skipped_samples: Vec<RuleSkipSample>,
}

#[derive(Debug, Clone)]
pub struct RuleSkipSample {
    pub path: String,
    pub reason: String,
    pub detail: String,
}

impl RuleEngine {
    /// 从目录加载所有规则文件
    pub fn load_from_dir<P: AsRef<Path>>(dir: P) -> Result<Self, String> {
        let mut rules = Vec::new();
        let mut stats = RuleLoadStats::default();
        let dir_path = dir.as_ref();

        if !dir_path.exists() {
            // 如果目录不存在，返回空规则引擎（不报错，允许无规则运行）
            return Ok(Self {
                rules,
                load_stats: stats,
            });
        }

        let mut id_owner: HashMap<String, String> = HashMap::new();
        for (path, pack) in collect_rule_files(dir_path)? {
            let content = fs::read_to_string(&path)
                .map_err(|e| format!("读取规则文件失败 {}: {}", path.display(), e))?;

            match serde_yaml::from_str::<RuleWire>(&content) {
                Ok(rule_wire) => match rule_wire.normalize() {
                    Ok((mut rule, legacy_status)) => {
                        if legacy_status == LegacySyntaxStatus::Migrated {
                            stats.legacy_total += 1;
                            stats.legacy_migratable_total += 1;
                        }
                        let final_id = match rule.id.clone() {
                            Some(id) if !id.trim().is_empty() => id,
                            Some(_) => {
                                eprintln!(
                                    "[rule-engine] 跳过无效规则文件 {}: id 不能为空字符串",
                                    path.display()
                                );
                                continue;
                            }
                            None => format!("{}.{}", pack, rule.scene),
                        };

                        if let Some(owner) =
                            id_owner.insert(final_id.clone(), path.display().to_string())
                        {
                            return Err(format!(
                                "规则 ID 冲突: id=`{}` 文件冲突: {} <-> {}",
                                final_id,
                                owner,
                                path.display()
                            ));
                        }

                        rule.id = Some(final_id);
                        rules.push(rule);
                        stats.loaded_rules += 1;
                    }
                    Err(e) => {
                        stats.legacy_total += 1;
                        stats.legacy_unsupported_total += 1;
                        let reason = "unsupported_legacy_condition".to_string();
                        let reason_counter =
                            stats.skipped_by_reason.entry(reason.clone()).or_insert(0);
                        *reason_counter += 1;
                        stats.skipped_rules += 1;
                        stats.skipped_samples.push(RuleSkipSample {
                            path: path.display().to_string(),
                            reason,
                            detail: e,
                        });
                    }
                },
                Err(e) => {
                    let reason = classify_skip_reason(&e.to_string());
                    let reason_counter = stats.skipped_by_reason.entry(reason.clone()).or_insert(0);
                    *reason_counter += 1;
                    stats.skipped_rules += 1;
                    stats.skipped_samples.push(RuleSkipSample {
                        path: path.display().to_string(),
                        reason,
                        detail: e.to_string(),
                    });
                }
            }
        }

        // 按优先级排序（优先级高的在前）
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        print_load_summary(&stats);
        maybe_fail_in_strict_mode(&stats)?;

        Ok(Self {
            rules,
            load_stats: stats,
        })
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
                            if !matches_value_pattern(&event.value, pattern) {
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

#[derive(Debug, Deserialize)]
struct RulesManifest {
    version: u32,
    #[serde(default)]
    packs: Vec<ManifestPack>,
    #[serde(default)]
    legacy: ManifestLegacy,
}

#[derive(Debug, Deserialize)]
struct ManifestPack {
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    dir: String,
}

#[derive(Debug, Deserialize, Default)]
struct ManifestLegacy {
    #[serde(default = "default_true")]
    enabled: bool,
    #[allow(dead_code)]
    #[serde(default)]
    patterns: Vec<String>,
    #[allow(dead_code)]
    #[serde(default)]
    exclude: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn is_rule_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("yaml") | Some("yml")
    )
}

fn collect_rule_files(dir_path: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let manifest_path = dir_path.join("manifest.yaml");
    let mut files = Vec::new();
    let mut seen = HashSet::new();

    if manifest_path.exists() {
        let manifest_raw = fs::read_to_string(&manifest_path)
            .map_err(|e| format!("读取规则 manifest 失败 {}: {}", manifest_path.display(), e))?;
        let manifest: RulesManifest = serde_yaml::from_str(&manifest_raw)
            .map_err(|e| format!("解析规则 manifest 失败 {}: {}", manifest_path.display(), e))?;
        if manifest.version != 1 {
            return Err(format!(
                "规则 manifest version 不支持: {} (expected 1)",
                manifest.version
            ));
        }

        for pack in manifest.packs.into_iter().filter(|p| p.enabled) {
            let pack_dir = dir_path.join(&pack.dir);
            if !pack_dir.exists() {
                eprintln!(
                    "[rule-engine] pack 目录不存在，跳过: name={} dir={}",
                    pack.name, pack.dir
                );
                continue;
            }
            collect_rules_recursively(&pack_dir, &pack.name, &mut seen, &mut files)?;
        }

        if manifest.legacy.enabled {
            collect_legacy_rules(dir_path, &mut seen, &mut files)?;
        }
    } else {
        collect_legacy_rules(dir_path, &mut seen, &mut files)?;
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

fn collect_rules_recursively(
    root: &Path,
    pack: &str,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<(PathBuf, String)>,
) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).map_err(|e| format!("读取规则目录失败 {}: {}", dir.display(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if is_rule_file(&path) && seen.insert(path.clone()) {
                out.push((path, pack.to_string()));
            }
        }
    }
    Ok(())
}

fn collect_legacy_rules(
    dir_path: &Path,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<(PathBuf, String)>,
) -> Result<(), String> {
    let entries = fs::read_dir(dir_path).map_err(|e| format!("读取规则目录失败: {}", e))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
        let path = entry.path();
        if path.is_file()
            && is_rule_file(&path)
            && path.file_name().and_then(|s| s.to_str()) != Some("manifest.yaml")
            && seen.insert(path.clone())
        {
            out.push((path, "legacy".to_string()));
        }
    }

    let legacy_dir = dir_path.join("legacy");
    if legacy_dir.exists() {
        collect_rules_recursively(&legacy_dir, "legacy", seen, out)?;
    }
    Ok(())
}

/// 简单的通配符模式匹配（从 matcher.rs 复制）
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

impl RuleEngine {
    /// 获取规则数量
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// 获取规则加载统计
    pub fn load_stats(&self) -> &RuleLoadStats {
        &self.load_stats
    }
}

fn classify_skip_reason(err: &str) -> String {
    if err.contains("invalid type: map, expected a sequence") {
        return "unsupported_syntax".to_string();
    }
    if err.contains("missing field") {
        return "missing_field".to_string();
    }
    "deserialize_error".to_string()
}

fn print_load_summary(stats: &RuleLoadStats) {
    println!(
        "[rule-engine] loaded_rules={} skipped_rules={} legacy_total={} legacy_migratable={} legacy_unsupported={}",
        stats.loaded_rules,
        stats.skipped_rules,
        stats.legacy_total,
        stats.legacy_migratable_total,
        stats.legacy_unsupported_total
    );
    if stats.skipped_rules == 0 {
        return;
    }

    let mut reasons: Vec<_> = stats.skipped_by_reason.iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, count) in reasons {
        println!("[rule-engine] skipped reason={} count={}", reason, count);
    }

    for sample in stats.skipped_samples.iter().take(10) {
        eprintln!(
            "[rule-engine] skipped file={} reason={} detail={}",
            sample.path, sample.reason, sample.detail
        );
    }
}

fn maybe_fail_in_strict_mode(stats: &RuleLoadStats) -> Result<(), String> {
    let strict = std::env::var("ARK_RULES_STRICT")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if strict && stats.skipped_rules > 0 {
        return Err(format!(
            "严格模式启用 (ARK_RULES_STRICT=1)，规则加载跳过 {} 条，请修复后重试",
            stats.skipped_rules
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::RuleEngine;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn base_rule(scene: &str) -> String {
        format!(
            r#"
name: "{scene}"
scene: "{scene}"
priority: 10
conditions:
  - type: "event"
    event_type: "transport.drop"
root_cause_pattern:
  primary: "test"
solution_steps:
  - step: 1
    action: "noop"
    manual: true
related_evidences: ["transport.drop"]
applicability:
  min_confidence: 0.5
"#
        )
    }

    fn legacy_rule(scene: &str) -> String {
        format!(
            r#"
name: "{scene}"
scene: "{scene}"
priority: 10
conditions:
  all:
    - type: "event"
      event_type: "transport.drop"
      value_threshold: 10
root_cause_pattern:
  primary: "legacy"
solution_steps:
  - step: 1
    action: "noop"
    manual: true
related_evidences: ["transport.drop"]
applicability:
  min_confidence: 0.5
"#
        )
    }

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

    #[test]
    fn load_from_dir_supports_manifest_pack_and_legacy() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ark-rule-manifest-test-{}", uniq));
        fs::create_dir_all(dir.join("core")).expect("create core dir");
        fs::create_dir_all(dir.join("legacy")).expect("create legacy dir");

        fs::write(
            dir.join("manifest.yaml"),
            r#"
version: 1
packs:
  - name: core
    enabled: true
    dir: core
legacy:
  enabled: true
"#,
        )
        .expect("write manifest");

        fs::write(
            dir.join("core").join("core-rule.yaml"),
            base_rule("scene_core"),
        )
        .expect("write core rule");
        fs::write(
            dir.join("legacy").join("legacy-rule.yaml"),
            base_rule("scene_legacy"),
        )
        .expect("write legacy rule");

        let engine = RuleEngine::load_from_dir(&dir).expect("load rules");
        assert_eq!(engine.rule_count(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_from_dir_normalizes_legacy_conditions() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ark-rule-legacy-test-{}", uniq));
        fs::create_dir_all(&dir).expect("create dir");

        fs::write(dir.join("legacy-style.yaml"), legacy_rule("legacy_scene"))
            .expect("write legacy rule");
        let engine = RuleEngine::load_from_dir(&dir).expect("load rules");
        assert_eq!(engine.rule_count(), 1);
        assert_eq!(engine.load_stats().legacy_total, 1);
        assert_eq!(engine.load_stats().legacy_migratable_total, 1);
        assert_eq!(engine.load_stats().legacy_unsupported_total, 0);

        let _ = fs::remove_dir_all(&dir);
    }
}
