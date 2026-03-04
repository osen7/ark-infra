# Ark 工程化开发计划（12 周）

更新日期：2026-03-05

## 1. 目标

围绕大规模 AI 训练集群的稳定性痛点，建立可复现、可回归、可控自动化的闭环能力：

- 观测：稳定采集关键故障与性能信号
- 诊断：结构化 RCA（根因归因）
- 策略：Diagnosis -> Policy -> ActionPlan
- 执行：默认 dry-run，逐步放开 execute
- 回归：每次变更可通过自动化验证

## 2. 工程原则

- 先接口后实现：先固化协议与错误码，再扩功能
- 小步迭代：每周必须有可验收交付物
- 默认安全：自动动作必须先支持 dry-run
- 反对大重写：优先增量改造，避免推倒重来
- 以测试为准：规则和策略改动必须有回归样例

## 3. 范围边界

### In Scope

- RDMA/RoCE 稳定性链路（pfc_storm / phy_degradation）
- Hub 诊断接口结构化输出
- Policy 层与动作计划
- 集成测试、CI、最小 Helm 交付

### Out of Scope（本周期）

- 全量 CRD/operator 体系
- 一次性目录重构到新 workspace 结构
- 同时推进 5 个以上 killer feature

## 4. 12 周分期计划

### Phase A（Week 1-2）：协议与回归基线

交付：

- 固化 Event/Diagnosis/ActionPlan 的字段与语义
- 增加 `examples/mock/rdma/` 基线数据
- 建立 core 侧契约测试（serde + schema-like 校验）

验收：

- `cargo test -p ark-core` 通过
- mock 数据可被 agent 消费并进入 hub

### Phase B（Week 3-5）：RDMA 诊断主链路

交付：

- RDMA probe（mock 优先，真实命令适配次之）
- 两条规则：`rdma-pfc-storm`、`rdma-physical-degradation`
- Hub 返回结构化诊断结果（含证据摘要）

验收：

- 给定 mock 事件可稳定命中目标规则
- 诊断 API 返回 `matched_rules`、`evidence`、`solution_steps`

### Phase C（Week 6-8）：Policy 与可控执行

交付：

- 新增 policy 模块：Diagnosis -> ActionPlan
- 支持 `dry-run` 和 `execute` 双模式
- 动作分级：notify -> fix -> taint/evict

验收：

- dry-run 仅输出计划，不执行
- execute 模式可触发已有 fix/K8s 控制路径

### Phase D（Week 9-10）：训练稳定性增强

交付：

- Preflight Gate（训练前健康准入）
- Slow Training Analyzer（comm/io/cpu 归因）

验收：

- 新作业可在准入阶段拦截明显坏节点
- 慢训练可输出至少 3 类可解释归因

### Phase E（Week 11-12）：工程化收口

交付：

- CI：fmt/clippy/test/integration
- 最小 Helm Chart 与部署文档
- 贡献规范与发布流程文档

验收：

- CI 全绿
- Helm lint/template 通过
- README + docs 可独立完成 demo

## 5. 每周执行模板

每周必须包含以下固定产物：

- 代码变更（最小可运行增量）
- 测试（单元或集成）
- 文档更新（行为变更说明）
- 回滚说明（失败时如何回退）

## 6. KPI（结果指标）

- 规则命中准确率（mock 基线）：>= 95%
- 关键 API 稳定性：集成测试通过率 100%
- 诊断时延（mock 场景）：持续下降并可观测
- 自动动作安全性：dry-run 覆盖率 100%
- 稳定性收益：训练中断率/慢作业占比下降

## 7. 风险与控制

- 采集输出异构（驱动/固件差异）
  - 控制：解析映射层 + mock 回放基线
- 规则漂移导致误报
  - 控制：规则改动必须附回归样例
- 自动化动作误伤
  - 控制：默认 dry-run + 分级放权 + 审计日志

## 8. 里程碑验收命令（建议）

- `cargo test -p ark-core`
- `cargo test -p ark-hub`
- `cargo test -p ark --tests`
- `cargo test --workspace`

说明：当前仓库若缺少相关测试目标，按阶段逐步补齐。

## 9. 结束状态定义（Definition of Done）

满足以下条件即视为本计划完成：

- RDMA 两条核心规则可稳定命中并输出结构化诊断
- Policy 层可生成动作计划，dry-run 全覆盖
- execute 模式可安全调用既有动作通路
- CI 与集成测试可稳定回归
- 文档可支持外部用户复现 demo
