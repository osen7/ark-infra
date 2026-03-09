# Performance Baseline

本页提供 Ark 当前版本的最小性能基线流程，目标是让每次优化都可量化回归。

## 1) 图引擎压测（本地）

```bash
cargo run -p ark-core --bin graph-stress -- --events 100000 --resources 8 --pids 1024
```

示例输出字段：

- `graph_stress.events`
- `graph_stress.elapsed_ms`
- `graph_stress.events_per_sec`
- `graph_stress.nodes_total`
- `graph_stress.edges_total`
- `graph_stress.edges_by_type.*`

也可通过 `make stress` 运行同等命令。

## 2) 回归建议

- 对比同一硬件上的 `events_per_sec` 与 `elapsed_ms`
- 关注 `edges_total` 是否稳定受控（避免无界增长）
- 修改 `StateGraph`/规则匹配后，至少复跑一次基线

## 3) 大规模阶段建议（后续）

- 按 `1e5 -> 5e5 -> 1e6` 事件阶梯压测
- 分离单节点事件与多节点混合事件分布
- 记录 `p95/p99` 处理时延与内存峰值
