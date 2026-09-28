# free-VPN 接入测试报告

**日期**：2026-09-26（报告）/ 2026-09-27（收口）　**结论**：接入链打通并有真实出口证明；
过程中修掉一批用户可见缺陷，已发布 [`v0.3.16`](https://github.com/lilyco-42/ghboost/releases/tag/v0.3.16)
与 [`v0.3.17`](https://github.com/lilyco-42/ghboost/releases/tag/v0.3.17)（第 8 节那 6 条遗留全部落地）。

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

这 4 条是**测试当天**发现的。测试之后又补了 6 条（`v0.3.17`，见
[第 8 节](#8-遗留--待办)），其中两条是那轮报告里记为「未修」的：ss cipher 白名单
（`a645107`）和 `nativeTest` 死导出（`bca9c1c`）。

---

## 1. 版本钉死

| 组件 | 版本 |
|---|---|
| 桌面 CLI / 托盘 | `main@945ea75`（`v0.3.17` 发版提交；`v0.3.16` 为 `365973c`） |
| mihomo（桌面自带） | v1.19.30 |
| mihomo（Android） | v1.19.31 |
| xray | v26.3.27 |
| sing-box | v1.14.2 |
| Rust（fmt/clippy/test 闸） | 1.98.1（`ci.yml:36`、`build-all.yml:37` 显式 `toolchain: 1.98.1`） |
| Rust（**出包**用的） | `@stable`，**没钉版本** —— 见下方注 |
| 测试用 AVD | `mc_test`，API 36 / x86_64 / 1080×2400 |

所有构建与校验都走 GitHub Actions（仓库无本地 cargo），本机只下载产物运行。

**AVD 上装的是 `v0.3.17` 发布资产本体，不是本地重编的产物**（AVD 复测只对发布件
有意义，重编件证明不了发布件）：

```
本地暂存 ghboost-android-x86_64.apk  SHA256 c4775d4526c665340b2674313b6f4e5dc84a8a4703c4b9d7d5d2bb26c56d98fc  79,114,073 B
v0.3.17  release asset digest        sha256:c4775d4526c665340b2674313b6f4e5dc84a8a4703c4b9d7d5d2bb26c56d98fc
                                      79,114,073 B
```


> **顺带记一个跟 8.2 同类的闸缺口**（写这行时才发现，尚未修）：**出包用的 Rust
> 和校验用的 Rust 不是同一个**。fmt/clippy/test 钉死 `1.98.1`，但 `ci.yml` 的
> zigbuild 矩阵、`build-all.yml` 的全部出包 job、以及整个 `tray.yml`（`:35`）
> 都只写 `dtolnay/rust-toolchain@stable` —— 跑的是「当时最新的 stable」。
> 也就是说**闸验的编译器不是产物的编译器**：stable 一升，闸可能还绿着，
> 而出包 job 先炸（或者更糟：闸红在 1.98.1 的新 lint 上，与出包成败无关）。
> 这和 8.2(b) 是同一个病根 —— **闸与被闸的东西不是同一份配置**。
> 要收就四处统一钉 `1.98.1`；本轮没动，因为它会让下一次出包换编译器，
> 属于发版决策而不是补丁。

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
（`src/nodes.rs`）就是那根保险丝。

> 补 cipher 白名单这条 follow-up 已做（`a645107`，发版 `v0.3.17`）：`SS_CIPHERS` 23 项
> 由 mihomo v1.19.30 逐个试出来，三个入口都守，`method` 归一化成小写再落盘。
> 这样即使 `b64_flex` 哪天真改成 lossy，解出来的垃圾 method 也会被白名单挡下来 ——
> 保险丝从「一条单测」变成「一张契约表」。详见 [5.6](#56-parse_line-对坏行的拒绝依赖-from_utf8-严格性已修-a645107见-22)。

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

> **2026-09-27 更新（`bca9c1c`）**：第一条已修 —— 加了 Test 按钮走
> `nativeTest`。查的时候还挖出两个「失败显示成成功」：
> `scanNodes()` 无条件写 "Scan complete" 而不看返回里有没有 `error` 字段；
> `nativeSetHomeDir` 收到目录后**直接扔掉**（空实现），于是所有相对路径都对着
> 进程 cwd `/` 解析 —— Android 上扫描其实一直必然失败，UI 却报成功。
> 这两条比「缺一个测速入口」严重得多：前者让用户拿到空清单，后者让扫描根本不可用。
> 剩下没闭环的：tun2socks 尚未接通（START 仍被 `startVpn()` 挡回）+ AVD 是
> production build 取不到私有目录做产物校验。

> **2026-09-28 更正（AVD 复测 `v0.3.17`）**：上面最后那句「`alive > 0` 仍需先解决
> tun2socks」是**错的**，而且是把两个不同的问题混成了一个。
> `nativeTest` → `ghboost_test()`，与桌面同一个函数：它起一个内核实例逐个去 dial
> 节点，量的是**节点可达性**，整条路径不碰 TUN。所以 `alive` 从来不依赖 tun2socks ——
> 只要有 Test 入口就能拿到。tun2socks 决定的是「能不能真的用这条隧道跑流量」，
> 那是 START 那条路的事，跟 `alive` 无关。实测 `alive: 8`（见 4.3）。
> 仍然没闭环的只剩两条：tun2socks 未接通（START 仍被挡回）、AVD 取不到私有目录
> 做产物校验（`adb root` / `run-as` 对 production build 都不可用）。

### 4.3 `v0.3.17` 在 AVD 上的复测（发布件本体）

装的是**发布资产本体**（SHA256 与 release asset digest 逐位一致，见第 1 节），
不是本地重编件 —— 重编件证明不了发布件。`mc_test` / API 36 / x86_64 / SDK 36。

| 检查项 | 结果 |
|---|---|
| APK 安装 | `Success`（79,114,073 B） |
| `libghboost_ffi.so` 加载 | `Load …/lib/x86_64/libghboost_ffi.so …: ok` |
| `nativeVersion()` | UI 显示 **`ghboost v0.3.17`** |
| Test 按钮存在 | `btnTest` 文案 `TEST 測速`，`enabled="true"` —— §4.2 第一条确实已闭环 |
| `nativeScan("{}")` | `Scan complete: 18846 nodes` |
| scan 返回体 | `{"clash_nodes":14799,"data_dir":"nodes_data","sources_ok":54,"sources_total":60,"uri_nodes":4047}` |
| `nativeTest` | `{"alive":8,"best_ms":825,"data_dir":"nodes_data","tested":60}` |
| Test 提示行 | `8/60 可用 · 最快 825ms` |
| 崩溃 | 无（无 `FATAL EXCEPTION` / `UnsatisfiedLinkError` / `SIGSEGV`） |

两条关键证据：

1. **`data_dir` 是相对路径 `"nodes_data"`**。它能落地**只可能**因为
   `nativeSetHomeDir` 现在真的把进程 cwd 切到了 `filesDir` —— 修之前这里对着 `/`
   解析，`/nodes_data` 建不出来。同一份 logcat 里所有落盘路径都是
   `/data/user/0/com.ghboost.app/files/...`（如
   `mihomo/configs/providers/ghboost.yaml`），没有一条跑到 `/` 根下。
2. **`alive = 8 > 0`**，`best_ms = 825`。这是本节此前判定「拿不到」的指标。

顺带记两条复测时的实测细节：

- 引擎选择器显示「內建 (meow)」，但 `nativeTest` 实际起的是**真 mihomo** ——
  logcat 里有独立进程 `libmihomo.so`（pid 7230，与 App 进程 6702 不同）。
  因为 APK 的 `jniLibs` 里放的是真内核（`build-all.yml:184-206`：
  `libmihomo.so` / `libxray.so` / `libsingbox.so`），而 `filesDir/mihomo/configs/`
  这个目录名是历史遗留，指的是**配置格式**是 mihomo 风格，不代表跑的是 meow。
  `ghboost-ffi/src/lib.rs:197-198` 的注释写明了这一点（「启动内嵌代理内核（meow-rs）」
  但「`config_path` 是 mihomo 风格 YAML，由 Kotlin 侧写在 `filesDir/mihomo/configs/`」），
  两件事，别混。
- 该进程有若干 SELinux `avc: denied`（`tests` 目录 `search`、`somaxconn` 的 `read`、
  `netlink_route_socket` 的 `bind`，bug=`b/155595000`）。不影响测速结果
  （`alive` 照样算出来），但记一笔：真机上如果内核起不来，这里是第一条线索。

### 4.4 外部导入路径 + BOM 修复的设备端验证

4.3 走的是 SCAN（网络抓取）。这一轮改推**文件导入**那条路，因为它才是
`nativeSetHomeDir` 修复的真正靶心：`LocalProxySetup.kt:290` 打印
`imported nodes: <src> -> <dst>`，**两个路径都是绝对路径**，所以 `dst` 本身就是证据。

推到 UI 提示的那个位置（`adb push` 逐字节，已核对设备端首 3 字节仍是 `ef bb bf`）：

```
/sdcard/Android/data/com.ghboost.app/files/ghboost-providers.yaml   2018 B
```

推的文件是 `w10/p1test/nodes.txt` —— 就是 5.1 那个带 BOM 的复现件（20 行有效 ss，
2018 字节）。冷启动后 logcat（tag `GhBoostProxy`，这条路不需要 root、不需要点 UI）：

```
config up to date (v9): /data/user/0/com.ghboost.app/files/mihomo/configs/config.yaml
provider exists, left untouched: /data/user/0/com.ghboost.app/files/mihomo/configs/providers/ghboost.yaml
import: try /storage/emulated/0/.../ghboost-providers.yaml exists=true
import: try /storage/emulated/0/.../ghboost/providers.yaml exists=false
import: read 2016 chars from /storage/emulated/0/.../ghboost-providers.yaml
sanitize: kept 20, all clean
imported nodes: /storage/emulated/0/.../ghboost-providers.yaml
             -> /data/user/0/com.ghboost.app/files/mihomo/configs/providers/ghboost.yaml
```

**`nativeSetHomeDir` 这条证据是硬的**：`dst` 落在
`/data/user/0/com.ghboost.app/files/…` 下。修之前 `nativeSetHomeDir` 是空实现，
进程 cwd 停在 `/`，这里会是 `/providers/ghboost.yaml` —— 那是建不出来的路径
（`/providers` 属主是 root，App 无权创建）。现在这条 import 是**真的写成功了**，
和 4.3 里 `data_dir: "nodes_data"` 这个相对路径能落地互相印证。

**BOM 修复**：`sanitize: kept 20, all clean`。20 = 20 行全部留下，包括**首行**。
首行就是那个带 `ef bb bf` 的行 —— 修之前它的 scheme 变成 `\u{feff}ss`，
`modelled_link` 查表落空被判「不是链接」，**连 `total` 都不加**，
所以修之前同一份文件只能 kept 19。差的这 1 行正是 5.1 说的「既不算保留也不算丢弃、
用户完全看不见地少一个节点」。

> 口径差异说明：4.1 记的是 `kept 20, dropped 1 of 21`，这里是 `kept 20, all clean`。
> `dropped` 从 1 变 0 不是修复倒退 —— 是两份文件不同。4.1 那份含一行 cipher/password
> 缺失的 ss，被 3（`a645107` cipher 白名单）判掉；这份 20 行全是合法 ss，没有该丢的。
> 与 BOM 相关的量是 **`kept` 20 而非 19**。

导入后 UI 的变化（对照 4.3 启动时的 dump，这是一次**行为**改变，不只是文案）：

| | 导入前 | 导入后 |
|---|---|---|
| `tvStatus` | `還差節點：填好節點清單才能開始加速` | `Ready` |
| `tvNodes` | 「把節點檔案放到：…再重開 App 就會自動匯入」 | `已從外部檔案匯入節點。按 Start 開始加速。` |
| `btnStart` | **`enabled="false"`** | **`enabled="true"`** |

`btnStart` 解锁是 `hasRealNodes()`（`LocalProxySetup.kt:362`）在起作用：它判
provider 文件里**不含占位标记**。出厂时 provider 里是
`DIRECT-PLACEHOLDER`（指向没人监听的 `127.0.0.1:1081`），所以 Start 锁死 —— 这正是
那段注释说的「比直接锁住按钮更难排查」那个坑。导入了真节点，占位标记消失，按钮解锁。
**注意这只是 UI 门禁解开**，`btnStart` 点下去仍会被 `startVpn()` 挡回（tun2socks 未接），
与 4.2 的结论一致，两者不是一回事。

`run-as com.ghboost.app ls -l files` → `package not debuggable: com.ghboost.app`，
印证 4.2 里「AVD 是 production build，取不到私有目录做产物校验」那条仍然成立。
不过本轮**换了个办法绕开**：不读文件内容，改读 logcat 里应用自己打印的绝对路径 ——
证据强度不降反升，因为它来自实际发生的一次写入，而不是外部窥探。


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

### 5.2 mihomo 的 stdout/stderr 接管了但从不读（**已修** `63b8d1f`）

`Stdio::piped()` 之后没有任何读取线程。后果两条：

1. **诊断信息全丢**：本轮 P1 定位时 `trace.log` 198 行里一条 provider 错误都没有，
   原子性结论只能靠手工搭 mihomo 复现才拿到；
2. **输出超 64 KB 会永久阻塞**：内核往管道写、没人读 → 管道满 → 内核卡在 write 上。

修法不是「加个线程排空」，而是**根本不建管道**：两个流直接 `Stdio::from(File)` 落到
`kernel.log`。64 KB 写死风险与诊断丢失一起消失，代价是零（本来就打算落文件）。
另加 `MihomoManager::log_tail`（尾部 N 行 + 单行超 200 字符截断——内核会把整份配置的
错误堆进一行，实测 400+ 字符，不截的话「最近 20 行」就是一堵墙）和 `/api/kernel-log`，
0 节点时 UI 直接带出内核原话。

### 5.3 `do_stop` 无条件 `unset_proxy()`，会关掉不属于 ghboost 的系统代理（**已修** `6b1b329`）

本机 Clash Verge 监听 7897，ghboost 停止时把系统代理一并清了（已按快照恢复：
`ProxyEnable=1` / `127.0.0.1:7897` / `ProxyOverride=localhost;127.*;…;<local>`）。
应当只关「自己开的那次」。

修法是记账而不是猜：`set_proxy` 成功时 `remember_owned()` 记下
（`ProxyEnable` 原值 + 原 `ProxyServer` + 原 `ProxyOverride`），停之前把
`ProxyServer` 与自己开的端点交叉核对；`unset_proxy` 成功路径 `forget_owned()`，
所以反复 start/stop 不会漏记也不会误关。核对不上就返回 `Ok(None)`，
UI 明说「系统代理不是 ghboost 开的，没有动它」。

### 5.4 只有 `tray.yml` 带 `--locked`，lock 文件一致性没被真正守住（已在 `365973c` 修）

`chore(release): 0.3.16` 只改了 `Cargo.toml` 的 version，CI / Build All 都绿，
因为它们不带 `--locked`，runner 上悄悄把 lock 改了；只有 `tray` 红了
（`error: cannot update the lock file … because --locked was passed`）。
等于把 lock 的一致性丢在 CI 里「顺便」维护，不可复现。已按 `ec6e512`（0.3.15 那次）
的同一套做法补齐三个 lock 文件 + `ghboost-tray/Cargo.toml`。
（`ghboost-ffi/Cargo.lock` 里另一处 `0.3.15` 是第三方包 `lwip`，与本次无关。）

补齐之后仍然只是「四个 workflow 都带 `--locked`」这一层；真正把 lock 变成硬闸的是
`194a86d`（4 个 workflow 全量 `--locked`），本轮 `0.3.17` 的 6 处版本号就是手改 lock
过去的——手改能过 `--locked`，正说明这道闸在起作用（不一致会被直接顶回来）。

### 5.5 `nativeTest` 是死导出（**已修** `bca9c1c`，见 4.2）

### 5.6 `parse_line` 对坏行的拒绝依赖 `from_utf8` 严格性（**已修** `a645107`，见 2.2）

补了 `SS_CIPHERS` 白名单（23 项，mihomo v1.19.30 逐个试出来的），把「碰巧能剔掉」
变成显式契约；三个入口都守，`method` 归一化成小写再落盘。

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
| `63b8d1f` kernel.log + `/api/kernel-log` | 四闸全绿 | ✅ |
| `6b1b329` proxy 归属记账 | 四闸全绿 | ✅ |
| `a645107` cipher 白名单 | CI ❌ 卡 fmt（**`cargo test` 因此一次没跑**，见 8.2a） | ❌ |
| `bca9c1c` Android Test 入口 + cwd 根因 | CI 36295416578 ❌ 3 测试红、Build All 36295696071 ❌ 同 3 测试、tray 36295696060 ✅、pages 36295695550 ✅ | ❌ |
| `6757c2b` 无名节点补名 | CI 36295696080 ❌ 同 3 测试（76 passed → 我新加的 3 条是绿的）、tray 36295696060 ✅ | ❌ |
| `eeb2e69` 修 3 条测试 | CI 36296246039 ✅ **79 passed / 0 failed** | ✅ |
| `c038844` 发版 0.3.17 | CI ✅、tray ✅、pages ✅、Build All ❌ clippy `doc_lazy_continuation`（见 8.2b） | ❌ |
| `945ea75` 修注释 + clippy 闸对齐 | CI 36296725819 ✅（clippy 带 `-D warnings`）、Build All 36296725806 ✅（17/17，含 Android APK）、tray 36296725850 ✅、pages 36296725982 ✅ | ✅ |

`v0.3.16` 的 30 个产物取自 Build All 36237323009 + CI 36237323026 + tray 36237323002；
`v0.3.17` 同构，取自 Build All 36296725806 + CI 36296725819 + tray 36296725850
（`ghboost-x86_64-pc-windows-gnu.exe` / `libghboost-aarch64-linux-android.so` /
两个 `libghboost-*.dll` 只有 CI 那份 zigbuild 产物有，Build All 不产）。

失败的三轮都不是设计问题，是「没有本地 cargo，靠 CI 当唯一裁判」的代价；
两次都靠明确信号定位而不是猜：fmt 那次是 `cargo fmt -- --check` 一次给全文件 diff 当权威裁判，
编译错那次是 job log 里 E0425 直接指出来，`--locked` 那次是 cargo 自己说的。

---

## 7. 发布

### 7.1 `v0.3.16`

- tag `v0.3.16` → `5ea1bcfe5a4e8927e8ea07dbce28dfe428b95d6f`（轻量 tag，与 `v0.3.15` 同形）
- https://github.com/lilyco-42/ghboost/releases/tag/v0.3.16
- 30 个资产 / 649.3 MB：4 个 APK（含 universal）、tray zip（含 mihomo+xray+sing-box 三内核）、
  MSI、wasm、7 个平台 CLI、8 个 `libghboost` 动态库、3 个 `libghboost_ffi`。
  资产名与 `v0.3.15` 逐一对应，只有 MSI 从 `0.3.15` 变 `0.3.16`。

### 7.2 `v0.3.17`

- tag `v0.3.17` → `945ea75b7672c225b5195168bf8f4a2f5be8705f`
  （**先建 release 再推 tag**：`gh release create --target <短 SHA>` 会被 API 以
  `target_commitish is invalid` 拒掉，必须给完整 40 位）
- https://github.com/lilyco-42/ghboost/releases/tag/v0.3.17
- 30 个资产 / 649.9 MB。逐个名字与 `v0.3.16` 对过，**只有 MSI 从
  `lilyco-ghboost-0.3.16-x86_64.msi` 变成 `lilyco-ghboost-0.3.17-x86_64.msi`**
  （脚本做的集合差，确认无遗漏无多余）；每个资产大小都与本地暂存文件逐字节对得上，
  排除大文件上传被截断。
- 649.9 MB 一次 `gh release create` 传不上去（universal APK 单个就 197 MB），
  分 6 批 `gh release upload`；先传大的。
- iOS 模拟器那份 `libghboost.dylib` 依旧**跳过**：它和真机那份重名，只能上一份
  （与 `v0.3.15`/`v0.3.16` 一致，不是这版新引入的取舍）。

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

**`ghboost-tray` 已补**（`533d46b`）：`tray.yml` 以前只有 build，等于「改这个 crate
只有编译器兜底」，而它恰恰是唯一要发给用户双击运行、还要走 SignPath 签名的二进制。
现在有 `cargo fmt -- --check` + `cargo clippy --all-targets --locked -- -D warnings`
两道闸，命令与根 crate 逐字一致；顺手把 toolchain 钉成 `1.98.1`（`fmt --check` 在
stable 升版时会毫无征兆飘红，`build-all.yml:32` 有前例）。首次即绿 ——
tray run 36411407842 的 step 5 / step 6 都是 `success`（不是 skipped）。

**`ghboost-ffi` 仍然没有，而这不是漏了，是照抄一行会更糟**：它的 `jni` 挂在
`default = ["android"]` 后面，`meow-*` 全部 `cfg(target_os = "android")`，所以跑
host clippy 会把 `nativeScan` / `nativeSetHomeDir` / `nativeTest` / `nativeListCores`
**一个都不编** —— 正好是这轮改的那批。这样的闸比没有闸更坏：绿灯是假的。
真要闸只能 `cargo clippy --target aarch64-linux-android`（要 NDK + 该 target 的
rust-std，且 `cargo-ndk` 本身不跑 clippy），是独立工作量，不夹在发版里做。

### 8.3 内核许可证：桌面侧漏了一份（已在发出去的两版里），Android 侧一份都没有

AVD 复测时顺手对了一下「产物里到底装了什么」，结果挖出**三层**问题，一层套一层。
先说最要命的：**已经发布出去的 v0.3.16 / v0.3.17 里，桌面 zip 缺 sing-box 的
GPL-3.0 正文**，而 81 MB 的 GPL 内核 `kernel/bin/sing-box.exe` 就在同一个包里。

**第一层：URL 永久失效 + 失败被吞。** `tray.yml` 取 sing-box 许可证打的是
`raw.githubusercontent.com/SagerNet/sing-box/main/LICENSE`，该 URL **现在 404**
（上游挪了文件），而当时包在 `try/catch` 里只打 `WARN`「non-fatal」——
于是**每一次**构建都静默少一份文本。从 v0.3.16 到 v0.3.17 一直如此。
实测已发布的 v0.3.17 zip：`kernel/` 下有 `mihomo-LICENSE.txt`（35,149 B）和
`xray-LICENSE.txt`（16,725 B），**没有** `sing-box-LICENSE.txt`，
而 `THIRD-PARTY-NOTICES.md` 里明明白白写着它在安装目录中。

**第二层：不能简单改成钉版本 tag。** `v1.14.2` 的 `LICENSE` 只有 **791 字节** ——
是 `Copyright (C) 2022 by nekohasekai` + 一句「详见 GPL-3.0」的**版权声明**，
不含正文。而 notices 承诺的是「GPL-3.0 全文」。所以「修好 URL」还不够，
必须换成 gnu.org 的规范全文（和 mihomo 那步同一个来源）。791 字节这个数字也顺手
定下了断言的阈值下限（见第三层）。

**第三层：没有任何断言，所以以上都无人察觉。** 承诺与产物对不上，没有任何东西
会发现。已修（`521c244`）：

- sing-box 许可证改取 `gnu.org/licenses/gpl-3.0.txt`，**取不到就炸**；
- xray 那处同样的 `try/catch` + WARN 一并去掉（它的 URL 现在还活着，但同样
  不该让「分发义务」变成可选项）；
- `Package` 步新增断言：从 `THIRD-PARTY-NOTICES.md` 里**正则抓**出承诺的
  `kernel/*-LICENSE.txt`，逐个要求存在且 `>= 10 KB`。文件名从 notices 抓而不写死
  列表 —— 以后往包里加内核，只要 notices 写了，文本就被自动要求。10 KB 下限专门
  挡第二层那种「只有一句『详见 GPL』」的指针声明，同时放行 GPL 全文（~35 KB）
  与 MPL 全文（~16.7 KB）。**这条断言我拿已发布的 v0.3.17 zip 验过：如实报 FAIL。**
- `install.ps1` 原本只搬 `kernel\bin\*` 和 `kernel\mihomo\*`，
  **`kernel\*-LICENSE.txt` 从来没被复制过** —— 所以 notices 那句「安装目录提供」
  对 mihomo / xray 同样不成立。现在显式搬并逐个报大小。
- 安装冒烟测试同步加上「**装完**这一层」的断言。该测试第 317-318 行本来就写着
  「只断言 zip 里有、不断言装完有，正是当年 mihomo 档名 bug 漏掉的那半段」——
  同样的道理对许可证成立，而且这里本来就有洞。

**Android 侧同一类缺口，零许可证文本：**

| | 桌面（tray） | Android APK（修复前） |
|---|---|---|
| 客户端自身许可证 | MIT（根 `LICENSE`，1087 B） | 同 |
| 分发 GPL-3.0 内核 | 是 | **是**（`jniLibs` 里是真 `libmihomo.so`） |
| GPL 正文 | 有（修后三份齐全） | **完全没有** |
| 树内 LICENSE/NOTICE 文件 | 有 | **0 个**，`assets/` 目录都不存在 |

证据：对 `ghboost-android` 整棵树穷举搜 `LICENSE|NOTICE|COPYING|THIRD` **零命中**；
`build-all.yml:184-206` 把真内核塞进 `jniLibs/{arm64-v8a,armeabi-v7a,x86_64}/`
（`:253` 还断言必须是真 ELF）；设备侧交叉印证 —— 4.3 的 logcat 里确实有独立进程
`libmihomo.so` 在跑。已修（同 `521c244` 之后的提交）：新增
「Stage kernel license texts into assets」把三份正文 + `THIRD-PARTY-NOTICES.md`
打进 `assets/licenses/`，带同样的 `>= 10 KB` 断言；再加一步
「Verify license texts are actually inside the APK」**直接开 APK 看** ——
「staging 目录里有」和「用户拿到的 APK 里有」是两件事，Gradle 的
`packagingOptions` 一改就可能把 assets 悄悄丢掉，而前一步的断言照样绿。
断言必须落在**产物**上，不是落在暂存目录上（和 tray 的安装冒烟测试同理）。

**连带一处文档过期**：`ghboost-ffi/Cargo.toml:21-24` 选 meow-rs 的理由写的是
「客户端是 MIT，把 GPL 以『库』的形式链进来会让整个客户端被传染」。W8 之后
APK 里放的就是 GPL 本体，所以**这条论证对发布件不再成立**（对 crate 本身仍成立，
两者现在不是一回事）。桌面侧早就处理了这个问题，Android 侧此前没跟上。

**剩下没闭环的**：已发布出去的 v0.3.16 / v0.3.17 **改不了内容** —— 桌面 zip 里那份
缺失的 sing-box 许可证只能靠下一版补齐（往 Release 挂一份 `sing-box-LICENSE.txt`
可以立刻止血，但正式修法是发新版）。另外 App 里**没有入口让用户看到**
`assets/licenses/`（许可证打进包了，但用户点不到）。这两条需要项目方拍板：
是走「下一版顺手补 UI」还是单独做一版。**在这两条决定之前，不建议再发新版** ——
每发一版就多一批需要同样补救的渠道。

**这一轮修的过程中，CI 又抓出一条**（不在我预判里）：第一版把许可证来源换成
`gnu.org` 规范全文，`tray` run **36415822727 当场红了** ——
`www.gnu.org` 从 GitHub runner 直接超时（`connected host has failed to respond`），
本机 curl 同样失败（curl exit 35），**不是偶发**。于是最终方案是每个文件给
多个来源、GitHub raw 优先（走自家 CDN）、gnu.org 只兜底，且**用大小当判据**
而不是「HTTP 200 就算」：

| 文件 | 首选来源（实测命中） | 大小 |
|---|---|---|
| `mihomo-LICENSE.txt` | `MetaCubeX/mihomo` 的 `Alpha/LICENSE` | 35,149 B |
| `xray-LICENSE.txt` | `XTLS/Xray-core` 的 `LICENSE` | 16,725 B |
| `sing-box-LICENSE.txt` | SPDX `license-list-data` 的 `GPL-3.0-only.txt` | 34,674 B |

第一条尤其合适：取到的就是 **mihomo 自己仓库的** LICENSE，且与 gnu.org 那份
**逐字节相同**（SHA256 `3972DC97…`，本地实测比对过）——既是 GitHub CDN 上的稳源，
语义上也最贴。

顺带一条通用教训，已写进预防清单：**普通依赖取不到可以重试或换源，许可证正文取不到
只有一个正确结果：炸。** 合规来源的失败不允许降级成 `WARN` —— 这正是当初
`try/catch` + `WARN` 让 sing-box 那份连续两版静默缺失的根因。

**收口证据（全部为 CI 实跑，非纸面推断）：**

- `tray` run **36416541453**（`3ad51f3`）`success`：新 step 从三个 GitHub 源取到
  35,149 / 16,725 / 34,674 B；`Package` 的 notices 派生断言通过（**sing-box 那份
  第一次真的在包里**）；`install.ps1` 把三份按大小装进安装目录，装完这一层的
  断言通过。
- `Build All` run **36416541475**（`3ad51f3`）`success`：APK 侧三份 + notices 全部
  staging 落位；「Verify license texts are actually inside the APK」对 **4 个 APK
  各 4 个文件**共 16 条 `ok` —— 许可证是真的在 APK 里，不只是暂存目录里有。
- 本地也真跑过：新 step 与 Android 那段 bash 都用真网络执行过，退出码 0。

顺带修一个既有缺陷：`install.bat` 调的是 **powershell（5.1）**，而 `install.ps1`
是**无 BOM** 的 UTF-8 且含 105 个中文字符 —— 5.1 按 ANSI 读，整个文件中文全是乱码
（实跑可见「已請求退出」变乱码）。`install.bat` 自己的注释已经意识到
「cmd.exe 用 OEM 代码页，非 ASCII 会乱码」，却只防了自己、没防它调用的那个文件。
加 UTF-8 BOM 后实跑确认中文正常。`uninstall.ps1` 本身是纯 ASCII，不受影响。

**需要用户配合**（沿用原清单的 7 / 8 编号 —— 上面 6 条已收口，剩的就是这两条，
都卡在「得有人操作设备」上，不是代码问题）：

7. 真机 USB 调试。**AVD 这条路本轮已经跑通了**（4.3 / 4.4：装机、扫描 18846 节点、
   `alive: 8`、外部导入全部实测通过），所以不再是「只有 AVD 路径」；真机是为了补
   AVD 补不了的两件事 —— tun2socks 真跑流量（START 在 AVD 上被 `startVpn()` 挡回）、
   以及 `avc: denied` 那些 SELinux 拒绝在真机 ROM 上是否更严。
8. simul 麦克风实测（`设置 → 隐私 → 麦克风` 权限）。
