package com.ghboost.app

import android.content.Context
import android.util.Log
import java.io.File

/**
 * 使用者第一次按 Start 时，本机还没有可用的 SOCKS5 代理。
 *
 * tun2socks 只会把 TUN 流量丢给 `127.0.0.1:1080`；如果那里没人监听，
 * 每个连线都是 `ECONNREFUSED` —— 界面写着「VPN running」，实际上整支
 * 手机连不上网。这比完全断网更难排查，所以宁可先替使用者把配置写好。
 *
 * 这里做三件事，全部是**幂等且不覆盖使用者改动**的：
 *   1. 建出 mihomo 的目录结构（configs/、providers/、state/、logs/）
 *   2. 写一份 `config.yaml`：SOCKS5 入口开在 127.0.0.1:1080，
 *      节点来自 `providers/ghboost.yaml`
 *   3. 写一份 `providers/ghboost.yaml` 模板，里面放一个「直连占位」节点
 *
 * ⚠️ 载入这份配置后，mihomo 的 SOCKS5 入口是本机唯一出口，而它自己的
 * 出站**也必须被 protect**（走 `VpnService.protect`），否则仍会回圈。
 * 所以这里只负责把「文件就位」，真正的连通要等 mihomo 随 App 启动、
 * 且 us 把 protect 接上它的 socket —— 在那之前 Start 仍然锁着
 * （`FORWARDING_IMPLEMENTED` = false）。
 *
 * TODO(m3): 内嵌 mihomo 二进制（arm64-v8a / armeabi-v7a / x86_64）
 * 并在 GhBoostVpnService 里拉起它、把它的进程 socket protect 掉。
 * 届时就由 [hasUsableConfig] 判断「能不能放开 Start」，而不是看
 * 一个写死的常数。
 */
object LocalProxySetup {

    private const val TAG = "GhBoostProxy"

    /** mihomo 的 SOCKS5 入口端口，必须和 tun2socks.rs 的 DEFAULT_SOCKS5 一致。 */
    const val SOCKS5_PORT = 1080

    /** mihomo 的 HTTP 入口（给不使用 VPN 的场景顺手用；不影响 tun2socks）。 */
    const val HTTP_PORT = 7890

    /** mihomo 的控制端口（只绑 127.0.0.1，不设 secret 也不外露）。 */
    private const val CONTROLLER_PORT = 9090

    /** 订阅地址：与 Windows 托盘端同一个来源，方便使用者对照。 */
    private const val SUBSCRIPTION_URL = "https://lain42.top/sub"

    /**
     * 出厂占位节点的名字。它的存在等价于「还没有任何真实节点」。
     * [hasRealNodes] 靠它判断该不该放开 Start，所以这个名字只能在这里出现一次。
     */
    private const val PLACEHOLDER_MARKER = "DIRECT-PLACEHOLDER"

    /**
     * 配置格式版本。**改了配置内容就要 +1** —— 否则老使用者盘上那份旧配置
     * 永远不会被更新（`ensureConfig` 默认不覆盖），新加的字段等于不存在。
     *
     * v1 → v2：修掉 `GEOIP,TW,DIRECT` 导致内核起不来（见 [defaultConfig] 注释）。
     * v2 → v3：provider 路径改成绝对路径 —— 相对路径按进程 CWD 解析，
     *   在 Android 上永远找不到，代理组会静默退回 DIRECT（走了直连不走节点）。
     */
    private const val CONFIG_VERSION = 3

    /** 配置里用来标记版本的注释行，形如 `# ghboost-config-version: 2`。 */
    private const val VERSION_MARKER = "# ghboost-config-version:"

    /** GeoIP 库文件名（放在 assets 里，首次启动时复制出来）。 */
    private const val MMDB_ASSET = "Country.mmdb"

    /**
     * 建立目录结构并（在不覆盖既有文件的前提下）写入配置。
     *
     * @return true 表示至少改动了盘上的东西；false 表示一切都已存在，
     *   使用者自己维护的配置原封不动。
     */
    fun ensureConfig(context: Context): Boolean {
        val root = File(context.filesDir, "mihomo")
        var changed = false

        for (sub in listOf("configs", "providers", "state", "logs")) {
            val d = File(root, sub)
            if (!d.exists() && d.mkdirs()) {
                changed = true
                Log.i(TAG, "created ${d.absolutePath}")
            }
        }

        // GeoIP 库：从 assets 复制到 filesDir。没有它就**不能用 GEOIP/GEOSITE 规则**
        // —— 内核会在加载配置时直接失败（实测：Failed to load GeoIP database）。
        val mmdb = ensureGeoData(context, root)
        if (mmdb != null) changed = true

        // provider 檔案路徑要在寫 config 之前算出來 —— config 裡要引用它的**絕對路徑**。
        val providerFile = File(root, "providers/ghboost.yaml")

        val config = File(root, "configs/config.yaml")
        // 版本不符就重写。只判断「文件是否存在」是不够的：配置内容会随版本演进，
        // 老使用者盘上的旧配置会永远卡在旧格式上（这次的 GEOIP 崩溃就是这来的）。
        val existingVersion = if (config.isFile) {
            runCatching { config.readLines().firstOrNull { it.startsWith(VERSION_MARKER) } }
                .getOrNull()
                ?.removePrefix(VERSION_MARKER)?.trim()?.toIntOrNull()
        } else {
            null
        }

        if (existingVersion != CONFIG_VERSION) {
            config.parentFile?.mkdirs()
            config.writeText(defaultConfig(mmdb?.absolutePath, providerFile.absolutePath))
            changed = true
            Log.i(
                TAG,
                "wrote ${config.absolutePath} (version $existingVersion -> $CONFIG_VERSION, " +
                    "geodata=${mmdb != null})",
            )
        } else {
            Log.i(TAG, "config up to date (v$CONFIG_VERSION): ${config.absolutePath}")
        }

        if (!providerFile.exists()) {
            providerFile.parentFile?.mkdirs()
            providerFile.writeText(placeholderProvider())
            changed = true
            Log.i(TAG, "wrote ${providerFile.absolutePath}")
        } else {
            Log.i(TAG, "provider exists, left untouched: ${providerFile.absolutePath}")
        }

        return changed
    }

    /**
     * 盘上的配置是否「看起来可用」。
     *
     * 只检查文件存在与关键字段，不尝试连网 —— 给 UI 一个诚实的说法，
     * 别让使用者以为按下去就有网。
     */
    fun hasUsableConfig(context: Context): Boolean {
        val root = File(context.filesDir, "mihomo")
        val config = File(root, "configs/config.yaml")
        val provider = File(root, "providers/ghboost.yaml")
        if (!config.isFile || !provider.isFile) return false
        val text = runCatching { config.readText() }.getOrNull() ?: return false
        return text.contains("mixed-port") && text.contains("proxy-providers")
    }

    /** 配置根目录，给将来的 UI（例如「打开设定」）用。 */
    fun configRoot(context: Context): File = File(context.filesDir, "mihomo")

    /**
     * 把 GeoIP 库从 assets 复制出来（幂等）。返回可用的路径，没有则 null。
     *
     * **为什么必须复制**：meow 的 `geodata.mmdb-path` 要一个**文件路径**，
     * 而 assets 里的东西不是普通文件、读不到真实路径，所以必须先落地。
     * 没有这个库就不能用 `GEOIP,TW,DIRECT` —— 内核加载配置时会直接失败。
     */
    private fun ensureGeoData(context: Context, root: File): File? {
        val dst = File(root, MMDB_ASSET)
        if (dst.isFile && dst.length() > 0) return dst

        return try {
            context.assets.open(MMDB_ASSET).use { input ->
                dst.parentFile?.mkdirs()
                dst.outputStream().use { out -> input.copyTo(out) }
            }
            if (dst.length() > 0) {
                Log.i(TAG, "extracted $MMDB_ASSET -> ${dst.absolutePath} (${dst.length()} bytes)")
                dst
            } else {
                Log.w(TAG, "$MMDB_ASSET extracted but empty")
                null
            }
        } catch (e: Exception) {
            // 没打包这个 asset 也不该让 App 挂掉：退回「不用 GEO 规则」的配置。
            Log.w(TAG, "$MMDB_ASSET not bundled, GeoIP rules will be disabled", e)
            null
        }
    }

    /**
     * 从外部目录导入节点清单（可选）。
     *
     * **为什么需要这条路**：`providers/ghboost.yaml` 在 app 私有目录
     * （`/data/data/.../files/`），使用者**根本碰不到** —— 普通 App 没有 root，
     * 也没有文件管理器权限。所以「把訂閱節點填進 providers/ghboost.yaml」
     * 这句文案本来是个死路：看得到、做不到。
     *
     * 给一条真能走的路：把节点文件放到 App 自己的**外部**目录
     * （不需要任何权限，插上数据线或用文件管理器都能放）：
     *
     *     /sdcard/Android/data/com.ghboost.app/files/ghboost/providers.yaml
     *
     * 下次开 App 时自动导入。内容与现状一致就不动，避免每次启动都写盘。
     *
     * @return true 表示这次真的导入了新内容
     */
    fun importExternalNodes(context: Context): Boolean {
        val ext = context.getExternalFilesDir(null)
        if (ext == null) {
            // 外部存储没挂载时这里会是 null —— 不是「没有节点」，是「看不到」。
            // 必须区分，否则使用者放了文件却毫无反馈。
            Log.i(TAG, "import: getExternalFilesDir() == null (external storage unavailable)")
            return false
        }

        // 两个候选：优先**直接放在 files/ 下**的那个。
        // 原因：files/ 本身由 App 创建（属主就是 App），一定能进；
        // 而 `ghboost/` 这种子目录如果是用 adb / 文件管理器建的，
        // 属主是 shell、权限 2770，App 可能连目录都进不去 → 文件「看得见列不出」。
        // 这是实测踩到的坑，不是理论顾虑。
        val candidates = listOf(
            File(ext, "ghboost-providers.yaml"),
            File(ext, "ghboost/providers.yaml"),
        )

        for (src in candidates) {
            Log.i(TAG, "import: try ${src.absolutePath} exists=${src.isFile}")
        }

        val src = candidates.firstOrNull { it.isFile } ?: return false

        val text = runCatching { src.readText() }.getOrNull()
        if (text == null) {
            Log.w(TAG, "import: ${src.absolutePath} exists but unreadable")
            return false
        }
        Log.i(TAG, "import: read ${text.length} chars from ${src.absolutePath}")

        // 空文件、或只是把占位又抄了一遍 —— 都不算「有节点」。
        if (text.isBlank() || text.contains(PLACEHOLDER_MARKER)) {
            Log.i(TAG, "import: content is blank or still the placeholder, ignoring")
            return false
        }

        val dst = File(configRoot(context), "providers/ghboost.yaml")
        if (dst.isFile && runCatching { dst.readText() }.getOrNull() == text) {
            Log.i(TAG, "import: already up to date")
            return false
        }

        return try {
            dst.parentFile?.mkdirs()
            dst.writeText(text)
            Log.i(TAG, "imported nodes: ${src.absolutePath} -> ${dst.absolutePath}")
            true
        } catch (e: Exception) {
            Log.w(TAG, "import nodes failed", e)
            false
        }
    }

    /** 外部导入文件应该放的位置，给 UI 显示用。 */
    fun externalImportPath(context: Context): String =
        File(context.getExternalFilesDir(null) ?: context.filesDir, "ghboost-providers.yaml")
            .absolutePath

    /**
     * 节点清单里是否有**真实节点**（而不是出厂占位）。
     *
     * 为什么需要这个判断：内嵌内核本身能跑起来，但如果节点清单还是那个
     * 「DIRECT-PLACEHOLDER（指向 127.0.0.1:1081，没人监听）」，
     * 内核会正常启动、VPN 也会显示已连接，**但每个连接都失败** ——
     * 使用者看到「已连接」却打不开网页，这比直接锁住按钮更难排查。
     *
     * 所以 Start 必须同时满足「原生会转发」+「有真实节点」两个条件。
     * 判据是「provider 里不含占位标记」：换成订阅（type: http）之后
     * 文件里是真实节点，占位标记自然消失。
     */
    fun hasRealNodes(context: Context): Boolean {
        val provider = File(configRoot(context), "providers/ghboost.yaml")
        if (!provider.isFile) return false
        val text = runCatching { provider.readText() }.getOrNull() ?: return false
        return !text.contains(PLACEHOLDER_MARKER)
    }

    /**
     * 生成出厂配置。
     *
     * @param mmdbPath GeoIP 库的绝对路径；为 null 时**不能**用 GEOIP/GEOSITE 规则
     *   （meow 会在加载配置时直接失败：`Failed to load GeoIP database`）。
     */
    private fun defaultConfig(mmdbPath: String?, providerPath: String): String {
        // 用占位符替换而不是字符串插值：插进来的多行内容会打乱
        // `trimIndent()` 的公共缩进推断，结果 YAML 缩进错乱、内核解析失败。
        val template = """
        # GhBoost on Android — mihomo 配置
        $VERSION_MARKER $CONFIG_VERSION
        #
        # 这个文件由 App 首次启动时自动生成，之后**不会再被覆盖**，
        # 你可以放心改。要恢复原样就把整个 mihomo/ 目录删掉再开一次 App。
        #
        # 订阅地址：$SUBSCRIPTION_URL
        # 节点来源：providers/ghboost.yaml（可换成你自己的订阅）
        #
        # ⚠️ 只监听 127.0.0.1：入口不对局域网开放，避免变成开放的代理。

        mixed-port: $HTTP_PORT
        redir-port: 0
        tproxy-port: 0
        socks-port: $SOCKS5_PORT
        allow-lan: false
        bind-address: 127.0.0.1
        mode: rule
        log-level: warning
        ipv6: false

        # 只绑本机；不设 secret，因为根本不出 127.0.0.1
        external-controller: 127.0.0.1:$CONTROLLER_PORT

        # 注意：mihomo 的 `geodata-mode` / `geox-url` 在 meow 里**不支持**
        # （会被忽略并打警告）。地理数据只认下面的 `geodata.mmdb-path`。

        profile:
          store-selected: true
          store-fake-ip: true

        dns:
          enable: true
          listen: 127.0.0.1:1053
          ipv6: false
          enhanced-mode: fake-ip
          fake-ip-range: 198.18.0.1/16
          nameserver:
            - 1.1.1.1
            - 8.8.8.8
          fallback:
            - https://dns.google/dns-query
            - https://1.1.1.1/dns-query

        # 节点从 provider 来；订阅 URL 写在 provider 里
        #
        # ⚠️ path **必须绝对路径**：meow 是直接 `fs::read_to_string(path)`，
        # 相对路径会按**进程 CWD**（Android 上是 `/`）解析 → 找不到文件 →
        # provider 为空 → 代理组退回 DIRECT（表现成「VPN 连上了但不走节点」）。
        proxy-providers:
          ghboost:
            type: file
            path: "$providerPath"
            health-check:
              enable: true
              url: https://www.gstatic.com/generate_204
              interval: 300

        proxies: []

        proxy-groups:
          - name: PROXY
            type: select
            use:
              - ghboost
            proxies:
              - DIRECT

        @GEODATA@

        rules:
        @RULES@
        """.trimIndent()

        return template
            .replace("@GEODATA@", geodataBlock(mmdbPath))
            .replace("@RULES@", rulesBlock(mmdbPath))
    }

    /**
     * `geodata:` 块。
     *
     * **必须给出真实存在的文件路径** —— meow 会在加载配置时就打开它，
     * 缺失直接导致内核启动失败（实测：`Failed to load GeoIP database at
     * ./meow/Country.mmdb`）。注意 mihomo 的 `geodata-mode` / `geox-url`
     * 在 meow 里是**不支持**的字段（会被忽略并告警），别照抄。
     */
    private fun geodataBlock(mmdbPath: String?): String =
        if (mmdbPath != null) {
            """
            geodata:
              mmdb-path: "$mmdbPath"
            """.trimIndent()
        } else {
            "# geodata 未启用：assets 里没有 $MMDB_ASSET"
        }

    /**
     * 规则列表。
     *
     * 有 GeoIP 库才用 `GEOIP` / `GEOSITE`（台湾本地站点、网银直连 —— 与桌面端一致）；
     * 没有就退回纯域名规则。**绝不能**在没有库的情况下写 GEO 规则，
     * 那会让内核连启动都做不到。
     */
    private fun rulesBlock(mmdbPath: String?): String =
        if (mmdbPath != null) {
            """
              # 中国台湾地区本地服务、网银直连（与桌面端规则一致）
              - GEOIP,TW,DIRECT
              - GEOSITE,category-ads-all,REJECT
              - GEOSITE,github,PROXY
              - GEOSITE,google,PROXY
              - GEOSITE,youtube,PROXY
              - MATCH,PROXY
            """.trimIndent()
        } else {
            """
              # 没有 GeoIP 库 → 不能用 GEOIP/GEOSITE，退回域名规则
              - DOMAIN-SUFFIX,github.com,PROXY
              - DOMAIN-SUFFIX,githubusercontent.com,PROXY
              - DOMAIN-SUFFIX,google.com,PROXY
              - DOMAIN-SUFFIX,gstatic.com,PROXY
              - DOMAIN-SUFFIX,googleapis.com,PROXY
              - DOMAIN-SUFFIX,youtube.com,PROXY
              - DOMAIN-SUFFIX,googlevideo.com,PROXY
              - MATCH,PROXY
            """.trimIndent()
        }

    private fun placeholderProvider(): String = """
        # GhBoost 节点清单（占位）
        #
        # 现在里面只有一个 DIRECT 占位，等于「不加速」。
        # 换上真实节点有两条路：
        #
        #   1. 直接改成订阅（推荐，App 会自动更新）：
        #        proxy-providers:
        #          ghboost:
        #            type: http
        #            url: "$SUBSCRIPTION_URL"
        #            interval: 3600
        #            path: ./providers/ghboost.yaml
        #      注意 type 要一起改成 http，并把 url 填上。
        #
        #   2. 或把下面 proxies 换成自己的 VLESS / Trojan / SS 节点。
        #
        # 改完不用重启手机，App 重开即可生效（会走 profile 重载）。

        proxies:
          - name: $PLACEHOLDER_MARKER
            type: socks5
            server: 127.0.0.1
            port: 1081
            udp: false
    """.trimIndent()
}
