# Ark Compatibility Matrix

## Hard Requirements（必须满足）

| 组件 | 基线 | 说明 |
| --- | --- | --- |
| Linux Kernel（基础模式） | 5.4+ | 运行 `agent/hub + mock probe` 的最低建议 |
| Rust | 1.70+ | 构建与测试基线 |
| Python | 3.7+ | `examples/*.py` 探针基线 |
| Kubernetes（集群部署） | 1.20+ | `deploy/` 清单按此基线设计 |

## Recommended（建议满足）

| 组件 | 建议 | 说明 |
| --- | --- | --- |
| Linux Kernel（eBPF/CO-RE） | 5.10+ 且具备 BTF | eBPF 可移植性和稳定性更好 |
| NVIDIA Driver / NVML | 驱动提供 NVML | `ark-probe-nvml.py` 必需 |
| CUDA | 与驱动匹配 | Ark 不强依赖 CUDA，但训练栈通常依赖 |
| RDMA / OFED | 与内核/网卡驱动一致 | RDMA probe 与诊断场景建议 |

## Unsupported / Not Guaranteed（不保证）

| 场景 | 状态 | 说明 |
| --- | --- | --- |
| 无 BTF 的 eBPF CO-RE 环境 | Not guaranteed | 可能因符号/类型差异导致 attach 失败 |
| 低于基线的 K8s 版本 | Unsupported | RBAC / Eviction 行为不保证 |
| 与驱动不匹配的 NVML/CUDA 组合 | Unsupported | 可能无法读取 GPU 指标 |

## Fallback Strategy

- eBPF 不满足条件时，回退到 `examples/mock/` 验证诊断链路。
- 生产环境建议先用 `dry-run`（默认）完成灰度后再开启执行模式。
