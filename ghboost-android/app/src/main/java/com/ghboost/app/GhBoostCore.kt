package com.ghboost.app

/**
 * JNI bridge to ghboost-ffi native library.
 *
 * Native symbols (verified via nm -D):
 *   Java_com_ghboost_app_GhBoostCore_Init
 *   Java_com_ghboost_app_GhBoostCore_SetHomeDir
 *   Java_com_ghboost_app_GhBoostCore_Scan
 *   Java_com_ghboost_app_GhBoostCore_Test
 *   Java_com_ghboost_app_GhBoostCore_Add
 *   Java_com_ghboost_app_GhBoostCore_StartTun2Socks
 *   Java_com_ghboost_app_GhBoostCore_StopTun2Socks
 *   Java_com_ghboost_app_GhBoostCore_Version
 */
object GhBoostCore {

    init {
        System.loadLibrary("ghboost_ffi")
    }

    // ── Lifecycle ──────────────────────────────────────────────

    /** Initialize the core. Call once at app startup. */
    external fun Init()

    /** Set working directory for config/state files. */
    external fun SetHomeDir(homeDir: String)

    // ── Node operations ────────────────────────────────────────

    /** Scan for available nodes. Returns JSON array string. */
    external fun Scan(): String

    /** Speed-test a node by URL. Returns JSON result string. */
    external fun Test(url: String): String

    /** Add a subscription URL. Returns JSON status string. */
    external fun Add(url: String): String

    // ── TUN mode ───────────────────────────────────────────────

    /**
     * Start tun2socks with the given TUN fd.
     * @param tunFd  file descriptor from VpnService.Builder.establish()
     * @param socksAddr  SOCKS5 address, e.g. "127.0.0.1:1080"
     * @return true on success
     */
    external fun StartTun2Socks(tunFd: Int, socksAddr: String): Boolean

    /** Stop the tun2socks proxy. */
    external fun StopTun2Socks(): Boolean

    // ── Info ───────────────────────────────────────────────────

    /** Return version string (semver from Cargo.toml). */
    external fun Version(): String
}
