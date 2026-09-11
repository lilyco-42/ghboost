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
         * 交给 Rust 侧的 DNS 端口提示值。
         *
         * Rust 侧 `tun2socks::start` 的参数名是 `_dns_port`，即**当前被忽略**：
         * DNS 不需要本地 resolver，靠 TUN 里真实的 DNS 查询走
         * SOCKS5 UDP ASSOCIATE 出去即可（见 `tun2socks.rs` 的 `udp_drain`，
         * 已实现）。保留这个参数是为了将来做「DNS 劫持到本地缓存」时用。
         *
         * 另外注意 `.addDnsServer()` 给的是 8.8.8.8/8.8.4.4：`VpnService`
         * 会把系统 DNS 指到这两个地址，而到它们的 UDP 53 会进 TUN，
         * 由上面的 UDP relay 转发到远端解析 —— 这条链是通的。
         */
        private const val DNS_PORT = 5353
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

            // Protect the app's own sockets from the VPN
            tunFd = builder.establish()
            if (tunFd == null) {
                Log.e(TAG, "Failed to establish TUN interface")
                stopSelf()
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
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start VPN", e)
            stopVpn()
        }
    }

    private fun stopVpn() {
        isRunning = false
        try {
            GhBoostCore.nativeStopTun2Socks()
        } catch (e: Exception) {
            Log.w(TAG, "StopTun2Socks error: ${e.message}")
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
