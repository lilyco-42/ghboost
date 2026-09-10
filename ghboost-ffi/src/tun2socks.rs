//! tun2socks module - User-space TCP/IP stack for mobile VPN
//!
//! Based on meow-android's tun2socks implementation using lwip.
//! This is a stub implementation for initial compilation.

use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(false);

/// tun2socks 是否真的会把流量转发出去。
///
/// **目前是 false**：`start()` 只置了个 RUNNING 标志，没有任何协议栈去读写 TUN fd。
/// 实测后果（API 36 模拟器）：按下 Start 之后 TUN 建得起来、系统也显示 VPN 已连接，
/// 但所有流量进到 tun0 就没人管 —— ping 8.8.8.8 是 100% 丢包，等于**整机断网**，
/// 而 App 界面还写着「VPN running」。这种「看起来连上了其实全断」比直接崩掉更难查，
/// 所以 App 端靠这个常量决定要不要放开 Start 按钮。
///
/// 接上真正的 lwip 协议栈之后，把这里改成 true，Android 端会自动解禁，
/// 不需要再动 Kotlin。
pub const FORWARDING_IMPLEMENTED: bool = false;

/// Start tun2socks with the given TUN file descriptor and DNS port
pub fn start(fd: c_int, dns_port: u16) -> Result<(), String> {
    if RUNNING.load(Ordering::Relaxed) {
        return Err("tun2socks already running".to_string());
    }

    // TODO: Implement actual tun2socks using lwip
    // 1. Set fd to non-blocking
    // 2. Create lwip netstack
    // 3. Start read/write tasks
    // 4. Dispatch TCP/UDP to proxy engine

    println!("tun2socks: starting with fd={}, dns_port={}", fd, dns_port);
    RUNNING.store(true, Ordering::Relaxed);
    Ok(())
}

/// Stop tun2socks
pub fn stop() {
    if RUNNING.load(Ordering::Relaxed) {
        println!("tun2socks: stopping");
        RUNNING.store(false, Ordering::Relaxed);
    }
}

/// Check if tun2socks is running
pub fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}
