# Ark 探针示例

本目录包含 Ark 的各种探针实现示例。

## 探针列表

### 1. ark-probe-nvml.py（生产级）

真实的 NVIDIA GPU 监控探针，使用 NVML API 抓取：
- GPU 利用率
- 显存使用率
- GPU 温度
- 功耗信息
- 硬件错误（ECC、XID 等）
- 进程关联（自动识别使用 GPU 的进程）

**依赖**: `pynvml` (pip install pynvml)

**使用**:
```bash
pip install pynvml
cargo run -- run --probe examples/ark-probe-nvml.py
```

### 2. ark-probe-network.py（网络监控）

基于 `/proc/net` 的网络探针，监控：
- 网络接口带宽使用
- 丢包和网络错误
- 进程的网络 I/O 阻塞
- 自动建立 WaitsOn 关系

**要求**: Linux 系统（需要 `/proc/net`）

**使用**:
```bash
cargo run -- run --probe examples/ark-probe-network.py
```

**环境变量**:
- `XCTL_NETWORK_INTERVAL`: 采样间隔（秒），默认 2.0

### 3. eBPF 网络探针（生产级）

使用 Rust Aya 框架实现的 eBPF 网络探针，提供内核级网络监控。

**位置**: `../ark-probe-ebpf/`（独立项目）

**文档**: 
- [eBPF 网络探针文档](../docs/EBPF_NETWORK_PROBE.md)
- [eBPF 探针集成指南](EBPF_PROBE_INTEGRATION.md)

### 4. ark-probe-dummy.py（测试用）

模拟探针，用于测试和演示，生成随机事件。

**使用**:
```bash
cargo run -- run --probe examples/ark-probe-dummy.py
```

### 5. mock/rdma（回归基线数据）

用于 RDMA 规则和诊断链路测试的 JSONL 样例：
- `examples/mock/rdma/events-baseline.jsonl`
- `examples/mock/rdma/events-pfc-storm.jsonl`
- `examples/mock/rdma/events-phy-degradation.jsonl`

这些样例可被 mock probe 逐行输出，验证 agent/hub 端到端行为。

### 6. ark-probe-rdma-mock.py（RDMA 回放探针）

把 `examples/mock/rdma/*.jsonl` 逐行回放到 stdout，用于本地联调和集成测试。

**使用**:
```bash
cargo run -- run --probe examples/ark-probe-rdma-mock.py
```

可选参数：
- `--interval 0.5` 控制回放间隔（秒）
- `--loop` 循环回放

由于 `ark run --probe` 当前只接收脚本路径，如需切换文件/间隔请使用环境变量：
- `ARK_RDMA_MOCK_FILE`（默认 `examples/mock/rdma/events-pfc-storm.jsonl`）
- `ARK_RDMA_MOCK_INTERVAL`（默认 `0.5`）
- `ARK_RDMA_MOCK_LOOP`（`1/true` 开启循环）

## 探针开发指南

### 探针接口规范

探针脚本必须：

1. **输出格式**: JSONL（每行一个 JSON 对象）
2. **事件格式**: 符合 `Event` 结构体定义
3. **事件类型**: 使用蛇形小写加点格式（如 `compute.util`）

### 事件示例

```json
{
  "ts": 1234567890123,
  "event_type": "compute.util",
  "entity_id": "gpu-00",
  "job_id": null,
  "pid": 12345,
  "value": "85"
}
```

### 支持的事件类型

- `compute.util` - 算力利用率
- `compute.mem` - 显存/内存使用率
- `transport.bw` - 网络吞吐
- `transport.drop` - 丢包/重传
- `storage.iops` - 存储 IO
- `storage.qdepth` - 队列深度
- `process.state` - 进程状态
- `error.hw` - 硬件级报错
- `error.net` - 网络阻塞报错
- `topo.link_down` - 拓扑降级
- `intent.run` - 调度器元数据
- `action.exec` - 系统干预动作

### 探针最佳实践

1. **错误处理**: 探针崩溃不会影响 Ark 主进程，但应该优雅处理错误
2. **心跳机制**: 即使数据不变，也要定期发送状态事件（防止节点被清理）
3. **性能**: 避免阻塞操作，保持低延迟
4. **日志**: 错误信息输出到 stderr，正常事件输出到 stdout

### 开发新探针

参考 `ark-probe-nvml.py` 的实现：

```python
#!/usr/bin/env python3
import json
import time
import sys

def main():
    try:
        while True:
            # 收集数据
            events = collect_events()
            
            # 输出 JSONL 格式
            for event in events:
                print(json.dumps(event, ensure_ascii=False))
                sys.stdout.flush()
            
            time.sleep(1.0)  # 采样间隔
    except KeyboardInterrupt:
        pass
    except BrokenPipeError:
        pass  # 父进程关闭管道

if __name__ == "__main__":
    main()
```
