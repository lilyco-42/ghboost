//! ghboost FFI bindings for Android (JNI) and iOS (C ABI)
//!
//! Provides cross-language bindings for:
//! - Android: JNI interface via `GhBoostCore.kt`
//! - iOS: C ABI interface via `GhBoostCore.swift`

pub mod tun2socks;

#[cfg(target_os = "android")]
pub mod protect;

/// 内嵌的 meow-rs 代理内核（Android 专用）。
///
/// 只在 Android 编译：它依赖 `meow-common` 的 `SocketProtector` 钩子，
/// 那个 trait 本身也只在 Android 上编译（其它平台没有 `VpnService` 这回事）。
#[cfg(target_os = "android")]
pub mod meow_kernel;

// ─────────────────────────────────────────────────────────────
// Android JNI bindings
// ─────────────────────────────────────────────────────────────

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{JClass, JObject, JString};
    use jni::sys::{jboolean, jint, jstring};
    use jni::JNIEnv;

    use crate::meow_kernel;
    use crate::tun2socks;
    use lilyco_ghboost as ghboost;

    /// Helper: convert Rust string to JNI string
    fn to_jstring(env: &mut JNIEnv, s: String) -> jstring {
        let jstr = env.new_string(s).unwrap_or_default();
        jstr.into_raw()
    }

    /// Helper: parse JNI string to Rust string
    fn from_jstring(env: &mut JNIEnv, js: &JString) -> String {
        env.get_string(js).map(|s| s.into()).unwrap_or_default()
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeInit(
        _env: JNIEnv,
        _class: JClass,
    ) {
        // Initialize logging if needed
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeSetHomeDir(
        mut env: JNIEnv,
        _class: JClass,
        dir: JString,
    ) {
        let dir_str = from_jstring(&mut env, &dir);
        // Set home directory for config storage
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeScan(
        mut env: JNIEnv,
        _class: JClass,
        params: JString,
    ) -> jstring {
        let params_str = from_jstring(&mut env, &params);
        let params_c = std::ffi::CString::new(params_str).unwrap_or_default();

        let result = unsafe { ghboost::ghboost_scan(params_c.as_ptr()) };

        let result_str = unsafe {
            std::ffi::CStr::from_ptr(result)
                .to_string_lossy()
                .into_owned()
        };

        unsafe { ghboost::ghboost_free(result) };

        to_jstring(&mut env, result_str)
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeTest(
        mut env: JNIEnv,
        _class: JClass,
        params: JString,
    ) -> jstring {
        let params_str = from_jstring(&mut env, &params);
        let params_c = std::ffi::CString::new(params_str).unwrap_or_default();

        let result = unsafe { ghboost::ghboost_test(params_c.as_ptr()) };

        let result_str = unsafe {
            std::ffi::CStr::from_ptr(result)
                .to_string_lossy()
                .into_owned()
        };

        unsafe { ghboost::ghboost_free(result) };

        to_jstring(&mut env, result_str)
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeAdd(
        mut env: JNIEnv,
        _class: JClass,
        params: JString,
    ) -> jstring {
        let params_str = from_jstring(&mut env, &params);
        let params_c = std::ffi::CString::new(params_str).unwrap_or_default();

        let result = unsafe { ghboost::ghboost_add(params_c.as_ptr()) };

        let result_str = unsafe {
            std::ffi::CStr::from_ptr(result)
                .to_string_lossy()
                .into_owned()
        };

        unsafe { ghboost::ghboost_free(result) };

        to_jstring(&mut env, result_str)
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeStartTun2Socks(
        mut env: JNIEnv,
        _class: JClass,
        vpn_service: JObject,
        fd: jint,
        dns_port: jint,
    ) -> jint {
        // Install socket protector to prevent routing loop
        crate::protect::install(&mut env, &vpn_service);

        // Start tun2socks with the TUN file descriptor
        match tun2socks::start(fd as i32, dns_port as u16) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("tun2socks start failed: {}", e);
                -1
            }
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeStopTun2Socks(
        _env: JNIEnv,
        _class: JClass,
    ) {
        tun2socks::stop();
    }

    /// 这个 .so 里的 tun2socks 是否真的会转发流量。
    ///
    /// 由 `tun2socks::FORWARDING_IMPLEMENTED` 决定，App 用它判断要不要放开 Start。
    /// 不加这道闸门的话：按下 Start 会建立 TUN 却没人转发 —— 实测整机断网，
    /// 界面还显示「VPN running」。
    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeTunForwardingImplemented(
        _env: JNIEnv,
        _class: JClass,
    ) -> jboolean {
        tun2socks::FORWARDING_IMPLEMENTED as jboolean
    }

    /// 启动内嵌代理内核（meow-rs）。
    ///
    /// `config_path` 是 mihomo 风格 YAML 的绝对路径，由 Kotlin 侧
    /// `LocalProxySetup` 写在 `filesDir/mihomo/configs/config.yaml`。
    ///
    /// **顺序**：先装 protector 再启动内核。内核一启动就会拉订阅 / 做健康检查，
    /// 那些出站 socket 必须已经被 protect —— 否则会被自己的 TUN 卷回，
    /// 表现成「开了 VPN 之后连订阅都拉不下来」。
    ///
    /// 返回 0 成功，-1 失败（失败原因写 stderr —— 与其它 native 方法一致）。
    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeStartProxyKernel(
        mut env: JNIEnv,
        _class: JClass,
        vpn_service: JObject,
        config_path: JString,
    ) -> jint {
        let path: String = match env.get_string(&config_path) {
            Ok(s) => s.into(),
            Err(e) => {
                eprintln!("proxy kernel: bad config path: {e}");
                return -1;
            }
        };

        // 必须先装：内核启动阶段（拉订阅、健康检查）就会开 socket。
        crate::protect::install(&mut env, &vpn_service);

        match meow_kernel::start(&path) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("proxy kernel start failed: {e}");
                -1
            }
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeStopProxyKernel(
        _env: JNIEnv,
        _class: JClass,
    ) {
        meow_kernel::stop();
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeProxyKernelRunning(
        _env: JNIEnv,
        _class: JClass,
    ) -> jboolean {
        meow_kernel::is_running() as jboolean
    }

    #[no_mangle]
    pub extern "system" fn Java_com_ghboost_app_GhBoostCore_nativeVersion(
        mut env: JNIEnv,
        _class: JClass,
    ) -> jstring {
        let result = unsafe { ghboost::ghboost_version() };

        let result_str = unsafe {
            std::ffi::CStr::from_ptr(result)
                .to_string_lossy()
                .into_owned()
        };

        unsafe { ghboost::ghboost_free(result) };

        to_jstring(&mut env, result_str)
    }
}

// ─────────────────────────────────────────────────────────────
// iOS C ABI bindings
// ─────────────────────────────────────────────────────────────

#[cfg(target_os = "ios")]
mod ios {
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;

    /// Initialize the library
    #[no_mangle]
    pub extern "C" fn ghboost_ffi_init() {
        // Initialize logging if needed
    }

    /// Set home directory for config storage
    #[no_mangle]
    pub extern "C" fn ghboost_ffi_set_home_dir(dir: *const c_char) {
        if dir.is_null() {
            return;
        }
        let _dir_str = unsafe { CStr::from_ptr(dir) }
            .to_string_lossy()
            .into_owned();
        // Set home directory
    }

    /// Start tun2socks with file descriptor
    #[no_mangle]
    pub extern "C" fn ghboost_ffi_start_tun2socks(fd: i32, dns_port: u16) -> i32 {
        match crate::tun2socks::start(fd, dns_port) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("tun2socks start failed: {}", e);
                -1
            }
        }
    }

    /// Stop tun2socks
    #[no_mangle]
    pub extern "C" fn ghboost_ffi_stop_tun2socks() {
        crate::tun2socks::stop();
    }
}
