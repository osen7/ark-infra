# Ark 文档索引

## 📚 核心文档（按生命周期）

### 1. 项目规划
- **[ROADMAP.md](ROADMAP.md)** - 项目路线图、开发计划和里程碑
- **[EXECUTION_PLAN.md](EXECUTION_PLAN.md)** - 6 周执行计划（按周里程碑 + 验收标准）
- **[ENGINEERING_PLAN.md](ENGINEERING_PLAN.md)** - 12 周工程化计划（KPI + 风险控制 + DoD）
- **[深度调研（清洗版）](../deep-research-report.md)** - 现状与落地计划（As-Is / To-Be）

### 2. 架构设计
- **[ARCHITECTURE.md](ARCHITECTURE.md)** - 系统架构设计文档
  - 系统架构总览（Mermaid 图表）
  - 数据流转图（单机/集群模式）
  - 核心组件详解
  - 组件交互图
  - 数据模型定义
  - 部署架构
  - 安全设计
  - 性能特性

### 3. 核心功能文档
- **[RULES_ENGINE.md](RULES_ENGINE.md)** - 规则引擎：声明式 YAML 规则系统
  - 规则定义格式
  - 条件匹配逻辑
  - 场景分析集成

- **[EBPF_NETWORK_PROBE.md](EBPF_NETWORK_PROBE.md)** - eBPF 网络探针
  - 内核级网络监控
  - TCP 重传捕获
  - 零侵入监控
  - PID 陷阱修复（软中断上下文问题、Socket 映射解决方案）

- **[WORKSPACE_ARCHITECTURE.md](WORKSPACE_ARCHITECTURE.md)** - Workspace 架构
  - Cargo Workspace 结构
  - 组件职责划分
  - 依赖关系

### 4. 已完成功能
- **[UDS_IPC_MIGRATION.md](UDS_IPC_MIGRATION.md)** - Unix Domain Socket IPC 迁移
  - 生产级 IPC 改造
  - 权限控制
  - 跨平台兼容

### 5. 生产级集成
- **[Kubernetes 部署](../deploy/README.md)** - 生产级 K8s 部署指南
  - RBAC 权限配置
  - DaemonSet/Deployment 配置
  - K8s 控制器集成（自动隔离故障节点）
  - Prometheus Metrics 集成
  - Audit Log 配置

## 🎯 文档组织原则

- **聚焦项目生命周期**：只保留与项目开发、使用、维护直接相关的文档
- **技术深度**：深入核心功能的技术实现细节
- **实用导向**：提供可操作的指南和示例

## 📖 其他位置文档

- [主 README](../README.md) - 项目概览和快速开始
- [快速开始](../QUICKSTART.md) - 5 分钟上手指南
- [探针示例](../examples/README.md) - 探针开发指南
- [eBPF 探针项目](../ark-probe-ebpf/README.md) - eBPF 探针完整文档
- [贡献指南](../CONTRIBUTING.md) - 开发与提交流程
- [安全策略](../SECURITY.md) - 漏洞披露与安全注意事项
- [发布指南](../RELEASE.md) - 版本发布流程
