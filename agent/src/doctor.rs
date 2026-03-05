use crate::ipc::IpcClient;
use ark_core::rules::RuleEngine;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    #[cfg(unix)]
    pub socket_path: Option<PathBuf>,
    #[cfg(windows)]
    pub port: u16,
}

pub async fn run_doctor(opts: DoctorOptions) -> Result<(), Box<dyn std::error::Error>> {
    let environment = detect_runtime_environment();
    let mut sections = Vec::new();
    sections.push(environment_checks(&environment));
    sections.push(ark_runtime_checks(&opts).await);
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
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }

    if opts.strict && report.summary.fail > 0 {
        return Err("doctor checks failed in strict mode".into());
    }

    Ok(())
}

fn environment_checks(environment: &str) -> DoctorSection {
    let mut checks = Vec::new();

    #[cfg(target_os = "linux")]
    {
        checks.push(check_kernel_version());
        checks.push(check_ebpf_mount());
        checks.push(check_ebpf_capability());
        checks.push(check_nvml());
        checks.push(check_k8s_runtime(environment));
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

async fn ark_runtime_checks(opts: &DoctorOptions) -> DoctorSection {
    let mut checks = Vec::new();
    checks.push(check_rules_load(&opts.rules_dir));
    checks.push(
        check_daemon_connectivity(
            #[cfg(unix)]
            opts.socket_path.clone(),
            #[cfg(windows)]
            opts.port,
        )
        .await,
    );
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

    let endpoint = format!("{}/api/v1/ps", hub.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build();
    match client {
        Ok(client) => match client.get(&endpoint).send().await {
            Ok(resp) if resp.status().is_success() => checks.push(CheckItem {
                name: "hub endpoint".to_string(),
                status: CheckStatus::Ok,
                detail: format!("{} (HTTP {})", endpoint, resp.status().as_u16()),
                suggestion: None,
            }),
            Ok(resp) => checks.push(CheckItem {
                name: "hub endpoint".to_string(),
                status: CheckStatus::Fail,
                detail: format!("{} (HTTP {})", endpoint, resp.status().as_u16()),
                suggestion: Some("verify hub is running and API route is reachable".to_string()),
            }),
            Err(e) => checks.push(CheckItem {
                name: "hub endpoint".to_string(),
                status: CheckStatus::Fail,
                detail: format!("{} ({})", endpoint, e),
                suggestion: Some("check network, service DNS, and hub address".to_string()),
            }),
        },
        Err(e) => checks.push(CheckItem {
            name: "hub endpoint".to_string(),
            status: CheckStatus::Fail,
            detail: format!("failed to build HTTP client: {}", e),
            suggestion: Some("check TLS/proxy environment variables".to_string()),
        }),
    }

    DoctorSection {
        name: "Hub Connectivity".to_string(),
        checks,
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
    let parsed = parse_kernel_version(&raw);
    match parsed {
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
fn check_ebpf_mount() -> CheckItem {
    if Path::new("/sys/fs/bpf").exists() {
        CheckItem {
            name: "eBPF filesystem".to_string(),
            status: CheckStatus::Ok,
            detail: "/sys/fs/bpf available".to_string(),
            suggestion: None,
        }
    } else {
        CheckItem {
            name: "eBPF filesystem".to_string(),
            status: CheckStatus::Warn,
            detail: "/sys/fs/bpf missing".to_string(),
            suggestion: Some("mount bpffs: mount -t bpf bpf /sys/fs/bpf".to_string()),
        }
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
fn check_nvml() -> CheckItem {
    let output = std::process::Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let count = stdout.lines().filter(|l| !l.trim().is_empty()).count();
            CheckItem {
                name: "NVML".to_string(),
                status: CheckStatus::Ok,
                detail: format!("available, gpu_count={}", count),
                suggestion: None,
            }
        }
        Ok(out) => CheckItem {
            name: "NVML".to_string(),
            status: CheckStatus::Warn,
            detail: format!("nvidia-smi exited with {}", out.status),
            suggestion: Some("install NVIDIA driver/NVML or run on GPU node".to_string()),
        },
        Err(_) => CheckItem {
            name: "NVML".to_string(),
            status: CheckStatus::Warn,
            detail: "nvidia-smi not found".to_string(),
            suggestion: Some("install NVIDIA utilities or skip GPU-specific checks".to_string()),
        },
    }
}

#[cfg(target_os = "linux")]
fn check_k8s_runtime(environment: &str) -> CheckItem {
    if environment != "k8s" {
        return CheckItem {
            name: "k8s runtime".to_string(),
            status: CheckStatus::Warn,
            detail: "not running in kubernetes".to_string(),
            suggestion: None,
        };
    }

    let token = Path::new("/var/run/secrets/kubernetes.io/serviceaccount/token").exists();
    let namespace = Path::new("/var/run/secrets/kubernetes.io/serviceaccount/namespace").exists();
    if token && namespace {
        CheckItem {
            name: "k8s runtime".to_string(),
            status: CheckStatus::Ok,
            detail: "serviceaccount credentials detected".to_string(),
            suggestion: None,
        }
    } else {
        CheckItem {
            name: "k8s runtime".to_string(),
            status: CheckStatus::Fail,
            detail: "missing serviceaccount credentials".to_string(),
            suggestion: Some(
                "verify pod automountServiceAccountToken and RBAC bindings".to_string(),
            ),
        }
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
                CheckStatus::Warn => {
                    Some(
                        "migrate remaining legacy rules with `cargo run -p ark-core --bin rules-migrate -- rules --dry-run`"
                            .to_string(),
                    )
                }
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

async fn check_daemon_connectivity(
    #[cfg(unix)] socket_path: Option<PathBuf>,
    #[cfg(windows)] port: u16,
) -> CheckItem {
    let client = IpcClient::new(
        #[cfg(unix)]
        socket_path,
        #[cfg(windows)]
        port,
    );
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

    #[test]
    #[cfg(target_os = "linux")]
    fn parse_kernel_version_accepts_common_formats() {
        assert_eq!(parse_kernel_version("5.15.0-101-generic"), Some((5, 15)));
        assert_eq!(parse_kernel_version("6.8.12"), Some((6, 8)));
        assert_eq!(parse_kernel_version("not-a-version"), None);
    }
}
