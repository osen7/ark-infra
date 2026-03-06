# Ark Workspace 架构

## 📦 项目结构

```
ark-infra/
├── Cargo.toml              # Workspace 根配置
│
├── core/                   # 共享底座（ark-core）
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs          # 重新导出常用类型
│       ├── event.rs        # 事件系统（Event, EventType, EventBus）
│       ├── graph.rs        # 状态图引擎（StateGraph, Edge, Node）
│       └── rules/          # 规则引擎（RuleEngine, Rule, Matcher）
│
├── agent/                  # 单机节点程序（ark）
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs         # CLI 入口（run, ps, why, zap, diag, fix）
│       ├── plugin/         # 探针插件系统
│       ├── exec/           # 执行引擎（FixEngine, ActionExecutor）
│       ├── ipc.rs          # IPC 通信（Unix Domain Socket / TCP）
│       ├── diag.rs         # AI 诊断
│       └── scene/          # 场景分析器
│
└── hub/                    # 全局中控（ark-hub）
    ├── Cargo.toml
    └── src/
        └── main.rs         # Hub 服务器（WebSocket，全局图）
```

## 🎯 设计原则

### 1. 共享底座（Core）

**职责**：
- 定义事件系统（Event, EventType）
- 实现状态图引擎（StateGraph）
- 实现规则引擎（RuleEngine）

**特点**：
- 无平台依赖（纯 Rust）
- 无外部依赖（仅标准库和 tokio）
- 可被 agent 和 hub 共同使用

### 2. 单机节点（Agent）

**职责**：
- 运行探针（eBPF, NVML, 自定义脚本）
- 维护本地状态图
- 提供 CLI 接口（run, ps, why, zap, diag, fix）
- 通过 IPC 提供服务

**特点**：
- 依赖 core 共享底座
- Linux-only（Unix Socket IPC）
- 可选的 Hub 连接（边缘上报）

### 3. 全局中控（Hub）

**职责**：
- 接收各节点的 WebSocket 连接
- 维护全局状态图（GlobalGraph）
- 提供跨节点查询接口
- 执行集群级修复操作

**特点**：
- 依赖 core 共享底座
- 无状态设计（内存图）
- 轻量级（无外部数据库）

## 🔗 依赖关系

```
hub ──┐
      ├──> core (共享底座)
agent ─┘
```

- **hub** 和 **agent** 都依赖 **core**
- **hub** 和 **agent** 之间无直接依赖
- 通过 WebSocket 协议通信

## 🧭 Workspace 边界与交付范围

- 根 `Cargo.toml` 的 workspace `members` 当前仅包含：`core`、`agent`、`hub`。
- `ark-probe-ebpf/` 是独立 eBPF 子项目，不在根 workspace 默认构建/测试范围内。
- `examples/`（含 `examples/mock/`）用于探针示例与回归数据，不是生产二进制交付件。
- `cargo build --workspace` 和默认 CI 的 Rust job 仅覆盖根 workspace；eBPF 子项目需进入其目录单独构建。

## 📡 通信协议

### Agent → Hub

**WebSocket 消息格式**：
```json
{
  "node_id": "node-01",
  "event": {
    "ts": 1234567890,
    "event_type": "transport.drop",
    "entity_id": "network-eth0",
    "pid": 12345,
    "value": "1"
  }
}
```

### Hub → Agent

**命令格式**：
```json
{
  "command": "fix",
  "target": "job-llm-train-99",
  "actions": ["signal", "kill"]
}
```

## 🚀 下一步

1. **Hub WebSocket 服务器**
   - 实现事件接收
   - 维护全局图
   - 提供查询接口

2. **Agent 边缘上报**
   - 添加 `--hub` 参数
   - 实现事件过滤
   - 实现 WebSocket 客户端

3. **集群级命令**
   - `ark cluster why <job-id>`
   - `ark cluster fix <job-id>`
