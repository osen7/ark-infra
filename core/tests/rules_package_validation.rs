use serde_yaml::{Mapping, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

fn expect_mapping<'a>(value: &'a Value, ctx: &str) -> Result<&'a Mapping, String> {
    value
        .as_mapping()
        .ok_or_else(|| format!("{} 必须是 YAML 对象", ctx))
}

fn expect_string<'a>(map: &'a Mapping, key: &str, ctx: &str) -> Result<&'a str, String> {
    let k = Value::String(key.to_string());
    let value = map
        .get(&k)
        .ok_or_else(|| format!("{} 缺少字段 `{}`", ctx, key))?;
    let s = value
        .as_str()
        .ok_or_else(|| format!("{} 字段 `{}` 必须是字符串", ctx, key))?;
    if s.trim().is_empty() {
        return Err(format!("{} 字段 `{}` 不能为空", ctx, key));
    }
    Ok(s)
}

fn expect_u32(map: &Mapping, key: &str, ctx: &str) -> Result<u32, String> {
    let k = Value::String(key.to_string());
    let value = map
        .get(&k)
        .ok_or_else(|| format!("{} 缺少字段 `{}`", ctx, key))?;
    let n = value
        .as_u64()
        .ok_or_else(|| format!("{} 字段 `{}` 必须是非负整数", ctx, key))?;
    u32::try_from(n).map_err(|_| format!("{} 字段 `{}` 超出 u32 范围", ctx, key))
}

fn validate_condition(cond: &Value, ctx: &str) -> Result<(), String> {
    let map = expect_mapping(cond, ctx)?;

    if let Some(t) = map.get(&Value::String("type".to_string())) {
        let ty = t
            .as_str()
            .ok_or_else(|| format!("{} 字段 `type` 必须是字符串", ctx))?;
        match ty {
            "event" => {
                expect_string(map, "event_type", ctx)?;
                Ok(())
            }
            "graph" => {
                expect_string(map, "edge_type", ctx)?;
                Ok(())
            }
            "metric" => {
                let metrics = map
                    .get(&Value::String("metrics".to_string()))
                    .ok_or_else(|| format!("{} metric 条件缺少 `metrics`", ctx))?
                    .as_sequence()
                    .ok_or_else(|| format!("{} 字段 `metrics` 必须是数组", ctx))?;
                if metrics.is_empty() {
                    return Err(format!("{} 字段 `metrics` 不能为空数组", ctx));
                }
                Ok(())
            }
            "signal" => {
                expect_string(map, "signal", ctx)?;
                expect_string(map, "target", ctx)?;
                let op = map
                    .get(Value::String("op".to_string()))
                    .ok_or_else(|| format!("{} signal 条件缺少 `op`", ctx))?
                    .as_str()
                    .ok_or_else(|| format!("{} signal 字段 `op` 必须是字符串", ctx))?;
                match op {
                    "gt" | "lt" | "eq" | "gte" | "lte" | "ne" | "contains" => Ok(()),
                    _ => Err(format!("{} signal 字段 `op` 非法: {}", ctx, op)),
                }
            }
            "all" | "any" => {
                let nested = map
                    .get(&Value::String("conditions".to_string()))
                    .ok_or_else(|| format!("{} {} 条件缺少 `conditions`", ctx, ty))?
                    .as_sequence()
                    .ok_or_else(|| format!("{} 字段 `conditions` 必须是数组", ctx))?;
                if nested.is_empty() {
                    return Err(format!("{} 字段 `conditions` 不能为空数组", ctx));
                }
                for (idx, item) in nested.iter().enumerate() {
                    validate_condition(item, &format!("{} -> conditions[{}]", ctx, idx))?;
                }
                Ok(())
            }
            _ => Err(format!("{} 字段 `type` 非法: {}", ctx, ty)),
        }
    } else if let Some(all) = map.get(&Value::String("all".to_string())) {
        let seq = all
            .as_sequence()
            .ok_or_else(|| format!("{} 字段 `all` 必须是数组", ctx))?;
        if seq.is_empty() {
            return Err(format!("{} 字段 `all` 不能为空数组", ctx));
        }
        for (idx, item) in seq.iter().enumerate() {
            validate_condition(item, &format!("{} -> all[{}]", ctx, idx))?;
        }
        Ok(())
    } else if let Some(any) = map.get(&Value::String("any".to_string())) {
        let seq = any
            .as_sequence()
            .ok_or_else(|| format!("{} 字段 `any` 必须是数组", ctx))?;
        if seq.is_empty() {
            return Err(format!("{} 字段 `any` 不能为空数组", ctx));
        }
        for (idx, item) in seq.iter().enumerate() {
            validate_condition(item, &format!("{} -> any[{}]", ctx, idx))?;
        }
        Ok(())
    } else {
        Err(format!(
            "{} 条件结构非法，必须包含 `type`，或使用 `all`/`any` 聚合写法",
            ctx
        ))
    }
}

fn validate_conditions(root: &Value, ctx: &str) -> Result<(), String> {
    if let Some(seq) = root.as_sequence() {
        if seq.is_empty() {
            return Err(format!("{} 不能为空数组", ctx));
        }
        for (idx, cond) in seq.iter().enumerate() {
            validate_condition(cond, &format!("{}[{}]", ctx, idx))?;
        }
        return Ok(());
    }

    if root.is_mapping() {
        return validate_condition(root, ctx);
    }

    Err(format!("{} 必须是数组或对象", ctx))
}

fn validate_rule(path: &Path, value: &Value) -> Result<(String, u32), String> {
    let ctx = format!("规则文件 {}", path.display());
    let map = expect_mapping(value, &ctx)?;

    let name = expect_string(map, "name", &ctx)?;
    let scene = expect_string(map, "scene", &ctx)?.to_string();
    let priority = expect_u32(map, "priority", &ctx)?;
    if !(1..=100).contains(&priority) {
        return Err(format!(
            "{}: scene={}: priority={} 超出范围 (expected 1..=100)。建议调整 priority 字段",
            ctx, scene, priority
        ));
    }

    let conditions = map
        .get(&Value::String("conditions".to_string()))
        .ok_or_else(|| format!("{} 缺少字段 `conditions`", ctx))?;
    validate_conditions(conditions, &format!("{}: conditions", ctx))?;

    if let Some(reason_codes) = map.get(&Value::String("reason_codes".to_string())) {
        let seq = reason_codes
            .as_sequence()
            .ok_or_else(|| format!("{}: reason_codes 必须是数组", ctx))?;
        for (idx, code) in seq.iter().enumerate() {
            let s = code
                .as_str()
                .ok_or_else(|| format!("{}: reason_codes[{}] 必须是字符串", ctx, idx))?;
            if s.trim().is_empty() {
                return Err(format!("{}: reason_codes[{}] 不能为空", ctx, idx));
            }
        }
    }

    let root_cause = map
        .get(&Value::String("root_cause_pattern".to_string()))
        .ok_or_else(|| format!("{} 缺少字段 `root_cause_pattern`", ctx))?;
    let root_cause_map = expect_mapping(root_cause, &ctx)?;
    expect_string(
        root_cause_map,
        "primary",
        &format!("{}: root_cause_pattern", ctx),
    )?;

    let steps = map
        .get(&Value::String("solution_steps".to_string()))
        .ok_or_else(|| format!("{} 缺少字段 `solution_steps`", ctx))?
        .as_sequence()
        .ok_or_else(|| format!("{} 字段 `solution_steps` 必须是数组", ctx))?;
    if steps.is_empty() {
        return Err(format!("{} 字段 `solution_steps` 不能为空数组", ctx));
    }
    let mut seen_steps = HashSet::new();
    for (idx, step) in steps.iter().enumerate() {
        let step_map = expect_mapping(step, &format!("{}: solution_steps[{}]", ctx, idx))?;
        let step_no = expect_u32(
            step_map,
            "step",
            &format!("{}: solution_steps[{}]", ctx, idx),
        )?;
        if step_no == 0 {
            return Err(format!("{}: solution_steps[{}].step 不能为 0", ctx, idx));
        }
        if !seen_steps.insert(step_no) {
            return Err(format!(
                "{}: scene={}: solution_steps.step 重复: {}。建议保证 step 唯一且递增",
                ctx, scene, step_no
            ));
        }
        let _ = expect_string(
            step_map,
            "action",
            &format!("{}: solution_steps[{}]", ctx, idx),
        )?;
        if let Some(manual) = step_map.get(&Value::String("manual".to_string())) {
            if !manual.is_bool() {
                return Err(format!(
                    "{}: solution_steps[{}].manual 必须是布尔值",
                    ctx, idx
                ));
            }
        }
        if let Some(cmd) = step_map.get(&Value::String("command".to_string())) {
            if !cmd.is_null() && cmd.as_str().is_none() {
                return Err(format!(
                    "{}: solution_steps[{}].command 必须是字符串或 null",
                    ctx, idx
                ));
            }
        }
    }

    let evidences = map
        .get(&Value::String("related_evidences".to_string()))
        .ok_or_else(|| format!("{} 缺少字段 `related_evidences`", ctx))?
        .as_sequence()
        .ok_or_else(|| format!("{} 字段 `related_evidences` 必须是数组", ctx))?;
    if evidences.is_empty() {
        return Err(format!("{} 字段 `related_evidences` 不能为空数组", ctx));
    }
    for (idx, item) in evidences.iter().enumerate() {
        if item.as_str().is_none() {
            return Err(format!("{}: related_evidences[{}] 必须是字符串", ctx, idx));
        }
    }

    let applicability = map
        .get(&Value::String("applicability".to_string()))
        .ok_or_else(|| format!("{} 缺少字段 `applicability`", ctx))?;
    let app_map = expect_mapping(applicability, &ctx)?;
    let min_conf = app_map
        .get(&Value::String("min_confidence".to_string()))
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{}: applicability.min_confidence 必须是数字", ctx))?;
    if !(0.0..=1.0).contains(&min_conf) {
        return Err(format!(
            "{}: applicability.min_confidence 必须在 [0,1] 区间",
            ctx
        ));
    }
    if let Some(required_events) = app_map.get(&Value::String("required_events".to_string())) {
        let seq = required_events
            .as_sequence()
            .ok_or_else(|| format!("{}: applicability.required_events 必须是数组", ctx))?;
        for (idx, event) in seq.iter().enumerate() {
            if event.as_str().is_none() {
                return Err(format!(
                    "{}: applicability.required_events[{}] 必须是字符串",
                    ctx, idx
                ));
            }
        }
    }

    if name.trim().is_empty() {
        return Err(format!("{}: name 不能为空", ctx));
    }

    Ok((scene, priority))
}

fn collect_rule_files(rules_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut stack = vec![rules_dir.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).map_err(|e| format!("读取规则目录失败 {}: {}", dir.display(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("读取规则目录项失败: {}", e))?;
            let path = entry.path();
            if path.is_dir() {
                let rel = path
                    .strip_prefix(rules_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                if rel.starts_with("fixtures") {
                    continue;
                }
                stack.push(path);
                continue;
            }

            let is_rule = matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("yaml") | Some("yml")
            );
            if !is_rule {
                continue;
            }
            if path.file_name().and_then(|s| s.to_str()) == Some("manifest.yaml") {
                continue;
            }
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

#[test]
fn validate_rules_package() {
    let rules_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rules");
    let files = collect_rule_files(&rules_dir).expect("读取规则文件列表");
    assert!(
        !files.is_empty(),
        "规则目录为空，至少应包含一个 rules/*.yaml 文件"
    );

    let mut scenes: HashSet<String> = HashSet::new();
    let mut scene_priority_owner: HashMap<(String, u32), String> = HashMap::new();
    let mut errors = Vec::new();

    for path in &files {
        let content = match fs::read_to_string(path) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("读取规则文件失败 {}: {}", path.display(), e));
                continue;
            }
        };

        let value: Value = match serde_yaml::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("YAML 解析失败 {}: {}", path.display(), e));
                continue;
            }
        };

        let (scene, priority) = match validate_rule(path, &value) {
            Ok(v) => v,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };

        if !scenes.insert(scene.clone()) {
            errors.push(format!(
                "scene 重复: `{}` (文件: {})",
                scene,
                path.display()
            ));
        }

        let key = (scene.clone(), priority);
        if let Some(owner) = scene_priority_owner.insert(key, path.display().to_string()) {
            errors.push(format!(
                "scene/priority 冲突: scene=`{}`, priority={}，文件冲突: {} <-> {}",
                scene,
                priority,
                owner,
                path.display()
            ));
        }
    }

    if !errors.is_empty() {
        panic!("规则包校验失败:\n- {}", errors.join("\n- "));
    }
}
