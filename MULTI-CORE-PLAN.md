# ghboost 多内核计划（对标 NekoBox：mihomo / xray / sing-box · 全协议）

> 状态权威文档。每完成一项打勾并注明日期；不删除未完成项。
> 用户指令（2026-09-24）：「ghboost 强调和 nekobox 类似的多内核支持 mihomo xray singbox 全协议支持。
> 直到完成前不许停止，本机有项目，不许本机编译，有问题自己选推荐的选项。」

## 目标

像 NekoBox 一样，把 **三个代理内核**（mihomo / xray-core / sing-box）做成可选引擎，
协议矩阵覆盖 NekoBox 全集，桌面托盘与 Android APK 双端落地，README/面板显式强调该能力。

## 协议 × 内核矩阵（目标，✅ = 必须支持）

| 协议 | mihomo | xray | sing-box | 备注 |
|---|---|---|---|---|
| Shadowsocks (含 2022-blake3) | ✅ | ✅ | ✅ | |
| VMess | ✅ | ✅ | ✅ | |
| VLESS（含 Reality / XTLS-flow） | ✅ | ✅ 首选 | ✅ | auto 模式 Reality → xray |
| Trojan | ✅ | ✅ | ✅ | |
| Hysteria2 | ✅ | — | ✅ 首选 | auto → sing-box |
| TUIC | ✅ | — | ✅ 首选 | auto → sing-box |
| AnyTLS | ✅(1.19+) | — | ✅ | auto → sing-box |
| ShadowTLS | ✅ | — | ✅ | auto → sing-box |
| WireGuard | ✅ | — | ✅ | auto → sing-box |
| SOCKS4/5 / HTTP(S) | ✅ | ✅ | ✅ | 出站型节点 |
| SSH | —（以实测为准） | ✅ | ✅ | auto → sing-box |
| 订阅格式：v2rayN URI / Clash YAML / sing-box JSON | ✅ parse-type | URI 解析 | URI 解析 | 三种都要能吃（NekoBox 三格式） |

- SSR：三个内核上游均不支持 → 明确不支持（记录，不实现）。
- auto 内核选择规则：`hysteria2/tuic/anytls/shadowtls/wireguard/ssh → sing-box`；
  `vless+reality/flow → xray`；`ss/vmess/trojan/ws系 → mihomo`（规则引擎最全）；
  不支持时逐级 fallback 到 sing-box。

## 架构决策（已定，推荐选项已拍板）

1. **执行形态：子进程**（三内核均为官方预编译二进制，CI 随包，不本机编译）。
   - 许可：客户端 MIT，三内核以**独立进程**执行属聚合（aggregate），不链接 ≠ 传染；
     mihomo/sing-box GPL-3.0、xray MPL-2.0，随包带 LICENSE 文本（沿用现有 mihomo 做法）。
   - 桌面：`kernel/bin/{mihomo.exe, xray.exe, sing-box.exe}`（tray.yml 已有 mihomo 随包模式，扩展之）。
   - Android：jniLibs 以 `libmihomo.so` / `libxray.so` / `libsingbox.so` 命名注入
     （`nativeLibraryDir` 可执行、系统自动按 ABI 解包），运行时 exec。
2. **Android 防回环：`Builder.addDisallowedApplication(自身包名)` 自排除**
   （exec 内核与 tun2socks 的出站 socket 全部绕开 TUN；`VpnService.protect()` 保留给
   meow 内嵌路径，两条腿互为兜底）。现有代码注释称「需要另一个 package name」不成立，
   以实测（模拟器）为准。
3. **内嵌 meow 内核保留为第 4 个引擎「內建」**（MIT、同进程 protect、冷启快），
   Android UI 提供：自動 / 內建(meow) / Mihomo / Xray / sing-box。
   meow `minimal` feature 目前只编了 ss+trojan —— 全协议主力是三内核。
4. **配置生成收敛到共享模块 `src/corecfg.rs`**（根 crate，桌面 web/CLI 与 Android FFI 共用）：
   share URI 解析（全协议）→ `emit_xray` / `emit_singbox` / `emit_clash`。
   mihomo/mehow 运行时订阅走现成的 proxy-provider（`parse-type: v2ray`，内核自己解析
   URI）。**测速（`nodes.rs::test_core`）已改内联 `proxies:`**（2026-09-25 实测
   provider 两处硬伤：原子解析一行坏节点打死全部、成员不进扁平 `/proxies` 表无法逐个
   测速；详见 nodes.rs 文件头）。
5. **DNS 策略**（TUN UDP/53 一律被 tun2socks 转到 127.0.0.1:1053）：
   - mihomo/meow：内核自带 `dns.listen: 1053` + redir-host + DoH 主用（现有 v9 配置直接复用）。
   - xray：无独立 DNS 监听 → dokodemo-door UDP 1053 经出站转发（DNS 包走隧道内，天然抗劫持）；
     内核内置 DNS 设 DoH。
   - sing-box：优先配 `dns.listen`（以 1.14 文档实测为准）；不支持则同 xray 方案
     （tun2socks 内建 DoH 中继兜底，作为最后手段）。
   - **W5 定稿**：exec 引擎（xray/sing-box）统一收敛到**内建 DoH 中继**
     （`exec_core::dns_doh`：占 1053、RFC 8484 原文透传、经 SOCKS5 走隧道、
     IP 形式上游 sticky 轮换）—— 不改共享的桌面 emit 配置、不赌 sing-box 的
     DNS 入站形态差异，xray 的 dokodemo-door 方案随之取消；mihomo/meow 仍由
     内核自带 `dns.listen` 应答（原样）。meow 内嵌的旧理由（protect 只能保护
     本进程）由 `addDisallowedApplication(自身包名)` 自排除取代，见决策 2。
6. **不本机编译**：一切构建走 GitHub Actions；本地只做代码、静态阅读、
   下载 CI 产物运行测试（运行 ≠ 编译）。CI 红了就修，循环到绿。

## 工作分解（✅ 打勾）

- [x] W1 `src/corecfg.rs`：CoreKind / 协议矩阵 / share-URI 全协议解析 / emit_xray / emit_singbox / 单测（2026-09-24 CI 全绿）
- [x] W2 `src/coreman.rs`：三内核二进制定位（kernel/bin → PATH → 常见目录）、spawn/探活/停止/版本（2026-09-24 CI 全绿）
- [x] W3 web 面板：`/api/cores` + subscribe 选内核（auto/mihomo/xray/singbox）+ panel.html 选择器（2026-09-24，CI 全绿 `750d7a1`）
- [x] W4 CLI：`ghboost core list/check` 子命令（2026-09-24，CI 全绿 `f6e9c80`）
- [x] W5 Android exec 内核：`exec_core.rs` + JNI（nativeStartCore/StopCore/ListCores）+ 自排除路由（2026-09-24 三闸全绿 `f532560`+修复 `f067227`，run 36083540091 / 36083540111 / 36083540087）
- [x] W6 Android UI：内核选择器（自動/內建/Mihomo/Xray/sing-box）+ 状态贯通 + 缺二进制回落 meow（2026-09-25 三闸全绿 `074c5fb`+修复 `2e79276`，run 36086877253 / 36086877272 / 36086877233）
- [x] W7 CI tray.yml：随包 xray + sing-box + geoip/geosite 数据 + LICENSE + 安装冒烟断言扩展（2026-09-25 tray ✓ `94288bb`：`93f2590`+产物结构修复 `d80e739`，含 libcronet.dll 随包）
- [x] W8 CI build-all.yml android-apk：三内核按 ABI 注入 jniLibs（xray armv7 缺席则跳过并告警）（2026-09-25 build-all ✓ `94288bb`：`8209854`+结构修复 `d80e739`+打包堆 4G `94288bb`）
- [x] W9 README 强调多内核（矩阵表 + 使用说明）+ 根 THIRD-PARTY 补 xray/sing-box 段 + 本文件勾选（2026-09-25）
- [x] W10 验证：CI 全绿 → Windows tray 实跑切三内核 → 模拟器 APK 装机切内核 + logcat 实测（2026-09-25 三闸全绿 `f62bcbb`+`fd26358`+`ab20208`，run 36090061323/36091341002/36092012210+对应 tray、build-all；桌面 mihomo/xray/sing-box 全链矩阵 = subscribe→独占互斥→E2E socks/http 200→stop 清场＋注册表快照还原，Android auto/內建 meow/mihomo/xray/sing-box 五引擎 VPN 实测＋logcat 内核证据＋外部节点导入；实跑修复 6 bug：xray 同端口双 inbound、sing-box `ss`→`shadowsocks`、reg.exe HKCU 键拆参、coreman 吞死因、提权副本被当重复启动全死（交接回归 TEST_A/B 过）、meow 状态渲染 `null`）
- [x] W11 Release `v0.3.15`：tag 出包 30 产物全齐（tray zip 含三内核；APK 含三内核）（2026-09-25 三 run 全绿：CI 36094857269 / tray 36094857256 / Build All Platforms 36094857254）

## 风险与坑（预防清单）

- xray 无 domain 可用时（SOCKS 只给 IP）路由只能 geoip 级 → geoip.dat 随包，
  `XRAY_LOCATION_ASSET` env 指向 kernel 目录；geosite 规则在 xray 模式降级（记录）。
- armeabi-v7a 的 xray 官方可能无 android armv7 资产 → CI 容错跳过，UI 显示该 ABI 不可用。
- exec 内核首次启动要等 1080 监听（同 meow 空窗问题）→ 复用 LISTENING 等待逻辑再放 tun2socks。
- 加 DisallowedApplication 后 meow 的 protect 仍在（不冲突）；如自排除在部分 ROM 失效，
  表现为开 VPN 全网断 → logcat 打印 establish 参数便于排障。
- `cargo fmt/clippy` 是 CI 硬闸（-D warnings build-all 版）→ 本地无法编译，写码必须贴 clippy 口味。
