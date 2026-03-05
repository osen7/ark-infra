use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const CATALOG_PACKS: &[&str] = &[
    "hardware",
    "interconnect",
    "network",
    "runtime",
    "scheduler",
    "storage",
    "cluster",
];
const SHARED_REASON_CODES_LOG_LIMIT: usize = 20;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn catalog_rule_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for pack in CATALOG_PACKS {
        let dir = root.join("rules").join(pack);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|s| s.to_str());
            if matches!(ext, Some("yaml") | Some("yml")) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn fixture_expected_files(root: &Path) -> Vec<PathBuf> {
    let fixtures_dir = root.join("rules/fixtures");
    let Ok(entries) = fs::read_dir(&fixtures_dir) else {
        return Vec::new();
    };

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }
        let Some(name) = case_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("catalog_") {
            continue;
        }
        let expected = case_dir.join("expected.json");
        if expected.exists() {
            files.push(expected);
        }
    }
    files.sort();
    files
}

fn parse_yaml_map(path: &Path) -> serde_yaml::Mapping {
    let content = fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("failed to read {}: {}", path.display(), e);
    });
    let value: YamlValue = serde_yaml::from_str(&content).unwrap_or_else(|e| {
        panic!("failed to parse {}: {}", path.display(), e);
    });
    value
        .as_mapping()
        .unwrap_or_else(|| panic!("rule {} must be a YAML mapping", path.display()))
        .clone()
}

fn id_is_valid(id: &str) -> bool {
    let mut parts = id.split('.');
    let Some(prefix) = parts.next() else {
        return false;
    };
    if !CATALOG_PACKS.iter().any(|p| p == &prefix) {
        return false;
    }
    let segments: Vec<&str> = parts.collect();
    if segments.is_empty() {
        return false;
    }
    segments.iter().all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    })
}

fn scene_matches_id(scene: &str, id: &str) -> bool {
    if scene == id {
        return true;
    }
    if let Some((base, version)) = id.rsplit_once(".v") {
        if !base.is_empty()
            && !version.is_empty()
            && version.chars().all(|c| c.is_ascii_digit())
        {
            return scene == base;
        }
    }
    false
}

#[test]
fn catalog_rules_lint() {
    let root = repo_root();
    let rules = catalog_rule_files(&root);
    assert!(
        !rules.is_empty(),
        "catalog rules not found under rules/<catalog-pack>/"
    );

    let mut scene_owner = BTreeMap::new();
    let mut reason_code_owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut scenes = BTreeSet::new();

    for rule_path in &rules {
        let map = parse_yaml_map(rule_path);
        let id = map
            .get(YamlValue::String("id".to_string()))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{} missing `id`", rule_path.display()));
        assert!(
            id_is_valid(id),
            "{} invalid id `{}`; expected <layer>.<name>[.<subname>...] with [a-z0-9_]",
            rule_path.display(),
            id
        );

        let scene = map
            .get(YamlValue::String("scene".to_string()))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{} missing `scene`", rule_path.display()));
        assert!(
            scene_matches_id(scene, id),
            "{} scene/id mismatch: scene=`{}`, id=`{}`",
            rule_path.display(),
            scene,
            id
        );

        if let Some(owner) = scene_owner.insert(scene.to_string(), rule_path.display().to_string()) {
            panic!(
                "duplicate scene `{}` across catalog packs: {} <-> {}",
                scene,
                owner,
                rule_path.display()
            );
        }
        scenes.insert(scene.to_string());

        if let Some(reason_codes) = map
            .get(YamlValue::String("reason_codes".to_string()))
            .and_then(|v| v.as_sequence())
        {
            for code in reason_codes {
                if let Some(code) = code.as_str() {
                    reason_code_owners
                        .entry(code.to_string())
                        .or_default()
                        .insert(scene.to_string());
                }
            }
        }
    }

    let shared_reason_codes: Vec<_> = reason_code_owners
        .iter()
        .filter(|(_, owners)| owners.len() > 1)
        .collect();
    if !shared_reason_codes.is_empty() {
        eprintln!("[catalog-lint] shared reason codes (allowed):");
        let total = shared_reason_codes.len();
        for (code, owners) in shared_reason_codes
            .iter()
            .take(SHARED_REASON_CODES_LOG_LIMIT)
        {
            eprintln!("  - {} => {:?}", code, owners);
        }
        if total > SHARED_REASON_CODES_LOG_LIMIT {
            eprintln!(
                "  ... and {} more",
                total - SHARED_REASON_CODES_LOG_LIMIT
            );
        }
    }

    let fixture_files = fixture_expected_files(&root);
    assert!(
        !fixture_files.is_empty(),
        "catalog fixture expected.json not found under rules/fixtures/catalog_*"
    );
    let mut fixture_scenes = BTreeSet::new();
    for path in fixture_files {
        let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("failed to read {}: {}", path.display(), e);
        });
        let expected: JsonValue = serde_json::from_str(&raw).unwrap_or_else(|e| {
            panic!("failed to parse {}: {}", path.display(), e);
        });
        let scenes_json = expected
            .get("expect_scenes")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{} missing non-empty `expect_scenes`", path.display()));
        for scene in scenes_json {
            if let Some(scene) = scene.as_str() {
                fixture_scenes.insert(scene.to_string());
            }
        }
    }

    for scene in scenes {
        assert!(
            fixture_scenes.contains(&scene),
            "catalog scene `{}` has no catalog fixture coverage",
            scene
        );
    }
}
