package com.ghboost.app

import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.widget.AdapterView
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.Spinner
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : AppCompatActivity() {

    private lateinit var tvVersion: TextView
    private lateinit var tvStatus: TextView
    private lateinit var tvNodes: TextView
    private lateinit var btnScan: Button
    private lateinit var btnStart: Button
    private lateinit var btnStop: Button
    private lateinit var spinnerEngine: Spinner
    private lateinit var tvEngineInfo: TextView

    /** 三内核可用性（id → available），`nativeListCores` 载入后填充。 */
    private val coreAvail = mutableMapOf<String, Boolean>()

    /**
     * Spinner 显示名，**就地改写**（+ notifyDataSetChanged），不换 adapter：
     * 换 adapter 会把选中项重置回第 0 项，还可能让 onItemSelected 拿程序化
     * 的位置当使用者选择，把 `auto` 静默写进 prefs。
     */
    private val engineDisplay = ENGINE_LABELS.toMutableList()

    private lateinit var engineAdapter: ArrayAdapter<String>

    private var vpnIntent: Intent? = null

    /**
     * 现在到底连没连上 —— **只读 Service 的真相**，不再自己维护一份。
     *
     * 两份真相一定会漂移：内核启动失败时 Service 会 stopSelf()，
     * Activity 却还停在「已连接」+ Start 被锁死，使用者出不来。
     * 而 Start 是使用者**唯一的逃生通道**，不能靠猜。
     */
    private val isRunning: Boolean
        get() = GhBoostVpnService.kernelRunning

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

        /**
         * 内核选择器的 token 与显示名（顺序即 Spinner 顺序）。
         * token 必须与 Rust `CoreKind::parse` 别名、Service 的
         * [GhBoostVpnService.PREF_ENGINE] 取值完全一致 —— 三处共用一套值。
         */
        private val ENGINE_TOKENS =
            listOf("auto", "meow", "mihomo", "xray", "sing-box")
        private val ENGINE_LABELS =
            listOf("自動", "內建 (meow)", "Mihomo", "Xray", "sing-box")
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
        spinnerEngine = findViewById(R.id.spinnerEngine)
        tvEngineInfo = findViewById(R.id.tvEngineInfo)
        setupEngineSelector()

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
                // 私有目录里的 providers/ghboost.yaml 使用者碰不到，
                // 所以允许从 App 的外部目录导入一份（插数据线或用文件管理器都能放）。
                val imported = try {
                    LocalProxySetup.importExternalNodes(this@MainActivity)
                } catch (e: Exception) {
                    Log.w("GhBoost", "importExternalNodes failed", e)
                    false
                }
                val configReady = LocalProxySetup.hasUsableConfig(this@MainActivity)
                // 光有设定档不够：占位节点等于「没有节点」，开了会连不上任何网站。
                val nodesReady = LocalProxySetup.hasRealNodes(this@MainActivity)

                // 三内核可用性（W8 注入前全部缺 → 选择器只放行「內建」）。
                val coresRaw = try {
                    GhBoostCore.nativeListCores(
                        applicationInfo.nativeLibraryDir,
                        LocalProxySetup.configRoot(this@MainActivity).absolutePath,
                    )
                } catch (e: Throwable) {
                    // 旧 .so 没这个符号也要当「三内核不可用」处理，不能让 Init 整个挂掉。
                    Log.w("GhBoost", "nativeListCores unavailable", e)
                    ""
                }
                parseCores(coresRaw)

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
                        if (imported) "已從外部檔案匯入節點。按 Start 開始加速。"
                        else "代理核心已就緒，按 Start 開始加速。"
                    } else {
                        buildConfigHint(configChanged, configReady, forwardingReady)
                    }
                    refreshEngineAvailability()
                    updateButtons()
                }
            } catch (e: Exception) {
                withContext(Dispatchers.Main) {
                    tvStatus.text = "Init failed: ${e.message}"
                    // 可用性没载入也别让状态行卡在「載入中」——
                    // coreAvail 是空的，全 ✕/只留內建就是此刻的真相。
                    refreshEngineAvailability()
                }
            }
        }

        btnScan.setOnClickListener { scanNodes() }
        btnStart.setOnClickListener { startVpn() }
        btnStop.setOnClickListener { stopVpn() }

        requestNotificationPermissionIfNeeded()
        updateButtons()
        startEngineStatusLoop()
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
            // 别说「填进 providers/ghboost.yaml」——那在 app 私有目录，使用者碰不到。
            // 要给出**真的能走**的那条路。
            lines += "把節點檔案放到："
            lines += LocalProxySetup.externalImportPath(this)
            lines += "再重開 App 就會自動匯入。"
        } else {
            lines += "沒能寫入代理設定檔，請確認 App 儲存空間是否可用。"
        }
        return lines.joinToString("\n")
    }

    // ── 多内核选择器 + 状态贯通（W6）──────────────────────────

    /**
     * 选择器初始化：显示名、当前 pref 落位、选择回写。
     *
     * 只在使用者真选时回写 prefs：`tok != currentEngine()` 同时挡掉
     * 初始化落位和 notifyDataSetChanged 后可能的回调 —— 程序化触发
     * 如果被当成选择写进去，使用者会莫名其妙被切成 auto。
     */
    private fun setupEngineSelector() {
        engineAdapter = object : ArrayAdapter<String>(
            this,
            android.R.layout.simple_spinner_item,
            engineDisplay,
        ) {
            // 三内核没随包注入就灰显；auto 只要任一内核在就放行。
            override fun isEnabled(position: Int): Boolean =
                engineEnabled(ENGINE_TOKENS[position])

            override fun getDropDownView(
                position: Int,
                convertView: View?,
                parent: ViewGroup,
            ): View = super.getDropDownView(position, convertView, parent).apply {
                // 下拉项布局（simple_spinner_dropdown_item）是 TextView，
                // 但基类声明返回 View —— 判一下再上色，否则 setTextColor 解析不到。
                if (this is TextView) {
                    setTextColor(
                        if (isEnabled(position)) 0xFFCCCCCC.toInt()
                        else 0xFF666666.toInt(),
                    )
                }
            }
        }
        engineAdapter.setDropDownViewResource(
            android.R.layout.simple_spinner_dropdown_item,
        )
        spinnerEngine.adapter = engineAdapter

        val cur = currentEngine()
        val idx = ENGINE_TOKENS.indexOf(cur)
        spinnerEngine.setSelection(
            if (idx >= 0) idx else ENGINE_TOKENS.indexOf("meow"),
        )

        spinnerEngine.onItemSelectedListener =
            object : AdapterView.OnItemSelectedListener {
                override fun onItemSelected(
                    parent: AdapterView<*>?,
                    view: View?,
                    position: Int,
                    id: Long,
                ) {
                    val tok = ENGINE_TOKENS[position]
                    if (tok != currentEngine()) {
                        enginePrefs().edit()
                            .putString(GhBoostVpnService.PREF_ENGINE, tok)
                            .apply()
                        renderEngineInfo()
                    }
                }

                override fun onNothingSelected(parent: AdapterView<*>?) {}
            }
    }

    /** `auto` 只要有任一内核就可用；具体内核看自己；內建永远可用。 */
    private fun engineEnabled(tok: String): Boolean = when {
        tok == GhBoostVpnService.ENGINE_EMBEDDED -> true
        tok == "auto" -> coreAvail.values.any { it }
        else -> coreAvail[tok] == true
    }

    private fun enginePrefs() =
        getSharedPreferences(GhBoostVpnService.PREFS_NAME, MODE_PRIVATE)

    private fun currentEngine(): String =
        enginePrefs()
            .getString(
                GhBoostVpnService.PREF_ENGINE,
                GhBoostVpnService.ENGINE_EMBEDDED,
            ) ?: GhBoostVpnService.ENGINE_EMBEDDED

    /** `nativeListCores` 的 JSON → `coreAvail`（解析失败就当全缺，只留內建）。 */
    private fun parseCores(raw: String) {
        if (raw.isBlank()) return
        try {
            val arr = org.json.JSONObject(raw).getJSONArray("cores")
            for (i in 0 until arr.length()) {
                val c = arr.getJSONObject(i)
                coreAvail[c.optString("id")] = c.optBoolean("available")
            }
        } catch (e: Exception) {
            Log.w("GhBoost", "parseCores failed: $raw", e)
        }
    }

    /**
     * 可用性载入后刷新显示名与灰显。就地改写 [engineDisplay] +
     * `notifyDataSetChanged()`（同 adapter 同长度，选中项不会被重置）。
     */
    private fun refreshEngineAvailability() {
        for ((i, tok) in ENGINE_TOKENS.withIndex()) {
            engineDisplay[i] = when {
                tok == "auto" && coreAvail.values.none { it } ->
                    "${ENGINE_LABELS[i]}（未注入）"
                tok != "auto" &&
                    tok != GhBoostVpnService.ENGINE_EMBEDDED &&
                    coreAvail[tok] != true -> "${ENGINE_LABELS[i]}（缺二進制）"
                else -> ENGINE_LABELS[i]
            }
        }
        engineAdapter.notifyDataSetChanged()
        renderEngineInfo()
    }

    /**
     * 内核信息两行：
     *   行1 三内核可用性（长期事实，选择器灰显的依据）；
     *   行2 运行时真相 —— exec 内核问 `nativeCoreStatus`，
     *        內建/回落由「在跑但 exec engine 为空」推出来
     *        （内建没有 exec 状态，回落也一样，按 pref 分叉说人话）。
     */
    private fun renderEngineInfo(core: org.json.JSONObject? = null) {
        val line1 = "三內核：" + listOf("mihomo", "xray", "sing-box")
            .joinToString(" · ") { id ->
                val label = ENGINE_LABELS[ENGINE_TOKENS.indexOf(id)]
                (if (coreAvail[id] == true) "✓ " else "✕ ") + label
            }
        val line2 = if (!isRunning) {
            "未連線；引擎選擇在下次按 Start 時生效。"
        } else {
            // org.json 的 optString 会把 JSON null 转成字符串 "null"
            // （JSONObject.NULL.toString()）—— 內建内核的 status 里 engine
            // 正是 JSON null，不剥掉就渲染出「執行中：null」（W10 实测抓到）。
            val raw = core?.optString("engine").orEmpty()
            val ex = if (raw == "null") "" else raw
            when {
                core == null -> "執行中：內建（狀態查詢失敗）"
                ex.isEmpty() && currentEngine() ==
                    GhBoostVpnService.ENGINE_EMBEDDED ->
                    "執行中：內建 meow（同進程 protect）"
                ex.isEmpty() -> "執行中：內建（exec內核缺二進制已自動回落）"
                else -> {
                    val listen = if (core.optBoolean("listening")) "✓" else "…"
                    "執行中：$ex · 1080 監聽 $listen"
                }
            }
        }
        tvEngineInfo.text = "$line1\n$line2"
    }

    /**
     * 2s 一轮的 exec 内核状态环。只在跑的时候问原生层；lifecycleScope
     * 在 onDestroy 自动收掉，不用手停。
     */
    private fun startEngineStatusLoop() {
        lifecycleScope.launch {
            while (true) {
                delay(2000)
                val core = if (isRunning) {
                    try {
                        withContext(Dispatchers.IO) {
                            org.json.JSONObject(GhBoostCore.nativeCoreStatus())
                        }
                    } catch (e: Throwable) {
                        // 旧 .so 没这符号也只是状态行降级，不能崩 UI。
                        Log.w("GhBoost", "nativeCoreStatus failed", e)
                        null
                    }
                } else {
                    null
                }
                renderEngineInfo(core)
            }
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
        // 不要乐观地说「VPN running」—— 内核是异步拉起来的，而且**可能失败**。
        // 先说实话（正在启动），再回来核对真实状态。
        tvStatus.text = "正在啟動…"
        updateButtons()
        lifecycleScope.launch { waitForKernel(expectRunning = true) }
    }

    private fun stopVpn() {
        val intent = Intent(this, GhBoostVpnService::class.java)
        intent.action = "STOP"
        startForegroundService(intent)
        tvStatus.text = "Stopped"
        updateButtons()
        lifecycleScope.launch { waitForKernel(expectRunning = false) }
    }

    /**
     * 等 Service 把状态翻到我们要的那一边，然后把界面同步过去。
     *
     * 为什么必须等：`startForegroundService` 只是把 Intent 丢过去，内核拉起是异步的，
     * 而且**失败时会自己 stopSelf()** —— 不等就核，界面会停在乐观的旧状态上。
     * 超时也照样同步一次：宁可显示「没连上」，也不要显示一个假的「已连接」。
     */
    private suspend fun waitForKernel(expectRunning: Boolean) {
        var i = 0
        while (i < 12 && GhBoostVpnService.kernelRunning != expectRunning) {
            delay(500)
            i++
        }
        refreshRunningState()
    }

    override fun onResume() {
        super.onResume()
        // 回到前台时重新核对一次：Service 可能在我们不在时自己停了
        // （内核启动失败 → stopSelf），那时界面必须跟着改口。
        refreshRunningState()
    }

    /**
     * 把界面同步到 Service 的真实状态。
     * 只有 kernelRunning 才算「已连接」；否则一律把 Start 放开，
     * 保证使用者永远有一条走得通的路。
     */
    private fun refreshRunningState() {
        updateButtons()
        if (isRunning) {
            tvStatus.text = "VPN running"
        } else {
            // 只改自己刚说过的两种状态，别踩到 Ready / Scan complete 这些。
            // 用 toString() 比：TextView.text 回的是 CharSequence，
            // 直接跟 String 比在换了实现之后会静默变成 false。
            val cur = tvStatus.text?.toString()
            if (cur == "VPN running" || cur == "正在啟動…") {
                if (tunReady) tvStatus.text = "未連線，按 Start 開始加速"
            }
        }
    }

    private fun updateButtons() {
        // 转发没接上时 Start 一律锁住：按下去只会断网，没有任何好处。
        btnStart.isEnabled = tunReady && !isRunning
        btnStop.isEnabled = isRunning
        // 跑着的时候不许换引擎 —— 选择只在下次 Start 生效，跑着改只会让
        // 使用者以为「切了就切了」，实际行为和显示对不上。
        spinnerEngine.isEnabled = !isRunning
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == VPN_REQUEST_CODE && resultCode == Activity.RESULT_OK) {
            onVpnPermissionGranted()
        }
    }
}
