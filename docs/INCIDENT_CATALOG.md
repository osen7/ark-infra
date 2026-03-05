# Ark Incident Catalog (v1)

本目录定义 Ark 在大规模 AI infra 集群中的统一事故模型，目标是让规则库可以持续扩展到 80-120 个 scenes 而不失控。

## 1. 模型

Ark 使用三层模型：

1. `Event`: 探针原始事实（eBPF/NVML/K8s/Storage 等）
2. `Signal`: 经过窗口聚合和降噪后的稳定信号
3. `Scene`: 对外诊断结果（可用于告警、工单、自动化动作）

## 2. Taxonomy

v1 按层级组织 scenes：

- `hardware`
- `interconnect`
- `network`
- `runtime`
- `scheduler`
- `storage`
- `cluster`

命名规范：`layer.domain.issue`

示例：

- `hardware.gpu.ecc_error`
- `network.rdma.latency_spike`
- `runtime.nccl.hang`
- `cluster.cascade_failure`

## 3. Scene Schema

每条规则建议字段：

```yaml
id: network.tcp.retransmit_burst.v1
scene: network.tcp.retransmit_burst
layer: network
severity: medium
blast_radius: node
reason_codes:
  - TCP_RETRANSMIT_BURST
signals:
  - tcp_retransmit_rate
conditions:
  - signal: tcp_retransmit_rate
    op: ">"
    value: 0.05
actions:
  - recommend.check_network
```

当前实现中，`layer/severity/blast_radius/signals` 通过目录和规则元数据逐步补齐。

## 4. Rule Packs

目录结构：

- `rules/hardware/`
- `rules/interconnect/`
- `rules/network/`
- `rules/runtime/`
- `rules/scheduler/`
- `rules/storage/`
- `rules/cluster/`

每个 pack 可独立演进，并通过 `rules/manifest.yaml` 管理启用状态。

## 5. Seed Scenes (v1)

本轮新增 12 条 seed rules：

- `hardware.gpu.ecc_error`
- `hardware.gpu.throttle`
- `interconnect.nvlink.error`
- `interconnect.pcie.link_degraded`
- `network.rdma.latency_spike`
- `network.tcp.retransmit_burst`
- `runtime.nccl.hang`
- `runtime.container.crashloop`
- `scheduler.k8s.node_not_ready`
- `scheduler.gpu.fragmentation`
- `storage.checkpoint.io_timeout`
- `cluster.cascade_failure`

## 6. Root-Cause Correlation (Hub)

Hub 侧建议维护跨节点因果链：

`network.tcp.retransmit_burst -> runtime.nccl.hang -> cluster.training_stall`

输出至少包含：

- `scene`
- `root_cause_scene`
- `reason_codes`
- `evidence_chain`

## 7. Action Taxonomy

建议动作分级：

- `low`: annotate/label/recommend
- `medium`: cordon/restart single pod
- `high`: drain/kill job

默认 `recommend`，`execute` 必须显式开关并具备审计、冷却和并发限制。

## 8. Evolution Plan

1. 从 12 条 seed scenes 扩展到 40+
2. 引入 scene-level 信号定义和拓扑约束
3. Hub 增加 `/api/v1/scenes` 与 `/api/v1/incidents`
4. 完成 80-120 scenes catalog
