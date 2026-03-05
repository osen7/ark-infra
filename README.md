# Ark 🚢 - The Autonomous "Ark" for AI Infrastructure

**守护 AI 算力集群的自愈方舟**

[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Ark** (formerly xctl) is a high-performance, expert-driven control plane built with Rust and eBPF. It monitors heterogenous AI clusters (NVIDIA/Ascend), performs real-time causal graph reasoning, and automatically heals workloads before human SREs even notice the failure.

**Ark** 是一个用 Rust 和 eBPF 打造的高性能、专家驱动的控制面。它监控异构 AI 集群（NVIDIA/昇腾），执行实时因果图推理，并在人类运维感知到故障前自动完成自愈。

## ✨ 特性

- 🚀 **事件驱动内核**：基于事件流的实时状态图，零轮询开销
- 🔌 **可插拔探针**：支持 GPU（NVML）、网络（eBPF/proc）、存储等多种探针
- 🧠 **AI 诊断**：集成大模型（OpenAI/Claude），自动生成修复建议
- 🔍 **因果分析**：自动推导进程-资源-错误的因果关系
- 💻 **极简 CLI**：类似 Docker 的 C/S 架构，轻量级客户端
- 🛡️ **生产级**：内存安全、错误处理完善、OOM 防护
- ☸️ **Kubernetes 原生**：DaemonSet + Deployment，一键部署到万卡集群
- 🤖 **自动驾驶控制面**：自动检测硬件故障，打污点、驱逐 Pod，与 K8s 调度器深度集成
- 📊 **Prometheus 集成**：暴露标准 Metrics 端点，无缝融入 Grafana 监控体系
- 📝 **审计日志**：完整记录所有系统干预动作，满足企业合规要求

## 🚀 快速开始

```bash
# 1. 克隆仓库
git clone https://github.com/osen7/ark-infra.git
cd ark-infra

# 2. 构建项目
cargo build --release

# 3. 启动守护进程（使用 GPU 探针）
cargo run -p ark --release -- run --probe examples/ark-probe-nvml.py

# 4. 在另一个终端查询
cargo run -p ark --release -- ps
cargo run -p ark --release -- why <PID>
cargo run -p ark --release -- diag <PID>  # AI 诊断
cargo run -p ark --release -- fix <PID> --audit-log /var/log/ark/audit.log  # 修复并记录审计日志

# 查看 Prometheus Metrics（Agent 端）
curl http://localhost:9091/metrics
```

详细使用指南请查看 [QUICKSTART.md](QUICKSTART.md) 和 [docs/INDEX.md](docs/INDEX.md)。

### 🌐 集群模式（Hub + Agent）

```bash
# 终端 1: 启动 Hub（启用 K8s 控制器）
cargo run -p ark-hub --release -- --enable-k8s-controller

# 终端 2: 启动 Agent 并连接到 Hub
cargo run -p ark --release -- run --hub-url ws://localhost:8080

# 终端 3: 集群级查询和修复
cargo run -p ark --release -- cluster ps --hub http://localhost:8081
cargo run -p ark --release -- cluster why job-1234 --hub http://localhost:8081
cargo run -p ark --release -- cluster fix job-1234 --hub http://localhost:8081

# 诊断与稳定性接口（Hub）
curl 'http://localhost:8081/api/v1/diagnose?job_id=job-1234&window_s=120'
curl 'http://localhost:8081/api/v1/preflight?node_id=node-a&window_s=120'
curl 'http://localhost:8081/api/v1/training_slow?job_id=job-1234&window_s=120'
```

### ☸️ Kubernetes 部署（生产环境推荐）

```bash
# 一键部署到 Kubernetes 集群（包含 RBAC 配置）
kubectl apply -k deploy/

# 查看部署状态
kubectl get pods -n ark-system
kubectl get deployment -n ark-system ark-hub
kubectl get daemonset -n ark-system ark-agent

# 查看 Hub 的 Prometheus Metrics
kubectl port-forward -n ark-system svc/ark-hub 8081:8081
curl http://localhost:8081/metrics

# 验证 RBAC 配置
kubectl get clusterrole ark-hub-controller
kubectl get clusterrolebinding ark-hub-controller-binding
```

**重要**: Hub 默认启用 K8s 控制器，会自动检测硬件故障并隔离节点。详细部署指南请查看 [deploy/README.md](deploy/README.md)。

## 📖 文档

- [文档索引](docs/INDEX.md) - 完整功能说明和专题文档导航
- [快速开始](QUICKSTART.md) - 5 分钟上手指南
- [架构设计](docs/ARCHITECTURE.md) - 系统架构和数据流转详解
- [Kubernetes 部署](deploy/README.md) - 生产级 K8s 部署指南
- [规则引擎](docs/RULES_ENGINE.md) - 声明式规则系统
- [eBPF 网络探针](docs/EBPF_NETWORK_PROBE.md) - 内核级网络监控
- [eBPF CO-RE 实现](ark-probe-ebpf/CO-RE_IMPLEMENTATION.md) - CO-RE 四元组提取指南
- [探针开发](examples/README.md) - 如何开发自定义探针
- [贡献指南](CONTRIBUTING.md) - 开发与提交流程
- [安全策略](SECURITY.md) - 漏洞披露与安全注意事项
- [发布指南](RELEASE.md) - 版本发布流程

## 🛠️ 开发命令

```bash
make fmt
make lint
make test
make helm-lint
make demo
```

### 🧪 RDMA Demo 验收

```bash
# 默认场景：PFC Storm
bash scripts/run-demo.sh

# 场景 2：物理层退化
ARK_RDMA_MOCK_FILE=examples/mock/rdma/events-phy-degradation.jsonl bash scripts/run-demo.sh
```

说明：
- `scripts/run-demo.sh` 会自动构建 `ark/ark-hub`，等待 Hub 就绪后再发起诊断请求。
- 可通过 `ARK_RDMA_MOCK_FILE` 切换 mock 数据集。

## 🏗️ 架构设计

### 系统架构

```mermaid
graph TB
    subgraph "单机节点 (Agent)"
        Probe[探针层<br/>NVML/eBPF/自定义]
        EventBus[事件总线]
        StateGraph[状态图引擎]
        RuleEngine[规则引擎]
        IPC[IPC 服务]
    end
    
    subgraph "全局中控 (Hub)"
        WSServer[WebSocket 服务器]
        GlobalGraph[全局状态图]
        HTTPAPI[HTTP API]
        K8sController[K8s 控制器<br/>自动隔离故障节点]
        Metrics[Prometheus Metrics]
    end
    
    Probe -->|事件流| EventBus
    EventBus -->|处理| StateGraph
    StateGraph -->|匹配| RuleEngine
    StateGraph -->|查询| IPC
    
    EventBus -->|边缘折叠| WSServer
    WSServer -->|更新| GlobalGraph
    GlobalGraph -->|查询| HTTPAPI
    GlobalGraph -->|检测故障| K8sController
    K8sController -->|打污点/驱逐| K8s
    GlobalGraph -->|指标| Metrics
```

详细架构说明请查看 [架构设计文档](docs/ARCHITECTURE.md)

### 核心原则

- **事件引擎为核心**：所有底层信号转化为追加写入的事件流
- **KISS 原则**：单机可运行，拒绝过度设计
- **探针彻底解耦**：核心不包含硬件 SDK，探针通过 stdout 输出 JSONL
- **内存极其克制**：使用 Ring Buffer 和无锁通道处理高频事件

### 数据流转

#### 单机模式

```mermaid
sequenceDiagram
    participant Probe as 探针
    participant EventBus as 事件总线
    participant Graph as 状态图
    participant Rule as 规则引擎
    participant CLI as CLI
    
    Probe->>EventBus: JSONL 事件
    EventBus->>Graph: 更新图
    Graph->>Rule: 匹配规则
    CLI->>Graph: 查询根因
    Graph-->>CLI: 返回结果
```

#### 集群模式

```mermaid
sequenceDiagram
    participant Agent as Agent
    participant Hub as Hub
    participant SRE as SRE
    
    Agent->>Hub: WebSocket 推送事件
    Hub->>Hub: 更新全局图
    SRE->>Hub: HTTP 查询
    Hub-->>SRE: 返回结果
    SRE->>Hub: HTTP 下发命令
    Hub->>Agent: WebSocket 命令
    Agent-->>Hub: 执行结果
```

详细数据流转请查看 [架构设计文档](docs/ARCHITECTURE.md)

### 数据模型

- **8 大原子事件**：计算、传输、存储、进程、错误、拓扑、意图、动作
- **3 大推导边**：Consumes（消耗）、WaitsOn（等待）、BlockedBy（阻塞）


## 📦 项目结构

```
ark-infra/
├── core/                # 共享底座（事件系统、状态图、规则引擎）
│   └── src/
│       ├── event.rs
│       ├── graph.rs
│       └── rules/       # 规则引擎实现（代码）
├── agent/               # 单机节点程序（ark CLI）
│   └── src/
│       ├── main.rs      # CLI 入口
│       ├── ipc.rs       # IPC 服务（Unix Domain Socket / TCP）
│       ├── diag.rs      # AI 诊断模块
│       ├── plugin/      # 探针系统
│       ├── exec/        # 执行引擎
│       └── scene/       # 场景分析器
├── hub/                 # 全局中控（ark-hub）
│   └── src/
│       ├── main.rs      # Hub 主程序
│       ├── metrics.rs   # Prometheus Metrics
│       └── k8s_controller.rs  # K8s 控制器（自动隔离故障节点）
├── ark-probe-ebpf/      # eBPF 网络探针（Rust Aya 框架）
│   ├── ark-probe-ebpf/          # 用户态程序
│   └── ark-probe-ebpf-ebpf/     # 内核态 eBPF 程序
├── rules/               # YAML 规则文件（配置数据）
│   ├── workload-stalled.yaml
│   ├── gpu-oom.yaml
│   ├── network-stall.yaml
│   └── ...              # 其他规则文件
├── examples/
│   ├── ark-probe-nvml.py       # NVIDIA GPU 探针
│   ├── ark-probe-network.py    # 网络探针
│   ├── ark-probe-dummy.py      # 模拟探针
│   └── ark-probe-rdma-mock.py  # RDMA mock 探针
├── scripts/
│   └── run-demo.sh             # Hub + Agent + RDMA mock 端到端演示
└── docs/                # 文档
```

详细架构说明请查看 [docs/WORKSPACE_ARCHITECTURE.md](docs/WORKSPACE_ARCHITECTURE.md)

## 🧭 Workspace 与探针边界

- **一等公民（生产交付）**：根 workspace 仅包含 `core`、`agent`、`hub`（见根目录 `Cargo.toml` 的 `members`）。
- **独立 eBPF 子项目**：`ark-probe-ebpf/` 是独立 Rust workspace 风格项目，不参与根 workspace 的默认 `cargo build --workspace`。
- **示例/参考实现**：`examples/` 下 Python 探针（含 `examples/mock/`）用于演示、联调与回归基线，不是生产二进制交付物。
- **默认构建命令**：`cargo build --workspace --release` 只构建 `core/agent/hub`；如需 eBPF 探针，请进入 `ark-probe-ebpf/` 单独构建。
- **CI 默认覆盖范围**：默认 CI 覆盖根 workspace Rust 检查 + Helm；规则包校验覆盖 `rules/*.yaml` 的结构与冲突检查。

## 🔧 开发

### 前置要求

- Rust 1.70+
- Python 3.7+（用于探针脚本）
- Linux（网络探针需要 `/proc/net`）

### 构建

```bash
# 构建所有项目
cargo build --workspace --release

# 或构建单个项目
cargo build -p ark --release        # agent
cargo build -p ark-hub --release   # hub
cargo build -p ark-core --release  # core
```

### 测试

```bash
# 运行内置探针测试
cargo run -p ark --release -- run

# 测试 GPU 探针（需要 NVIDIA GPU）
pip install pynvml
cargo run -p ark --release -- run --probe examples/ark-probe-nvml.py
```

## 🤝 贡献

欢迎提交 Issue 和 Pull Request！

## 📄 许可证

MIT License

## 🙏 致谢

本项目遵循极简主义设计哲学，致力于为 AI 基础设施提供轻量、高效的监控和诊断能力。

## 📊 核心数据模型

### 8 大原子事件

- **计算域**: `compute.util` (算力利用率), `compute.mem` (显存/内存使用率)
- **传输域**: `transport.bw` (网络吞吐), `transport.drop` (丢包/重传)
- **存储域**: `storage.iops` (存储 IO), `storage.qdepth` (队列深度)
- **进程域**: `process.state` (进程状态)
- **错误域**: `error.hw` (硬件级报错), `error.net` (网络阻塞报错)
- **拓扑域**: `topo.link_down` (NVLink/PCIe 降级)
- **意图域**: `intent.run` (调度器元数据)
- **动作域**: `action.exec` (系统干预动作)

### 3 大推导边

在状态图中，事件转化为 DAG（有向无环图），边只有三种：

1. **Consumes** (消耗)：进程 PID 消耗某物理资源
2. **WaitsOn** (等待)：进程 PID 正在等待某网络/存储资源完成
3. **BlockedBy** (阻塞于)：资源/进程被某个 Error 彻底阻塞（根因）

## 🔗 相关链接

- [GitHub 仓库](https://github.com/osen7/ark-infra)
- [问题反馈](https://github.com/osen7/ark-infra/issues)
- [功能建议](https://github.com/osen7/ark-infra/issues/new)
