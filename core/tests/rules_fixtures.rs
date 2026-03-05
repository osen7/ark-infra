use ark_core::event::Event;
use ark_core::graph::StateGraph;
use ark_core::rules::RuleEngine;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Default)]
struct ExpectedAction {
    name: String,
    mode: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FixtureExpected {
    #[serde(default)]
    expect_scenes: Vec<String>,
    #[serde(default)]
    expect_reason_codes: Vec<String>,
    #[serde(default)]
    expect_actions: Vec<ExpectedAction>,
    #[serde(default)]
    expect_evidence_contains: Vec<String>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn fixture_dirs(root: &Path) -> Vec<PathBuf> {
    let fixtures_root = root.join("rules/fixtures");
    let entries = match fs::read_dir(&fixtures_root) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut dirs = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}

fn parse_event_line(line: &str, ts_fallback: u64) -> Result<Event, String> {
    let mut value: Value =
        serde_json::from_str(line).map_err(|e| format!("解析 JSONL 行失败: {}", e))?;
    if value.get("ts").is_none() {
        value["ts"] = Value::Number(ts_fallback.into());
    }
    serde_json::from_value(value).map_err(|e| format!("解析 Event 失败: {}", e))
}

fn load_fixture_events(input_path: &Path) -> Result<Vec<Event>, String> {
    let content = fs::read_to_string(input_path)
        .map_err(|e| format!("读取 fixture 输入失败 {}: {}", input_path.display(), e))?;
    let mut events = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event = parse_event_line(line, (idx as u64) + 1)
            .map_err(|e| format!("{} (line={})", e, idx + 1))?;
        events.push(event);
    }
    Ok(events)
}

#[tokio::test]
async fn run_rules_fixtures_contract() {
    let root = repo_root();
    let dirs = fixture_dirs(&root);
    assert!(
        !dirs.is_empty(),
        "未找到 fixtures，请至少提供 rules/fixtures/*"
    );

    let rule_engine = RuleEngine::load_from_dir(root.join("rules")).expect("load rule engine");
    for dir in dirs {
        let graph = StateGraph::new();
        let input = dir.join("input_events.jsonl");
        let expected_path = dir.join("expected.json");

        let events = load_fixture_events(&input).expect("load fixture events");
        let expected: FixtureExpected =
            serde_json::from_str(&fs::read_to_string(&expected_path).expect("read expected.json"))
                .expect("parse expected.json");

        for event in &events {
            graph.process_event(event).await.unwrap_or_else(|e| {
                panic!("fixture {} process_event failed: {}", dir.display(), e)
            });
        }

        let matched = rule_engine.match_rules(&graph, &events).await;
        let scenes: HashSet<String> = matched.iter().map(|r| r.scene.clone()).collect();

        for expected_scene in &expected.expect_scenes {
            assert!(
                scenes.contains(expected_scene),
                "fixture {}: missing expected scene `{}`; got {:?}",
                dir.display(),
                expected_scene,
                scenes
            );
        }

        if !expected.expect_reason_codes.is_empty() {
            let reason_codes: HashSet<String> = matched
                .iter()
                .flat_map(|r| r.reason_codes.iter().cloned())
                .collect();
            for code in &expected.expect_reason_codes {
                assert!(
                    reason_codes.contains(code),
                    "fixture {}: missing expected reason_code `{}`; got {:?}",
                    dir.display(),
                    code,
                    reason_codes
                );
            }
        }

        // P1: keep structure stable; validate only if expected fields are provided.
        if !expected.expect_actions.is_empty() {
            let actions: HashSet<String> = matched
                .iter()
                .flat_map(|r| r.solution_steps.iter().map(|s| s.action.clone()))
                .collect();
            for action in &expected.expect_actions {
                assert!(
                    actions.contains(&action.name),
                    "fixture {}: missing expected action `{}`",
                    dir.display(),
                    action.name
                );
                let _ = &action.mode;
            }
        }

        if !expected.expect_evidence_contains.is_empty() {
            let evidences: Vec<String> = matched
                .iter()
                .flat_map(|r| r.related_evidences.iter().cloned())
                .collect();
            for expected_evidence in &expected.expect_evidence_contains {
                assert!(
                    evidences.iter().any(|e| e.contains(expected_evidence)),
                    "fixture {}: expected evidence fragment `{}` not found in {:?}",
                    dir.display(),
                    expected_evidence,
                    evidences
                );
            }
        }
    }
}
