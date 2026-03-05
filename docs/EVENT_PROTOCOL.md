# Agent-Hub Event Protocol

## 版本策略

- `protocol_version`：协议版本（当前 `1.0`）
- `schema_version`：事件 payload 语义版本（当前 `1.0`）
- `feature_flags`：能力标识（如 `edge_rollup`）
- 向后兼容：Hub 同时接受 `Envelope` 和旧版裸 `Event`。

## 消息格式（当前）

```json
{
  "kind": "event",
  "protocol_version": "1.0",
  "schema_version": "1.0",
  "agent_id": "cluster-a/node-01/agent-0",
  "feature_flags": ["edge_rollup"],
  "event": {
    "ts": 1741234567890,
    "event_type": "transport.drop",
    "entity_id": "roce-mlx5_0",
    "job_id": "job-42",
    "pid": 1234,
    "value": "12.5",
    "node_id": "node-a"
  }
}
```

`kind` 预留扩展（如 `hello`/`heartbeat`/`ack`/`error`），当前实现处理 `kind=event`。

## Event 字段约定

- `ts`：毫秒时间戳（必填）
- `event_type`：事件类型（必填，命名空间风格，如 `compute.util`）
- `entity_id`：资源实体标识（必填）
- `job_id`：任务标识（可选）
- `pid`：进程标识（可选）
- `value`：载荷字符串（必填）
- `node_id`：节点标识（可选；Hub 端可补全）

## 兼容性原则

- 新字段默认可选，避免破坏旧 Agent。
- 协议升级先增加 `feature_flags`，再考虑提升 `protocol_version`。
- `protocol_version` 负责封包框架，`schema_version` 负责 `event` 字段语义演进。
