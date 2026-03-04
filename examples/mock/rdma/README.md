# RDMA Mock Data

本目录提供 RDMA 场景的最小可复现事件流，用于本地开发和集成测试。

## 文件

- `events-baseline.jsonl`: 健康基线事件（无故障）
- `events-pfc-storm.jsonl`: PFC 风暴场景（`error.net=pfc_storm`）
- `events-phy-degradation.jsonl`: 物理层退化场景（`error.net=phy_degradation`）

## 使用方式

1. 用自定义 mock probe 读取 JSONL 并逐行输出到 stdout。
2. 启动 agent 并指定该 probe。
3. 通过 hub API 验证规则命中与诊断输出。

示例事件字段遵循 `ark_core::event::Event` 契约，可直接用于回归测试。
