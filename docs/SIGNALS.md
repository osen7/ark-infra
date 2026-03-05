# Ark Signals (MVP)

Signal Layer 将原始事件聚合为稳定语义，支撑 `Event -> Signal -> Scene`。

## MVP 范围

当前实现优先提供低侵入闭环：

- `network.tcp.retransmit_rate_1m` (CounterRate)
- `hardware.gpu.util_avg_1m` (GaugeAvg)

窗口与步长：

- `window_ms = 60000`
- `step_ms = 1000`

## 数据流

1. Probe 上报 `Event`
2. `StateGraph::process_event` 调用 `SignalEngine::on_event`
3. 输出 `SignalPoint` 并写入图节点：`signal::<signal_name>::<entity>`
4. Rule 使用 `type: signal` 条件匹配

## 规则写法示例

```yaml
conditions:
  - type: signal
    signal: network.tcp.retransmit_rate_1m
    op: gt
    target: "0.03"
    value_type: numeric
```

## 设计约束

- 实体维度 MVP 先支持 `Node`
- 乱序事件与 P99 聚合将在后续迭代
- Hub 侧跨节点信号聚合后置到下一阶段
