# Ark 快速开始指南

## 🚀 5 分钟上手真实 GPU 监控

### 前置条件

1. **Rust 工具链**（如果还没有安装）:
   ```bash
   # Windows (使用 rustup)
   # 访问 https://rustup.rs/
   
   # Linux/Mac
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Python 3.7+** 和 pynvml:
   ```bash
   pip install pynvml
   ```

3. **NVIDIA GPU** 和驱动（用于 NVML 探针）

### 步骤 1: 构建项目

```bash
cd ark-infra
cargo build --release
```

### 步骤 2: 启动 Daemon（使用真实 GPU 探针）

```bash
# 在终端 1
cargo run -p ark --release -- run --probe examples/ark-probe-nvml.py
```

你应该看到：
```
[ark] 启动事件总线...
已检测到 X 个 GPU，开始监控...
[ark] 探针已启动，状态图已初始化
[ark] IPC 服务器已启动，监听端口 9090
[ark] 按 Ctrl+C 退出
```

### 步骤 3: 查询进程列表

在另一个终端中：

```bash
# 在终端 2
cargo run -p ark --release -- ps
```

你应该看到类似输出：
```
      PID |      JOB_ID |            RESOURCES |  STATE
     1234 |     job-567 | gpu-00, gpu-01 | running
     5678 |     job-890 | gpu-02 | running
```

### 步骤 4: 分析进程阻塞根因

```bash
cargo run -p ark --release -- why 1234
```

### 步骤 5: 强制终止进程（如果需要）

```bash
cargo run -p ark --release -- zap 1234
```

## 🧪 测试模式（无 GPU 环境）

如果没有 NVIDIA GPU，可以使用模拟探针：

```bash
cargo run -p ark --release -- run --probe examples/ark-probe-dummy.py
```

## 📊 验证探针工作

### 方法 1: 查看 daemon 输出

daemon 启动后，你应该看到事件不断输出（如果启用了详细日志）。

### 方法 2: 运行 GPU 工作负载

在另一个终端运行 GPU 任务（如 `nvidia-smi` 或训练脚本），然后运行：

```bash
cargo run -p ark --release -- ps
```

你应该看到进程出现在列表中，并且 `RESOURCES` 列显示它使用的 GPU。

### 方法 3: 检查 IPC 连接

```bash
# 测试 ping
telnet 127.0.0.1 9090
# 或使用 curl（需要手动构造请求）
```

## 🔧 故障排查

### 问题: "无法连接到 daemon"

**原因**: daemon 未运行或端口不匹配

**解决**: 
1. 确保 daemon 正在运行（步骤 2）
2. 检查端口是否正确（默认 9090）
3. 使用 `--port` 参数指定端口

### 问题: "NVML 初始化失败"

**原因**: 
- 未安装 NVIDIA 驱动
- 没有 NVIDIA GPU
- pynvml 未正确安装

**解决**:
1. 检查 `nvidia-smi` 命令是否可用
2. 重新安装 pynvml: `pip install --upgrade pynvml`
3. 使用模拟探针进行测试

### 问题: "未检测到 NVIDIA GPU"

**原因**: 系统没有 NVIDIA GPU 或驱动未加载

**解决**: 使用模拟探针或检查 GPU 驱动

## 🎯 下一步

- 查看 [docs/INDEX.md](docs/INDEX.md) 了解完整功能文档导航
- 查看 [examples/README.md](examples/README.md) 了解探针开发
- 准备进入路线 B（eBPF 网络探针）或路线 C（大模型诊断）
