# Ark 执行计划（6 周）

更新日期：2026-03-05

## 目标

在不引入大规模重构的前提下，完成 RDMA 场景的最小生产闭环：

- 可采集
- 可诊断
- 可执行（含 dry-run）
- 可回归

## 执行原则

- 先闭环后扩展：优先打通 mock -> diagnose -> policy -> action 链路
- 默认安全：所有自动化动作先支持 `dry-run`
- 小步快跑：每周必须有可验证产物（测试、接口、演示）

## 周计划

### Week 1

- 固化事件契约与命名约定（文档 + 样例）
- 新增 `examples/mock/rdma/` 基线数据
- 为 `core` 增加契约测试

完成标准：

- `ark-core` 测试通过
- mock 事件可被 `agent` 消费

### Week 2

- 实现 RDMA probe（先 mock，再真实命令适配）
- 输出 `transport.drop`、`error.net`、`transport.bw`
- 增加解析单测

完成标准：

- 无 RDMA 环境也能稳定演示
- probe 错误可观测（metrics/log）

### Week 3

- Hub 接入规则评估路径
- 新增 `rdma-pfc-storm.yaml`
- 新增 `rdma-physical-degradation.yaml`

完成标准：

- mock 事件触发规则命中
- 接口返回命中结果与证据摘要

### Week 4

- 增加结构化诊断 API（或扩展 `/api/v1/why`）
- 返回 `matched_rules`、`solution_steps`

完成标准：

- API 输出结构稳定
- 集成测试可断言

### Week 5

- 新增 Policy 层（Diagnosis -> Action Plan）
- 实现 `dry-run` 与 `execute` 双模式

完成标准：

- dry-run 不执行动作，仅返回计划
- execute 可走 fix 或 K8s 控制器

### Week 6

- 建立 CI（fmt/clippy/test）
- 增加最小 Helm chart
- 文档与贡献流程收敛

完成标准：

- CI 全绿
- Helm lint/template 通过

## 里程碑 KPI（验收指标）

- 规则命中准确率（mock 基线）：>= 95%
- 关键接口稳定性（diagnose/why/fix）：集成测试通过率 100%
- 回归时长：`cargo test --workspace` 在 CI 中可稳定完成
- 干跑覆盖率：所有策略动作均支持 dry-run 输出
- 演示可复现性：无 RDMA 环境可在本地完整跑通

## 质量门槛

- 规则变更必须附回归样例
- 涉及动作执行的改动必须先支持 dry-run
- 新增 probe 必须提供 mock 模式

## Not Now（本阶段不做）

- 不做大规模目录重构（如一次性迁移到 `crates/*`）
- 不引入完整 CRD/operator 体系
- 不并行推进 5 个以上 killer feature
- 不在缺少回归测试前开启默认自动执行

## 最小验收清单

- [x] `cargo test --workspace` 通过
- [x] hub + agent + mock probe 集成测试通过
- [x] 至少 2 条 RDMA 规则可命中
- [x] 诊断 API 返回结构化结果
- [x] dry-run 输出动作计划
