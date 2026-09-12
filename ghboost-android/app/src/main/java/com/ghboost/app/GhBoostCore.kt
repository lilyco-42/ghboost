package com.ghboost.app

import android.net.VpnService

/**
 * JNI bridge to ghboost-ffi native library.
 *
 * ⚠️ 这些名字必须和 `ghboost-ffi/src/lib.rs` 里 `#[no_mangle]` 导出的符号**逐字一致**：
 * JNI 按 `Java_<包名>_<类名>_<方法名>` 在 .so 里查找，写错一个字符就只在运行时炸
 * （`UnsatisfiedLinkError`），编译期和 CI 都发现不了 —— 所以改任一侧都要同步改另一侧。
 *
 * 实际用 `nm -D libghboost_ffi.so` 核对过的符号：
 *   Java_com_ghboost_app_GhBoostCore_nativeInit
 *   Java_com_ghboost_app_GhBoostCore_nativeSetHomeDir
 *   Java_com_ghboost_app_GhBoostCore_nativeScan
 *   Java_com_ghboost_app_GhBoostCore_nativeTest
 *   Java_com_ghboost_app_GhBoostCore_nativeAdd
   *   Java_com_ghboost_app_GhBoostCore_nativeStartTun2Socks
   *   Java_com_ghboost_app_GhBoostCore_nativeStopTun2Socks
   *   Java_com_ghboost_app_GhBoostCore_nativeTunForwardingImplemented
   *   Java_com_ghboost_app_GhBoostCore_nativeVersion
 *
 * 历史坑：这里曾声明成 `Init / Scan / ...`（少了 native 前缀），且 StartTun2Socks 的
 * 签名与 Rust 完全不符，结果 APK 一启动就 FATAL EXCEPTION。CI 一直全绿，
 * 因为从来没人在真机/模拟器上跑过它。
 */
object GhBoostCore {

    init {
        System.loadLibrary("ghboost_ffi")
    }

    // ── Lifecycle ──────────────────────────────────────────────

    /** Initialize the core. Call once at app startup. */
    external fun nativeInit()

    /** Set working directory for config/state files. */
    external fun nativeSetHomeDir(homeDir: String)

    // ── Node operations ────────────────────────────────────────

    /**
     * Scan for available nodes.
     * @param params JSON 参数串；全部使用默认值时传 "{}"
     * @return JSON 结果串
     */
    external fun nativeScan(params: String): String

    /**
     * Speed-test nodes.
     * @param params JSON 参数串
     * @return JSON 结果串
     */
    external fun nativeTest(params: String): String

    /**
     * Export / inject a subscription.
     * @param params JSON 参数串
     * @return JSON 结果串
     */
    external fun nativeAdd(params: String): String

    // ── TUN mode ───────────────────────────────────────────────

    /**
     * Start tun2socks.
     *
     * @param service 正在运行的 VpnService。Rust 侧要用它回调
     *   `VpnService.protect(fd)`，让本 App 自己的出向 socket 绕过 TUN ——
     *   否则这些流量会按路由规则又被卷回隧道，形成死循环，永远连不上。
     * @param tunFd 来自 `VpnService.Builder.establish()` 的 fd
     * @param dnsPort tun2socks 本地监听的 DNS 端口
     * @return 0 成功；负值失败（Rust 侧目前失败返回 -1）
     */
    external fun nativeStartTun2Socks(service: VpnService, tunFd: Int, dnsPort: Int): Int

    /** Stop the tun2socks proxy. */
    external fun nativeStopTun2Socks()

    /**
     * 这个 .so 里的 tun2socks 是否**真的会转发流量**。
     *
     * 现在是 false —— `start()` 只是置了个标志，TUN fd 没有任何人读写。
     * 实测（API 36 模拟器）：按下 Start 后 TUN 建起来、系统显示 VPN 已连接，
     * 但 ping 8.8.8.8 是 100% 丢包，等于整机断网。所以在它变成 true 之前，
     * UI 必须拦住 Start，不能让使用者按下一个「按了就断网」的按钮。
     *
     * 由 Rust 侧 `tun2socks::FORWARDING_IMPLEMENTED` 决定，接上 lwip 后翻成 true
     * 即可，这里不用改。
     */
    external fun nativeTunForwardingImplemented(): Boolean

    // ── 代理内核（meow-rs，内嵌）─────────────────────────────────

    /**
     * 启动内嵌的代理内核。
     *
     * 内核负责把 tun2socks 送来的 SOCKS5 流量按订阅节点送出去（协议解析、
     * 规则匹配、选路）。它必须跑在**本进程内** —— `VpnService.protect(fd)`
     * 只能保护本进程已打开的 fd，没有进程级 API，spawn 出去的内核出站
     * 会被自己的 TUN 卷回，形成死循环。
     *
     * @param service 正在运行的 VpnService。Rust 侧要用它回调
     *   `VpnService.protect(fd)` —— 内核启动阶段（拉订阅、健康检查）就会
     *   开 socket，那些出站必须已经被 protect，否则会被自己的 TUN 卷回。
     * @param configPath mihomo 风格 YAML 的绝对路径，由 [LocalProxySetup] 写好
     * @return 0 成功；-1 失败（原因写 stderr）
     */
    external fun nativeStartProxyKernel(service: VpnService, configPath: String): Int

    /** 停止代理内核，并等它真正退出（否则会占着 1080 端口）。 */
    external fun nativeStopProxyKernel()

    /** 代理内核是否在运行。 */
    external fun nativeProxyKernelRunning(): Boolean

    // ── Info ───────────────────────────────────────────────────

    /** Return version string (semver from Cargo.toml). */
    external fun nativeVersion(): String
}
