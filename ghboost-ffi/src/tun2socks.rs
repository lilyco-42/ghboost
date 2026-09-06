//! tun2socks module - User-space TCP/IP stack for mobile VPN
//!
//! Based on meow-android's tun2socks implementation using lwip.
//! This is a stub implementation for initial compilation.

use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(false);

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
