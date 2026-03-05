use serde_yaml::{Mapping, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
struct Cli {
    input: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    in_place: bool,
    dry_run: bool,
    filter_pack: Option<String>,
    strict: bool,
}

#[derive(Debug, Default)]
struct Summary {
    migrated_rules: usize,
    partial_rules: usize,
    failed_rules: usize,
    unchanged_rules: usize,
    unsupported_ops: HashMap<String, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrateStatus {
    Migrated,
    Partial,
    Unchanged,
}

#[derive(Debug)]
struct MigrateResult {
    status: MigrateStatus,
    unsupported_ops: Vec<String>,
}

fn usage() -> String {
    "Usage: rules-migrate <INPUT> [--out-dir <DIR>] [--in-place] [--dry-run] [--filter-pack <pack>] [--strict]"
        .to_string()
}

fn parse_args() -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out-dir" => {
                let dir = args
                    .next()
                    .ok_or_else(|| "--out-dir requires value".to_string())?;
                cli.out_dir = Some(PathBuf::from(dir));
            }
            "--in-place" => cli.in_place = true,
            "--dry-run" => cli.dry_run = true,
            "--filter-pack" => {
                let pack = args
                    .next()
                    .ok_or_else(|| "--filter-pack requires value".to_string())?;
                cli.filter_pack = Some(pack);
            }
            "--strict" => cli.strict = true,
            "-h" | "--help" => return Err(usage()),
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option: {}\n{}", arg, usage()))
            }
            _ => {
                if cli.input.is_some() {
                    return Err(format!("multiple input paths provided\n{}", usage()));
                }
                cli.input = Some(PathBuf::from(arg));
            }
        }
    }

    if cli.input.is_none() {
        return Err(usage());
    }
    if cli.in_place && cli.out_dir.is_some() {
        return Err("--in-place cannot be used with --out-dir".to_string());
    }
    Ok(cli)
}

fn is_yaml_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("yaml") | Some("yml")
    )
}

fn collect_files(input: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if input.is_file() {
        if is_yaml_file(input) {
            files.push(input.to_path_buf());
        }
        return Ok(files);
    }

    if !input.is_dir() {
        return Err(format!("input does not exist: {}", input.display()));
    }

    let mut stack = vec![input.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).map_err(|e| format!("read_dir failed {}: {}", dir.display(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("read_dir entry failed: {}", e))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !is_yaml_file(&path) {
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

fn matches_pack_filter(path: &Path, filter_pack: &Option<String>) -> bool {
    let Some(pack) = filter_pack else {
        return true;
    };
    let needle = format!("/rules/{}/", pack);
    let normalized = path.to_string_lossy().replace('\\', "/");
    normalized.contains(&needle) || normalized.ends_with(&format!("/rules/{}", pack))
}

fn infer_pack(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    for i in 0..parts.len() {
        if parts[i] == "rules" && i + 1 < parts.len() {
            let p = parts[i + 1].as_str();
            if p == "core" || p == "nvidia" || p == "ascend" || p == "k8s" || p == "legacy" {
                return p.to_string();
            }
        }
    }
    "legacy".to_string()
}

fn key(s: &str) -> Value {
    Value::String(s.to_string())
}

fn mapping_mut<'a>(v: &'a mut Value, ctx: &str) -> Result<&'a mut Mapping, String> {
    v.as_mapping_mut()
        .ok_or_else(|| format!("{} must be YAML mapping", ctx))
}

fn supports_condition(cond: &Value, unsupported: &mut Vec<String>) -> bool {
    let Some(map) = cond.as_mapping() else {
        unsupported.push("condition_not_mapping".to_string());
        return false;
    };

    let Some(t) = map.get(key("type")) else {
        unsupported.push("condition_missing_type".to_string());
        return false;
    };
    let Some(ty) = t.as_str() else {
        unsupported.push("condition_type_not_string".to_string());
        return false;
    };

    match ty {
        "event" | "graph" | "metric" => true,
        "all" | "any" => {
            let Some(nested) = map.get(key("conditions")).and_then(Value::as_sequence) else {
                unsupported.push(format!("{}_missing_conditions", ty));
                return false;
            };
            let mut ok = true;
            for c in nested {
                if !supports_condition(c, unsupported) {
                    ok = false;
                }
            }
            ok
        }
        _ => {
            unsupported.push(format!("unsupported_type_{}", ty));
            false
        }
    }
}

fn ensure_rule_id(map: &mut Mapping, pack: &str) {
    if map.get(key("id")).is_some() {
        return;
    }
    let scene = map
        .get(key("scene"))
        .and_then(Value::as_str)
        .unwrap_or("unknown_scene");
    map.insert(key("id"), Value::String(format!("{}.{}", pack, scene)));
}

fn migrate_rule_value(path: &Path, value: &mut Value) -> Result<MigrateResult, String> {
    let map = mapping_mut(value, "rule")?;
    ensure_rule_id(map, &infer_pack(path));

    let Some(conditions) = map.get(key("conditions")).cloned() else {
        return Ok(MigrateResult {
            status: MigrateStatus::Unchanged,
            unsupported_ops: Vec::new(),
        });
    };

    let Some(legacy_map) = conditions.as_mapping() else {
        return Ok(MigrateResult {
            status: MigrateStatus::Unchanged,
            unsupported_ops: Vec::new(),
        });
    };

    let all = legacy_map
        .get(key("all"))
        .and_then(Value::as_sequence)
        .cloned();
    let any = legacy_map
        .get(key("any"))
        .and_then(Value::as_sequence)
        .cloned();

    let mut unsupported = Vec::new();
    let mut notes = Vec::new();

    let new_conditions = match (all, any) {
        (Some(all_seq), None) => {
            let mut ok = true;
            for c in &all_seq {
                if !supports_condition(c, &mut unsupported) {
                    ok = false;
                }
            }
            if ok {
                Value::Sequence(all_seq)
            } else {
                notes.push("legacy all-conditions contains unsupported condition".to_string());
                Value::Null
            }
        }
        (None, Some(any_seq)) => {
            let mut ok = true;
            for c in &any_seq {
                if !supports_condition(c, &mut unsupported) {
                    ok = false;
                }
            }
            if ok {
                let mut any_wrapper = Mapping::new();
                any_wrapper.insert(key("type"), Value::String("any".to_string()));
                any_wrapper.insert(key("conditions"), Value::Sequence(any_seq));
                Value::Sequence(vec![Value::Mapping(any_wrapper)])
            } else {
                notes.push("legacy any-conditions contains unsupported condition".to_string());
                Value::Null
            }
        }
        (Some(_), Some(_)) => {
            notes.push("legacy conditions contains both all and any".to_string());
            unsupported.push("legacy_all_and_any".to_string());
            Value::Null
        }
        (None, None) => {
            return Ok(MigrateResult {
                status: MigrateStatus::Unchanged,
                unsupported_ops: Vec::new(),
            });
        }
    };

    if new_conditions.is_null() {
        map.insert(key("migration_incomplete"), Value::Bool(true));
        map.insert(
            key("migration_notes"),
            Value::Sequence(notes.iter().map(|n| Value::String(n.clone())).collect()),
        );
        map.insert(key("migration_legacy_conditions"), conditions);
        return Ok(MigrateResult {
            status: MigrateStatus::Partial,
            unsupported_ops: unsupported,
        });
    }

    map.insert(key("conditions"), new_conditions);
    map.insert(key("migration_incomplete"), Value::Bool(false));
    map.insert(
        key("migration_notes"),
        Value::Sequence(vec![Value::String(
            "migrated legacy conditions (all/any) to normalized syntax".to_string(),
        )]),
    );

    Ok(MigrateResult {
        status: MigrateStatus::Migrated,
        unsupported_ops: unsupported,
    })
}

fn output_path(cli: &Cli, input_base: &Path, file: &Path) -> Result<PathBuf, String> {
    if cli.in_place {
        return Ok(file.to_path_buf());
    }

    if let Some(ref out_dir) = cli.out_dir {
        let rel = file.strip_prefix(input_base).unwrap_or(file);
        let mut out = out_dir.join(rel);
        if out.extension().is_none() {
            out.set_extension("yaml");
        }
        return Ok(out);
    }

    let mut out = file.to_path_buf();
    let stem = out
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid file name: {}", out.display()))?;
    out.set_file_name(format!("{}.migrated.yaml", stem));
    Ok(out)
}

fn write_migrated(path: &Path, value: &Value, status: MigrateStatus) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create_dir_all failed {}: {}", parent.display(), e))?;
    }
    let body = serde_yaml::to_string(value)
        .map_err(|e| format!("serialize yaml failed {}: {}", path.display(), e))?;
    let mut content = String::new();
    content.push_str("# Migrated from legacy conditions (all/any) by rules-migrate\n");
    if status == MigrateStatus::Partial {
        content.push_str("# WARNING: migration incomplete, see migration_notes\n");
    }
    content.push_str(&body);
    fs::write(path, content).map_err(|e| format!("write failed {}: {}", path.display(), e))
}

fn run(cli: Cli) -> Result<Summary, String> {
    let input = cli.input.clone().expect("input set");
    let files = collect_files(&input)?;
    let input_base = if input.is_dir() {
        input.clone()
    } else {
        input.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    let mut summary = Summary::default();

    for file in files {
        if !matches_pack_filter(&file, &cli.filter_pack) {
            continue;
        }

        let raw = fs::read_to_string(&file)
            .map_err(|e| format!("read failed {}: {}", file.display(), e))?;
        let mut value: Value = match serde_yaml::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[rules-migrate] failed parse {}: {}", file.display(), e);
                summary.failed_rules += 1;
                continue;
            }
        };

        let result = match migrate_rule_value(&file, &mut value) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[rules-migrate] failed migrate {}: {}", file.display(), e);
                summary.failed_rules += 1;
                continue;
            }
        };

        for op in result.unsupported_ops {
            *summary.unsupported_ops.entry(op).or_insert(0) += 1;
        }

        match result.status {
            MigrateStatus::Unchanged => {
                summary.unchanged_rules += 1;
                println!("[rules-migrate] unchanged {}", file.display());
            }
            MigrateStatus::Migrated => {
                summary.migrated_rules += 1;
                println!("[rules-migrate] migrated {}", file.display());
            }
            MigrateStatus::Partial => {
                summary.partial_rules += 1;
                println!("[rules-migrate] partial {}", file.display());
            }
        }

        if cli.dry_run {
            continue;
        }

        let out = output_path(&cli, &input_base, &file)?;
        write_migrated(&out, &value, result.status)?;
    }

    Ok(summary)
}

fn print_summary(summary: &Summary) {
    println!(
        "[rules-migrate] summary migrated_rules={} partial_rules={} failed_rules={} unchanged_rules={}",
        summary.migrated_rules, summary.partial_rules, summary.failed_rules, summary.unchanged_rules
    );
    if !summary.unsupported_ops.is_empty() {
        let mut pairs: Vec<_> = summary.unsupported_ops.iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(a.1));
        println!("[rules-migrate] unsupported_ops_top:");
        for (op, count) in pairs.into_iter().take(10) {
            println!("  - {}: {}", op, count);
        }
    }
}

fn main() {
    let cli = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    };

    let strict = cli.strict;
    match run(cli) {
        Ok(summary) => {
            print_summary(&summary);
            if strict && (summary.partial_rules > 0 || summary.failed_rules > 0) {
                eprintln!(
                    "[rules-migrate] strict mode failed: partial={} failed={}",
                    summary.partial_rules, summary.failed_rules
                );
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("[rules-migrate] {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_legacy_all_to_new_conditions() {
        let yaml = r#"
name: "legacy"
scene: "legacy_scene"
priority: 1
conditions:
  all:
    - type: "event"
      event_type: "transport.drop"
root_cause_pattern:
  primary: "x"
solution_steps:
  - step: 1
    action: "noop"
related_evidences: ["transport.drop"]
applicability:
  min_confidence: 0.5
"#;
        let mut v: Value = serde_yaml::from_str(yaml).expect("parse");
        let result =
            migrate_rule_value(Path::new("rules/legacy/test.yaml"), &mut v).expect("migrate");
        assert_eq!(result.status, MigrateStatus::Migrated);
        let map = v.as_mapping().expect("map");
        assert!(map
            .get(key("conditions"))
            .expect("conditions")
            .is_sequence());
        assert_eq!(
            map.get(key("migration_incomplete"))
                .and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn migrate_partial_with_unsupported_condition() {
        let yaml = r#"
name: "legacy"
scene: "legacy_scene"
priority: 1
conditions:
  all:
    - op: "weird"
root_cause_pattern:
  primary: "x"
solution_steps:
  - step: 1
    action: "noop"
related_evidences: ["transport.drop"]
applicability:
  min_confidence: 0.5
"#;
        let mut v: Value = serde_yaml::from_str(yaml).expect("parse");
        let result =
            migrate_rule_value(Path::new("rules/legacy/test.yaml"), &mut v).expect("migrate");
        assert_eq!(result.status, MigrateStatus::Partial);
        let map = v.as_mapping().expect("map");
        assert_eq!(
            map.get(key("migration_incomplete"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(map.get(key("migration_notes")).is_some());
        assert!(map.get(key("migration_legacy_conditions")).is_some());
    }
}
