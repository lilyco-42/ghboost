# free-VPN 接入测试报告

**日期**：2026-09-26　**结论**：接入链打通并有真实出口证明；过程中修掉两个用户可见缺陷，已发布 [`v0.3.16`](https://github.com/lilyco-42/ghboost/releases/tag/v0.3.16)。

测试数据源：[`lilyco-42/free-VPN`](https://github.com/lilyco-42/free-VPN) 的 README
（`src/nodes.rs:38` 把它当订阅源索引）。

---

## 0. 一句话结论

free-VPN README → `scan` → `test` → `add` → 托盘订阅 → 出口 IP，整条链在真实免费节点上跑通，
出口从直连基线 `43.108.11.215` 切到节点 `5.78.51.123`，且**产品自己的数据源经自发现节点可达**
（`raw.githubusercontent.com/lilyco-42/free-VPN/main/README.md` → `200` / 43338 B / 1.46s）。

过程中定位并修掉两个用户可见缺陷：

| 缺陷 | 级别 | 状态 | 提交 |
|---|---|---|---|
| mihomo file provider 原子解析，一行坏节点打死整批（20 好 + 1 坏 = 0 节点） | P1 | ✅ 已修 | `9648974` `8278314` `3c47b1a` |
| 节点名不全局唯一（扫描结果 708 行重名） | P1 | ✅ 已修 | `69d77a5` |
| 行首 UTF-8 BOM 静默吃掉第一个节点 | P1 | ✅ 已修 | `a3395cd` `55a40d2` |
| 测速改内联 `proxies:`（provider 成员不进扁平表，`/proxies/{name}/delay` 恒 404） | P0 | ✅ 已发布 `v0.3.15` | `7fc8ab2` `9315463` |

---

## 1. 版本钉死

| 组件 | 版本 |
|---|---|
| 桌面 CLI / 托盘 | `main@3c47b1a`（BOM 修复合于 `55a40d2`，发版 `706db64`+`365973c`） |
| mihomo（桌面自带） | v1.19.30 |
| mihomo（Android） | v1.19.31 |
| xray | v26.3.27 |
| sing-box | v1.14.2 |
| Rust | 1.98.1 |
| 测试用 AVD | `mc_test`，API 36 / x86_64 / 1080×2400 |

所有构建与校验都走 GitHub Actions（仓库无本地 cargo），本机只下载产物运行。

---

## 2. P1 根因：mihomo file provider 的原子性（实测 v1.19.30）

### 2.1 四种坏行，结果完全不同

| 输入 | mihomo 行为 |
|---|---|
| 20 条里 1 条 cipher 不认识 | 只 warning 跳过那一条，**仍载入 19**（逐行容错） |
| 20 条里 1 条 `ss://<uuid>@host?security=tls&encryption=none` | **整个 provider 初始化失败 → 0 节点** |
| 内联 `proxies:` 里出现重名 | **整份拒收**（`proxy dup is the duplicate name`） |
| file provider 里重名 | 容得下，但 `/proxies/{name}` 只能寻址其中一个 |

第二条的日志原文：

```
initial proxy provider subscription error: proxy 19 error: ss 162.159.1.33:443 cipher: ... unknown method
```

**「原子」只在特定坏行上成立，不是「所有坏行都原子」** —— 这一点决定了修法不能是一刀切的「全丢」。

### 2.2 为什么我们能剔掉那一行

坏行长这样：`ss://15298f41-e80b-463a-b85b-0c903258a1c8@162.159.1.33:443?security=tls&encryption=none`

1. userinfo `15298f41-…` 不是 `method:password`；
2. `b64_flex` 先试 STANDARD（`-` 不在标准表 → 失败），再试 URL_SAFE（解出 27 字节，
   但 `F1 FE` 不是合法 UTF-8 → `from_utf8` 失败）；
3. 两条路都拿不到字符串 → 落到 `split_once(':')`，而 UUID 里没有冒号 → `parse_line` 返回 `None`。

**这条依赖有脆性**：哪天 `b64_flex` 改用 `from_utf8_lossy`，它会解出一个垃圾 method，
内核对不上 cipher 又会整批拒收。单测 `sanitize_links_drops_the_kernel_poison_line`
（`src/nodes.rs`）就是那根保险丝。补 cipher 白名单列为 follow-up。

### 2.3 判据的一处纠错（重要）

第一版修法把「`parse_line` 解析不了」直接当「内核一定不认」。**这是错的**：
mihomo 支持的协议比我们建模的多（`ssr` / `juicity` / `mieru`），
内核本来能用的节点会被连坐删掉 —— 那是自己制造新故障。

改法：新增 `corecfg::MODELLED_SCHEMES` + `modelled_link()`，**只对我们确实建模过的协议**
才允许「我们读不动 → 内核也读不动」这个推理；`ssr://` / `juicity://` / `mieru://` 一律原样留给内核。
`total` 也只统计「建模过 + 确实是链接」的行，否则用户填 `proxy-providers: type: http` 的
订阅配置时会看到「4 条里掉了 4 条」这种吓人且没意义的话。

### 2.4 前后对比（同一份输入、同一端点）

输入：21 行（20 条好 + 1 条上面那条坏行），端点 `POST /api/proxy/subscribe`。

| | 修复前（`v0.3.15` 二进制 @`9315463`） | 修复后（`main@3c47b1a`） |
|---|---|---|
| HTTP 响应 | `{"ok":false,"error":"内核没能认出这些链接里的任何节点。…"}` | `{"ok":true,"nodes":20,"dropped_bad":1,…}` |
| 落盘 `nodes.txt` | 21 行（坏行原样写入） | 20 行，`162.159.1.33` 不存在 |
| 应用 stderr | — | `[subscribe] 剔除 1 条内核一定不认的链接` |
| mihomo 实际载入 | **0 节点** | **20 节点，全部 alive（508–689 ms）** |
| 出口 IP | 无（连不上） | `5.78.51.123` |
| `api.github.com/zen` | — | `Favor focus over features.` |
| `gstatic generate_204` | — | `204` / `0.59 s` |
| free-VPN README | — | `200` / 43338 B |

provider 侧 `GET /providers/proxies`：`vehicleType: File`，`proxies` 数组 20 项，
`節點選擇` 组选中 `vpnclashfa-112`。以上出口数据都是**经产品自己的 mixed_port 17890** 测的，
不是手工搭 mihomo。

### 2.5 修法两处（桌面 / Android）

- **桌面**：落盘前预筛（`web.rs::split_parsable_links`），响应新增 `dropped_bad`，
  0 节点时错误文案说明剔除了几条。
- **Android**：新 C ABI `ghboost_sanitize_nodes`（`src/lib.rs`）→ JNI `nativeSanitizeNodes`
  （`ghboost-ffi/src/lib.rs`）→ `LocalProxySetup.sanitize()`，复用 P0 已验证的
  `mihomo_entry` / `clash_passthrough` 路径。`kept == 0` 时**保留用户原文**
  （那份文件可能是 `proxy-providers: type: http` 的订阅配置，覆写等于替用户删订阅）。
  Kotlin 侧抓 `Throwable` 而非 `Exception` —— `UnsatisfiedLinkError` 是 `Error`。

---

## 3. 接入链全量复测

参数 `--max-sources 30 --per-limit 300 --concurrency 16`，新旧二进制同参数对比
（数据源是活的，绝对值会漂，看趋势）：

| 指标 | 旧 @`9315463`（`v0.3.15`） | 新 @`3c47b1a` |
|---|---|---|
| 索引提取到的订阅源 | — | 183 |
| 参与扫描 / 成功 | — | 30 / 27 |
| URI 链接行 | 2,536 | 1,930 |
| 其中重名行 | **708** | **86** |
| 全局唯一名 | 1,828 | 1,844 |
| Clash 节点 | — | 9,428 |
| 测速 | 100 测 / 54 活 / best 503 ms | 100 测 / **57 活 / best 480 ms** |
| `add --keep 20` 导出 | 22 行 / 20 个名（`US-VPNine1` ×3） | **20 行 / 20 个名，无重名** |

重名行 708 → 86 是 `69d77a5` 的效果（`scan` 去重到 name 全局唯一 + `NodeInfo.source` 回填）。
残余 86 行的构成：2 组同源同名不同 server（各 2 行）+ 83 行**没有 `#` 片段**的 `http://`
（无名单不算重名，且 `add` 本来就导不出无名行 —— `filter_uri_by_names` 按 name 硬匹配）。
不影响正确性：`test_core` 内联装载与 `add` 导出都按 name 去重。

测速日志里 `内联装载 100 个待测节点（跳过 29/0）` 是 P0 内联化生效的直接证据
（跳过 29 = 协议内核不支持，0 = 重名）。

---

## 4. Android（AVD `mc_test`，APK 来自 Build All run）

安装 `ghboost-android-x86_64.apk`（79.1 MB）→ 启动 → 全部通过：

| 检查项 | 结果 |
|---|---|
| APK 安装 | `Success` |
| `libghboost_ffi.so` 加载 | `Load …/lib/x86_64/libghboost_ffi.so: ok` |
| `nativeVersion()` | UI 显示 `ghboost v0.3.15` |
| `nativeListCores()` | 引擎识别成功（meow / Mihomo） |
| `nativeScan("{}")` | 模拟器内实网扫描完成（状态 `Scan complete`，未抛异常） |
| **`nativeSanitizeNodes`（本轮新符号）** | 设备上跑出正确判定并落盘，见下 |
| 导入落盘 | `imported nodes: …/ghboost-providers.yaml -> /data/user/0/…/providers/ghboost.yaml` |

测试方式：把桌面那份 21 行清单推到 `/sdcard/Android/data/com.ghboost.app/files/ghboost-providers.yaml`，
冷启动 App，从 logcat（tag `GhBoostProxy`）读导入日志 —— 这条路是 `MainActivity` 启动时自动跑的，
不需要 root、不需要点 UI。

### 4.1 BOM 修复的设备端前后对比

同一份文件、同一台设备，只换 APK：

| APK | logcat |
|---|---|
| `3c47b1a`（修前） | `sanitize: kept 19, dropped 1 of 20 (kernel would drop the whole batch)` |
| `55a40d2`（修后） | `sanitize: kept 20, dropped 1 of 21 (kernel would drop the whole batch)` |

差的不是 kept，是 **`of 20` vs `of 21`**：修前有 1 行既不在 kept 也不在 dropped 里凭空消失
（见 5.3）。修后 Android 与桌面口径一致，都是 21 行里留 20 丢 1。

### 4.2 Android 侧拿不到 `alive > 0`，原因在 App 设计

- App 只有 SCAN / START / STOP 三个按钮，**`nativeTest` 有 FFI 导出但没有任何 UI 调用它**
  （死导出，`alive` 指标在 Android 上没有入口）；
- START 会被 `startVpn()` 挡回去：`tunReady == false` 时提示「Android 端尚未完成：現在開啟只會斷網
  （tun2socks 還沒接上）」—— 内核根本不会起，自然没有节点可用性；
- AVD 是 production build，`adb root` 与 `run-as` 都不可用，写进私有目录的
  `providers/ghboost.yaml` 无法从外面读回校验。

结论：Android 侧本轮能证明的是**修复本身生效**（新符号在设备上跑出正确判定并落盘）；
`alive` 需要 App 侧先补一个测试入口、或先接上 tun2socks。列为 follow-up。

---

## 5. 本轮新发现

### 5.1 行首 UTF-8 BOM 静默吃掉第一个节点（**已修** `a3395cd` `55a40d2`）

Android 那条 `kept 19, dropped 1 of 20` 与桌面 `nodes=20` 对不上，追出来的根因：
测试清单是 PowerShell 写的，**带 UTF-8 BOM**；而 U+FEFF 在 Rust 里属于 Cf 类格式字符，
**不是** `char::is_whitespace` 认的空白 —— `str::trim()` 不动它。

于是首行的 scheme 变成 `\u{feff}ss`：

- `modelled_link` 查表落空 → 判「不是链接」→ `continue`，**连 `total` 都不加**；
- `parse_line` 的 `split_once("://")` 同样拿到错 scheme → 解不出来。

那一行既不算「保留」也不算「丢弃」，用户完全看不见地少了一个节点。
内核自己解析链接是吃 BOM 的，所以这条坑只有我们这条路上有。Windows 上记事本 /
PowerShell 的 `>` 重定向存出来的文本默认都带 BOM，触发条件非常常见。

修法：新增 `corecfg::strip_bom`（`trim` → 去行首 U+FEFF → 再 `trim`），
在 `modelled_link`、`parse_line`、`sanitize_nodes_text`、`split_parsable_links`
四处统一收口，别的调用方不必各自记得处理。两个新单测守着，其中一个专门确认
`proxy-providers:` 这类非链接行**不会**因为去 BOM 而被误认成节点（保住
`kept == 0 → 保留用户原文` 那条语义）。

### 5.2 mihomo 的 stdout/stderr 接管了但从不读（`src/mihomo.rs:259-260`，未修）

`Stdio::piped()` 之后没有任何读取线程。后果两条：

1. **诊断信息全丢**：本轮 P1 定位时 `trace.log` 198 行里一条 provider 错误都没有，
   原子性结论只能靠手工搭 mihomo 复现才拿到；
2. **输出超 64 KB 会永久阻塞**：内核往管道写、没人读 → 管道满 → 内核卡在 write 上。

优先级最高 —— 它是后续所有内核侧问题的盲区。

### 5.3 `do_stop` 无条件 `unset_proxy()`，会关掉不属于 ghboost 的系统代理（未修）

本机 Clash Verge 监听 7897，ghboost 停止时把系统代理一并清了（已按快照恢复：
`ProxyEnable=1` / `127.0.0.1:7897` / `ProxyOverride=localhost;127.*;…;<local>`）。
应当只关「自己开的那次」。

### 5.4 只有 `tray.yml` 带 `--locked`，lock 文件一致性没被真正守住（已在 `365973c` 修）

`chore(release): 0.3.16` 只改了 `Cargo.toml` 的 version，CI / Build All 都绿，
因为它们不带 `--locked`，runner 上悄悄把 lock 改了；只有 `tray` 红了
（`error: cannot update the lock file … because --locked was passed`）。
等于把 lock 的一致性丢在 CI 里「顺便」维护，不可复现。已按 `ec6e512`（0.3.15 那次）
的同一套做法补齐三个 lock 文件 + `ghboost-tray/Cargo.toml`。
（`ghboost-ffi/Cargo.lock` 里另一处 `0.3.15` 是第三方包 `lwip`，与本次无关。）

### 5.5 `nativeTest` 是死导出（见 4.2）

### 5.6 `parse_line` 对坏行的拒绝依赖 `from_utf8` 严格性（见 2.2，未加 cipher 白名单）

---

## 6. CI 证据

| 提交 | run | 结果 |
|---|---|---|
| `69d77a5` scan 去重 | CI 36213094002 | ✅ |
| `9648974` P1 首版 | CI 36213877116 / tray 36213877232 | ❌ 编译错（`else { let doc = … }` 里 `let` 是语句，块值成 `()`） |
| `8278314` 修编译错 + 收紧判据 | CI 36217407128 | ❌ fmt / ✅ 编译 |
| `3c47b1a` 按 fmt diff 改三处 | CI 36217513990 ✅、Build All 36217513952 ✅（Android aarch64 含 Verify JNI symbols）、tray 36217513922 ✅ | ✅ |
| `a3395cd` BOM 修复 | CI 36235555198 | ❌ `parse_line` 不在作用域（E0425） |
| `55a40d2` 修作用域 + `strip_bom` 补 web.rs | CI 36235852968 ✅、Build All 36235852986 ✅、tray 36235852987 ✅、pages 36235853184 ✅ | ✅ |
| `706db64` 发版 0.3.16 | CI 36236421634 ✅、Build All 36236421612 ✅、pages 36236421376 ✅、tray 36236421522 ❌ `--locked` | 部分 |
| `365973c` 补 lock 文件 | CI 36237266673 ✅、tray 36237266699 ✅、Build All 36237266660 ✅、pages 36237266406 ✅ | ✅ |
| `5ea1bcf` 报告入仓 | CI 36237323026 ✅、Build All 36237323009 ✅、tray 36237323002 ✅、pages 36237322594 ✅ | ✅ |

`v0.3.16` 的 30 个产物取自 Build All 36237323009 + CI 36237323026 + tray 36237323002
（`ghboost-x86_64-pc-windows-gnu.exe` / `libghboost-aarch64-linux-android.so` /
两个 `libghboost-*.dll` 只有 CI 那份 zigbuild 产物有，Build All 不产）。

失败的三轮都不是设计问题，是「没有本地 cargo，靠 CI 当唯一裁判」的代价；
两次都靠明确信号定位而不是猜：fmt 那次是 `cargo fmt -- --check` 一次给全文件 diff 当权威裁判，
编译错那次是 job log 里 E0425 直接指出来，`--locked` 那次是 cargo 自己说的。

---

## 7. 发布 `v0.3.16`

- tag `v0.3.16` → `5ea1bcfe5a4e8927e8ea07dbce28dfe428b95d6f`（轻量 tag，与 `v0.3.15` 同形）
- https://github.com/lilyco-42/ghboost/releases/tag/v0.3.16
- 30 个资产 / 649.3 MB：4 个 APK（含 universal）、tray zip（含 mihomo+xray+sing-box 三内核）、
  MSI、wasm、7 个平台 CLI、8 个 `libghboost` 动态库、3 个 `libghboost_ffi`。
  资产名与 `v0.3.15` 逐一对应，只有 MSI 从 `0.3.15` 变 `0.3.16`。

---

## 8. 遗留 / 待办

**代码 —— 6 条已全部落地**（2026-09-27 收口。这份清单是 2026-09-26 写的，
当时把「已提交但 CI 没验过」和「还没提交」混着列，容易读成都没做）：

| # | 项 | 落在 | 怎么做的 |
|---|---|---|---|
| 1 | `mihomo.rs` 排空 stdout/stderr | `63b8d1f` | 不是「排空」而是**不建管道**：两个流直接 `Stdio::from(File)` 落到 `kernel.log`，64 KB 写死与诊断丢失一起消失，另加 `/api/kernel-log` |
| 2 | `do_stop` 只关自己开的系统代理 | `6b1b329` | `proxy.owned` 记账 + 停前核对 `ProxyServer` 端点；不是自己的就返回 `Ok(None)`，UI 明说「系统代理不是 ghboost 开的，没有动它」 |
| 3 | `parse_line` 加 cipher 白名单 | `a645107` | `SS_CIPHERS` 23 项（mihomo v1.19.30 逐个试出来的），三个入口都守：share URI / Clash YAML / `clash_passthrough`；method **归一化成小写再落盘**（内核大小写敏感），缺 cipher 或缺 password 的 ss 整条丢 |
| 4 | Android 节点测试入口 | `bca9c1c` | Test 按钮（`nativeTest` 之前是死导出）。顺带修两处「失败显示成成功」：`scanNodes()` 改成看 `error` 字段；`nativeSetHomeDir` 原来**是空实现**（`dir_str` 拿到就扔），所有相对路径都对着进程 cwd `/` 解析 → Android 扫描一直必然失败，而 UI 无条件写 "Scan complete" |
| 5 | `nodes_uri.txt` 清理 | `6757c2b` + `69d77a5` | 拆成两件事，见 8.1 |
| 6 | CI / Build All 加 `--locked` | `194a86d` | 4 个 workflow 全量 `--locked` |

### 8.1 第 5 项拆开是两件事，其中一件早就是旧账

**无名行（`6757c2b` 今天做的）**：`load_test_nodes` 装载、`add` 的 good_uri 导出、
索引回填全都按 name 硬匹配，所以「没有 `#` 片段」不是少个显示名，而是这个
节点**永远选不中、也永远导不出去**，还白占 `--top` 槽位。实测那份 2539 行的
scan 产物里 **88 行**没有名字：21 行 `http://ip:port` + 4 行 `socks://` + 63 行
其它协议。来源两类 —— 源里本来就没有片段；名字被 `clean_label` 清空（emoji /
纯中文名逐字都不在 `[A-Za-z0-9-_.]` 里，而旧代码在这里**直接把片段丢掉**，
节点当场变死）。改成按 `协议_主机_端口` 合成后写进片段，产出即自洽，下游一行
没动。不用 `display_name()` 的 `host:port` 是因为冒号过不了 `clean_label`。

实测（mihomo v1.19.30）：

- 88 行里正则能取到 host:port 的 81 条，按 `http`/`socks5` + 合成名拼
  `proxies:` 喂 `mihomo -t -f` → **exit=0，0 error / 0 warning**；
- 再按 `add` 注入 profile 的真实形态（file provider + `parse-type: v2ray`）
  起 mihomo 查 `/providers/proxies` → **85/86 条载入，21 个 `http_*` 全在，
  provider 无报错**（差的 1 条是 mihomo 自己给重名节点补 `-01` 后缀）；
- 剩下 7 条是整段 base64 的 `ss://`（SIP002-JSON），Rust 侧 `parse_ss` 解得开，
  一样能补名。

**重名行（早就是旧账）**：那份样本里有 **161 组重名**（`EPODONIOS` ×172、
`JoinTelegramFarah_VPN` ×22…），但文件 mtime 是 **2026-09-25 18:18**，而
`dedup_scanned`（name 全局唯一）是 **2026-09-26 10:53**（`69d77a5`）才落的 ——
本报告第 5 项写的「2 组同源同名行」，是在同一份 **dedup 之前**的产物上数的，
实际是 161 组。现在 name 全局唯一有单测钉着
（`dedup_scanned_keeps_names_globally_unique`）。

### 8.2 顺带挖出来的两条：CI 的两道闸都比它们该有的样子松

**(a) fmt 闸把 `cargo test` 整个挡在后面。** `a645107` 起 CI 一直红在
`cargo fmt --check`，而 `ci.yml` 的步骤顺序执行、一红就 abort —— **`cargo test`
一次都没执行过**。`bca9c1c` 把 fmt 修好之后测试才第一次真跑，立刻 3 条红灯
（`73 passed / 3 failed`）。两条同一个根因：手写的多行 YAML 靠**源码缩进**拼出来，
而 Rust 字符串的 `\`+换行会吃掉下一行的**全部**前导空白 → 块状 YAML 塌成非法标量
流 → `serde_yaml` 解析失败 → 一条都读不出来。第三条是 `log_tail` 测试取错下标
（它按时间顺序返回，超长行是 `got[0]`）。`eeb2e69` 修掉，并按 Rust 续行语义机械
重建两份 fixture 喂 `mihomo -t -f` 验证（旧的形状做对照，直接
`proxy 0: missing type` / exit=1）。

**(b) clippy 那道闸没有 `-D warnings`，于是 warning 是只打印不拦的。**
`ci.yml` 写的是 `cargo clippy --all-targets --locked`，而 `build-all.yml` 写的是
`-- -D warnings` —— 同一条命令，两种严格度。所以 `c038844` 的 CI 报 success，
`eeb2e69` 的 Build All 却挂在 `Clippy (default features)`：
`doc list item without indentation` ×2（`src/nodes.rs` 里一段 `- ` 列表后面紧跟的
段落被 CommonMark 吸成 lazy continuation）。一条注释，红掉整个发版。
已修（列表后补空 `///`），并把 `ci.yml` 的 clippy 步补上 `-D warnings`，让两道闸
字面一致 —— 快的闸不拦，红的就只会晚 20 分钟到慢的闸那里才炸。

**仍未覆盖**：`ghboost-tray` / `ghboost-ffi` 两个 crate 任何 workflow 都没跑过
clippy（只 build）。机械扫过它们的 doc 注释没有同类问题，但「没有 clippy 闸」
这件事本身还在，想收紧就照 (b) 的方式给 `tray.yml` 补一步。

**需要用户配合**

7. 真机 USB 调试（Android 运行时复测目前只有 AVD 路径）。
8. simul 麦克风实测（`设置 → 隐私 → 麦克风` 权限）。
