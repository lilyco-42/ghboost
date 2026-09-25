# 第三方组件声明

本产品由多份独立程序组成。本仓库（`ghboost-tray`）自身代码为 **MIT**，
以下列出的第三方组件各自遵循其原始许可证。

## 运行时依赖（编译进 ghboost-tray.exe）

| 组件 | 许可证 | 用途 |
|---|---|---|
| [lilyco-42/ghboost](https://github.com/lilyco-42/ghboost) | MIT | hosts 加速 / 节点测速核心逻辑 |
| [tauri-apps/tray-icon](https://github.com/tauri-apps/tray-icon) | Apache-2.0 | 系统托盘 |
| [muda](https://github.com/tauri-apps/muda) | Apache-2.0 | 托盘菜单（tray-icon 传递依赖） |
| [axum](https://github.com/tokio-rs/axum) | MIT | 本机控制台 HTTP 服务 |
| [tokio](https://github.com/tokio-rs/tokio) | MIT | 异步运行时 |
| [reqwest](https://github.com/seanmonstar/reqwest) | MIT/Apache-2.0 | 连通性探测 |
| [webbrowser](https://github.com/amodm/webbrowser-rs) | MIT/Apache-2.0 | 调用系统默认浏览器 |
| [serde / serde_json](https://github.com/serde-rs/serde) | MIT/Apache-2.0 | 序列化 |
| [windows-sys](https://github.com/microsoft/windows-rs) | MIT/Apache-2.0 | Win32 消息泵（`PeekMessageW` 等） |

## 独立可执行程序（不链接、不一起编译，运行时以子进程方式调用）

### mihomo —— GPL-3.0

- 项目：<https://github.com/MetaCubeX/mihomo>
- 许可证：**GNU General Public License v3.0**
- 本产品**不修改** mihomo，也不将其链接进本程序；仅以**独立子进程**方式
  调用官方发布的预编译二进制，通过 HTTP RESTful API（`127.0.0.1`）与
  `proxy-providers` 配置进行通信。二者属 GPL-3.0 第 5 条所述的
  **聚合体（aggregate）**，因此本仓库代码仍为 MIT。
- 我们使用时的固定版本与下载来源（见 `tools/install.ps1`）：

  ```
  https://github.com/MetaCubeX/mihomo/releases/download/v1.19.30/mihomo-windows-amd64-compatible-v1.19.30.zip
  ```

  发行包（`ghboost-tray-windows-x64.zip`）里**随包内置**该二进制，位于
  `kernel\bin\mihomo.exe`，安装时直接复制、不联网 —— 因为本产品的目标用户
  恰恰是访问 GitHub 不畅的人，要求他们装完再联网下载等于功能不可用。
  从源码构建时若包内没有 `kernel\`，才需要显式传 `-WithKernel` 触发下载。
- 依据 GPL-3.0 第 6 条，mihomo 的完整对应源码可从上述项目地址获取；
  若你从本项目的 Release 中获得了 mihomo 二进制，可直接到
  <https://github.com/MetaCubeX/mihomo> 取得同样版本的源码。
  发行包内同时附带了 GPL-3.0 全文（`kernel\mihomo-LICENSE.txt`）。

### Xray-core —— MPL-2.0

- 项目：<https://github.com/XTLS/Xray-core>
- 许可证：**Mozilla Public License 2.0**
- 与 mihomo 同理：不修改、不链接，仅以**独立子进程**方式调用官方预编译
  二进制，属**聚合体（aggregate）**，本仓库代码不因此改变许可。
- 固定版本与下载来源（见 `.github/workflows/tray.yml` 的
  “Stage xray + sing-box kernels”步骤）：

  ```
  https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-windows-64.zip
  ```

  发行包内位于 `kernel\bin\xray.exe`，安装时直接复制、不联网。
- MPL-2.0 全文随包附带（`kernel\xray-LICENSE.txt`）；对应源码见
  <https://github.com/XTLS/Xray-core>（tag `v26.3.27`）。

### sing-box —— GPL-3.0

- 项目：<https://github.com/SagerNet/sing-box>
- 许可证：**GNU General Public License v3.0**
- 同样以**独立子进程**方式聚合运行，不链接、不修改。
- 固定版本与下载来源：

  ```
  https://github.com/SagerNet/sing-box/releases/download/v1.14.2/sing-box-1.14.2-windows-amd64.zip
  ```

  发行包内位于 `kernel\bin\sing-box.exe`。
- GPL-3.0 全文随包附带（`kernel\sing-box-LICENSE.txt`）；对应源码见
  <https://github.com/SagerNet/sing-box>（tag `v1.14.2`）。

### 规则数据库

`country.mmdb`（MaxMind GeoLite2 格式）与 `geosite.dat` 同样随 mihomo 官方
发布分发，用于 `GEOIP` / `GEOSITE` 规则匹配。**缺少 `country.mmdb` 时
`GEOIP,TW,DIRECT` 这类规则会静默永不命中**，所以安装脚本把它列为必需项。

`kernel\bin\geoip.dat` 与 `kernel\bin\geosite.dat` 来自
[Loyalsoldier/v2ray-rules-dat](https://github.com/Loyalsoldier/v2ray-rules-dat)
（公开规则数据聚合，与 xray 配套）。当前 xray 发射配置只用字面 CIDR、
不引用 geo 规则，这两份属预置数据，随 xray 二进制一同分发。

## 再分发提示

若你要二次分发本产品并内置 mihomo / Xray-core / sing-box，请一并保留本
文件，并确保三者的对应源码可获取（保留上方下载链接或随包提供源码）。
