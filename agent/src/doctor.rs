use crate::ipc::IpcClient;
use ark_core::rules::{RuleEngine, RuleWire};
use serde::Serialize;
use serde_yaml::Value as YamlValue;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_tungstenite::connect_async;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckItem {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorSection {
    pub name: String,
    pub checks: Vec<CheckItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorSummary {
    pub ok: usize,
    pub warn: usize,
    pub fail: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub environment: String,
    pub sections: Vec<DoctorSection>,
    pub summary: DoctorSummary,
}

pub struct DoctorOptions {
    pub rules_dir: PathBuf,
    pub hub: Option<String>,
    pub strict: bool,
    pub json: bool,
    pub check_rules_validate: bool,
    pub check_fixtures: bool,
    pub socket_path: Option<PathBuf>,
}

#[derive(Debug)]
pub enum DoctorError {
    StrictFailed,
    InvalidConfig(String),
    Runtime(String),
}

impl DoctorError {
    pub fn exit_code(&self) -> i32 {
        match self {
            DoctorError::StrictFailed => 2,
            DoctorError::InvalidConfig(_) => 3,
            DoctorError::Runtime(_) => 1,
        }
    }
}

impl std::fmt::Display for DoctorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DoctorError::StrictFailed => write!(f, "doctor checks failed in strict mode"),
            DoctorError::InvalidConfig(msg) => write!(f, "invalid config: {}", msg),
            DoctorError::Runtime(msg) => write!(f, "runtime error: {}", msg),
        }
    }
}

impl std::error::Error for DoctorError {}

pub async fn run_doctor(opts: DoctorOptions) -> Result<(), DoctorError> {
    if !opts.rules_dir.exists() {
        return Err(DoctorError::InvalidConfig(format!(
            "rules dir does not exist: {}",
            opts.rules_dir.display()
        )));
    }

    let environment = detect_runtime_environment();
    let mut sections = Vec::new();
    sections.push(environment_checks(&environment).await);
    sections.push(ark_runtime_checks(&opts, opts.socket_path.clone()).await);
    sections.push(hub_connectivity_checks(&opts).await);

    let summary = summarize(&sections);
    let report = DoctorReport {
        environment,
        sections,
        summary: DoctorSummary {
            ok: summary.0,
            warn: summary.1,
            fail: summary.2,
        },
    };

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|e| DoctorError::Runtime(format!("serialize report: {}", e)))?
        );
    } else {
        print_report(&report);
    }

    if opts.strict && report.summary.fail > 0 {
        return Err(DoctorError::StrictFailed);
    }

    Ok(())
}

async fn environment_checks(environment: &str) -> DoctorSection {
    let mut checks = Vec::new();

    #[cfg(target_os = "linux")]
    {
        checks.push(check_kernel_version());
        checks.push(check_ebpf_mount_and_writable());
        checks.push(check_ebpf_capability());
        checks.push(check_ebpf_btf());
        checks.push(check_unprivileged_bpf_disabled());
        checks.push(check_memlock_limit());
        checks.push(check_nvml());
        checks.extend(check_k8s_runtime(environment).await);
    }

    #[cfg(not(target_os = "linux"))]
    {
        checks.push(CheckItem {
            name: "kernel".to_string(),
            status: CheckStatus::Warn,
            detail: "non-linux platform, partial checks only".to_string(),
            suggestion: Some("run on Linux for full eBPF diagnostics".to_string()),
        });
    }

    DoctorSection {
        name: "Environment Check".to_string(),
        checks,
    }
}

async fn ark_runtime_checks(opts: &DoctorOptions, socket_path: Option<PathBuf>) -> DoctorSection {
    let mut checks = Vec::new();
    checks.push(check_rules_load(&opts.rules_dir));
    if opts.check_rules_validate {
        checks.push(check_rules_validate(&opts.rules_dir));
    }
    if opts.check_fixtures {
        checks.push(check_fixtures_contract(&opts.rules_dir));
    }
    checks.push(check_daemon_connectivity(socket_path).await);

    DoctorSection {
        name: "Ark Runtime".to_string(),
        checks,
    }
}

async fn hub_connectivity_checks(opts: &DoctorOptions) -> DoctorSection {
    let mut checks = Vec::new();
    let Some(hub) = opts.hub.as_ref() else {
        checks.push(CheckItem {
            name: "hub endpoint".to_string(),
            status: CheckStatus::Warn,
            detail: "not configured".to_string(),
            suggestion: Some("pass --hub http://<hub-host>:8081".to_string()),
        });
        return DoctorSection {
            name: "Hub Connectivity".to_string(),
            checks,
        };
    };

    let http_endpoint = format!("{}/api/v1/ps", hub.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build();

    match client {
        Ok(client) => {
            let started = Instant::now();
            match client.get(&http_endpoint).send().await {
                Ok(resp) if resp.status().is_success() => checks.push(CheckItem {
                    name: "hub http".to_string(),
                    status: CheckStatus::Ok,
                    detail: format!(
                        "{} (HTTP {}, {}ms)",
                        http_endpoint,
                        resp.status().as_u16(),
                        started.elapsed().as_millis()
                    ),
                    suggestion: None,
                }),
                Ok(resp) => checks.push(CheckItem {
                    name: "hub http".to_string(),
                    status: CheckStatus::Fail,
                    detail: format!(
                        "{} (HTTP {}, {}ms)",
                        http_endpoint,
                        resp.status().as_u16(),
                        started.elapsed().as_millis()
                    ),
                    suggestion: Some("verify hub HTTP API is reachable".to_string()),
                }),
                Err(e) => checks.push(CheckItem {
                    name: "hub http".to_string(),
                    status: CheckStatus::Fail,
                    detail: format!("{} ({})", http_endpoint, e),
                    suggestion: Some("check network/DNS and hub address".to_string()),
                }),
            }
        }
        Err(e) => checks.push(CheckItem {
            name: "hub http".to_string(),
            status: CheckStatus::Fail,
            detail: format!("failed to build HTTP client: {}", e),
            suggestion: Some("check TLS/proxy environment".to_string()),
        }),
    }

    if let Some(ws_endpoint) = derive_ws_endpoint(hub) {
        let started = Instant::now();
        match tokio::time::timeout(Duration::from_secs(3), connect_async(ws_endpoint.as_str()))
            .await
        {
            Ok(Ok((mut stream, _))) => {
                let _ = stream.close(None).await;
                checks.push(CheckItem {
                    name: "hub ws".to_string(),
                    status: CheckStatus::Ok,
                    detail: format!("{} ({}ms)", ws_endpoint, started.elapsed().as_millis()),
                    suggestion: None,
                });
            }
            Ok(Err(e)) => checks.push(CheckItem {
                name: "hub ws".to_string(),
                status: CheckStatus::Warn,
                detail: format!("{} ({})", ws_endpoint, e),
                suggestion: Some("verify hub WebSocket listener (default :8080)".to_string()),
            }),
            Err(_) => checks.push(CheckItem {
                name: "hub ws".to_string(),
                status: CheckStatus::Warn,
                detail: format!("{} (timeout)", ws_endpoint),
                suggestion: Some("check firewall and websocket listener".to_string()),
            }),
        }
    } else {
        checks.push(CheckItem {
            name: "hub ws".to_string(),
            status: CheckStatus::Warn,
            detail: "cannot derive ws endpoint from --hub".to_string(),
            suggestion: Some("use --hub http://host:8081".to_string()),
        });
    }

    let health_endpoint = format!("{}/api/v1/health", hub.trim_end_matches('/'));
    match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => {
            let started = Instant::now();
            match client.get(&health_endpoint).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let elapsed_ms = started.elapsed().as_millis();
                    match resp.text().await {
                        Ok(body) => {
                            checks.push(CheckItem {
                                name: "hub health".to_string(),
                                status: CheckStatus::Ok,
                                detail: format!("{} ({}ms)", health_endpoint, elapsed_ms),
                                suggestion: None,
                            });
                            checks.push(evaluate_hub_wal_health(&body));
                        }
                        Err(e) => checks.push(CheckItem {
                            name: "hub health".to_string(),
                            status: CheckStatus::Warn,
                            detail: format!("{} response read failed ({})", health_endpoint, e),
                            suggestion: Some(
                                "verify hub health response serialization".to_string(),
                            ),
                        }),
                    }
                }
                Ok(resp) => checks.push(CheckItem {
                    name: "hub health".to_string(),
                    status: CheckStatus::Warn,
                    detail: format!("{} (HTTP {})", health_endpoint, resp.status().as_u16()),
                    suggestion: Some(
                        "upgrade hub to a version exposing /api/v1/health".to_string(),
                    ),
                }),
                Err(e) => checks.push(CheckItem {
                    name: "hub health".to_string(),
                    status: CheckStatus::Warn,
                    detail: format!("{} ({})", health_endpoint, e),
                    suggestion: Some("verify hub HTTP API is reachable".to_string()),
                }),
            }
        }
        Err(e) => checks.push(CheckItem {
            name: "hub health".to_string(),
            status: CheckStatus::Warn,
            detail: format!("failed to build HTTP client: {}", e),
            suggestion: None,
        }),
    }

    DoctorSection {
        name: "Hub Connectivity".to_string(),
        checks,
    }
}

fn evaluate_hub_wal_health(raw_json: &str) -> CheckItem {
    let value: serde_json::Value = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(e) => {
            return CheckItem {
                name: "hub wal".to_string(),
                status: CheckStatus::Warn,
                detail: format!("health payload parse failed: {}", e),
                suggestion: Some("ensure /api/v1/health returns valid JSON".to_string()),
            }
        }
    };

    let wal = value.get("wal").and_then(|v| v.as_object());
    let Some(wal) = wal else {
        return CheckItem {
            name: "hub wal".to_string(),
            status: CheckStatus::Warn,
            detail: "health payload missing wal section".to_string(),
            suggestion: Some("upgrade hub to include wal health fields".to_string()),
        };
    };

    let active_exists = wal
        .get("active_exists")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let active_size = wal
        .get("active_size_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let rotated_exists = wal
        .get("rotated_exists")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let rotated_size = wal
        .get("rotated_size_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if !active_exists {
        return CheckItem {
            name: "hub wal".to_string(),
            status: CheckStatus::Warn,
            detail: "active WAL file not found".to_string(),
            suggestion: Some("check hub --wal-path and writable volume mount".to_string()),
        };
    }

    CheckItem {
        name: "hub wal".to_string(),
        status: CheckStatus::Ok,
        detail: format!(
            "active={}B, rotated_exists={}, rotated={}B",
            active_size, rotated_exists, rotated_size
        ),
        suggestion: None,
    }
}

fn detect_runtime_environment() -> String {
    if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
        return "k8s".to_string();
    }
    if Path::new("/.dockerenv").exists() {
        return "container".to_string();
    }
    let cgroup = std::fs::read_to_string("/proc/1/cgroup").unwrap_or_default();
    if cgroup.contains("docker") || cgroup.contains("containerd") || cgroup.contains("kubepods") {
        return "container".to_string();
    }
    "local".to_string()
}

#[cfg(target_os = "linux")]
fn check_kernel_version() -> CheckItem {
    let min_major = 5_u64;
    let min_minor = 10_u64;
    let raw = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_else(|_| "unknown".to_string())
        .trim()
        .to_string();
    match parse_kernel_version(&raw) {
        Some((major, minor)) if major > min_major || (major == min_major && minor >= min_minor) => {
            CheckItem {
                name: "kernel version".to_string(),
                status: CheckStatus::Ok,
                detail: format!("{} (>= 5.10)", raw),
                suggestion: None,
            }
        }
        Some((major, minor)) => CheckItem {
            name: "kernel version".to_string(),
            status: CheckStatus::Warn,
            detail: format!("{}.{} detected (< 5.10)", major, minor),
            suggestion: Some("upgrade kernel for better eBPF compatibility".to_string()),
        },
        None => CheckItem {
            name: "kernel version".to_string(),
            status: CheckStatus::Warn,
            detail: format!("unrecognized format: {}", raw),
            suggestion: Some("verify /proc/sys/kernel/osrelease".to_string()),
        },
    }
}

#[cfg(target_os = "linux")]
fn check_ebpf_mount_and_writable() -> CheckItem {
    let path = Path::new("/sys/fs/bpf");
    if !path.exists() {
        return CheckItem {
            name: "bpffs".to_string(),
            status: CheckStatus::Warn,
            detail: "/sys/fs/bpf missing".to_string(),
            suggestion: Some("mount bpffs: mount -t bpf bpf /sys/fs/bpf".to_string()),
        };
    }

    let marker = format!(".ark_doctor_{}", std::process::id());
    let marker_path = path.join(marker);
    match fs::write(&marker_path, b"ok") {
        Ok(_) => {
            let _ = fs::remove_file(&marker_path);
            CheckItem {
                name: "bpffs".to_string(),
                status: CheckStatus::Ok,
                detail: "/sys/fs/bpf available and writable".to_string(),
                suggestion: None,
            }
        }
        Err(e) => CheckItem {
            name: "bpffs".to_string(),
            status: CheckStatus::Warn,
            detail: format!("/sys/fs/bpf exists but not writable ({})", e),
            suggestion: Some("grant write permission or run with elevated privileges".to_string()),
        },
    }
}

#[cfg(target_os = "linux")]
fn check_ebpf_capability() -> CheckItem {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let cap_eff_hex = status
        .lines()
        .find(|line| line.starts_with("CapEff:"))
        .and_then(|line| line.split_whitespace().nth(1));

    let Some(hex) = cap_eff_hex else {
        return CheckItem {
            name: "eBPF capability".to_string(),
            status: CheckStatus::Warn,
            detail: "failed to read CapEff".to_string(),
            suggestion: Some("run with CAP_BPF/CAP_PERFMON or CAP_SYS_ADMIN".to_string()),
        };
    };

    let value = u64::from_str_radix(hex, 16).unwrap_or(0);
    let has_cap_sys_admin = (value & (1u64 << 21)) != 0;
    let has_cap_perfmon = (value & (1u64 << 38)) != 0;
    let has_cap_bpf = (value & (1u64 << 39)) != 0;

    if has_cap_sys_admin || (has_cap_bpf && has_cap_perfmon) {
        CheckItem {
            name: "eBPF capability".to_string(),
            status: CheckStatus::Ok,
            detail: format!(
                "CapEff=0x{} (sys_admin={}, bpf={}, perfmon={})",
                hex, has_cap_sys_admin, has_cap_bpf, has_cap_perfmon
            ),
            suggestion: None,
        }
    } else {
        CheckItem {
            name: "eBPF capability".to_string(),
            status: CheckStatus::Warn,
            detail: format!(
                "CapEff=0x{} (sys_admin={}, bpf={}, perfmon={})",
                hex, has_cap_sys_admin, has_cap_bpf, has_cap_perfmon
            ),
            suggestion: Some(
                "add CAP_BPF + CAP_PERFMON (or CAP_SYS_ADMIN) for eBPF probes".to_string(),
            ),
        }
    }
}

#[cfg(target_os = "linux")]
fn check_ebpf_btf() -> CheckItem {
    let path = Path::new("/sys/kernel/btf/vmlinux");
    if path.exists() {
        CheckItem {
            name: "BTF vmlinux".to_string(),
            status: CheckStatus::Ok,
            detail: "/sys/kernel/btf/vmlinux found".to_string(),
            suggestion: None,
        }
    } else {
        CheckItem {
            name: "BTF vmlinux".to_string(),
            status: CheckStatus::Warn,
            detail: "/sys/kernel/btf/vmlinux not found".to_string(),
            suggestion: Some("CO-RE probes may fail; install kernel BTF package".to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
fn check_unprivileged_bpf_disabled() -> CheckItem {
    let path = Path::new("/proc/sys/kernel/unprivileged_bpf_disabled");
    let raw = fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string();
    if raw.is_empty() {
        return CheckItem {
            name: "unprivileged_bpf_disabled".to_string(),
            status: CheckStatus::Warn,
            detail: "cannot read kernel.unprivileged_bpf_disabled".to_string(),
            suggestion: None,
        };
    }

    let status = match raw.as_str() {
        "0" => CheckStatus::Ok,
        "1" | "2" => CheckStatus::Warn,
        _ => CheckStatus::Warn,
    };
    let suggestion = if status == CheckStatus::Warn {
        Some("unprivileged eBPF is disabled; run with capabilities/root".to_string())
    } else {
        None
    };

    CheckItem {
        name: "unprivileged_bpf_disabled".to_string(),
        status,
        detail: format!("kernel.unprivileged_bpf_disabled={}", raw),
        suggestion,
    }
}

#[cfg(target_os = "linux")]
fn check_memlock_limit() -> CheckItem {
    let limits = fs::read_to_string("/proc/self/limits").unwrap_or_default();
    let line = limits.lines().find(|l| l.starts_with("Max locked memory"));

    let Some(raw) = line else {
        return CheckItem {
            name: "memlock".to_string(),
            status: CheckStatus::Warn,
            detail: "cannot read Max locked memory".to_string(),
            suggestion: Some("set ulimit -l unlimited for eBPF workloads".to_string()),
        };
    };

    let parts: Vec<&str> = raw.split_whitespace().collect();
    let soft = parts.get(3).copied().unwrap_or("unknown");
    let unit = parts.get(5).copied().unwrap_or("");

    if soft.eq_ignore_ascii_case("unlimited") {
        CheckItem {
            name: "memlock".to_string(),
            status: CheckStatus::Ok,
            detail: "Max locked memory: unlimited".to_string(),
            suggestion: None,
        }
    } else {
        CheckItem {
            name: "memlock".to_string(),
            status: CheckStatus::Warn,
            detail: format!("Max locked memory: {} {}", soft, unit),
            suggestion: Some("increase memlock (e.g. `ulimit -l unlimited`)".to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
fn check_nvml() -> CheckItem {
    let list_output = std::process::Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .output();

    match list_output {
        Ok(out) if out.status.success() => {
            let names_raw = String::from_utf8_lossy(&out.stdout);
            let gpu_count = names_raw.lines().filter(|l| !l.trim().is_empty()).count();
            let drv = read_first_query("driver_version").unwrap_or_else(|| "unknown".to_string());
            let cuda = read_cuda_version().unwrap_or_else(|| "unknown".to_string());
            CheckItem {
                name: "NVML".to_string(),
                status: CheckStatus::Ok,
                detail: format!(
                    "available, gpu_count={}, driver={}, cuda={}",
                    gpu_count, drv, cuda
                ),
                suggestion: None,
            }
        }
        Ok(out) => CheckItem {
            name: "NVML".to_string(),
            status: CheckStatus::Warn,
            detail: format!("nvidia-smi exited with {}", out.status),
            suggestion: Some(
                "check NVIDIA driver and container /dev/nvidia* device mounts".to_string(),
            ),
        },
        Err(_) => CheckItem {
            name: "NVML".to_string(),
            status: CheckStatus::Warn,
            detail: "nvidia-smi not found".to_string(),
            suggestion: Some(
                "install NVIDIA utilities or mount driver stack into container".to_string(),
            ),
        },
    }
}

#[cfg(target_os = "linux")]
fn read_first_query(field: &str) -> Option<String> {
    let out = std::process::Command::new("nvidia-smi")
        .arg(format!("--query-gpu={}", field))
        .arg("--format=csv,noheader")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().next().map(|v| v.trim().to_string())
}

#[cfg(target_os = "linux")]
fn read_cuda_version() -> Option<String> {
    let out = std::process::Command::new("nvidia-smi").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(idx) = line.find("CUDA Version:") {
            let v = &line[idx + "CUDA Version:".len()..];
            let token = v.split_whitespace().next().unwrap_or("");
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
async fn check_k8s_runtime(environment: &str) -> Vec<CheckItem> {
    let mut checks = Vec::new();
    let kubeconfig = std::env::var("KUBECONFIG").ok().or_else(|| {
        std::env::var("HOME")
            .ok()
            .map(|h| format!("{}/.kube/config", h))
    });

    if environment == "k8s" {
        checks.push(CheckItem {
            name: "k8s config".to_string(),
            status: CheckStatus::Ok,
            detail: "detected in-cluster config".to_string(),
            suggestion: None,
        });

        checks.push(check_k8s_apiserver_reachability().await);
        return checks;
    }

    if let Some(path) = kubeconfig {
        if Path::new(&path).exists() {
            checks.push(CheckItem {
                name: "k8s config".to_string(),
                status: CheckStatus::Warn,
                detail: format!("detected kubeconfig at {}", path),
                suggestion: Some("run in-cluster for serviceaccount/RBAC checks".to_string()),
            });
        } else {
            checks.push(CheckItem {
                name: "k8s config".to_string(),
                status: CheckStatus::Warn,
                detail: "not running in kubernetes".to_string(),
                suggestion: None,
            });
        }
    } else {
        checks.push(CheckItem {
            name: "k8s config".to_string(),
            status: CheckStatus::Warn,
            detail: "not running in kubernetes".to_string(),
            suggestion: None,
        });
    }

    checks
}

#[cfg(target_os = "linux")]
async fn check_k8s_apiserver_reachability() -> CheckItem {
    let host = std::env::var("KUBERNETES_SERVICE_HOST").unwrap_or_default();
    let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".to_string());
    if host.is_empty() {
        return CheckItem {
            name: "k8s apiserver".to_string(),
            status: CheckStatus::Warn,
            detail: "KUBERNETES_SERVICE_HOST is empty".to_string(),
            suggestion: Some("verify in-cluster environment variables".to_string()),
        };
    }

    let token_path = "/var/run/secrets/kubernetes.io/serviceaccount/token";
    let token = fs::read_to_string(token_path).unwrap_or_default();
    let url = format!("https://{}:{}/version", host, port);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckItem {
                name: "k8s apiserver".to_string(),
                status: CheckStatus::Fail,
                detail: format!("failed to build client: {}", e),
                suggestion: Some("check TLS/proxy env".to_string()),
            }
        }
    };

    let req = if token.trim().is_empty() {
        client.get(&url)
    } else {
        client.get(&url).bearer_auth(token.trim())
    };

    let started = Instant::now();
    match req.send().await {
        Ok(resp) if resp.status().is_success() => CheckItem {
            name: "k8s apiserver".to_string(),
            status: CheckStatus::Ok,
            detail: format!(
                "GET /version -> {} ({}ms)",
                resp.status(),
                started.elapsed().as_millis()
            ),
            suggestion: None,
        },
        Ok(resp) => CheckItem {
            name: "k8s apiserver".to_string(),
            status: CheckStatus::Fail,
            detail: format!("GET /version -> {}", resp.status()),
            suggestion: Some("check ServiceAccount token and RBAC".to_string()),
        },
        Err(e) => CheckItem {
            name: "k8s apiserver".to_string(),
            status: CheckStatus::Fail,
            detail: format!("GET /version failed: {}", e),
            suggestion: Some("check in-cluster DNS/network policies".to_string()),
        },
    }
}

fn check_rules_load(rules_dir: &Path) -> CheckItem {
    match RuleEngine::load_from_dir(rules_dir) {
        Ok(engine) => {
            let stats = engine.load_stats();
            let status = if stats.skipped_rules > 0 {
                CheckStatus::Fail
            } else if stats.loaded_rules == 0 || stats.legacy_total > 0 {
                CheckStatus::Warn
            } else {
                CheckStatus::Ok
            };
            let suggestion = match status {
                CheckStatus::Ok => None,
                CheckStatus::Warn if stats.loaded_rules == 0 => {
                    Some("add rules under rules/ packs and verify manifest.yaml".to_string())
                }
                CheckStatus::Warn => Some(
                    "migrate remaining legacy rules with `cargo run -p ark-core --bin rules-migrate -- rules --dry-run`"
                        .to_string(),
                ),
                CheckStatus::Fail => {
                    Some("fix skipped rules and re-run with ARK_RULES_STRICT=1".to_string())
                }
            };
            CheckItem {
                name: "rules package".to_string(),
                status,
                detail: format!(
                    "loaded={}, skipped={}, legacy={}",
                    stats.loaded_rules, stats.skipped_rules, stats.legacy_total
                ),
                suggestion,
            }
        }
        Err(e) => CheckItem {
            name: "rules package".to_string(),
            status: CheckStatus::Fail,
            detail: format!("failed to load rules: {}", e),
            suggestion: Some("check rules manifest and YAML syntax".to_string()),
        },
    }
}

fn check_rules_validate(rules_dir: &Path) -> CheckItem {
    let files = collect_rule_files(rules_dir);
    let mut scene_owner: HashMap<String, String> = HashMap::new();
    let mut errors = Vec::new();

    for path in &files {
        let content = match fs::read_to_string(path) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("{}: read failed: {}", path.display(), e));
                continue;
            }
        };

        let value: YamlValue = match serde_yaml::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("{}: yaml parse failed: {}", path.display(), e));
                continue;
            }
        };

        let map = match value.as_mapping() {
            Some(m) => m,
            None => {
                errors.push(format!("{}: rule must be YAML object", path.display()));
                continue;
            }
        };

        let scene = get_yaml_string(map, "scene");
        let priority = get_yaml_u64(map, "priority");
        if let Some(scene) = scene {
            if let Some(prev) = scene_owner.insert(scene.clone(), path.display().to_string()) {
                errors.push(format!(
                    "scene duplicated: {} in {} and {}",
                    scene,
                    prev,
                    path.display()
                ));
            }
        } else {
            errors.push(format!("{}: missing scene", path.display()));
        }

        match priority {
            Some(p) if (1..=100).contains(&p) => {}
            Some(p) => errors.push(format!(
                "{}: priority {} out of range 1..=100",
                path.display(),
                p
            )),
            None => errors.push(format!("{}: missing/invalid priority", path.display())),
        }

        match serde_yaml::from_str::<RuleWire>(&content) {
            Ok(w) => {
                if let Err(e) = w.normalize() {
                    errors.push(format!("{}: normalize failed: {}", path.display(), e));
                }
            }
            Err(e) => errors.push(format!("{}: deserialize failed: {}", path.display(), e)),
        }
    }

    if errors.is_empty() {
        CheckItem {
            name: "rules validate".to_string(),
            status: CheckStatus::Ok,
            detail: format!("{} files validated", files.len()),
            suggestion: None,
        }
    } else {
        CheckItem {
            name: "rules validate".to_string(),
            status: CheckStatus::Fail,
            detail: format!("{} errors (first: {})", errors.len(), errors[0]),
            suggestion: Some("fix rule schema/priority/scene conflicts".to_string()),
        }
    }
}

fn check_fixtures_contract(rules_dir: &Path) -> CheckItem {
    let fixtures_root = rules_dir.join("fixtures");
    if !fixtures_root.exists() {
        return CheckItem {
            name: "fixtures".to_string(),
            status: CheckStatus::Fail,
            detail: format!("fixtures dir missing: {}", fixtures_root.display()),
            suggestion: Some(
                "create rules/fixtures/<case>/input_events.jsonl + expected.json".to_string(),
            ),
        };
    }

    let mut case_count = 0usize;
    let mut errors = Vec::new();
    let entries = match fs::read_dir(&fixtures_root) {
        Ok(v) => v,
        Err(e) => {
            return CheckItem {
                name: "fixtures".to_string(),
                status: CheckStatus::Fail,
                detail: format!("read fixtures dir failed: {}", e),
                suggestion: None,
            }
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        case_count += 1;
        let input = path.join("input_events.jsonl");
        let expected = path.join("expected.json");
        if !input.exists() {
            errors.push(format!("{} missing input_events.jsonl", path.display()));
        }
        if !expected.exists() {
            errors.push(format!("{} missing expected.json", path.display()));
            continue;
        }
        let raw = match fs::read_to_string(&expected) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("{} read failed: {}", expected.display(), e));
                continue;
            }
        };
        let json: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("{} invalid json: {}", expected.display(), e));
                continue;
            }
        };
        let scenes = json.get("expect_scenes").and_then(|v| v.as_array());
        if scenes.map(|a| a.is_empty()).unwrap_or(true) {
            errors.push(format!(
                "{} expect_scenes must be non-empty array",
                expected.display()
            ));
        }
    }

    if case_count == 0 {
        return CheckItem {
            name: "fixtures".to_string(),
            status: CheckStatus::Warn,
            detail: "no fixture cases found".to_string(),
            suggestion: Some("add fixture contract cases under rules/fixtures".to_string()),
        };
    }

    if errors.is_empty() {
        CheckItem {
            name: "fixtures".to_string(),
            status: CheckStatus::Ok,
            detail: format!("{} fixture cases validated", case_count),
            suggestion: None,
        }
    } else {
        CheckItem {
            name: "fixtures".to_string(),
            status: CheckStatus::Fail,
            detail: format!("{} errors (first: {})", errors.len(), errors[0]),
            suggestion: Some("fix fixture files and expected.json schema".to_string()),
        }
    }
}

fn collect_rule_files(rules_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![rules_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let rel = path.strip_prefix(rules_dir).unwrap_or(&path);
                if rel == Path::new("fixtures") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if (ext == "yaml" || ext == "yml")
                && path.file_name().and_then(|s| s.to_str()) != Some("manifest.yaml")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn get_yaml_string(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    map.get(YamlValue::String(key.to_string()))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn get_yaml_u64(map: &serde_yaml::Mapping, key: &str) -> Option<u64> {
    map.get(YamlValue::String(key.to_string()))
        .and_then(|v| v.as_u64())
}

async fn check_daemon_connectivity(socket_path: Option<PathBuf>) -> CheckItem {
    let client = IpcClient::new(socket_path);
    match client.ping().await {
        Ok(true) => CheckItem {
            name: "daemon connectivity".to_string(),
            status: CheckStatus::Ok,
            detail: "ark daemon is reachable".to_string(),
            suggestion: None,
        },
        Ok(false) => CheckItem {
            name: "daemon connectivity".to_string(),
            status: CheckStatus::Warn,
            detail: "daemon is not reachable".to_string(),
            suggestion: Some("start daemon with `ark run`".to_string()),
        },
        Err(e) => CheckItem {
            name: "daemon connectivity".to_string(),
            status: CheckStatus::Warn,
            detail: format!("ping failed: {}", e),
            suggestion: Some("check IPC socket/port and daemon status".to_string()),
        },
    }
}

fn summarize(sections: &[DoctorSection]) -> (usize, usize, usize) {
    let mut ok = 0;
    let mut warn = 0;
    let mut fail = 0;
    for item in sections.iter().flat_map(|s| s.checks.iter()) {
        match item.status {
            CheckStatus::Ok => ok += 1,
            CheckStatus::Warn => warn += 1,
            CheckStatus::Fail => fail += 1,
        }
    }
    (ok, warn, fail)
}

fn print_report(report: &DoctorReport) {
    println!("Environment: {}", report.environment);
    println!();
    for section in &report.sections {
        println!("{}", section.name);
        println!("{}", "-".repeat(section.name.len().max(24)));
        for check in &section.checks {
            let status = match check.status {
                CheckStatus::Ok => "OK",
                CheckStatus::Warn => "WARN",
                CheckStatus::Fail => "FAIL",
            };
            println!("{:<24} {:<5} {}", check.name, status, check.detail);
            if let Some(suggestion) = &check.suggestion {
                println!("{:<24}       hint: {}", "", suggestion);
            }
        }
        println!();
    }
    println!(
        "Summary: OK={} WARN={} FAIL={}",
        report.summary.ok, report.summary.warn, report.summary.fail
    );
}

fn derive_ws_endpoint(hub: &str) -> Option<String> {
    let url = url::Url::parse(hub).ok()?;
    let host = url.host_str()?.to_string();
    let http_port = url.port_or_known_default()?;
    let ws_port = if http_port == 8081 { 8080 } else { http_port };
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    Some(format!("{}://{}:{}", scheme, host, ws_port))
}

#[cfg(target_os = "linux")]
fn parse_kernel_version(raw: &str) -> Option<(u64, u64)> {
    let mut iter = raw.split('.');
    let major = iter.next()?.parse::<u64>().ok()?;
    let minor_str = iter.next()?;
    let minor = minor_str
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse::<u64>()
        .ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::parse_kernel_version;
    use super::{derive_ws_endpoint, evaluate_hub_wal_health, CheckStatus};

    #[test]
    #[cfg(target_os = "linux")]
    fn parse_kernel_version_accepts_common_formats() {
        assert_eq!(parse_kernel_version("5.15.0-101-generic"), Some((5, 15)));
        assert_eq!(parse_kernel_version("6.8.12"), Some((6, 8)));
        assert_eq!(parse_kernel_version("not-a-version"), None);
    }

    #[test]
    fn derive_ws_endpoint_maps_default_hub_port() {
        assert_eq!(
            derive_ws_endpoint("http://localhost:8081").as_deref(),
            Some("ws://localhost:8080")
        );
        assert_eq!(
            derive_ws_endpoint("https://hub.example.com:9443").as_deref(),
            Some("wss://hub.example.com:9443")
        );
    }

    #[test]
    fn evaluate_hub_wal_health_handles_valid_payload() {
        let payload = r#"{
          "status":"ok",
          "wal":{
            "active_exists":true,
            "active_size_bytes":4096,
            "rotated_exists":true,
            "rotated_size_bytes":8192
          }
        }"#;
        let item = evaluate_hub_wal_health(payload);
        assert_eq!(item.status, CheckStatus::Ok);
        assert!(item.detail.contains("active=4096B"));
    }

    #[test]
    fn evaluate_hub_wal_health_warns_when_missing_active_wal() {
        let payload = r#"{
          "status":"ok",
          "wal":{
            "active_exists":false,
            "active_size_bytes":0,
            "rotated_exists":false,
            "rotated_size_bytes":0
          }
        }"#;
        let item = evaluate_hub_wal_health(payload);
        assert_eq!(item.status, CheckStatus::Warn);
    }
}
