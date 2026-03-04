# ark-infra Helm Chart

部署组件：

- `ark-hub`（Deployment + Service）
- `ark-agent`（DaemonSet）
- Hub RBAC（ServiceAccount/ClusterRole/ClusterRoleBinding，可选）

快速验证：

```bash
helm lint charts/ark-infra
helm template ark charts/ark-infra --namespace ark-system >/tmp/ark.yaml
```
