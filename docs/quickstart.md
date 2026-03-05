# Ark Quickstart (Minimum Viable Paths)

本页只保留“第一次成功”所需步骤，分为单机、集群、eBPF 三条路径。

## 0. 构建

```bash
git clone https://github.com/osen7/ark-infra.git
cd ark-infra
cargo build --release
```

环境自检：

```bash
cargo run -p ark --release -- --help | head -n 5
cargo run -p ark-hub --release -- --help | head -n 5
cargo run -p ark --release -- doctor
```

## 1) 单机最短路径（无 eBPF，推荐先跑通）

```bash
# 终端 1：用 mock probe 启动 agent
cargo run -p ark --release -- run --probe examples/ark-probe-dummy.py

# 终端 2：查询
cargo run -p ark --release -- ps
cargo run -p ark --release -- why 1234
cargo run -p ark --release -- why 1234 --json
```

成功判定（示例）：

- `ps` 输出包含表头 `PID | JOB_ID | RESOURCES | STATE`
- `why` 返回一个可读根因链路（即使是 mock 数据）

失败时自检：

```bash
ps aux | rg "ark.* run"
ls -l /var/run/ark/ark.sock || ls -l ~/.ark/ark.sock
```

## 2) 集群最短路径（Hub + Agent，本地三终端）

```bash
# 终端 1：启动 Hub（默认 dry-run）
cargo run -p ark-hub --release -- --enable-k8s-controller

# 终端 2：启动 Agent 并连接 Hub
cargo run -p ark --release -- run --hub-url ws://localhost:8080 --probe examples/ark-probe-dummy.py

# 终端 3：集群查询
cargo run -p ark --release -- cluster ps --hub http://localhost:8081
curl 'http://localhost:8081/api/v1/diagnose?job_id=job-1234&window_s=120'
```

成功判定：

- Hub 日志出现 `WebSocket 服务器已启动`
- Agent 日志出现 `已连接到 Hub`
- `cluster ps` 返回 JSON/表格而非连接错误

失败时自检：

```bash
curl -sf http://localhost:8081/api/v1/ps
ss -lntp | rg "8080|8081"
```

## 3) eBPF 路径（需要内核能力）

eBPF probe 是独立子项目：

```bash
cd ark-probe-ebpf
cargo build --release
```

运行 eBPF probe 至少需要 root 或下列能力（示例）：

```bash
sudo setcap cap_bpf,cap_sys_admin+ep ./target/release/ark-probe-ebpf
```

eBPF 启动后建议检查：

```bash
./target/release/ark-probe-ebpf 2>&1 | head -n 20
```

预期看到 probe 启动/attach 成功相关日志；若出现 `kprobe not supported` 或权限错误，优先回退 mock 路径。

无 eBPF 条件时，使用降级路径继续验证链路：

```bash
cargo run -p ark --release -- run --probe examples/ark-probe-rdma-mock.py
```

## 4) K8s 权限自检（部署后）

```bash
kubectl -n ark-system get ds ark-agent
kubectl -n ark-system get deploy ark-hub
kubectl auth can-i patch nodes --as=system:serviceaccount:ark-system:ark-hub-sa
kubectl auth can-i create pods/eviction --as=system:serviceaccount:ark-system:ark-hub-sa
```

检查点：

- `ark-agent` DaemonSet Pod 安全上下文包含 `privileged` 与所需 capabilities
- Hub ServiceAccount 具备预期 RBAC（尤其 `nodes patch` 和 `pods/eviction create`）

## 5. 执行动作安全开关

Hub 默认强制 dry-run。只有显式开启 `--allow-execute` 才会执行 `execute=true` 动作：

```bash
cargo run -p ark-hub --release -- --enable-k8s-controller --allow-execute
```

## 6. Doctor 与 Why 的推荐用法

```bash
# 输出环境/规则/连通性检查结果（JSON 可直接喂 CI）
cargo run -p ark --release -- doctor --json

# 可选：附加规则/fixtures 健康检查（不依赖 cargo test）
cargo run -p ark --release -- doctor --check-rules-validate --check-fixtures

# 根因解释输出稳定 JSON（适合接入 UI 或归档）
cargo run -p ark --release -- why 1234 --json
```

`ark doctor --strict` 退出码约定：

- `0`: 全部 OK 或仅 WARN
- `2`: 存在 FAIL
- `3`: 参数/配置错误（如 rules 目录不存在）
