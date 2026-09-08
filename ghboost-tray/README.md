# ghboost-tray · 公开版桌面外壳

> 一句话：**给不懂命令行的人用的那个壳**。托盘状态灯 + 一个大按钮 + 代理设置。
> 核心能力（hosts 优选、节点扫描/测速、mihomo 进程管理）全部来自
> 上级 [`ghboost` 主 crate](../)，本目录**不实现任何核心逻辑**，只负责"让人点得动"。
>
> 本目录是 ghboost 仓库里的一个**独立 crate**（仓库根 `Cargo.toml` 没有
> `[workspace]` 段，所以它不会被根 CI 一起编，也不会拖垮 Linux/macOS 构建）。
> 依赖主 crate 的方式与 `ghboost-ffi/` 一致：`lilyco-ghboost = { path = ".." }`。

---

## 一、我负责什么 / 不负责什么

| 区域 | 归属 | 说明 |
|---|---|---|
| 托盘图标（状态灯、菜单、点击行为） | **本目录** | 灰=待机 / 黄=处理中 / 绿=已加速 / 红=失败 |
| 傻瓜式面板 `src/panel.html` | **本目录** | 一个大按钮 + 三站点灯 + 代理卡片，零外部请求 |
| 代理设置（系统代理开关 / 订阅导入） | **本目录 + 主 crate 的 `/api/proxy/*`** | 见下节 |
| 公开版要加速哪些域名 | **本目录** `PUBLIC_DOMAINS` | GitHub 9 个 + Google / YouTube 常用 |
| 安装、桌面快捷方式、开机自启 | **本目录** `tools/install.ps1` | |
| hosts 优选算法、DoH、测速 | `../src/hosts.rs` | 本目录只调用，不改 |
| 节点扫描 / 测速 / 注入 | `../src/nodes.rs` | 同上 |
| mihomo 进程管理 | `../src/mihomo.rs` | 本目录只通过 `/api/proxy/*` 驱动 |
| C ABI / cdylib | 主 crate（`../src/ffi*`） | |

**刻意不做的事**：
- 不在本目录重写任何协议解析。订阅导入直接交给 mihomo 的 `proxy-providers`
  （vless / vmess / ss / trojan / hysteria2 / tuic 它原生就支持，还自带健康检查）。
  自己写解析器 = 重复造轮子 + 永远追不上新协议。
- 不内嵌 WebView / 浏览器内核。用系统默认浏览器，LTSC / Server Core 不会白屏。

---

## 二、代理设置（台湾市场）

面向的是**已经有节点、只差一个开关**的用户。在台湾，自建或购买代理节点是
合法的技术服务（Clash Verge、v2rayN 之类的工具都是公开流通的商品），
所以这块做成面板里的一等公民，而不是藏起来的高级选项。

两条路径，UI 上用分段控件切换：

1. **用现成代理**（零依赖，立即可用）
   机器上已经在跑 Clash Verge / v2rayN → 填端口（默认 7890/7891）→ 点开启。
   只做系统代理开关，不启动任何内核。
2. **导入订阅**（需要内置 mihomo 内核）
   填订阅链接 → 生成配置 → 启动内核 → 打开系统代理。
   配置文件里的规则按台湾用户调过：

   ```yaml
   rules:
     - GEOIP,TW,DIRECT      # 台湾本地直连：PTT、露天、蝦皮、各家网银
     - GEOIP,LAN,DIRECT     # 区网直连
     - MATCH,"節點選擇"
   ```

   台湾站点绕一圈出去反而更慢，有些网银还会因异地登录被挡，所以必须直连。
   DNS 的 `fallback-filter.geoip-code` 也相应设成 `TW` 而不是 `CN`。

**我们不提供节点、不中转流量、不收集任何数据。** 订阅链接只在本机使用，
由本机启动的 mihomo 进程直接向订阅地址发起请求。

### 内核与规则库的布局

`install.ps1 -WithKernel` 会把文件放到：

```
%LOCALAPPDATA%\ghboost\bin\mihomo.exe        内核（~50MB）
%LOCALAPPDATA%\ghboost\mihomo\country.mmdb   GEOIP 规则（7.8MB）
%LOCALAPPDATA%\ghboost\mihomo\geosite.dat    geosite 规则（4.2MB）
```

`country.mmdb` **不是可选的**：没有它 `GEOIP,TW,DIRECT` 会静默地永远不匹配，
结果就是台湾本地站点也被送去绕一圈 —— 更慢，而且网银可能判成异地登录。
没有内核时面板会明确提示改用「用现成代理」，不会静默失败。

规则库用 `GEOIP` 而不是 `RULE-SET`：后者要额外联网拉取，多一个失败点，
对"打开就能用"这个目标是负收益。

### 安装与卸载：必须能双击

Windows 双击 `.ps1` **默认是用记事本打开**，根本不会执行；就算改成执行，
默认的执行策略也会拦。所以发行包里带 `.bat` 包装器：

- **安装**：双击 `install.bat`（加 `-WithKernel` 才会顺带下载内核）
- **卸载**：双击 `uninstall.bat`（自动提权 → 还原 hosts 与系统代理 →
  删开机自启与桌面快捷方式；`-RemoveData` 连数据目录一起删）

命令行等价写法：`install.ps1 -WithKernel -NoAutostart` / `uninstall.ps1 -RemoveData`。
`.bat` 一律 ASCII：cmd.exe 走 OEM codepage，写了中文会变乱码。

---

## 三、已经修掉的坑（都写进注释了，看代码能找到）

### 1. 托盘关不掉 —— 根因是缺 Win32 消息泵
`tray-icon` 的官方约束：*Windows 上托盘图标必须在**同一个线程**上跑 win32 事件循环。*
原来的主循环只有 `sleep(80ms)` + `try_recv()`，从没 `DispatchMessage`，
托盘窗口的 WndProc 永远不被调用 → 菜单点击（包括「退出」）根本送不到。

修法见 `src/main.rs` 的 `mod win`。已用自动化脚本验证：`PostMessage` 投递一次
左键点击，`trace.log` 里出现 `tray left click`，说明链路通了 —— 同一条链路上的
菜单事件自然也能收到。

顺带留了三条退路，任何一条都能收掉进程：
- 托盘菜单「退出」
- 面板上的「退出程序」按钮
- `ghboost-tray.exe --quit`（命令行后门）

### 2. 点「一键加速」报 `Failed to fetch` —— 根因是 localhost/127.0.0.1 跨源
浏览器把 `localhost` 和 `127.0.0.1` 当成**两个 origin**。
`GET /api/check` 是简单请求，不预检，照常成功；
`POST /run` 带 `Content-Type: application/json`，会先发 `OPTIONS` 预检，
axum 没有对应 handler → 405 → fetch 直接 reject，前端只看到一句
毫无信息量的 `Failed to fetch`。表现就是"能检测、不能加速"。

修法三处：`guard_loopback_mw` 里放行 OPTIONS 并回 `ACAO: *`；
页面加载时若发现 hostname 是 localhost 就跳回 127.0.0.1；
`fetch` 包装成 `jfetch`，失败时把 origin 一起报出来（原来等于没有错误信息）。

### 3. 非管理员点加速必定失败
与其让用户点一个注定失败的大按钮，不如第一步就引导提权：
`/api/info` 返回 `admin:false` 时，主按钮直接变成「以管理员身份启用加速」。
提权重启时**故意丢掉** `--no-browser` 参数 —— 否则新进程不开浏览器，
用户会面对一个点不动的死页面。

### 4. `--no-browser` 把服务也一起关了
原本这个参数意味着"不起服务"，但开机自启需要的是"不起窗口、服务照跑"，
否则点托盘「打开面板」要现起服务、干等。已拆成 `ensure_console()` + `open_panel()`。

---

### 5. 内核一次都起不来 —— 默认配置本身是坏的（在 `ghboost-main` 修的）
`MihomoManager::start()` 内部先 `generate_config()` 写一份默认配置，那份配置里
`proxy-groups: []` 是空的，但 `rules` 却引用了 `MATCH,🚀 节点选择`
→ mihomo 直接 fatal：`proxy [🚀 节点选择] not found`。

也就是说**不管有没有订阅，内核从来没起来过**，而错误信息只有一句
"Mihomo 启动后立即退出，状态码: exit code: 1"，看不出任何线索。
修法是让默认配置自洽（先定义 group 再引用）。

同一个函数里还有 `dns.listen: 0.0.0.0:53` —— 53 是特权端口，非管理员绑定即失败，
内核同样 fatal。普通代理模式根本用不到它，已去掉。

排查这类问题的方法：**直接把内核拉起来看它的输出**，别只读我们自己的日志。
```bash
cd %LOCALAPPDATA%\ghboost\mihomo && ..\..\ghboost\bin\mihomo.exe -t -d .
```

---

## 四、命令与用法

```bash
ghboost-tray.exe                # 启动，自动打开面板
ghboost-tray.exe --no-browser   # 启动但不开窗口（开机自启用）
ghboost-tray.exe --quit         # 让已运行的实例退出
ghboost-tray.exe --selftest     # 无头跑一遍加速链路并打印结果
```

运行期文件（都在这一个目录里，`uninstall` 直接删）：
`%LOCALAPPDATA%\ghboost\` → `console.port`（当前端口）、`trace.log`（排障日志）、
`mihomo\`（内核配置与订阅缓存）

---

## 五、当前状态与已知限制

已验证（Windows 实机 + 本机构建产物，14 项全绿）：

```
OK   控制台端口 / 首页 200 / 零外部引用 / 域名注入 / 代理卡片
OK   OPTIONS /run 预检 → 204 + ACAO:*
OK   Google 200 / YouTube 200 / GitHub 200
OK   代理状态接口 / POST /run（带 only） / 退出生效 0.5s
```

已知限制：

- **内核未随包分发**：「导入订阅」需要 `mihomo` 二进制在 PATH 或配置目录里。
  缺失时面板会明确提示改用「用现成代理」，不会静默失败。
  正式发行包需要把内核打进去（注意 mihomo 是 GPL-3.0，分发要遵守其条款）。
- **hosts 加速的边界**：能优化建连与首包；YouTube 的视频流走 `googlevideo.com`，
  那是 CDN，IP 段变动极快，写死反而变慢 —— 所以域名列表**刻意不含**它。
  视频部分属于代理产品的职责，不要混进 hosts 里做。
- **写入 hosts 需要管理员**：这是操作系统限制，不是 bug。

---

## 六、代码签名（Windows 发行的硬门槛）

没签名的 exe 会被 SmartScreen 拦成「Windows 已保护你的电脑」，
小白看到就直接关掉 —— 这跟功能好坏无关，是发行侧的门槛。

### 2026 年的现实：三条旧认知已经作废

调研（微软 Learn《Code signing options for Windows app developers》，2026-08 更新）：

| 旧假设 | 现在的实际情况 |
|---|---|
| 买 EV 证书就能跳过 SmartScreen | **2024 年起 EV 不再有即时信誉特权**，与 OV 走同一套累积流程。为 SmartScreen 付 EV 溢价已不成立 |
| Azure Trusted Signing 最便宜，$9.99/月 | 价格属实，但**地域限制致命**：组织限美/加/欧盟/英国，**个人开发者仅限美国、加拿大** —— 台湾身份用不了 |
| 签名 = 立刻没有警告 | 不是。签名只是让信誉能**开始累积**。新发布者的最初几个版本仍会警告，要连续用同一发行者身份发版才会消退 |

### 决策

**台湾身份 + 开源项目 → 走 SignPath Foundation，免费。**

- 给符合资格的开源项目签发 **OV 级**证书（私钥在 SignPath 的 HSM，不落我们手里）
- 无地域限制，台湾可用
- 代价：证书发行者显示 **"SignPath Foundation"** 而不是我们的名字；
  且构建必须**全自动化并可追溯到仓库源码**（这就是 `.github/workflows/tray.yml`
  存在的理由 —— 人工本地编完再上传的不给签）
- 资格条件（对应我们已具备的）：OSI 许可（MIT ✅）、公开仓库 ✅、
  已有 release（待发）、下载页写明功能（本 README + Release 说明）、
  **提供卸载方式**（已补 `tools/uninstall.ps1`）、维护者开 MFA

**商业化后备**：想让发行者显示自己的品牌名，只能买 OV 证书（约 $150–300/年，
台湾可买），届时再切。

### 签名之外的三件事（否则签名也白签）

1. **发行者身份必须稳定**：换证书 / 换名字 = 信誉从零重来。
2. **下载页要教用户怎么点过去**：「详细信息 → 仍要执行」，并给出 SHA256
   校验和（CI 已生成 `SHA256SUMS.txt`）。
3. **别让用户以为中毒**：Release 页面第一屏就说明这是什么、为什么需要管理员。
