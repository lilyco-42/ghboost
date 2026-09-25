package com.ghboost.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.system.OsConstants
import android.util.Log
import java.io.File
import org.json.JSONObject

/**
 * Android VpnService that creates a TUN interface and hands the fd to
 * the Rust ghboost-ffi tun2socks module.
 *
 * Flow:
 *   1. Builder.establish() → TUN fd
 *   2. GhBoostCore.nativeStartTun2Socks(this, fd, DNS_PORT)
 *   3. Rust lwip 栈读写 TUN fd，TCP 经 SOCKS5 出站转发
 *      （UDP 尚未转发；且 Start 按钮仍锁着，见 tun2socks.rs 的
 *        FORWARDING_IMPLEMENTED —— 要等本地 SOCKS5 代理就位）
 */
class GhBoostVpnService : VpnService() {

    companion object {
        private const val TAG = "GhBoostVPN"
        private const val CHANNEL_ID = "ghboost_vpn"
        private const val NOTIFICATION_ID = 1

        /**
         * 交给 Rust 侧 tun2socks 的「DNS 重定向目标端口」。
         *
         * 必须等于 meow 的 DNS 监听口（与 [LocalProxySetup] 里的
         * `dns.listen: 127.0.0.1:1053` 一致）。tun2socks 会拦截 TUN 里
         * 所有 `dst.port()==53` 的 UDP，直接（plain UDP）转发到这个端口，
         * 由 meow 的 fake-ip DNS 解析 —— **不能**走 SOCKS5 UDP ASSOCIATE 到
         * 1080：那样 meow 只会把包 relay 到 8.8.8.8:53，既不触发 fake-ip
         * 映射、也连不上节点，表现就是 DNS 永远解析不出来（已实测）。
         *
         * 注意：`.addDnsServer()` 给的 8.8.8.8/8.8.4.4 只是「系统 DNS 指到哪」，
         * 真正进 TUN 的 UDP/53 会被 tun2socks 截下来转到 1053，所以改了 meow 的
         * `dns.listen` 端口这里必须同步改。
         */
        private const val DNS_PORT = 1053

        /**
         * 内核选择（W6 选择器读写同一个 SharedPreferences）。
         *
         * `meow` = 内嵌内核（出厂默认，行为与多内核改造前完全一致 ——
         * 三内核二进制要等 W8 CI 注入 jniLibs 才存在，默认 `auto` 会让
         * 没注入的构建直接起不来）；其余 `auto`/`mihomo`/`xray`/`sing-box`
         * 走 exec 子进程，由 [GhBoostCore.nativeStartCore] 处理。
         */
        const val PREFS_NAME = "ghboost"
        const val PREF_ENGINE = "engine"
        const val ENGINE_EMBEDDED = "meow"

        /**
         * 代理内核**真的**起来了吗 —— 这是 UI 唯一该信的真相。
         *
         * 为什么不能让 Activity 自己维护一个 isRunning：
         *   [startVpn] 里每一步失败（establish 拿不到 fd、内核起不来、
         *   tun2socks 起不来）都会 `stopSelf()`，而 **Activity 完全收不到通知**。
         *   Activity 一旦乐观地把界面切成「已连接」，就会停在
         *   「显示 VPN running、Start 却按不下去」的死路上 ——
         *   使用者唯一的出路是卸载。这正是本专案最想避免的失败形态。
         *
         * 所以状态只在这里写、只由 Activity 读；Activity 在 onResume 时重新读一次。
         * Service 与 Activity 同进程（Manifest 没写 android:process），静态字段可见。
         */
        @Volatile
        var kernelRunning: Boolean = false
            private set

        internal fun markKernelRunning() {
            kernelRunning = true
        }

        internal fun markKernelStopped() {
            kernelRunning = false
        }
    }

    private var tunFd: ParcelFileDescriptor? = null
    private var isRunning = false

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "STOP") {
            stopVpn()
            return START_NOT_STICKY
        }

        // startForeground 是從系統端拋回來的：一旦失敗（例如 Android 15+ 的
        // MissingForegroundServiceTypeException）會一路炸到 ActivityThread，
        // 整個 App 當掉，使用者只看到「一直在停止」。
        // 類型已在 Manifest 宣告，這裡再兜一層 —— 真出事就記 log 收掉服務，
        // 絕不再讓它彈崩潰對話框。
        try {
            startForeground(NOTIFICATION_ID, buildNotification("Starting..."))
        } catch (e: Exception) {
            Log.e(TAG, "startForeground failed, aborting", e)
            stopSelf()
            return START_NOT_STICKY
        }
        startVpn()
        return START_STICKY
    }

    private fun startVpn() {
        try {
            // Build TUN interface
            val builder = Builder()
                .setSession("GhBoost")
                .setMtu(1500)
                .addAddress("10.0.0.2", 32)
                .addRoute("0.0.0.0", 0)
                .addDnsServer("8.8.8.8")
                .addDnsServer("8.8.4.4")

            // ── API 29+（Android 10）才有的设定 ────────────────────────
            // setMetered(false)：告诉系统这是不计费的通道（本产品走自建节点），
            // 避免系统在计费网络下限制后台流量。
            // allowFamily：**白名单**语义，只放行会进 TUN 的协议族。
            // 必须显式放行 AF_INET/AF_INET6 —— 否则在部分 ROM 上
            // socket() 会被系统直接拒绝，表现为「VPN 已连接但整个没网」。
            // 注意：Android 的 VpnService.Builder **没有**按协议（tcp/udp）
            // 过滤的 API，唯一的开关就是这里按地址族放行。
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                builder.setMetered(false)
                builder.allowFamily(OsConstants.AF_INET)
                builder.allowFamily(OsConstants.AF_INET6)
            }

            // ── 自排除（MULTI-CORE-PLAN 决策 2）：整个 App（本进程 + exec
            // 内核子进程）不进自己的 TUN —— 内核出站天然绕开隧道，不依赖
            // protect(fd)（内嵌 meow 的 protect 保留，两条腿互为兜底）。
            // 若个别 ROM 上失效，表现是「开 VPN 全网断」，这行日志
            // （连同下面的 establish 结果）就是排障入口。
            try {
                builder.addDisallowedApplication(packageName)
                Log.i(TAG, "self-excluded package=$packageName from VPN")
            } catch (e: Exception) {
                Log.w(TAG, "addDisallowedApplication failed: ${e.message}")
            }

            // Protect the app's own sockets from the VPN
            tunFd = builder.establish()
            if (tunFd == null) {
                Log.e(TAG, "Failed to establish TUN interface")
                stopSelf()
                return
            }

            // 先把代理内核拉起来。顺序不能反：
            //   - 自排除已在 establish() 生效：exec 子进程出站绕开 TUN
            //   - 内嵌 meow 走 protector → bind 127.0.0.1:1080（旧路径不变）
            //   - tun2socks 一启动就会往 1080 送流量
            val configPath = java.io.File(
                LocalProxySetup.configRoot(this@GhBoostVpnService),
                "configs/config.yaml",
            ).absolutePath
            val engine = getSharedPreferences(PREFS_NAME, MODE_PRIVATE)
                .getString(PREF_ENGINE, ENGINE_EMBEDDED) ?: ENGINE_EMBEDDED
            Log.i(TAG, "starting proxy kernel: engine=$engine, config=$configPath")
            val krc = startProxyKernel(engine, configPath)
            if (krc != 0) {
                Log.e(TAG, "proxy kernel start failed (engine=$engine)")
                stopVpn()
                return
            }

            // Hand fd to Rust tun2socks
            val fd = tunFd!!.fd
            Log.i(TAG, "TUN fd=$fd, starting tun2socks...")
            // 必须把 this（VpnService）交给 Rust：它要回调 protect(fd) 让本 App
            // 自己的 socket 绕过 TUN，否则流量会被路由规则卷回隧道形成死循环。
            val rc = GhBoostCore.nativeStartTun2Socks(this@GhBoostVpnService, fd, DNS_PORT)
            if (rc != 0) {
                Log.e(TAG, "nativeStartTun2Socks failed, rc=$rc")
                stopVpn()
                return
            }

            isRunning = true
            updateNotification("Connected")
            Log.i(TAG, "VPN started successfully")
            // 到这里才算真的连上。之前一律不许对外宣称「已连接」。
            markKernelRunning()
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start VPN", e)
            stopVpn()
        }
    }

    /**
     * 按选择拉起内核：`meow` 走内嵌（原路径，protector 那条腿），
     * 其余走 exec 子进程（[GhBoostCore.nativeStartCore]，`auto` 会在
     * Rust 侧按节点协议矩阵选内核）。
     *
     * 选了 exec 但二进制不在（W8 注入前是常态）→ **回落内建**，
     * 不能让整条 VPN 起不来：使用者选 Xray 只是想用它，不是想断网。
     *
     * 返回 0 成功 —— 与两条原生路径的 rc 语义对齐。
     */
    private fun startProxyKernel(engine: String, configPath: String): Int {
        val eff = if (engine != ENGINE_EMBEDDED && !engineAvailable(engine)) {
            Log.w(TAG, "engine=$engine 二进制缺失，回落内建 meow")
            ENGINE_EMBEDDED
        } else {
            engine
        }
        if (eff == ENGINE_EMBEDDED) {
            return GhBoostCore.nativeStartProxyKernel(this@GhBoostVpnService, configPath)
        }
        val root = LocalProxySetup.configRoot(this@GhBoostVpnService).absolutePath
        val raw = try {
            GhBoostCore.nativeStartCore(applicationInfo.nativeLibraryDir, eff, root)
        } catch (e: Exception) {
            Log.e(TAG, "nativeStartCore threw", e)
            return -1
        }
        val r = try {
            JSONObject(raw)
        } catch (e: Exception) {
            Log.e(TAG, "nativeStartCore returned non-JSON: $raw")
            return -1
        }
        if (r.optBoolean("ok")) {
            Log.i(TAG, "exec kernel started: ${r.optString("engine")}")
            return 0
        }
        Log.e(TAG, "nativeStartCore failed: ${r.optString("error")}")
        return -1
    }

    /**
     * exec 引擎的二进制在不在（`nativeListCores` 的 available 位）。
     * `auto` 看它建议的那个内核；list() 问不到一律当缺 ——
     * 回落内建总能跑，比起不来强。
     */
    private fun engineAvailable(engine: String): Boolean {
        val root = LocalProxySetup.configRoot(this@GhBoostVpnService).absolutePath
        val raw = try {
            GhBoostCore.nativeListCores(applicationInfo.nativeLibraryDir, root)
        } catch (e: Throwable) {
            Log.w(TAG, "nativeListCores failed", e)
            return false
        }
        return try {
            val obj = JSONObject(raw)
            val want = if (engine == "auto") obj.optString("auto") else engine
            val arr = obj.getJSONArray("cores")
            for (i in 0 until arr.length()) {
                val c = arr.getJSONObject(i)
                if (c.optString("id") == want) return c.optBoolean("available")
            }
            false
        } catch (e: Exception) {
            Log.w(TAG, "engineAvailable parse failed: $raw", e)
            false
        }
    }

    private fun stopVpn() {
        isRunning = false
        // 先把真相翻成「没在跑」，再去做收尾。
        // 顺序反过来的话，收尾途中 Activity 来读会读到「还在跑」，
        // 界面就又会停在「已连接」而 Start 按不下去。
        markKernelStopped()
        try {
            GhBoostCore.nativeStopTun2Socks()
        } catch (e: Exception) {
            Log.w(TAG, "StopTun2Socks error: ${e.message}")
        }
        // 内核要等它真的退出 —— 否则它还占着 127.0.0.1:1080，
        // 下一次开 VPN 会 bind 失败（表现成「第二次起不来」）。
        try {
            GhBoostCore.nativeStopProxyKernel()
        } catch (e: Exception) {
            Log.w(TAG, "StopProxyKernel error: ${e.message}")
        }
        // exec 内核与它的 DoH 中继同理要真退：1080/1053 不让位，
        // 下一次开 VPN 会起不来（表现成「第二次起不来」）。幂等。
        try {
            GhBoostCore.nativeStopCore()
        } catch (e: Exception) {
            Log.w(TAG, "StopCore error: ${e.message}")
        }
        try {
            tunFd?.close()
        } catch (_: Exception) {}
        tunFd = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
        Log.i(TAG, "VPN stopped")
    }

    override fun onDestroy() {
        stopVpn()
        super.onDestroy()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "GhBoost VPN",
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "GhBoost proxy VPN service"
            }
            val nm = getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(text: String): Notification {
        val pendingIntent = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE
        )

        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
                .setContentTitle("GhBoost")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.ic_lock_lock)
                .setContentIntent(pendingIntent)
                .setOngoing(true)
                .build()
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
                .setContentTitle("GhBoost")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.ic_lock_lock)
                .setContentIntent(pendingIntent)
                .setOngoing(true)
                .build()
        }
    }

    private fun updateNotification(text: String) {
        val nm = getSystemService(NotificationManager::class.java)
        nm.notify(NOTIFICATION_ID, buildNotification(text))
    }
}
