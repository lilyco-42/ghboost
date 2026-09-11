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

            // 先把代理内核拉起来。顺序不能反：
            //   - 内核启动时会先装 protector，再 bind 127.0.0.1:1080
            //   - tun2socks 一启动就会往 1080 送流量
            // 反过来的话，内核的出站（拉订阅、健康检查）还没被 protect，
            // 会被自己的 TUN 卷回去形成死循环。
            val configPath = java.io.File(
                LocalProxySetup.configRoot(this@GhBoostVpnService),
                "configs/config.yaml",
            ).absolutePath
            Log.i(TAG, "starting proxy kernel, config=$configPath")
            val krc = GhBoostCore.nativeStartProxyKernel(this@GhBoostVpnService, configPath)
            if (krc != 0) {
                Log.e(TAG, "nativeStartProxyKernel failed, rc=$krc")
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
        // 内核要等它真的退出 —— 否则它还占着 127.0.0.1:1080，
        // 下一次开 VPN 会 bind 失败（表现成「第二次起不来」）。
        try {
            GhBoostCore.nativeStopProxyKernel()
        } catch (e: Exception) {
            Log.w(TAG, "StopProxyKernel error: ${e.message}")
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
