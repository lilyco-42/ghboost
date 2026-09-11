//! 日志出口：Android 走 logcat，其它平台走 stderr。
//!
//! **为什么不能直接用 `eprintln!`**：Android 上 native 库的 stderr 默认进
//! `/dev/null`，`adb logcat` 完全看不到。内核/转发器启动失败时只留一行
//! eprintln，结果就是「VPN 显示已连接、内部却没起来、且没有任何线索」——
//! 这个坑实测踩过一次，排查成本极高。
//!
//! 这里零新依赖：直接声明 `__android_log_print`（liblog，Android 必有）。
//! 非 Android 平台退化成 stderr，保证同一份代码在 host 上也能编译/测试。

#[cfg(target_os = "android")]
mod imp {
    use std::ffi::{c_char, c_int, CString};

    extern "C" {
        fn __android_log_print(prio: c_int, tag: *const c_char, fmt: *const c_char, ...) -> c_int;
    }

    const INFO: c_int = 4;
    const ERROR: c_int = 6;

    fn write(prio: c_int, msg: &str) {
        // 消息里可能有 NUL（理论上不该有），有就替换掉，绝不 panic。
        let Ok(tag) = CString::new("GhBoostNative") else {
            return;
        };
        let Ok(fmt) = CString::new("%s") else { return };
        let Ok(body) = CString::new(msg.replace('\0', " ")) else {
            return;
        };
        unsafe {
            __android_log_print(prio, tag.as_ptr(), fmt.as_ptr(), body.as_ptr());
        }
    }

    pub fn info(msg: &str) {
        write(INFO, msg);
    }
    pub fn error(msg: &str) {
        write(ERROR, msg);
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    pub fn info(msg: &str) {
        eprintln!("[ghboost] {msg}");
    }
    pub fn error(msg: &str) {
        eprintln!("[ghboost][error] {msg}");
    }
}

pub use imp::{error, info};
