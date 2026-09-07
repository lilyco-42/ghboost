package com.ghboost.app

import android.app.Activity
import android.content.Intent
import android.net.VpnService
import android.os.Bundle
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

    companion object {
        private const val VPN_REQUEST_CODE = 100
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
                GhBoostCore.Init()
                GhBoostCore.SetHomeDir(filesDir.absolutePath)
                val version = GhBoostCore.Version()
                withContext(Dispatchers.Main) {
                    tvVersion.text = "ghboost v$version"
                    tvStatus.text = "Ready"
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

        updateButtons()
    }

    private fun scanNodes() {
        tvNodes.text = "Scanning..."
        btnScan.isEnabled = false
        lifecycleScope.launch(Dispatchers.IO) {
            try {
                val json = GhBoostCore.Scan()
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
        btnStart.isEnabled = !isRunning
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
