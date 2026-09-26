# free-VPN 接入测试报告

**日期**：2026-09-26　**结论**：接入链打通并有真实出口证明；过程中修掉两个用户可见缺陷，发布 `v0.3.16`。

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
| `365973c` 补 lock 文件 | 见 7. 发布 | — |

失败的三轮都不是设计问题，是「没有本地 cargo，靠 CI 当唯一裁判」的代价；
两次都靠明确信号定位而不是猜：fmt 那次是 `cargo fmt -- --check` 一次给全文件 diff 当权威裁判，
编译错那次是 job log 里 E0425 直接指出来，`--locked` 那次是 cargo 自己说的。

---

## 7. 发布 `v0.3.16`

含 P1 file provider 修复 + BOM 修复 + `69d77a5` 的 scan 去重。tag 与产物待
`365973c` 三闸全绿后补。

---

## 8. 遗留 / 待办

**代码**

1. `mihomo.rs` 排空 stdout/stderr（顺带解掉 64 KB 阻塞风险）—— 优先级最高。
2. `do_stop` 只关自己开的系统代理。
3. `parse_line` 加 cipher 白名单，把 2.2 那条脆性依赖变成显式契约。
4. Android 补节点测试入口（`nativeTest` 已有导出，缺 UI 调用），或先接 tun2socks。
5. `nodes_uri.txt` 里 2 组同源同名行 + 83 行无名 `http://` 的清理（不影响正确性）。
6. 给 CI / Build All 也加 `--locked`，让 lock 一致性有硬闸（见 5.4）。

**需要用户配合**

7. 真机 USB 调试（Android 运行时复测目前只有 AVD 路径）。
8. simul 麦克风实测（`设置 → 隐私 → 麦克风` 权限）。
