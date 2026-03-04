1️⃣ 完整系统架构图（大厂级）
1.1 逻辑架构（Agent 数据面 / Hub 分析面 / Controller 控制面 / K8s 交互）
1.2 数据流分层（你写 README/架构说明时可以直接用）

数据面（ark-agent）：每节点采集 GPU/RDMA/Kernel/Storage/Process 信号 → 统一转成 Events + Metrics + Snapshots

分析面（ark-hub）：做跨节点相关性（correlation）、规则引擎、异常评分、根因归因（RCA）

控制面（ark-controller）：把诊断结果转成 K8s 可执行动作（taint/cordon/drain/evict/rollout）+ 输出 CRD 状态

调度器交互（K8s Scheduler）：通过 node taints/labels/conditions 让 scheduler 自动绕开坏节点/坏拓扑

2️⃣ 完整 Repo 结构（目录树 + 重构落点）

目标：把你现有 workspace 变成“像生产项目”的结构：CRD / Operator / Helm / e2e / dashboards / docs 都齐全。

2.1 推荐目录树（Rust workspace 友好）
ark-infra/
├── README.md
├── LICENSE
├── SECURITY.md
├── CONTRIBUTING.md
├── CODEOWNERS
├── ROADMAP.md
├── docs/
│   ├── architecture.md
│   ├── design/
│   │   ├── nccl-hang-detector.md
│   │   ├── rdma-congestion.md
│   │   ├── node-quarantine.md
│   │   └── job-analyzer.md
│   ├── runbooks/
│   │   ├── nccl-hang.md
│   │   ├── rdma-pfc-storm.md
│   │   └── gpu-xid-ecc.md
│   └── images/
│       └── architecture.svg
├── crates/                      # Rust workspace crates
│   ├── ark-agent/
│   │   ├── src/
│   │   ├── Cargo.toml
│   │   └── probes/
│   │       ├── gpu/             # NVML/DCGM/XID/ECC
│   │       ├── rdma/            # mlxlink/ethtool/rdma
│   │       ├── kernel/          # eBPF or procfs
│   │       ├── storage/         # lustre/nvme/fs
│   │       └── process/         # pid->cgroup->pod
│   ├── ark-hub/
│   │   ├── src/
│   │   ├── Cargo.toml
│   │   └── engines/
│   │       ├── ingest/          # gRPC/HTTP ingest
│   │       ├── correlate/       # cross-signal join
│   │       ├── rules/           # rule engine
│   │       └── rca/             # root cause attribution
│   ├── ark-controller/
│   │   ├── src/
│   │   ├── Cargo.toml
│   │   └── crds/                # Rust types for CRDs
│   ├── ark-cli/
│   │   ├── src/
│   │   └── Cargo.toml
│   ├── ark-common/
│   │   ├── src/
│   │   └── Cargo.toml           # shared types: Event, Metric, Snapshot, IDs
│   └── ark-ebpf/ (optional)
│       ├── bpf/
│       └── src/
├── api/                         # versioned Kubernetes APIs + CRDs
│   ├── v1alpha1/
│   │   ├── arkdiagnosis_types.go (optional if go) / or yaml-only
│   │   ├── arkpolicy_types.go
│   │   └── zz_generated.deepcopy.go
│   └── crds/
│       ├── arkdiagnoses.yaml
│       ├── arknodehealths.yaml
│       └── arkpolicies.yaml
├── deploy/
│   ├── helm/ark-infra/
│   │   ├── Chart.yaml
│   │   ├── values.yaml
│   │   └── templates/
│   │       ├── agent-daemonset.yaml
│   │       ├── hub-deployment.yaml
│   │       ├── controller-deployment.yaml
│   │       ├── serviceaccount-rbac.yaml
│   │       ├── crds.yaml
│   │       └── servicemonitor.yaml
│   ├── kustomize/
│   │   ├── base/
│   │   └── overlays/
│   │       ├── dev/
│   │       └── prod/
│   └── manifests/               # raw yamls (optional)
├── dashboards/
│   ├── grafana/
│   │   ├── gpu-overview.json
│   │   ├── rdma-health.json
│   │   └── training-diagnosis.json
│   └── alerts/
│       ├── prom-rules.yaml
│       └── alertmanager.yaml
├── examples/
│   ├── kind/                    # local kind cluster demo
│   ├── minikube/
│   └── production/              # sample policies & runbooks
├── test/
│   ├── unit/
│   ├── integration/
│   └── e2e/
│       ├── kind/
│       └── kuttl/ (or ginkgo)
├── scripts/
│   ├── build-images.sh
│   ├── release.sh
│   └── gen-crds.sh
├── .github/
│   ├── workflows/
│   │   ├── ci.yml
│   │   ├── release.yml
│   │   └── security.yml
│   └── dependabot.yml
├── Dockerfile.agent
├── Dockerfile.hub
├── Dockerfile.controller
├── Cargo.toml                    # workspace root
└── Makefile
2.2 重构规则（保证“优雅”）

你现有 workspace：保持 crates 不动，只做拆分归档：agent/hub/controller/cli/common

所有对外契约（Event/Metric/Snapshot/IDs）集中在 ark-common

K8s CRD/YAML 由 api/crds 统一产出；controller 只“消费”这些 CRD（避免散落）

部署只认 deploy/helm（主路径）+ deploy/kustomize（可选），这样开源用户一键装

3️⃣ 5 个 AI Infra Killer Feature 详细技术设计

（重点：NCCL Hang + RDMA 拥塞，我会给“信号→规则→归因→动作→验证”的全链路）

Killer Feature #1：NCCL Hang Detector（跨节点、可归因、可自愈）
目标

训练卡住最常见：集体通信死锁/错配/网络拥塞导致的 hang。你要做到：

不依赖用户改训练代码（最佳）

能把 hang 定位为：GPU / 网络 / 节点 / 某个 rank

给出可执行动作：重启 job / 隔离 node / 降级拓扑

采集信号（Agent）

GPU 侧

gpu_utilization, sm_active, mem_bw（NVML/DCGM）

xid_errors, ecc_errors（dmesg + nvml）

网络侧

RoCE 端口吞吐：tx_bytes/rx_bytes

RDMA counters（见 Feature #2）

进程/容器侧

pid -> cgroup -> pod 映射（/proc + cgroupfs + CRI）

关键进程识别：python, torchrun, deepspeed, mpirun

可选增强（强加分但可后置）

eBPF：抓 sendmsg/recvmsg 或 TCP retrans/latency，用来区分“进程活着但通信断了”

事件模型（ark-common）

NodeSnapshot：每 10s 上报一次节点摘要（GPU/RDMA/CPU/NET）

JobHint：发现训练 pod 时发一次（pod uid、ns、node、container、pids）

HangSuspect：规则触发时上报“疑似 hang”

检测规则（Hub 规则引擎）

你要做的是多信号一致性判断，避免误报（比如数据加载慢）。

核心判定（建议默认 120s 窗口）

GPU idle：训练进程存在，但 GPU util < 5% 持续 > 120s

NET idle：同节点 RDMA/TCP 吞吐在窗口内极低（低于阈值）

Process alive：训练主进程仍在，且 CPU 有一定活跃（排除进程已死）

Cluster correlation：同一个 Job 的多个节点同时满足 idle（强信号）

排除条件

data loader 瓶颈：CPU/IO 高、网络入流明显、GPU util 低

checkpoint/save：存储写入高、GPU util 低

归因（RCA）

Hub 将 hang 分为四类（这是“顶级”关键）

Network Congestion / PFC Storm：RDMA counters 异常（ECN/PFC/FEC/重传）

Single Rank / Single Node Fault：只有某节点异常，其他节点仍有通信流量

GPU Link Fault：XID/ECC/NVLink 错误出现，且 util 掉到 0

Config / Collective mismatch：全局无异常 counters，但 job 全体 idle（需要结合日志/用户侧）

自愈动作（Controller）

动作要“可控”，用 ArkPolicy 控制：

RestartPod：先小动作（只重启 training pod）

QuarantineNode：若判定为 Node Fault（cordon + taint + drain）

JobAbort：明确 hang 且重启无效时（可选）

典型流程

Hub 产出 ArkDiagnosis{type=nccl_hang, job=..., suspected_nodes=[...], rca=...}

Controller 读取 Diagnosis + Policy：

if rca=network → 先 taint 网络异常节点

if rca=gpu_fault → cordon/drain + 标记 ark.ai/quarantined=true

if rca=unknown → 仅重启 pod，不动节点

验证指标（让项目看起来像生产）

ark_diagnosis_total{type="nccl_hang"}

ark_remediation_total{action="restart_pod|cordon"}

ark_false_positive_total（提供手工反馈入口）

Killer Feature #2：RDMA / RoCE Congestion & PFC Storm Detector（可定位到端口/链路）
目标

训练慢/卡/抖，往往不是“带宽不够”，而是：

拥塞控制异常（ECN/DCQCN）

PFC pause 风暴导致 head-of-line blocking

物理层误码（FEC/BER）导致吞吐掉

你要做到：把拥塞定位到端口/链路/节点组合，并给出建议或动作。

采集信号（Agent RDMA Probe）

建议先做到“用户态+shell命令收集”，后续再 eBPF。

mlxlink：SNR、FEC corrected/uncorrected、BER、link downshift

ethtool -S：port counters（pause frames、rx/tx errors）

rdma statistic / perfquery（若可用）：重传/拥塞相关计数

tc -s qdisc（可选）：队列拥塞、丢包

统一上报为：

RdmaPortSnapshot{dev, port, speed, fec_corr, fec_uncorr, pfc_rx, pfc_tx, ecn_marks, tx, rx, errors}

检测链路（Hub）

核心：做 “物理层→链路层→拥塞层→性能层” 的层层归因。

规则 1：PFC Storm

pfc_pause_frames 在窗口内暴涨（阈值：相对 baseline 的倍数，或绝对值）

同时吞吐下降明显（tx/rx 下降）

多节点在同一个 ToR/网段同步出现 → 判定 storm

规则 2：ECN/DCQCN Persistent Congestion

ecn_marks 长时间高位

吞吐未掉但 tail latency 上升（若你有 eBPF RTT/重传就更强）

对应训练侧：AllReduce step time 波动大

规则 3：Physical Degradation（光模块/线缆）

fec_uncorrectable 或 raw_ber 上升

同时 link speed downshift 或 retraining 事件出现

归因：物理层问题（建议更换线/模块）

输出（Diagnosis + Topology Hint）

ArkDiagnosis{type="rdma_congestion", node, dev, port, rca="pfc_storm|ecn_congestion|phy_degradation"}

并输出 suspect_links=[nodeA:portX <-> nodeB:portY]（如果你能拿到 LLDP/拓扑就更炸裂；拿不到也可先做单端口）

动作（Controller）

Taint node：ark.ai/rdma-degraded=true:NoSchedule

Quarantine node：若物理层严重错误（防止训练继续踩坑）

Notify only：默认先告警不自动处理（大厂常见策略）

验证指标

ark_rdma_pfc_storm_total

ark_rdma_phy_degradation_total

ark_rdma_congestion_score（0-100）

Killer Feature #3：GPU XID/ECC Fault → Node Quarantine（生产级安全动作）
目标

AI 集群最怕“隐性坏卡/坏节点”，训练跑几小时突然挂。你要做到：

实时捕捉 XID/ECC

给出“可解释”的隔离原因

自动 cordon/drain，并把节点纳入“隔离池”避免再次调度

信号

dmesg/journalctl 中 NVIDIA XID

NVML ECC counters

温度/功耗异常（可选）

策略

XID 黑名单（比如某些是可忽略，某些必须隔离）

ECC uncorrectable 立即隔离

连续 soft error 达阈值隔离

动作

cordon + taint ark.ai/gpu-fault=true:NoSchedule

drain（可配置是否驱逐非关键 pod）

生成 ArkNodeHealth CR（可视化追踪）

Killer Feature #4：Training Performance Analyzer（把“慢”变成可解释数据）
目标

不只是“挂了”，还要解决“慢”。面试官看到这个会觉得你在做“AI 平台”。

思路（无需侵入训练代码）

识别训练 pod（label/command）

采集每节点 GPU util / NET throughput / RDMA congestion score

计算一个 CommOverheadScore：当 GPU util 低而 NET busy 或 congestion 高时，判为通信瓶颈

输出：

ArkDiagnosis{type="training_slow", rca="comm_bound|io_bound|cpu_bound"}
再给建议：

comm_bound：检查 RDMA、NCCL ring/tree、拓扑

io_bound：检查 Lustre/对象存储延迟

cpu_bound：data loader、num_workers

Killer Feature #5：Cluster Baseline & Drift Detection（“突然变慢/变差”的元凶）
目标

很多生产事故是“配置漂移”：同一型号节点表现不同。你要做到：

自动建立 baseline（每个机型/网卡/交换域）

发现 drift（吞吐掉、错误升）

关联变更（如果能接入变更事件更强）

实现：

Hub 每天对关键指标做 baseline（P50/P90）

新数据偏离 baseline 触发 drift 事件

将 drift 与训练 job 的 slow/hang 关联

附：你可以直接拿去写 README 的“项目卖点段落”

你 README 里可以把 Features 写成这样（非常像大厂开源味）：

GPU Observability: NVML/DCGM metrics + XID/ECC fault detection

RDMA Health & Congestion: PFC/ECN/FEC/BER monitoring with RCA

NCCL Hang Detection: multi-signal correlation across nodes + remediation

Kubernetes Operator: CRDs + policy-based automated remediation

Training Performance Analyzer: explain “slow training” with infra signals
