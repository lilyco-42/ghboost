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

    /**
     * Start 是否可以按。**两个条件都满足**才放开：
     *   1. 原生层真的会转发（`nativeTunForwardingImplemented`）
     *   2. 节点清单里有真实节点（不是出厂占位）
     *
     * 少了第 2 条就会出现最难排查的失败形态：内核正常启动、VPN 显示已连接，
     * 但每个连接都指向那个没人监听的占位节点，使用者「已连接却打不开网页」。
     */
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
                val forwardingReady = try {
                    GhBoostCore.nativeTunForwardingImplemented()
                } catch (e: Throwable) {
                    // .so 裡沒這個符號（例如舊版庫）也要當成不可用，不能假設可以用。
                    Log.w("GhBoost", "nativeTunForwardingImplemented unavailable", e)
                    false
                }

                // 本機代理設定檔（mihomo）。第一次開啟時寫入，之後不覆蓋。
                // 這一步不依賴 tun2socks 是否就緒：先把檔案備好，等原生端
                // 能拉起內核時直接就能用，也讓進階使用者現在就能自己改。
                val configChanged = try {
                    LocalProxySetup.ensureConfig(this@MainActivity)
                } catch (e: Exception) {
                    Log.w("GhBoost", "ensureConfig failed", e)
                    false
                }
                val configReady = LocalProxySetup.hasUsableConfig(this@MainActivity)
                // 光有設定檔不夠：占位節點等於「沒有節點」，開了會連不上任何網站。
                val nodesReady = LocalProxySetup.hasRealNodes(this@MainActivity)

                tunReady = forwardingReady && nodesReady

                withContext(Dispatchers.Main) {
                    tvVersion.text = "ghboost v$pretty"
                    when {
                        tunReady -> tvStatus.text = "Ready"
                        !forwardingReady ->
                            tvStatus.text = "Android 端尚未完成：開啟會斷網，先別按 Start"
                        else ->
                            // 转发能力有了，缺的是节点。
                            tvStatus.text = "還差節點：填好節點清單才能開始加速"
                    }
                    tvNodes.text = if (tunReady) {
                        "代理核心已就緒，按 Start 開始加速。"
                    } else {
                        buildConfigHint(configChanged, configReady, forwardingReady)
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

    /**
     * Start 被锁住时，给使用者的说明。
     *
     * 重点不是解释技术细节，而是让他知道「我做了什么」以及「现在能做什么」——
     * 一个只说「还没完成」的画面等于死路，使用者下一步只能卸载。
     *
     * 两种锁法要说不同的话：转发能力没就绪（原生问题）vs 节点没填（使用者一步就能解决）。
     * 说反了会让使用者做无用功。
     */
    private fun buildConfigHint(
        configChanged: Boolean,
        configReady: Boolean,
        forwardingReady: Boolean,
    ): String {
        val lines = mutableListOf<String>()
        lines += if (forwardingReady) {
            "TUN 轉發已就緒，但節點清單還是空的 —— 填進去就能開始加速。"
        } else {
            "TUN 轉發尚未啟用，現在按 Start 只會讓手機連不上網，所以按鈕先鎖著。"
        }
        if (configReady) {
            lines += if (configChanged) {
                "已在本機寫好代理設定檔（Socks5 127.0.0.1:${LocalProxySetup.SOCKS5_PORT}）。"
            } else {
                "本機代理設定檔已就緒（Socks5 127.0.0.1:${LocalProxySetup.SOCKS5_PORT}）。"
            }
            lines += "把訂閱節點填進 providers/ghboost.yaml 才算真的能加速。"
        } else {
            lines += "沒能寫入代理設定檔，請確認 App 儲存空間是否可用。"
        }
        return lines.joinToString("\n")
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
