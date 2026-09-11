//! 内嵌 meow-rs 代理内核（MIT）—— Android 端的真实加速引擎。
//!
//! ## 为什么是 meow-rs，不是 mihomo
//!
//! mihomo 与 ClashMetaForAndroid 都是 **GPL-3.0**。客户端是 MIT，
//! 把 GPL 代码以「库」的形式链进同一个进程，会让整个客户端被 GPL 传染，
//! 与「客户端 MIT 开源」这条产品前提直接冲突。
//!
//! meow-rs 是同一件事的 MIT 实现：协议（SS/Trojan/VLESS/VMess/Hysteria2…）、
//! 规则引擎、订阅、健康检查、REST API 都有。可以合法内嵌，也能商用。
//!
//! ## 为什么必须内嵌成「库」，不能 spawn 子进程
//!
//! `VpnService.protect(fd)` 只能保护**本进程已打开的 fd** —— Android 没有
//! 任何进程级 / 子进程级 API（`addDisallowedApplication` 是排除整个 app，
//! 需要另一个 package name，起不到代理作用）。spawn 出去的内核，它的出站
//! socket 一律保护不了，于是「出站 → 命中自己的 TUN → 又要出站」形成死循环，
//! 表现为一开 VPN 就全网断。
//!
//! meow-common 自带 [`SocketProtector`] 钩子：内核**每建一个出站 socket
//! 就回调一次**，我们在这里转调 Android 的 `protect(fd)`。同进程 + 回调，
//! 这是非 root Android 上唯一可行的形态。
//!
//! ## 与 tun2socks 的分工
//!
//! ```text
//!   应用流量 → TUN fd
//!        │
//!        ├─ tun2socks.rs   TUN fd ↔ 本地 SOCKS5（lwIP 用户态栈）
//!        │                 （已实测：TCP + UDP，回包四元组正确）
//!        │
//!        └─ 本模块          本地 SOCKS5 ↔ 订阅节点
//!                          （协议解析、规则匹配、选路、健康检查）
//! ```
//!
//! 两段都在**同一个进程**里，所以两段的出站 socket 都能被 protect 到。
//! 我们只借 meow 的「SOCKS5 入站 + 协议出站 + 规则」这一层，
//! **不用**它的 TUN 入站 —— 它只在 Windows(Wintun)/Linux/macOS 上有实现，
//! Android 上没有取现成 fd 的路径，而我们的 lwIP 方案已经验证过了。
//!
//! ## 上游对 Android 的支持程度（已核对源码）
//!
//! meow-rs 明确为「被 Android VPN app 内嵌」设计，不是我们硬凑：
//! `meow-app` 里有 `const VPN_PLATFORM: bool = cfg!(any(target_os = "android",
//! target_os = "ios"))`，Android 上**无条件**安装 host-resolver 钩子，
//! 并在注释里写明原因 —— libc 的 `getaddrinfo` 会自己开 DNS socket，
//! 那些 socket 绕过 `VpnService.protect(fd)`，导致「解析代理服务器域名 →
//! 走 TUN → 又要连代理 → 又要解析」的死循环。所以这里的 host-resolver
//! 不是画质优化，是**正确性**要求，必须照装。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use meow_listener::MixedListener;
use meow_tunnel::Tunnel;

/// 日志走 crate 级的 [`crate::logcat`]（Android → logcat，其它平台 → stderr）。
///
/// **为什么不能用 `eprintln!`**：Android 上 native 库的 stderr 默认进
/// `/dev/null`，`adb logcat` 完全看不到。内核启动失败时只有一行 eprintln，
/// 结果就是「VPN 显示已连接、内核却没在监听、且没有任何线索」——
/// 这次实测就卡在这里：`nc 127.0.0.1 1080` 返回 Connection refused，
/// 但 logcat 里一个字的错误都没有。诊断能力必须内建。
use crate::logcat;

/// 本地 SOCKS5/mixed 入口。
///
/// **必须**与 `tun2socks::DEFAULT_SOCKS5` 一致 —— 那是 tun2socks 唯一会去连的
/// 地址，两边不一致就会「VPN 显示已连接、实际每个连接 ECONNREFUSED」。
const LISTEN_ADDR: &str = "127.0.0.1:1080";

/// 监听器在 meow 里的名字，只用于日志与 `GET /listeners` 快照。
const LISTENER_NAME: &str = "ghboost-mixed";

static RUNNING: AtomicBool = AtomicBool::new(false);
/// 用 Notify 而不是 Condvar：整条链是 async，锁跨 await 容易死锁。
static SHUTDOWN: Mutex<Option<Arc<tokio::sync::Notify>>> = Mutex::new(None);
static THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// 把 meow-rs 的出站 socket 转接到 Android 的 `VpnService.protect(fd)`。
///
/// 实现**必须不阻塞** —— meow 是在 dial 的异步 worker 上同步调用它。
/// 我们的 `protect` 底层是一次 JNI 调用（`call_method` + `attach_current_thread`），
/// 不涉及网络等待，满足要求。
struct VpnSocketProtector;

impl meow_common::SocketProtector for VpnSocketProtector {
    fn protect(&self, fd: std::os::fd::RawFd) -> std::io::Result<()> {
        crate::protect::protect(fd)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
}

/// 内核是否在跑。给 Kotlin 侧查状态用。
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// 启动内核。
///
/// `config_path` 是 mihomo 风格的 YAML 路径（`configs/config.yaml`），
/// 由 Kotlin 侧 `LocalProxySetup` 写好。
///
/// 顺序很重要：**先装 protector，再让任何 socket 出去**。反过来的话，
/// 内核在启动阶段（拉订阅、健康检查）开的 socket 会被自己的 TUN 卷回。
pub fn start(config_path: &str) -> Result<(), String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("meow kernel already running".into());
    }
    logcat::info(&format!("start requested, config={config_path}"));

    meow_common::set_socket_protector(Arc::new(VpnSocketProtector));
    logcat::info("socket protector installed");

    let notify = Arc::new(tokio::sync::Notify::new());
    match SHUTDOWN.lock() {
        Ok(mut g) => *g = Some(Arc::clone(&notify)),
        Err(_) => {
            cleanup();
            return Err("meow shutdown slot poisoned".into());
        }
    }

    let path = config_path.to_string();
    let handle = match std::thread::Builder::new()
        .name("ghboost-meow".into())
        .spawn(move || run_kernel(&path, notify))
    {
        Ok(h) => h,
        Err(e) => {
            cleanup();
            return Err(format!("spawn kernel thread: {e}"));
        }
    };

    match THREAD.lock() {
        Ok(mut g) => *g = Some(handle),
        Err(_) => {
            cleanup();
            return Err("meow thread slot poisoned".into());
        }
    }

    Ok(())
}

/// 停止内核，并等线程真正退出。
///
/// 不做 join 的话，紧接着重新 start 会和上一个实例抢 `127.0.0.1:1080`，
/// 表现成「第二次开 VPN 起不来」。
pub fn stop() {
    if let Ok(g) = SHUTDOWN.lock() {
        if let Some(n) = g.as_ref() {
            n.notify_waiters();
        }
    }
    if let Ok(mut g) = THREAD.lock() {
        if let Some(h) = g.take() {
            let _ = h.join();
        }
    }
    cleanup();
}

fn cleanup() {
    RUNNING.store(false, Ordering::SeqCst);
    // protector 指向已失效的 VpnService 就危险，宁可清掉：
    // 没有 protector 时 meow 的 dial 会直接报错，而不是静默走错路。
    meow_common::clear_socket_protector();
    if let Ok(mut g) = SHUTDOWN.lock() {
        *g = None;
    }
}

fn run_kernel(config_path: &str, shutdown: Arc<tokio::sync::Notify>) {
    // 代理内核是多连接并发的，用 multi_thread；不像 lwIP 那样有
    // 「栈内部状态不可跨线程」的约束（那是 tun2socks 才需要的 LocalSet）。
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            logcat::error(&format!("build runtime: {e}"));
            cleanup();
            return;
        }
    };

    let res: Result<(), String> = rt.block_on(async {
        // 先确认 provider 檔案到底在不在、多大 —— 這是「節點沒生效」最常見的原因
        // （meow 對 provider path 有校驗，而且解析失敗時不一定有明顯報錯）。
        {
            let cfg_dir = std::path::Path::new(config_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            let prov = cfg_dir.join("providers/ghboost.yaml");
            match std::fs::metadata(&prov) {
                Ok(m) => logcat::info(&format!(
                    "provider file OK: {} ({} bytes)",
                    prov.display(),
                    m.len()
                )),
                Err(e) => logcat::error(&format!(
                    "provider file MISSING: {} ({e})",
                    prov.display()
                )),
            }
        }

        logcat::info(&format!("loading config {config_path}"));
        let config = meow_config::load_config(config_path)
            .await
            .map_err(|e| format!("load {config_path}: {e}"))?;
        logcat::info(&format!(
            "config ok: {} proxies, {} rules, mode={:?}",
            config.proxies.len(),
            config.rules.len(),
            config.general.mode
        ));
        // 把節點名打出來：provider 沒載進來時這裡會只剩內建的
        // DIRECT/GLOBAL/PROXY/REJECT/REJECT-DROP，一眼可辨。
        {
            let mut names: Vec<String> = config.proxies.keys().map(|k| k.to_string()).collect();
            names.sort();
            logcat::info(&format!("proxies: {}", names.join(", ")));
        }
        // provider 的節點不在 `proxies` 裡，而是在這裡 —— 兩個都要看才知道
        // 「是 provider 沒載入」還是「載入了但組沒引用對」。
        {
            let mut provs: Vec<String> = config.proxy_providers.keys().cloned().collect();
            provs.sort();
            logcat::info(&format!(
                "proxy_providers: [{}]",
                provs.join(", ")
            ));
        }

        // 与 meow-app 的 VPN_PLATFORM 分支一致：Android 上无条件装。
        // 理由见文件头 —— 这是防 DNS 死循环的正确性要求，不是优化。
        meow_common::set_host_resolver(Arc::new(
            meow_dns::ResolverHostHook::new_with_proxy_resolver(
                Arc::clone(&config.dns.resolver),
                config.dns.proxy_resolver.clone(),
            ),
        ));
        logcat::info("host resolver installed");

        let tunnel = Tunnel::new(Arc::clone(&config.dns.resolver));
        tunnel.set_mode(config.general.mode);
        // 0.21.2 把装配拆成两步（main 分支后来合并成了 `update_routing`）。
        // 先装节点再装规则：规则按名字引用节点，反过来会有一瞬间匹配不到。
        tunnel.update_proxies(config.proxies);
        tunnel.update_rules(config.rules);
        tunnel.spawn_background_tasks();
        logcat::info("tunnel ready");

        let addr: SocketAddr = LISTEN_ADDR
            .parse()
            .map_err(|e| format!("bad listen addr {LISTEN_ADDR}: {e}"))?;

        // 先 bind 再 spawn：端口被占时立刻失败，而不是「启动成功但没在听」。
        let socket = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;

        let listener =
            MixedListener::new(tunnel.clone(), addr, LISTENER_NAME.to_string());
        let listen_task = tokio::spawn(async move {
            if let Err(e) = listener.run_on(socket).await {
                logcat::error(&format!("listener exited: {e}"));
            }
        });

        logcat::info(&format!("LISTENING on {addr}"));

        shutdown.notified().await;
        listen_task.abort();
        logcat::info("shutdown signalled");
        Ok(())
    });

    if let Err(e) = res {
        logcat::error(&format!("kernel stopped with error: {e}"));
    }
    cleanup();
}
