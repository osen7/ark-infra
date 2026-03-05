# Ark Kubernetes 部署指南

本目录包含将 `Ark` 部署到 Kubernetes 集群的完整配置。

## 🚀 快速部署

### 前置要求

1. Kubernetes 集群（版本 >= 1.20）
2. `kubectl` 已配置并可以访问集群
3. `ark-hub` 和 `ark` 的 Docker 镜像已构建并推送到镜像仓库

### 一键部署

```bash
# 使用 kustomize 部署（推荐）
kubectl apply -k deploy/

# 或手动部署各个组件
kubectl apply -f deploy/namespace.yaml
kubectl apply -f deploy/hub-deployment.yaml
kubectl apply -f deploy/agent-daemonset.yaml
```

### 验证部署

```bash
# 检查 Hub 状态
kubectl get deployment -n ark-system ark-hub
kubectl get svc -n ark-system ark-hub

# 检查 Agent 状态（应该在每个节点上运行）
kubectl get daemonset -n ark-system ark-agent
kubectl get pods -n ark-system -l app=ark-agent

# 查看 Hub 日志
kubectl logs -n ark-system -l app=ark-hub --tail=50

# 查看 Agent 日志（选择任意一个节点）
kubectl logs -n ark-system -l app=ark-agent --tail=50
```

## 📋 组件说明

### RBAC 配置（最小权限）

**重要**: Hub 需要 Kubernetes API 权限才能执行节点隔离和 Pod 驱逐操作。

- **ServiceAccount**: `ark-hub-sa`（在 `ark-system` 命名空间）
- **ClusterRole**: `ark-hub-controller`
  - 必需（只读+节点操作）：`nodes` 的 `get/list/patch`（查询状态、打污点）
  - 必需（故障定位）：`pods` 的 `get/list`（查询受影响 Pod）
  - 仅在启用自动隔离/驱逐时需要：`pods/eviction` 的 `create`、`pods` 的 `delete`
- **ClusterRoleBinding**: 将 ServiceAccount 绑定到 ClusterRole

如果你的环境只允许“观察和告警”，不做自动驱逐，可将 `pods/eviction:create` 和 `pods:delete` 去掉。

### Hub Deployment

- **服务类型**: ClusterIP（集群内部访问）
- **ServiceAccount**: `ark-hub-sa`（用于 K8s API 调用）
- **端口**:
  - `8080`: WebSocket（Agent 连接）
  - `8081`: HTTP API（CLI 查询）
- **资源限制**: 256Mi-512Mi 内存，100m-500m CPU
- **健康检查**: HTTP GET `/api/v1/ps`
- **K8s 控制器**: 默认启用（通过 `--enable-k8s-controller` 参数）
- **动作执行门禁**: 默认强制 dry-run。仅当显式增加 `--allow-execute` 参数时，`/api/v1/diagnose?execute=true` 才会真正下发动作

### Agent DaemonSet

- **运行模式**: 每个节点一个 Pod
- **特权要求**:
  - `hostNetwork: true` - 访问宿主机网络命名空间
  - `hostPID: true` - 访问宿主机进程命名空间
  - `privileged: true` - 挂载 eBPF 程序
  - `CAP_SYS_ADMIN`, `CAP_NET_ADMIN`, `CAP_BPF` - 内核级操作权限
- **IPC Socket**: `/var/run/ark/ark.sock`（宿主机路径）
- **资源限制**: 128Mi-256Mi 内存，50m-200m CPU

## 🔧 配置自定义

### 修改 Hub 地址

编辑 `agent-daemonset.yaml`，修改 `--hub-url` 参数：

```yaml
args:
  - "run"
  - "--hub-url"
  - "ws://your-hub-service:8080"  # 修改这里
```

### 添加规则和探针

1. 取消注释 `kustomization.yaml` 中的 `configMapGenerator`
2. 将规则文件添加到 `rules/` 目录
3. 将探针脚本添加到 `examples/` 目录
4. 重新应用：`kubectl apply -k deploy/`

### 镜像配置

编辑 `kustomization.yaml`，取消注释 `images` 部分并修改镜像地址：

```yaml
images:
  - name: ark-hub
    newName: registry.example.com/ark-hub
    newTag: v1.0.0
  - name: ark
    newName: registry.example.com/ark
    newTag: v1.0.0
```

## 🐳 构建 Docker 镜像

### Hub 镜像

```bash
# 在项目根目录
docker build -t ark-hub:latest -f deploy/Dockerfile.hub .
# 或使用多阶段构建
docker build -t ark-hub:v1.0.0 \
  --build-arg BINARY=ark-hub \
  -f deploy/Dockerfile .
```

### Agent 镜像

```bash
docker build -t ark:latest -f deploy/Dockerfile.agent .
# 或
docker build -t ark:v1.0.0 \
  --build-arg BINARY=ark \
  -f deploy/Dockerfile .
```

## 📊 使用示例

### 通过 Port-Forward 访问 Hub API

```bash
# 转发 HTTP API 端口
kubectl port-forward -n ark-system svc/ark-hub 8081:8081

# 在另一个终端使用 CLI
ark cluster ps --hub http://localhost:8081
ark cluster why job-1234 --hub http://localhost:8081
```

### 在 Pod 中使用 ark CLI

```bash
# 进入 Agent Pod
kubectl exec -it -n ark-system $(kubectl get pod -n ark-system -l app=ark-agent -o jsonpath='{.items[0].metadata.name}') -- /bin/sh

# 使用本地 IPC
/opt/ark/ark ps
/opt/ark/ark why <PID>
```

## ⚠️ 安全注意事项

1. **特权模式**: Agent 需要特权模式以访问内核资源，请确保：
   - 使用 Pod Security Policy 或 Pod Security Standards 限制
   - 仅在受信任的节点上运行
   - 定期更新镜像以修复安全漏洞

2. **网络隔离**: Hub 使用 ClusterIP，默认只能在集群内部访问。如需外部访问：
   - 使用 Ingress 或 LoadBalancer
   - 配置 TLS/HTTPS
   - 使用 NetworkPolicy 限制访问

3. **资源限制**: 已设置合理的资源限制，可根据实际负载调整
4. **默认干跑**: 建议保持 Hub 不带 `--allow-execute` 运行，在灰度验证稳定后再按变更流程开启执行能力

## 🔍 故障排查

### Hub 无法启动

```bash
# 查看事件
kubectl describe pod -n ark-system -l app=ark-hub

# 查看日志
kubectl logs -n ark-system -l app=ark-hub
```

### Agent 无法连接 Hub

```bash
# 检查 Hub Service
kubectl get svc -n ark-system ark-hub

# 检查 DNS 解析
kubectl run -it --rm debug --image=busybox --restart=Never -- nslookup ark-hub.ark-system.svc.cluster.local

# 检查网络连通性
kubectl exec -n ark-system -l app=ark-agent -- wget -O- http://ark-hub.ark-system.svc.cluster.local:8081/api/v1/ps
```

### Agent 无法访问宿主机进程

确保 DaemonSet 配置了：
- `hostPID: true`
- `hostNetwork: true`
- `privileged: true`

## 📚 相关文档

- [项目 README](../README.md)
- [架构文档](../docs/WORKSPACE_ARCHITECTURE.md)
- [快速开始](../QUICKSTART.md)
