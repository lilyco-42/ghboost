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

        val config = File(root, "configs/config.yaml")
        if (!config.exists()) {
            config.parentFile?.mkdirs()
            config.writeText(defaultConfig())
            changed = true
            Log.i(TAG, "wrote ${config.absolutePath}")
        } else {
            Log.i(TAG, "config exists, left untouched: ${config.absolutePath}")
        }

        val provider = File(root, "providers/ghboost.yaml")
        if (!provider.exists()) {
            provider.parentFile?.mkdirs()
            provider.writeText(placeholderProvider())
            changed = true
            Log.i(TAG, "wrote ${provider.absolutePath}")
        } else {
            Log.i(TAG, "provider exists, left untouched: ${provider.absolutePath}")
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

    private fun defaultConfig(): String = """
        # GhBoost on Android — mihomo 配置
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

        # 地理数据：用官方 GeoIP / GeoSite，规则里引用 GEOIP,TW / GEOSITE,github
        geodata-mode: true
        geox-url:
          geoip: "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.dat"
          geosite: "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geosite.dat"

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
        proxy-providers:
          ghboost:
            type: file
            path: ./providers/ghboost.yaml
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

        rules:
          # 中国台湾地区本地服务、网银直连（与桌面端规则一致）
          - GEOIP,TW,DIRECT
          - GEOSITE,category-ads-all,REJECT
          - GEOSITE,github,PROXY
          - GEOSITE,google,PROXY
          - GEOSITE,youtube,PROXY
          - MATCH,PROXY
    """.trimIndent()

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
