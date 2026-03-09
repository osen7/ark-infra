# API Schemas (Current Version)

本页定义当前版本最常用 HTTP API 的稳定 JSON 字段，用于平台对接与 CI 校验。

## GET `/api/v1/health`

用途：Hub 健康、规则加载和 WAL 状态检查。

关键字段：

- `status`: `ok`
- `timestamp_ms`: 当前毫秒时间戳
- `graph.nodes_total` / `graph.edges_total`
- `connections.agents_connected`
- `rules.loaded` / `rules.skipped` / `rules.legacy`
- `wal.path`
- `wal.active_exists` / `wal.active_size_bytes` / `wal.active_last_modified_ms`
- `wal.rotated_path` / `wal.rotated_exists` / `wal.rotated_size_bytes`

## GET `/api/v1/why?job_id=<id>`

用途：按 job 进行根因查询。

关键字段：

- `job_id`
- `causes`: string array
- `processes`: array

`processes` 元素字段：

- `node_id`
- `pid`
- `node_id_full`

## GET `/api/v1/incidents?window_s=<sec>&limit=<n>`

用途：在时间窗口内聚合集群事故。

关键字段：

- `status`: `ok`
- `timestamp_ms`
- `window_s`
- `total_events`
- `incidents`: array

`incidents` 元素字段：

- `scene`
- `rule`
- `job_id` (nullable)
- `nodes`: string array
- `priority`
- `severity`: `critical|high|medium|low`
- `event_count`
- `root_cause`

## GET `/api/v1/diagnose?job_id=<id>&window_s=<sec>&execute=<bool>`

用途：规则匹配 + 策略下发（默认 dry-run）。

关键字段：

- `job_id`
- `window_s`
- `event_count`
- `matched_rules`
- `processes`
- `policy`
- `dry_run`
- `execute_requested`
- `execute_enabled`
- `request_id`
- `policy_version`
- `execution_guard.max_actions`
- `execution_guard.cooldown_s`
- `execution_guard.truncated`
- `execution_guard.concurrency_limited`
- `execution`
