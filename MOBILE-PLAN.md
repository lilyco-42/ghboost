# ghboost 移动端实现方案

基于 `madeye/meow` (Android 114★, iOS 220★, MIT license) 的成功实现模式设计。

## 架构概览

```
┌─────────────────────────────────────────────────────┐
│                 ghboost (Rust core)                 │
│  ┌─────────────┐  ┌──────────┐  ┌────────────────┐  │
│  │  nodes.rs   │  │ tun2socks│  │ mihomo.rs      │  │
│  │ scan/add    │  │ (lwip)   │  │ config gen     │  │
│  └──────┬──────┘  └────┬─────┘  └───────┬────────┘  │
│         │              │                │            │
│         └──────────────┼────────────────┘            │
│                        │                             │
│              ┌─────────▼─────────┐                   │
│              │  C ABI / JNI FFI  │                   │
│              │  (lib.rs 已有)    │                   │
│              └─────────┬─────────┘                   │
└────────────────────────┼────────────────────────────┘
                         │
          ┌──────────────┼──────────────┐
          │              │              │
    ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐
    │  Android  │  │    iOS    │  │  Desktop  │
    │ VpnService│  │    NE     │  │  WebView  │
    │ + JNI     │  │ PacketTun │  │  GUI      │
    └───────────┘  └───────────┘  └───────────┘
```

## Android 实现

### 目录结构

```
ghboost-android/
├── app/
│   └── src/main/
│       ├── java/com/ghboost/app/
│       │   ├── GhBoostApp.kt           # Application
│       │   ├── MainActivity.kt         # 主界面
│       │   ├── VpnService.kt           # VPN 服务
│       │   └── GhBoostCore.kt          # JNI 桥接类
│       ├── jni/
│       │   └── ghboost_ffi.so          # Rust 编译产物
│       └── AndroidManifest.xml
├── build.gradle.kts
└── settings.gradle.kts
```

### 核心文件

#### 1. GhBoostCore.kt (JNI 桥接)

```kotlin
package com.ghboost.app

object GhBoostCore {
    init {
        System.loadLibrary("ghboost_ffi")
    }

    // 初始化
    external fun nativeInit()

    // 设置 home 目录
    external fun nativeSetHomeDir(dir: String)

    // 节点扫描
    external fun nativeScan(params: String): String

    // 节点测试
    external fun nativeTest(params: String): String

    // 节点添加
    external fun nativeAdd(params: String): String

    // 启动 tun2socks
    external fun nativeStartTun2Socks(
        vpnService: Any,
        fd: Int,
        dnsPort: Int
    ): Int

    // 停止 tun2socks
    external fun nativeStopTun2Socks()

    // 获取日志
    external fun nativeGetLogs(): String

    // 获取错误
    external fun nativeGetLastError(): String

    // 版本信息
    external fun nativeVersion(): String
}
```

#### 2. VpnService.kt (VPN 服务)

```kotlin
package com.ghboost.app

import android.content.Intent
import android.net.VpnService
import android.os.ParcelFileDescriptor

class GhBoostVpnService : VpnService() {
    private var tunFd: ParcelFileDescriptor? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> startVpn()
            ACTION_STOP -> stopVpn()
        }
        return START_STICKY
    }

    private fun startVpn() {
        // 1. 建立 TUN 接口
        val builder = Builder()
            .setSession("ghboost")
            .setMtu(1400)
            .addAddress("172.19.0.1", 30)
            .addRoute("0.0.0.0", 0)
            .addDnsServer("172.19.0.2")

        tunFd = builder.establish()

        // 2. 启动 tun2socks
        tunFd?.let { fd ->
            GhBoostCore.nativeStartTun2Socks(
                this,
                fd.fd,
                53
            )
        }
    }

    private fun stopVpn() {
        GhBoostCore.nativeStopTun2Socks()
        tunFd?.close()
        stopSelf()
    }

    companion object {
        const val ACTION_START = "com.ghboost.START"
        const val ACTION_STOP = "com.ghboost.STOP"
    }
}
```

#### 3. Rust FFI 扩展 (ghboost-ffi/src/lib.rs)

基于 meow-android 的 JNI 模式，在现有 `lib.rs` 基础上添加：

```rust
// Android JNI 入口点
#[cfg(target_os = "android")]
mod android {
    use jni::objects::{JClass, JObject, JString};
    use jni::sys::{jboolean, jint, jlong, jstring};
    use jni::JNIEnv;

    use super::*;

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeInit(
        _env: JNIEnv,
        _class: JClass,
    ) {
        // 初始化日志
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeSetHomeDir(
        mut env: JNIEnv,
        _class: JClass,
        dir: JString,
    ) {
        let dir_str: String = env.get_string(&dir).map(|s| s.into()).unwrap_or_default();
        // 设置 home 目录
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeScan(
        mut env: JNIEnv,
        _class: JClass,
        params: JString,
    ) -> jstring {
        let params_str: String = env.get_string(&params).map(|s| s.into()).unwrap_or_default();
        // 调用 scan_core
        let result = match run_blocking(nodes::scan_core(...)) {
            Ok(v) => ok_json(v),
            Err(e) => err_json(e),
        };
        env.new_string(result).unwrap().into_raw()
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeStartTun2Socks(
        env: JNIEnv,
        _class: JClass,
        vpn_service: JObject,
        fd: jint,
        dns_port: jint,
    ) -> jint {
        // 1. 安装 socket protector (JNI 回调 VpnService.protect)
        protect::install(&env, &vpn_service);

        // 2. 启动 tun2socks
        match tun2socks::start(fd, dns_port as u16) {
            Ok(()) => 0,
            Err(e) => -1,
        }
    }
}
```

#### 4. tun2socks 模块 (ghboost-ffi/src/tun2socks.rs)

参考 meow-android 的实现，使用 lwip netstack：

```rust
use lwip::NetStack;
use std::os::unix::io::RawFd;

pub fn start(fd: RawFd, dns_port: u16) -> Result<(), String> {
    // 1. 设置 fd 为非阻塞
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }

    // 2. 创建 lwip netstack
    let (mut stack, mut tcp_listener, udp_socket) =
        NetStack::with_buffer_size(1024, 256)?;

    // 3. 启动读取/写入任务
    // - 从 TUN fd 读取 IP 包 → stack.send()
    // - stack.next() → 写入 TUN fd
    // - tcp_listener.next() → dispatch_tcp()
    // - udp_read.next() → dispatch_udp()

    Ok(())
}

async fn dispatch_tcp(stream: lwip::TcpStream, src: SocketAddr, dst: SocketAddr) {
    // 调用 ghboost 的代理引擎处理 TCP 流
    // 类似 meow 的 meow_tunnel::tcp::handle_tcp
}
```

#### 5. protect 模块 (ghboost-ffi/src/protect.rs)

参考 meow-android，防止路由回环：

```rust
use jni::{GlobalRef, JavaVM};
use meow_common::SocketProtector;
use std::os::fd::RawFd;
use std::sync::Arc;

struct VpnSocketProtector {
    jvm: JavaVM,
    service: GlobalRef,
}

impl SocketProtector for VpnSocketProtector {
    fn protect(&self, fd: RawFd) -> std::io::Result<()> {
        let mut env = self.jvm.attach_current_thread()?;
        env.call_method(
            &self.service,
            "protect",
            "(I)Z",
            &[jni::objects::JValue::Int(fd)],
        )?;
        Ok(())
    }
}

pub fn install(env: &jni::JNIEnv, service: &jni::objects::JObject) {
    let protector = VpnSocketProtector {
        jvm: env.get_java_vm().unwrap(),
        service: env.new_global_ref(service).unwrap(),
    };
    meow_common::set_socket_protector(Arc::new(protector));
}
```

## iOS 实现

### 目录结构

```
ghboost-ios/
├── GhBoost/
│   ├── GhBoostApp.swift              # 主 App
│   ├── ContentView.swift             # 主界面
│   └── GhBoostCore.swift            # C FFI 桥接
├── PacketTunnel/
│   └── PacketTunnelProvider.swift    # NEPacketTunnelProvider
├── GhBoostKit/
│   └── Sources/
│       └── GhBoostKit/
│           ├── Tunnel/
│           │   └── GhBoostTunnel.swift
│           └── Storage/
│               └── ConfigStorage.swift
├── Frameworks/
│   └── GhBoostCore.xcframework      # Rust 静态库
└── scripts/
    └── build-core.sh                 # 编译 Rust → xcframework
```

### 核心文件

#### 1. GhBoostCore.swift (C FFI 桥接)

```swift
import Foundation

class GhBoostCore {
    // C FFI 函数声明
    static func nativeInit()
    static func nativeSetHomeDir(_ dir: UnsafePointer<CChar>)
    static func nativeScan(_ params: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?
    static func nativeTest(_ params: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?
    static func nativeAdd(_ params: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?
    static func nativeStartTun2Socks(_ fd: Int32, _ dnsPort: Int32) -> Int32
    static func nativeStopTun2Socks()
    static func nativeGetLogs() -> UnsafeMutablePointer<CChar>?
    static func nativeLastError() -> UnsafePointer<CChar>?
    static func nativeVersion() -> UnsafeMutablePointer<CChar>?

    // 释放内存
    static func free(_ ptr: UnsafeMutablePointer<CChar>?)
}
```

#### 2. PacketTunnelProvider.swift

```swift
import NetworkExtension

class PacketTunnelProvider: NEPacketTunnelProvider {
    override func startTunnel(options: [String: NSObject]?,
                              completionHandler: @escaping (Error?) -> Void) {
        // 1. 读取 App Group 中的配置
        let config = ConfigStorage.readConfig()

        // 2. 设置 tunnel 网络参数
        let settings = NEPacketTunnelNetworkSettings(tunnelRemoteAddress: "172.19.0.1")
        settings.mtu = 1400
        settings.ipv4Settings = NEIPv4Settings(
            addresses: ["172.19.0.2"],
            subnetMasks: ["255.255.255.252"]
        )
        settings.ipv4Settings?.includedRoutes = [NEIPv4Route.default()]
        settings.dnsSettings = NEDNSSettings(servers: ["172.19.0.1"])

        // 3. 应用设置
        setTunnelNetworkSettings(settings) { error in
            if let error = error {
                completionHandler(error)
                return
            }

            // 4. 启动 tun2socks
            let fd = self.packetFlow.fileHandleForReading.fileDescriptor
            let result = GhBoostCore.nativeStartTun2Socks(fd, 53)
            if result == 0 {
                completionHandler(nil)
            } else {
                completionHandler(NSError(domain: "GhBoost", code: result))
            }
        }
    }

    override func stopTunnel(with reason: NEProviderStopReason,
                             completionHandler: @escaping () -> Void) {
        GhBoostCore.nativeStopTun2Socks()
        completionHandler()
    }
}
```

#### 3. build-core.sh (编译 Rust → xcframework)

```bash
#!/bin/bash
# 编译 Rust 为 iOS 静态库

TARGETS=(
    "aarch64-apple-ios"
    "aarch64-apple-ios-sim"
    "x86_64-apple-ios-sim"
)

# 编译每个目标
for target in "${TARGETS[@]}"; do
    cargo build --release --target $target --lib
done

# 创建 xcframework
xcodebuild -create-xcframework \
    -library target/aarch64-apple-ios/release/libghboost.a \
    -library target/aarch64-apple-ios-sim/release/libghboost.a \
    -library target/x86_64-apple-ios-sim/release/libghboost.a \
    -output Frameworks/GhBoostCore.xcframework
```

## 共享组件

### 1. tun2socks (Rust)

Android 和 iOS 共享同一份 tun2socks 实现：
- `lwip` crate 作为用户态 TCP/IP 栈
- 从 TUN fd 读取 IP 包 → lwip 处理 → 分发到代理引擎
- TCP 流直接 dispatch 到代理引擎 (类似 meow 的 in-process 模式)
- UDP/53 拦截 → 本地 DNS 解析 (fake-ip 模式)

### 2. Socket Protector

- Android: JNI 回调 `VpnService.protect(fd)` 
- iOS: NEPacketTunnelProvider 内置 socket protection
- 作用: 防止代理出站流量被路由回 TUN (路由回环)

### 3. Config Patching

在 Rust 端修补配置:
- 注入 `mixed-port` (混合代理端口)
- 注入 `dns.listen` (本地 DNS)
- 注入 `external-controller` (随机端口 + secret)
- 注入 `GLOBAL` selector (全局选择器)

## 依赖

### Android

```toml
# ghboost-ffi/Cargo.toml
[dependencies]
lwip = "0.3"
jni = "0.21"
meow-common = { git = "..." }  # SocketProtector trait

# 编译 Android .so
[lib]
crate-type = ["cdylib"]
```

### iOS

```toml
# ghboost-ffi/Cargo.toml
[dependencies]
lwip = "0.3"

# 编译 iOS 静态库
[lib]
crate-type = ["staticlib"]
```

## 实现步骤

### Phase 1: Rust FFI 扩展 (1-2 天)

1. 创建 `ghboost-ffi/` crate
2. 添加 Android JNI 入口点
3. 添加 iOS C ABI 入口点
4. 实现 tun2socks 模块 (lwip)
5. 实现 protect 模块 (Android JNI)
6. 实现 config patching

### Phase 2: Android (2-3 天)

1. 创建 Android 项目结构
2. 实现 GhBoostCore.kt (JNI 桥接)
3. 实现 GhBoostVpnService.kt
4. 实现 MainActivity.kt (UI)
5. 集成 Rust .so
6. 测试 VPN 隧道

### Phase 3: iOS (2-3 天)

1. 创建 iOS 项目结构
2. 编译 Rust → xcframework
3. 实现 GhBoostCore.swift (C FFI)
4. 实现 PacketTunnelProvider.swift
5. 实现 SwiftUI 界面
6. 测试 NetworkExtension

### Phase 4: 测试与优化 (1-2 天)

1. Android 端到端测试
2. iOS 端到端测试
3. 性能优化 (内存/延迟)
4. 发布准备

## 总计: 7-10 天

## 风险与注意事项

1. **Apple 签名**: iOS NetworkExtension 需要 Apple Developer 账号 + entitlement
2. **Google Play**: Android VPN 权限需要隐私政策说明
3. **mihomo 版本**: 需要跟踪 mihomo 上游更新
4. **lwip 稳定性**: meow 已验证的 lwip fork (madeye/lwip) 可直接复用
5. **内存管理**: tun2socks 的 per-flow 分配需要关注 RSS
