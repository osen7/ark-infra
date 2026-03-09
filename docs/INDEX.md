# Ark 文档索引（当前版本）

只保留可直接用于当前版本部署、运行、排障的文档。

## 入门与使用

- [主 README](../README.md) - 项目概览与常用命令
- [quickstart.md](quickstart.md) - 最小可用运行路径（单机/集群/eBPF）
- [快速开始（兼容）](../QUICKSTART.md) - 旧版快速开始
- [Kubernetes 部署](../deploy/README.md) - 生产部署与权限说明
- [COMPATIBILITY_MATRIX.md](COMPATIBILITY_MATRIX.md) - 版本兼容矩阵

## 架构与实现

- [ARCHITECTURE.md](ARCHITECTURE.md) - 系统架构与数据流
- [WORKSPACE_ARCHITECTURE.md](WORKSPACE_ARCHITECTURE.md) - workspace 边界与职责
- [RULES_ENGINE.md](RULES_ENGINE.md) - 规则格式与匹配机制
- [SIGNALS.md](SIGNALS.md) - Signal Layer 设计与条件写法
- [INCIDENT_CATALOG.md](INCIDENT_CATALOG.md) - AI Infra 事故 taxonomy 与 seed scenes
- [EBPF_NETWORK_PROBE.md](EBPF_NETWORK_PROBE.md) - eBPF 网络探针说明
- [EVENT_PROTOCOL.md](EVENT_PROTOCOL.md) - agent↔hub 事件协议与版本策略
- [API_SCHEMAS.md](API_SCHEMAS.md) - Hub HTTP API JSON 字段契约
- [PERFORMANCE.md](PERFORMANCE.md) - 性能基线与压测入口

## 生态与维护

- [探针示例](../examples/README.md) - 示例探针与 mock 数据
- [eBPF 子项目](../ark-probe-ebpf/README.md) - 独立 eBPF 子项目文档
- [贡献指南](../CONTRIBUTING.md)
- [安全策略](../SECURITY.md)
- [发布指南](../RELEASE.md)
