# ghboost

GitHub 访问加速 + 免费代理节点扫描 / 测速 / 注入工具，基于 [lilyco](https://github.com/lilyco-42/lilyco) 框架（一个 struct + `#[derive(App)]` 自动生成 CLI / TUI / Web / MCP）。

[![CI](https://github.com/lilyco-42/ghboost/actions/workflows/ci.yml/badge.svg)](https://github.com/lilyco-42/ghboost/actions/workflows/ci.yml)

## 功能特性

- **GitHub hosts 加速**：多路 DoH 解析 + 公开 hosts 源聚合候选 IP，再对每个 IP 做**真实 TLS 握手测速**（SNI=域名、校验证书）。只选"快且真的服务该域名"的 IP，不会选到"快但不服务 GitHub"的假 IP。
- **一键写入 / 清理 hosts**：`--apply` 写入系统 hosts，`--clean` 清理（块用 `# BEGIN/END ghboost` 标记，幂等可重入）。
- **MCP 服务器**：`--mcp` 暴露为 AI Agent 可直接调用的工具；`--schema` 打印 JSON Schema。
- **免费节点库**：`scan` 扫描公开订阅源 → `test` 用独立 Mihomo 内核做真实延迟测速 → `add` 把最优节点注入 Clash/Mihomo 配置（自动备份）。

## 安装

### 预编译二进制（推荐）

从 [Releases](https://github.com/lilyco-42/ghboost/releases) 下载对应平台二进制，开箱即用：

| 平台 | 文件 |
| --- | --- |
| Linux x86_64 | `ghboost-x86_64-unknown-linux-gnu` / `-musl` |
| Linux aarch64 (Radxa 等) | `ghboost-aarch64-unknown-linux-gnu` / `-musl` |
| Windows x86_64 | `ghboost-x86_64-pc-windows-gnu` |
| Windows aarch64 | `ghboost-aarch64-pc-windows-gnu` |
| macOS Apple Silicon | `ghboost-aarch64-apple-darwin` |
| macOS Intel | `ghboost-x86_64-apple-darwin` |
| Android (Termux) | `ghboost-aarch64-linux-android` |

### 从源码（cargo）

```bash
cargo install --git https://github.com/lilyco-42/ghboost
```

### 从源码（cargo-zigbuild 交叉编译）

```bash
# 本机一次性装好 zig + cargo-zigbuild 即可出所有平台二进制
cargo install cargo-zigbuild
cargo zigbuild --release --target aarch64-unknown-linux-musl
```

## 使用

### GitHub 加速（默认命令）

```bash
ghboost                 # 优选并把 hosts 块打印到 stdout（不修改系统）
ghboost --apply         # 写入系统 hosts（Windows 需管理员 / Linux 需 root）
ghboost --clean         # 清理 ghboost 写入的 hosts 条目
ghboost --only github.com --top 2   # 只处理指定域名，每域保留 2 个最优 IP
ghboost --timeout-ms 5000 --concurrency 32
ghboost --mcp           # 启动 MCP 服务器（供 AI Agent 调用）
ghboost --schema        # 打印命令 JSON Schema
```

字段说明（`GhBoost`）：

- `--timeout-ms`：单 IP 测速超时（默认 3000，200–15000）
- `--concurrency`：并发测速数（默认 16，过高易失败）
- `--top`：每域名保留最优 IP 数（默认 1）
- `--extra-ip`：额外候选 IP（可多次指定）
- `--only`：只处理指定域名（可多次指定，默认全部）
- `--apply` / `--clean`：写入 / 清理系统 hosts

### 节点扫描 / 测速 / 注入

```bash
ghboost scan                       # 扫描免费订阅源 → ./nodes_data/{nodes_uri.txt,nodes_clash.yaml,nodes_index.json}
ghboost scan --source https://...  # 追加自定义订阅源
ghboost test                       # 启动独立 Mihomo 内核，对扫描结果做延迟测速
ghboost add --keep 20 --max-ms 800    # 导出延迟最低的 20 个节点（>800ms 丢弃）
ghboost add --apply                # 把最优节点注入当前激活的 Clash Verge profile（带备份）
```

`scan` / `test` / `add` 共享 `./nodes_data` 目录作为节点库。

## 支持平台

通过 GitHub Actions + **cargo-zigbuild**（Linux / Windows）与 **原生 runner / NDK**（macOS / Android）全平台构建，详见 `.github/workflows/ci.yml`。打 `v*` tag 自动发布 GitHub Release。

## 开发

```bash
# 依赖 lilyco 框架（git 依赖，见 Cargo.toml）
cargo test                        # 运行离线单元测试（解析 / 优选 / 节点分类等）
cargo clippy --all-targets
cargo fmt --all -- --check
```

- 仓库：`https://github.com/lilyco-42/ghboost`
- 框架：`https://github.com/lilyco-42/lilyco`

## License

MIT
