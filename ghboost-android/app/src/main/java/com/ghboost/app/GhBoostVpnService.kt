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
 *   2. GhBoostCore.StartTun2Socks(fd, "127.0.0.1:1080")
 *   3. Rust stack reads/writes the TUN fd, proxies via SOCKS5
 */
class GhBoostVpnService : VpnService() {

    companion object {
        private const val TAG = "GhBoostVPN"
        private const val CHANNEL_ID = "ghboost_vpn"
        private const val NOTIFICATION_ID = 1
        private const val SOCKS_ADDR = "127.0.0.1:1080"
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

        startForeground(NOTIFICATION_ID, buildNotification("Starting..."))
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
            val ok = GhBoostCore.StartTun2Socks(fd, SOCKS_ADDR)
            if (!ok) {
                Log.e(TAG, "StartTun2Socks returned false")
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
            GhBoostCore.StopTun2Socks()
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
