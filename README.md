# ghboost

GitHub 访问加速 + 免费节点扫描/测速/注入 + 一键部署服务器工具

## 功能特性

- **GitHub 加速**：修改 hosts 文件加速 GitHub 访问
- **节点扫描**：自动从多个订阅源扫描免费代理节点（trojan/vless/ss/vmess）
- **节点测速**：使用 mihomo 内核进行延迟测试
- **节点注入**：自动将最优节点注入 Clash Verge 配置
- **一键部署**：SSH 远程部署代理服务器（VLESS-Reality / VLESS-WS / Trojan / Shadowsocks / Hysteria2）
- **跨平台**：支持 Windows/macOS/Linux/Android/iOS
- **桌面托盘版**：Windows 一键加速 —— 托盘状态灯 + 一个大按钮，不用碰命令行（见 [`ghboost-tray/`](ghboost-tray/)）

## 安装

### 从 Release 下载

从 [GitHub Releases](https://github.com/lilyco-42/ghboost/releases) 下载对应平台的二进制文件。

### 从源码编译

```bash
# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 克隆仓库
git clone https://github.com/lilyco-42/ghboost.git
cd ghboost

# 编译
cargo build --release
```

## 桌面托盘版（Windows，不含在上述 CI 内）

[`ghboost-tray/`](ghboost-tray/) 是给不懂命令行的人用的外壳：托盘常驻、状态灯、
一个大按钮，面板用系统默认浏览器打开，不内嵌 WebView。
它是仓库里的**独立 crate**（根 `Cargo.toml` 无 `[workspace]` 段，根 CI 不会编它，
因此不会影响 Linux/macOS 构建）。

```bash
cd ghboost-tray
cargo build --release
# 安装（桌面快捷方式 + 开机自启 + 可选下载 mihomo 内核）
powershell -ExecutionPolicy Bypass -File tools/install.ps1 -WithKernel
```

详细设计与踩坑记录见 [`ghboost-tray/README.md`](ghboost-tray/README.md)。

## 使用方法

### 1. 扫描节点

```bash
# 自动扫描订阅源
ghboost scan

# 指定额外订阅源
ghboost scan --source https://example.com/sub.yaml

# 自定义参数
ghboost scan --max-sources 100 --concurrency 32
```

### 2. 测速

```bash
# 使用 mihomo 内核测速
ghboost test

# 指定 mihomo 路径
ghboost test --mihomo /path/to/mihomo

# 自定义测试参数
ghboost test --top 100 --timeout 10000 --test-url https://www.google.com/generate_204
```

### 3. 注入最优节点

```bash
# 导出最优节点（默认保留 20 个）
ghboost add

# 注入到 Clash Verge 配置
ghboost add --apply

# 自定义保留数量
ghboost add --keep 50 --max-ms 1000
```

### 4. GitHub 加速

```bash
# 修改 hosts 文件
ghboost boost

# 恢复 hosts 文件
ghboost unboost
```

### 5. 一键部署服务器

```bash
# 部署 VLESS + Reality（默认）
ghboost deploy 1.2.3.4 --password your_ssh_password

# 部署 VLESS + WebSocket
ghboost deploy 1.2.3.4 --password your_ssh_password --protocol vless-ws

# 部署 Trojan
ghboost deploy 1.2.3.4 --password your_ssh_password --protocol trojan

# 部署 Shadowsocks
ghboost deploy 1.2.3.4 --password your_ssh_password --protocol shadowsocks

# 部署 Hysteria2
ghboost deploy 1.2.3.4 --password your_ssh_password --protocol hysteria2

# 使用密钥认证
ghboost deploy 1.2.3.4 --user root --key-path ~/.ssh/id_rsa

# 自定义端口和域名
ghboost deploy 1.2.3.4 --password your_ssh_password --port-out 8443 --domain your-domain.com

# 不安装 BBR / 不配置防火墙
ghboost deploy 1.2.3.4 --password your_ssh_password --no-bbr --no-firewall
```

## 命令行选项

### scan 命令

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--source` | - | 额外订阅源 URL（可多次指定） |
| `--include-repo` | true | 是否扫描 free-VPN 仓库索引 |
| `--max-sources` | 60 | 最大处理订阅源数 |
| `--concurrency` | 16 | 并发拉取数 |
| `--per-limit` | 500 | 单源最多取 N 行节点 |
| `--output` | nodes_data | 节点库数据目录 |

### test 命令

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--input` | nodes_data | 节点库数据目录 |
| `--top` | 300 | 最大测试节点数 |
| `--concurrency` | 32 | 并发测试数 |
| `--timeout-ms` | 8000 | 单节点测试超时（毫秒） |
| `--test-url` | gstatic.com/generate_204 | 测速用的探测 URL |
| `--mihomo` | 自动探测 | mihomo 二进制路径 |

### add 命令

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--input` | nodes_data | 测试结果数据目录 |
| `--keep` | 20 | 保留延迟最低的 N 个节点 |
| `--max-ms` | 0 | 延迟上限（毫秒），超过的丢弃（= 不限） |
| `--apply` | false | 是否写入用户当前激活的 local profile |
| `--profile` | 自动定位 | 目标 profile 路径 |

### deploy 命令

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `host` | (必填) | 服务器 IP 地址 |
| `--port` | 22 | SSH 端口 |
| `--user` | root | SSH 用户名 |
| `--password` | - | SSH 密码（可选，优先使用密钥） |
| `--key-path` | - | SSH 私钥路径 |
| `--protocol` | vless-reality | 协议：vless-reality, vless-ws, trojan, shadowsocks, hysteria2 |
| `--port-out` | 自动分配 | 服务端口 |
| `--domain` | www.microsoft.com | 域名（Reality/TLS 需要） |
| `--no-bbr` | false | 不安装 BBR 加速 |
| `--no-firewall` | false | 不配置防火墙 |

#### 支持的协议

| 协议 | 说明 | 默认端口 |
|------|------|----------|
| vless-reality | VLESS + Reality（推荐，无需域名证书） | 443 |
| vless-ws | VLESS + WebSocket（可配合 CDN） | 443 |
| trojan | Trojan 协议 | 443 |
| shadowsocks | Shadowsocks（AES-256-GCM） | 8388 |
| hysteria2 | Hysteria2（QUIC 协议，高速） | 8443 |

## 数据格式

### nodes_index.json

节点索引文件，包含所有扫描到的节点：

```json
[
  {
    "name": "节点名称",
    "protocol": "trojan",
    "source": "来源URL",
    "raw": "trojan://password@server:port?params#name"
  }
]
```

### nodes_tested.json

测速结果文件，包含所有测试过的节点：

```json
[
  {
    "name": "节点名称",
    "delay_ms": 929,
    "protocol": "trojan",
    "source": "来源URL"
  }
]
```

## 架构

```
ghboost/
├── src/
│   ├── main.rs          # CLI 入口 + WebView
│   ├── lib.rs           # 模块声明 + C ABI FFI
│   ├── hosts.rs         # GitHub 加速核心
│   ├── nodes.rs         # 扫描/测速/添加核心
│   ├── proxy.rs         # 跨平台代理设置
│   ├── mihomo.rs        # Mihomo 内核管理
│   ├── deploy.rs        # 一键部署服务器（SSH + 多协议）
│   ├── webview.rs       # WebView FFI
│   └── gui.html         # Clash Verge 风格 GUI
├── ghboost-ffi/         # FFI crate (JNI + iOS)
├── ghboost-android/     # Android 项目
├── mihomo_bin/          # Mihomo 二进制
├── nodes_data/          # 节点数据
├── build.rs             # 编译脚本
└── Cargo.toml           # 项目配置
```

## 平台支持

| 平台 | 状态 | 说明 |
|------|------|------|
| Windows x64 | ✅ | 完整支持 |
| macOS ARM64 | ✅ | 完整支持 |
| Linux x64 | ✅ | 完整支持 |
| Android ARM64 | ✅ | VpnService |
| iOS ARM64 | ⚠️ | 需要 Apple Developer 账号 |
| Linux ARM64 | ✅ | 完整支持 |
| Windows ARM64 | ✅ | 完整支持 |
| macOS x64 | ✅ | 完整支持 |

## CI/CD

使用 GitHub Actions 自动构建 8 个平台的二进制文件。

构建触发条件：
- Push 到 `main` 分支
- 创建新的 Release
- 手动触发

## 依赖

- **Rust**: 1.75+
- **Mihomo**: v1.19+ (可选，用于测速)
- **Clash Verge**: (可选，用于节点注入)
- **sshpass**: (可选，用于密码认证部署)
- **OpenSSH**: (可选，用于密钥认证部署)

## 许可证

MIT License
