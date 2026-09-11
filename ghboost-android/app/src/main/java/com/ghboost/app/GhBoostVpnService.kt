package com.ghboost.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
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
         * Rust 侧 `tun2socks::start` 已接上 lwip 协议栈，DNS 由栈内的
         * TCP/UDP 处理，这个值目前**被忽略**（参数名 `_dns_port`），
         * 保留是为了将来把 DNS 劫持到本地 resolver 时用。
         * 注意 `.addDnsServer()` 给的是 8.8.8.8/8.8.4.4 —— 要让它们
         * 生效，UDP 转发（SOCKS5 UDP ASSOCIATE）得先做出来，见
         * ghboost-ffi/src/tun2socks.rs 的 `udp_drain`。
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
