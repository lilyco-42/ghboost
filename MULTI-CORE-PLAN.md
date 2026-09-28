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
- [x] W12 free-VPN 接入测试 + P1 修复：README → scan → test → add → 托盘订阅 → 出口 IP
  全链打通（出口 `43.108.11.215` 直连基线 → `5.78.51.123` 节点），修掉两个用户可见缺陷
  （2026-09-26）：
  1. **mihomo file provider 的原子性** —— 一行 `ss://<uuid>@host?security=tls&encryption=none`
     就能让整份 provider 初始化失败（20 好 + 1 坏 = 0 节点，症状是「显示已连接、
     每个请求都失败」）。桌面落盘前预筛（`web.rs::split_parsable_links`，响应新增
     `dropped_bad`），Android 新 C ABI `ghboost_sanitize_nodes` + JNI
     `nativeSanitizeNodes`（`kept==0` 保留用户原文，那份文件可能是
     `proxy-providers: type: http` 的订阅配置）。判据只用 `corecfg::MODELLED_SCHEMES`
     ——「我们解析不了」≠「内核一定不认」，`ssr`/`juicity`/`mieru` 一律留给内核。
     实测 `ok:false/0 节点` → `ok:true/nodes:20/dropped_bad:1`。
  2. **行首 UTF-8 BOM 静默吃掉第一个节点** —— BOM 不是 Rust 认的空白，`trim()` 不动它
     → 首行 scheme 变 `\u{feff}ss` → `modelled_link` 判「不是链接」直接 continue，
     连 `total` 都不进，节点凭空消失。Android 侧 `kept 19, dropped 1 of 20` 与桌面
     `nodes=20` 对不上才暴露出来。新增 `corecfg::strip_bom` 在 4 个入口统一收口，
     修后设备实测 `kept 20, dropped 1 of 21`。
  同时 `69d77a5` 的 scan 去重让重名行 708 → 86，`add` 导出从「22 行 20 个名
  （`US-VPNine1`×3）」变成 20 行 20 个名。完整报告见
  [`FREEVPN-TEST-2026-09-26.md`](FREEVPN-TEST-2026-09-26.md)。
  遗留 6 条已在 W14 全部收口。
- [x] W13 Release `v0.3.16`：tag `5ea1bcf`，30 资产 / 649.3 MB
  （`706db64` 发版 → `365973c` 补 lock 修红灯 → `5ea1bcf` 报告入仓；
  run CI 36237323026 / tray 36237323002 / Build All 36237323009 / pages 36237322594 全绿）
- [x] W14 报告遗留 6 条 + `v0.3.17`（2026-09-27 收口，四闸全绿，run 36296725819 /
  36296725850 / 36296725806 / 36296725982，79 passed / 0 failed）：
  1. **内核日志不落盘**（`63b8d1f`）—— 以前 `Stdio::piped()` 接了管道却没人读：诊断全丢，
     且输出超 64 KB 会把内核写死在 write 上。改成两个流直接 `Stdio::from(File)` 落
     `kernel.log`（不建管道），另加 `log_tail`（单行超 200 字符截断，内核会把整份配置的
     错误堆进一行）与 `/api/kernel-log`。
  2. **`do_stop` 误关别人的系统代理**（`6b1b329`）—— `set_proxy` 成功时记账
     （`ProxyEnable`/`ProxyServer`/`ProxyOverride` 原值），停前交叉核对端点，
     对不上就明说「不是 ghboost 开的，没有动它」；`unset_proxy` 成功路径销账。
  3. **ss cipher 白名单**（`a645107`）—— `SS_CIPHERS` 23 项由 mihomo v1.19.30 逐个试出，
     三个入口都守（share URI / Clash YAML / 外来节点清洗），`method` 归一化成小写再落盘
     （内核大小写敏感）；缺 cipher 或缺 password 的 ss 整条丢，否则就是「整份 provider
     被拒 = 20 好 + 1 坏 = 0 节点」。
  4. **Android 补测速入口**（`bca9c1c`）—— `nativeTest` 之前是有导出没 UI 调用的死函数。
     顺带修两处「失败显示成成功」：`scanNodes()` 无条件写 "Scan complete" 而不看有没有
     `error` 字段；`nativeSetHomeDir` 是**空实现**，所有相对路径都对着进程 cwd `/` 解析
     —— Android 扫描其实一直必然失败。后者比「缺个按钮」严重得多。
  5. **无名节点补名**（`6757c2b`）—— 没有 `#` 片段的行在清单里看得见但永远选不中、导不出去。
     在唯一产出处 `clean_uri_name` / `synth_name` 合成 `协议_主机_端口` 写进片段，
     名字被 `clean_label` 清空（emoji / 纯中文名）的那类一并救回。
  6. **CI / Build All 加 `--locked`**（`194a86d`）—— lock 一致性从「CI 顺便维护」变成硬闸；
     0.3.17 的 6 处版本号就是手改 lock 过去的，能过 `--locked` 正说明这道闸在起作用。
  外加两条「闸本身是松的」：`ci.yml` 的 clippy 补 `-D warnings`（此前 warning 只打印不拦，
  快的闸形同虚设），以及 3 条**从来没跑过**的测试（`a645107` 起 CI 卡在 fmt 闸，
  而 `ci.yml` 步骤顺序执行、一红就 abort，`cargo test` 一次没执行过）—— 根因是手写的
  多行 YAML 靠源码缩进拼，而 Rust 字符串的 `\`+换行会吃掉下一行**全部**前导空白，
  块状 YAML 塌成非法标量流（`eeb2e69`）。
  Release：tag `v0.3.17` → `945ea75`，30 资产 / 649.9 MB，资产名与 `v0.3.16` 逐一对应，
  只有 MSI 变 `0.3.17`。

### W15 —— AVD 上对 `v0.3.17` 发布件本体复测（纯验证，无代码改动）

装的是**发布资产本体**（SHA256 与 release asset digest 逐位一致，不是本地重编件），
跑通了 4.3 扫描/测速与 4.4 外部导入两条路：

- `nativeScan("{}")` → `Scan complete: 18846 nodes`（`uri 4047 + clash 14799`，
  `sources_ok 54/60`），返回体里 `data_dir` 是**相对路径** `nodes_data`；
- `nativeTest` → `{"alive":8,"best_ms":825,"tested":60}`，即 **`alive > 0`**；
- 外部导入：`imported nodes: /storage/… -> /data/user/0/com.ghboost.app/files/
  mihomo/configs/providers/ghboost.yaml`，`sanitize: kept 20, all clean`；
- 无崩溃。导入后 `btnStart` 由 `enabled=false` 变 `true`（`hasRealNodes()` 占位标记消失）。

**两条被这轮实测推翻/坐实的既有结论：**

1. 「`alive > 0` 仍需先解决 tun2socks」**是错的** —— `nativeTest` 起一个内核实例去
   dial 节点，整条路径不碰 TUN，量的是**节点可达性**；tun2socks 决定的是「能不能
   真的用这条隧道跑流量」，是 START 那条路的事。两个问题被混成了一个。
2. `nativeSetHomeDir` 与 BOM 两处修复拿到**设备端硬证据**（导入 `dst` 落在
   `/data/user/0/…/files/` 下而非 `/providers/`；`kept 20` 而非 19）。

**新挖出一条发布合规缺口**：APK 分发 GPL-3.0 的 `libmihomo.so` 却零许可证文本
（桌面侧是有的），详见测试报告 8.3；`ghboost-ffi` 关于「避免 GPL 传染」的论证
对发布件已不成立。

## 风险与坑（预防清单）

- xray 无 domain 可用时（SOCKS 只给 IP）路由只能 geoip 级 → geoip.dat 随包，
  `XRAY_LOCATION_ASSET` env 指向 kernel 目录；geosite 规则在 xray 模式降级（记录）。
- armeabi-v7a 的 xray 官方可能无 android armv7 资产 → CI 容错跳过，UI 显示该 ABI 不可用。
- exec 内核首次启动要等 1080 监听（同 meow 空窗问题）→ 复用 LISTENING 等待逻辑再放 tun2socks。
- 加 DisallowedApplication 后 meow 的 protect 仍在（不冲突）；如自排除在部分 ROM 失效，
  表现为开 VPN 全网断 → logcat 打印 establish 参数便于排障。
- `cargo fmt/clippy` 是 CI 硬闸 → 本地无法编译，写码必须贴 clippy 口味。
- **`ci.yml` 与 `build-all.yml` 的闸必须字面一致**：`ci.yml` 的 clippy 原本没带
  `-D warnings`，warning 只打印、步骤照样绿，快的闸形同虚设（红灯要等 20 分钟后的
  build-all 才炸）。反过来 `ci.yml` 的步骤是顺序执行、一红就 abort —— fmt 闸红了
  同 job 的 `cargo test` 一次都不跑。**两道闸不一致 = 快的闸形同虚设**。
- **多行 YAML 写进 Rust 字符串字面量时，缩进必须写在字面量内部**（接在 `\n` 之后、
  行尾 `\` 之前），不能靠源码缩进：`\`+换行会吃掉下一行的**全部**前导空白，
  靠源码拼的块状 YAML 会塌成非法标量流，`serde_yaml` 解析失败 → 一条都读不出来。
  rustfmt 完全不重排 `\`-续行的字符串字面量，所以 CI 不会帮你发现。拿 `mihomo -t -f`
  当 YAML oracle 验（含故意写坏的对照）。
- **`ghboost-tray` 的 fmt/clippy 闸已补**（`533d46b` / tray.yml，命令与根 crate 逐字
  一致，toolchain 一并钉 `1.98.1`）。这个 crate 以前只有 build，等于改它只有编译器
  兜底，而它是**要发给用户双击运行、还要走 SignPath 签名**的那份二进制。
  首次即绿（run 36411407842 step 5/6 均 success）。
- **`ghboost-ffi` 的 clippy 闸仍然没有，而且不能照抄一行就了事** —— 加之前先算了
  覆盖率：它的 `jni` 在 `default = ["android"]` 后面，`meow-*` 全部
  `cfg(target_os = "android")`，所以**跑 host clippy 会把 `nativeScan` /
  `nativeSetHomeDir` / `nativeTest` / `nativeListCores` 一个都不编** ——
  正好是我这轮改的那批函数。这样的闸比没有闸更坏：绿灯是假的。
  真要闸只能 `cargo clippy --target aarch64-linux-android`（要 NDK + 该 target 的
  rust-std，cargo-ndk 本身不跑 clippy），属于独立工作量，本轮记为待办不做。
- **闸验的 Rust ≠ 出包的 Rust**：`ci.yml:36` / `build-all.yml:37` 的 fmt+clippy+test
  钉死 `toolchain: 1.98.1`，但 `ci.yml` 的 zigbuild 矩阵、`build-all.yml` 的全部出包
  job 都只写 `@stable`（= 当时最新 stable）。stable 一升，闸还绿着
  而出包 job 先炸，或闸红在与出包无关的新 lint 上。同一病根：闸与被闸的东西不是同一份配置。
  要收就统一钉 `1.98.1`；本轮没动（会让下次出包换编译器，是发版决策不是补丁）。
  > `tray.yml` 原先也在这份名单里，但 `533d46b` 之后它只有一个 toolchain action 且
  > 钉了 `1.98.1`，闸与出包已经统一，**不再属于此列**。
- **Windows 上 `-no-window` 起模拟器时，进程名是 `qemu-system-x86_64-headless.exe`**，
  不是 `emulator` / `qemu-system-x86_64`。按名字 kill 会漏掉它，残留进程会一直占着
  AVD 的 `multiinstance.lock`，之后每次启动都直接
  `FATAL | Running multiple emulators with the same AVD`；而它又在 adb 里注册成
  `emulator-5554 offline`，看起来像「启动很慢」，实际是启动**已经失败**了。
  判断依据要看 emulator 自己的 stdout 有没有 `FATAL`，不要只看 `adb get-state`。
- **强 kill 模拟器会把 AVD 的 userdata 搞脏，然后进入崩溃重启循环**，而症状极具
  误导性：adb 传输层反复 `offline` ↔ `device`，`sys.boot_completed` 永远空，
  但 guest 其实**每次只活了 ~10 秒就重启**（`/proc/uptime` 是唯一能揭穿它的读数，
  正常冷启后应单调涨到几十分钟）。看起来像「启动慢 / 传输抖」，实际是无限重启。
  解法是 `emulator -avd <name> -wipe-data` 冷启一次，本轮实测 `-wipe-data` 后
  `pm` 立刻可用（poll 1 就 serving），装机扫描全流程一次通。
- **在模拟器上跑自动化时只能有一个 adb 客户端**：并发两个（比如一个在轮询、一个在
  跑测试）会互相把传输层搞flap，症状同样是「device offline」，但真因是竞争。
  另外 PowerShell 里包一层 adb 重试**不能用 `ValueFromRemainingArguments`**：
  写成 `Adb -s emulator-5554 shell ...` 时 `-s` 会被当成**参数名**去绑定，
  于是每次调用死在参数绑定阶段、循环只会刷「not ready」而一条命令都没发出去。
  要用 `[string[]]$Arguments` + splatting（`Invoke-Adb -Arguments @("-s",$dev,"shell",...)`），
  并在脚本开头加一条 `echo` 自检，自检不过就退出而不是继续报「未就绪」。
- **`filesDir/mihomo/configs/` 这个目录名会骗人**：它指的是**配置格式**是 mihomo 风格
  YAML，**不代表跑的是 mihomo**。`ghboost-ffi/src/lib.rs:197-198` 的注释把这两件事
  写在一段里（「启动内嵌代理内核（meow-rs）」+「`config_path` 是 mihomo 风格 YAML，
  由 Kotlin 侧写在 `filesDir/mihomo/configs/`」），但实测 `nativeTest` 起的是
  **真 mihomo**（logcat 里独立进程 `libmihomo.so`，pid 与 App 不同）。因为 W8 之后
  `jniLibs` 里放的就是真内核。看目录名判断内核种类会得出相反结论。
- **APK 分发 GPL-3.0 内核但不带许可证文本**：`build-all.yml:184-206` 把
  `libmihomo.so`（GPL-3.0）塞进 jniLibs，而 `ghboost-android` 整棵树没有任何
  LICENSE/NOTICE/THIRD-PARTY 文件、`assets/` 目录都不存在。桌面侧是处理过的
  （`kernel/mihomo-LICENSE.txt` + `THIRD-PARTY-NOTICES.md` 的专门小节），Android 侧
  没跟上。连带 `ghboost-ffi/Cargo.toml:21-24` 选 meow-rs 的那条「避免 GPL 传染 MIT
  客户端」论证**对发布件已不成立**（对 crate 仍成立）。详见测试报告 8.3。
- **同一个坑在桌面侧也踩了，而且已经发出去两版**：`tray.yml` 取 sing-box 许可证的
  URL（`.../sing-box/main/LICENSE`）**已 404**，而包在 `try/catch` 里只打 WARN，于是
  v0.3.16 / v0.3.17 的 zip 都没有 `kernel/sing-box-LICENSE.txt`，尽管 notices 承诺它
  在安装目录里。三层叠加：URL 失效 + 失败被吞 + 没有任何断言。更深一层：**光修 URL
  也不够** —— `v1.14.2` 的 `LICENSE` 只有 791 字节，是「详见 GPL-3.0」的版权声明、
  不含正文，必须取 gnu.org 规范全文。教训两条：
  (a) **分发义务类的外部依赖不许 soft-fail**，`try/catch` + WARN 在合规问题上等于
      「静默发一个不合规的包」；
  (b) **断言要落在产物上，不是落在暂存目录/源码目录上**。桌面的「装完这一层」和
      Android 的「开 APK 看」都是这个道理 —— 暂存目录里有，产物里未必有。
  791 字节这个数字也顺手成了断言阈值下限（10 KB）：既挡指针声明，又放行
  GPL 全文（~35 KB）与 MPL 全文（~16.7 KB）。
- **别把 gnu.org 当构建依赖**：`tray` run 36415822727 就红在这儿 ——
  `www.gnu.org` 从 GitHub runner 直接超时（"connected host has failed to respond"），
  本机 curl 同样失败（exit 35，SSL connect error），不是偶发。取许可证正文改成
  **多来源轮询、GitHub raw 优先（自家 CDN）、gnu.org 只兜底**，且**用大小当判据**
  而不是「HTTP 200 就算」。实测可用源：GPL-3.0 全文见
  `MetaCubeX/mihomo` 的 `Alpha/LICENSE`（35,149 B，与 gnu.org 那份**逐字节相同**，
  SHA256 `3972DC97…`）或 SPDX 的 `license-list-data`（34,674 B）；
  MPL-2.0 见 `XTLS/Xray-core`（16,725 B）或 SPDX（16,727 B）。
  附带一条通用教训：**合规来源要比普通依赖更严** —— 普通依赖取不到可以重试或换源，
  许可证正文取不到只有一个正确结果：炸。
- **`install.ps1` 是无 BOM 的 UTF-8 且含中文，`install.bat` 调的却是 powershell 5.1**：
  5.1 按 ANSI 读，整个文件的中文全是乱码。`.bat` 自己的注释已经意识到
  「cmd.exe 用 OEM 代码页，非 ASCII 会乱码」，却只防了自己、没防它调用的那个文件。
  加 UTF-8 BOM 即可（PS 5.1 认 BOM）。注意 `uninstall.ps1` 是纯 ASCII，不受影响 ——
  这种不一致说明「.ps1 纯 ASCII」的约定没被一致执行。
- **`uiautomator dump` 的输出不能经控制台读**：本机控制台代码页是 GBK，
  `adb exec-out` 的 UTF-8 CJK 会被解成替换字符，而替换字符可能**吞掉一个引号**，
  于是 `[xml]` 报「根元素不匹配」——看起来像 App 的 UI 坏了，实际是自己的读取方式坏了。
  用 `adb pull` 把字节直接落盘、再 `[System.IO.File]::ReadAllText(..., UTF8)` 读。
  同理，PowerShell 函数里任何 `Write-Output` 都会**混进返回值**：
  `$x = DumpUi` 拿到的是打印出来的字符串数组而不是 XML，症状是「节点找不到」。
  诊断输出一律走 `Write-Host`。
