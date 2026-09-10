package com.ghboost.app

import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : AppCompatActivity() {

    private lateinit var tvVersion: TextView
    private lateinit var tvStatus: TextView
    private lateinit var tvNodes: TextView
    private lateinit var btnScan: Button
    private lateinit var btnStart: Button
    private lateinit var btnStop: Button

    private var vpnIntent: Intent? = null
    private var isRunning = false

    /** 原生 tun2socks 是否真的会转发流量；false 时 Start 必须锁住（详见 nativeTunForwardingImplemented）。 */
    private var tunReady = false

    companion object {
        private const val VPN_REQUEST_CODE = 100
        private const val NOTIFICATION_PERMISSION_REQUEST_CODE = 101
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        tvVersion = findViewById(R.id.tvVersion)
        tvStatus = findViewById(R.id.tvStatus)
        tvNodes = findViewById(R.id.tvNodes)
        btnScan = findViewById(R.id.btnScan)
        btnStart = findViewById(R.id.btnStart)
        btnStop = findViewById(R.id.btnStop)

        // Initialize native core
        lifecycleScope.launch(Dispatchers.IO) {
            try {
                GhBoostCore.nativeInit()
                GhBoostCore.nativeSetHomeDir(filesDir.absolutePath)
                val version = GhBoostCore.nativeVersion()
                // nativeVersion() 回的是 JSON（例如 {"name":"ghboost","version":"0.3.12"}），
                // 直接拼進字串使用者就會看到一坨原始 JSON。取出 version 欄位，
                // 取不到就退回原樣（總比顯示壞掉的東西好）。
                val pretty = try {
                    org.json.JSONObject(version).optString("version").ifBlank { version }
                } catch (e: Exception) {
                    version
                }
                // 原生层到底会不会转发流量？不会的话**不能放开 Start** ——
                // 实测按下 Start 会建立 TUN 但没人转发，整机 100% 丢包，
                // 界面却还显示「VPN running」。宁可把按钮锁住并说清楚原因。
                tunReady = try {
                    GhBoostCore.nativeTunForwardingImplemented()
                } catch (e: Throwable) {
                    // .so 裡沒這個符號（例如舊版庫）也要當成不可用，不能假設可以用。
                    Log.w("GhBoost", "nativeTunForwardingImplemented unavailable", e)
                    false
                }

                withContext(Dispatchers.Main) {
                    tvVersion.text = "ghboost v$pretty"
                    if (tunReady) {
                        tvStatus.text = "Ready"
                    } else {
                        tvStatus.text = "Android 端尚未完成：開啟會斷網，先別按 Start"
                        tvNodes.text = "TUN 轉發（tun2socks）還沒接上，現在按 Start 只會讓整支手機連不上網。"
                    }
                    updateButtons()
                }
            } catch (e: Exception) {
                withContext(Dispatchers.Main) {
                    tvStatus.text = "Init failed: ${e.message}"
                }
            }
        }

        btnScan.setOnClickListener { scanNodes() }
        btnStart.setOnClickListener { startVpn() }
        btnStop.setOnClickListener { stopVpn() }

        requestNotificationPermissionIfNeeded()
        updateButtons()
    }

    /**
     * Android 13+ 要另外授權才看得到常駐通知。沒授權的話 VPN 還是能跑，
     * 但使用者看不到任何東西，無從判斷它是不是還活著 —— 所以一進來就問。
     */
    private fun requestNotificationPermissionIfNeeded() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(
                arrayOf(android.Manifest.permission.POST_NOTIFICATIONS),
                NOTIFICATION_PERMISSION_REQUEST_CODE
            )
        }
    }

    private fun scanNodes() {
        tvNodes.text = "Scanning..."
        btnScan.isEnabled = false
        lifecycleScope.launch(Dispatchers.IO) {
            try {
                // nativeScan 需要一个 JSON 参数串；"{}" = 全部走默认参数
                val json = GhBoostCore.nativeScan("{}")
                withContext(Dispatchers.Main) {
                    tvNodes.text = json
                    tvStatus.text = "Scan complete"
                }
            } catch (e: Exception) {
                withContext(Dispatchers.Main) {
                    tvNodes.text = "Scan failed: ${e.message}"
                }
            } finally {
                withContext(Dispatchers.Main) { btnScan.isEnabled = true }
            }
        }
    }

    private fun startVpn() {
        if (!tunReady) {
            tvStatus.text = "Android 端尚未完成：現在開啟只會斷網（tun2socks 還沒接上）"
            return
        }
        // 用局部 val：直接对可变的成员属性 vpnIntent 做 null 检查后，
        // Kotlin 无法智能转换成非空 Intent（可能被并发修改），编译会报
        // "Smart cast to 'android.content.Intent' is impossible"。
        val prepared = VpnService.prepare(this)
        vpnIntent = prepared
        if (prepared != null) {
            startActivityForResult(prepared, VPN_REQUEST_CODE)
        } else {
            onVpnPermissionGranted()
        }
    }

    private fun onVpnPermissionGranted() {
        val intent = Intent(this, GhBoostVpnService::class.java)
        startForegroundService(intent)
        isRunning = true
        updateButtons()
        tvStatus.text = "VPN running"
    }

    private fun stopVpn() {
        val intent = Intent(this, GhBoostVpnService::class.java)
        intent.action = "STOP"
        startForegroundService(intent)
        isRunning = false
        updateButtons()
        tvStatus.text = "Stopped"
    }

    private fun updateButtons() {
        // 转发没接上时 Start 一律锁住：按下去只会断网，没有任何好处。
        btnStart.isEnabled = tunReady && !isRunning
        btnStop.isEnabled = isRunning
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == VPN_REQUEST_CODE && resultCode == Activity.RESULT_OK) {
            onVpnPermissionGranted()
        }
    }
}
