# Ark Infra 深度调研与落地计划（清洗版）

更新日期：2026-03-05

## 1. 执行摘要

Ark Infra 已具备完整的技术主链路：

- 统一事件模型（Event）
- 因果状态图（StateGraph）
- YAML 规则引擎（RuleEngine）
- Agent 执行动作（fix/zap 等）
- Hub 集群汇聚与查询（WebSocket + HTTP API）
- 基础自动化控制（K8s taint + eviction）

这意味着项目已经跨过“概念验证”阶段，进入“工程化与生态化”阶段。

当前最关键的短板不是架构，而是可复现性与生产信号闭环：

- RDMA/RoCE 生产级信号采集仍不完整
- Hub 侧规则与策略闭环仍偏基础
- 集成测试、CI、可演示路径需要标准化

## 2. 当前状态（As-Is）

本节只描述仓库中已存在能力。

### 2.1 Workspace 与组件

- `core/`：事件模型、状态图、规则引擎
- `agent/`：本机守护与 CLI、IPC、执行器、探针接入
- `hub/`：集群汇聚、HTTP API、K8s 控制器
- `ark-probe-ebpf/`：Aya eBPF 探针子项目
- `rules/`：声明式 YAML 规则库

### 2.2 关键能力

- 事件契约统一：`EventType` + `Event`
- 图推理闭环：`Consumes / WaitsOn / BlockedBy`
- 规则匹配：`event / graph / metric / any / all`
- Agent 可接入外部 JSONL 探针，支持 Hub 转发
- Hub 提供 `/api/v1/ps`、`/api/v1/why`、`/api/v1/fix` 与 `/metrics`
- K8s 控制器可在故障下执行 taint + eviction

### 2.3 已知缺口

- NVML/CANN 原生探针仍是占位实现
- RDMA 场景规则更偏“拥塞”而非“PFC 风暴/物理层退化”
- Hub 规则评估与动作策略尚未形成稳定 policy 层
- 集成测试与一键 demo 路径缺少统一入口

## 3. 目标状态（To-Be）

目标不是新增更多模块，而是把现有能力升级为“可复现、可回归、可扩展”的开源工程。

### 3.1 P0（必须优先）

1. RDMA 可观测性补全（含 mock）
2. Hub 两条硬规则落地（PFC storm / physical degradation）
3. 诊断输出标准化（结构化返回）

### 3.2 P1（紧随其后）

1. Diagnosis -> Policy -> Action 最小闭环
2. 干跑（dry-run）与真实执行双模式
3. 集成测试与 CI 主链路

### 3.3 P2（工程化与生态）

1. Helm Chart 与部署体验统一
2. 文档与贡献模板标准化
3. Release 资产与版本节奏规范

## 4. 建议 PR 拆分

### PR-A：数据契约与样例稳定化（P0）

范围：`core/` + `examples/` + `docs/`

交付：

- 明确事件字段语义与命名约定
- 给出 RDMA 相关事件样例（JSONL）
- 补充 serde round-trip 测试

验收标准：

- `cargo test -p ark-core` 稳定通过
- 样例可被 agent 探针通道直接消费

### PR-B：RDMA Probe（生产/Mock 双模式）（P0）

范围：`agent/src/probe/` + `examples/mock/rdma/`

交付：

- 新增 RDMA 探针实现
- 支持 mock 输入文件驱动
- 输出稳定 `transport.drop` / `error.net` 事件

验收标准：

- 无 RDMA 环境可本地跑通
- 解析与转换逻辑有单测

### PR-C：Hub 规则诊断 API（P0）

范围：`hub/` + `rules/`

交付：

- 新增（或扩展）诊断接口，返回 `matched_rules`
- 落地两条 RDMA 规则：
  - `rdma-pfc-storm`
  - `rdma-physical-degradation`

验收标准：

- 注入 mock 事件可稳定命中规则
- HTTP 返回结构稳定、可断言

### PR-D：Policy 闭环与 dry-run（P1）

范围：`hub/` + `agent/`

交付：

- 规则命中后映射动作计划（policy）
- dry-run 仅输出计划，不执行
- execute 模式调用 fix 命令或 K8s 控制器

验收标准：

- diagnosis -> policy 映射有单测
- dry-run 输出可快照测试

### PR-E：工程化（P1/P2）

范围：`.github/` + `charts/` + `docs/`

交付：

- CI（fmt/clippy/test）
- Helm 模板与最小 values
- 贡献与发布流程文档

验收标准：

- CI 全绿
- Helm 可 lint/template

## 5. 里程碑建议（6 周）

- 第 1-2 周：PR-A + PR-B
- 第 3-4 周：PR-C
- 第 5 周：PR-D
- 第 6 周：PR-E + 文档收敛

## 6. 风险与对策

- 命令行采集输出差异大（驱动/版本差异）
  - 对策：解析层做映射与降级；保留 mock 基线
- 规则语义漂移
  - 对策：以回归测试固定规则命中行为
- 自动化动作风险
  - 对策：默认 dry-run，逐步开放 execute

## 7. 本仓库下一步建议（立即可做）

1. 新增 `examples/mock/rdma/` 与最小事件样例
2. 在 Hub 增加结构化诊断字段（可先扩展 `/api/v1/why`）
3. 为 `rules/` 每条规则补充最小回归样例
4. 建立集成测试脚手架（hub + agent + mock probe）

---

说明：本版本已去除外部对话引用标记，内容按“仓库事实 + 可执行计划”整理，适合作为团队内部执行基线。
