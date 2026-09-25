# 第三方组件声明

## 本项目本体

ghboost —— MIT License，见 [LICENSE](LICENSE)。

## mihomo（加速器内核）

- **授权**：GPL-3.0
- **版本**：v1.19.30（随 Windows 发行包附上的构建）
- **源码**：<https://github.com/MetaCubeX/mihomo>
- **使用方式**：以**独立子进程**调用（`mihomo -f <配置文件>`），
  **未**被链接进 ghboost 的二进制文件。

按 GPL-3.0 对聚合体（aggregate）的界定，ghboost 与 mihomo 分属独立程序、
仅在运行时通过进程调用与 HTTP 控制端口通信，因此 ghboost 本体仍以 MIT 授权，
不受 GPL 传染。

**但这不代表可以省略义务**：分发 mihomo 二进制时必须同时提供其源码或书面索取
途径，并附上 GPL-3.0 全文。Windows 发行包内已包含
`kernel\mihomo-LICENSE.txt`（GPL-3.0 全文），源码链接也写在本文件里。

## Xray-core（加速器内核）

- **授权**：MPL-2.0
- **版本**：v26.3.27（随 Windows 发行包附上的构建）
- **源码**：<https://github.com/XTLS/Xray-core>
- **使用方式**：以**独立子进程**调用（`xray run -c <配置>`），**未**被链接进
  ghboost 的二进制文件。

MPL-2.0 属 weak copyleft：对未修改的二进制分发，只需保留授权声明与源码可得性。
发行包内附 `kernel\xray-LICENSE.txt`（MPL-2.0 全文），源码链接在本文件里。

## sing-box（加速器内核）

- **授权**：GPL-3.0
- **版本**：v1.14.2（随 Windows 发行包附上的构建）
- **源码**：<https://github.com/SagerNet/sing-box>
- **使用方式**：以**独立子进程**调用，**未**被链接进 ghboost 的二进制文件；
  与 mihomo 同理属聚合体（aggregate），ghboost 本体不受 GPL 传染。

分发义务同 mihomo：发行包内附 `kernel\sing-box-LICENSE.txt`（GPL-3.0 全文），
源码链接在本文件里。

## 规则数据库

- `country.mmdb`、`geosite.dat` 来自
  [MetaCubeX/meta-rules-dat](https://github.com/MetaCubeX/meta-rules-dat)。
- 这两个文件是**必需的**，不是可选优化：没有 `country.mmdb` 时
  `GEOIP,TW,DIRECT` 这类规则会**静默地永不匹配**，导致台湾本地站点被送去绕
  代理 —— 更慢，且部分网银会判定为异地登录而挡下。
- `kernel\bin\geoip.dat`、`kernel\bin\geosite.dat` 来自
  [Loyalsoldier/v2ray-rules-dat](https://github.com/Loyalsoldier/v2ray-rules-dat)，
  与 xray 配套（当前发射配置只用字面 CIDR，属预置数据）。

## 编译期依赖

Rust 依赖的许可证由 `cargo-deny` / `cargo tree` 可查，均为 MIT / Apache-2.0 /
BSD 等宽松许可证，无 copyleft 强制要求。
