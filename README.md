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

## C ABI / 多语言调用

除了 CLI / TUI / Web / MCP 四端，核心能力还以 **C ABI 动态库**形式导出，可从任意支持 FFI 的语言 / 设备 / 平台嵌入调用（无需 Rust 工具链）。

Release 中随二进制一起附带：

| 平台 | 库文件名 | 头文件 |
| --- | --- | --- |
| Linux x86_64 / aarch64 (gnu) | `libghboost-x86_64-unknown-linux-gnu.so` / `...-aarch64...so` | `ghboost.h` |
| macOS (Apple Silicon / Intel) | `libghboost-aarch64-apple-darwin.dylib` / `...-x86_64...dylib` | `ghboost.h` |
| Windows x86_64 | `ghboost-x86_64-pc-windows-gnu.dll` | `ghboost.h` |

> 注：`*-unknown-linux-musl` 目标因 musl 仅支持静态链接，Rust 不会产出 cdylib，故无对应 `.so`（属预期行为）。

### 调用约定

- 入参：JSON 字符串（`const char*`，UTF-8）。传 `NULL` / 空串 / `"{}"` 即全部默认；缺字段取默认。
- 出参：JSON 字符串（`char*`，堆分配）。成功为结果对象，失败为 `{"error":"..."}`。
- **调用方必须用 `ghboost_free()` 释放返回的指针**，否则内存泄漏；传 `NULL` 安全。
- 6 个导出符号：`ghboost_boost` / `ghboost_scan` / `ghboost_test` / `ghboost_add`（均接收 `const char* params_json`，返回 `char*`）、`ghboost_version`（无参，返回 `char*`）、`ghboost_free`（`char*` → `void`）。

### Python（ctypes）

```python
import ctypes, json

lib = ctypes.CDLL("./libghboost.so")          # Windows 用 "ghboost.dll"
lib.ghboost_free.argtypes = [ctypes.c_char_p]
lib.ghboost_free.restype = None
for fn in ("ghboost_boost","ghboost_scan","ghboost_test","ghboost_add","ghboost_version"):
    f = getattr(lib, fn)
    f.restype = ctypes.c_char_p
    f.argtypes = [ctypes.c_char_p]

def call(f, params=None):
    raw = f((json.dumps(params) if params is not None else None).encode("utf-8")
            if params is not None else None)
    s = ctypes.string_at(raw).decode("utf-8") if raw else "null"
    lib.ghboost_free(raw)
    return s

print(call(lib.ghboost_version))                       # 版本信息
print(call(lib.ghboost_boost, {"apply": False}))      # 优选（不写 hosts）
print(call(lib.ghboost_scan, {"per_limit": 200}))     # 扫描
```

### C#

```csharp
using System;
using System.Runtime.InteropServices;

class GhBoost
{
    [DllImport("ghboost", CallingConvention = CallingConvention.Cdecl)]
    static extern IntPtr ghboost_boost(IntPtr paramsJson);
    [DllImport("ghboost", CallingConvention = CallingConvention.Cdecl)]
    static extern IntPtr ghboost_scan(IntPtr paramsJson);
    [DllImport("ghboost", CallingConvention = CallingConvention.Cdecl)]
    static extern IntPtr ghboost_test(IntPtr paramsJson);
    [DllImport("ghboost", CallingConvention = CallingConvention.Cdecl)]
    static extern IntPtr ghboost_add(IntPtr paramsJson);
    [DllImport("ghboost", CallingConvention = CallingConvention.Cdecl)]
    static extern IntPtr ghboost_version();
    [DllImport("ghboost", CallingConvention = CallingConvention.Cdecl)]
    static extern void ghboost_free(IntPtr ptr);

    static string Call(Func<IntPtr, IntPtr> f, string json = null)
    {
        IntPtr arg = json == null ? IntPtr.Zero
                                  : Marshal.StringToHGlobalAnsi(json);
        IntPtr raw = f(arg);
        if (arg != IntPtr.Zero) Marshal.FreeHGlobal(arg);
        if (raw == IntPtr.Zero) return null;
        string s = Marshal.PtrToStringAnsi(raw);
        ghboost_free(raw);
        return s;
    }

    static void Main()
    {
        Console.WriteLine(Call(ghboost_version));
        Console.WriteLine(Call(ghboost_boost, "{\"apply\":false}"));
    }
}
```

### C / C++

```c
#include "ghboost.h"
#include <stdio.h>
#include <stdlib.h>

int main(void) {
    char *ver = ghboost_version();
    printf("version: %s\n", ver);
    ghboost_free(ver);

    char *out = ghboost_boost("{\"apply\":false}");
    printf("boost: %s\n", out);
    ghboost_free(out);
    return 0;
}
```

编译：`cc main.c -L. -lghboost -o demo`（Windows 把 `ghboost.dll` 与 `ghboost.lib` 放链接路径）。

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
