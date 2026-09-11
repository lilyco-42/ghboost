//! tun2socks - 用户态 TCP/IP 栈，给 Android VPN 用。
//!
//! 架构：TUN fd → lwIP netstack → TCP/UDP → SOCKS5 出站
//! （每条出站 socket 先调 `protect::protect(fd)` 绕过 TUN，
//!  避免「TUN→SOCKS5→远端→TUN」死循环）。
//!
//! 关键点：
//! - `lwip` crate（0.3，lwIP 封装）做协议栈：NetStack / TcpListener /
//!   TcpStream（Send）/ UdpSocket（Stream，Item=(Vec<u8>, src, dst)）。
//!   NetStack::new() 返回 (NetStack, TcpListener, Box<UdpSocket>)，
//!   stack.split() 拆成 SplitSink/SplitStream 两半。
//! - tokio current_thread + LocalSet：把整个转发器跑在命名线程
//!   `ghboost-tun2socks` 单线程上，避免 lwip 栈内部状态跨线程
//!   （SplitSink/SplitStream 不一定 Send，LocalSet 能兜住）；
//!   SOCKS5 握手前的 protect+connect 放 `spawn_blocking`（不卡主循环），
//!   握手完的 std TcpStream 经 `from_std` 装回 tokio，再
//!   `copy_bidirectional` 双向搬运。
//! - `protect` 是 Android 上唯一非 root 治回环的办法；其它平台留
//!   no-op（iOS 走 NE 的 socket protect，另算）。
//!
//! ## lwip 在这个 crate 里的行为模型（很重要，决定了怎么写回包）
//!
//! crate 自带的 `old-src/custom/lwipopts.h` 打开了 `TUN2SOCKS 1`，
//! 它把 lwIP 内核改成了「无路由、单接口」模式：
//!
//! - `LWIP_HAVE_LOOPIF 1` + `netif_init()` 建的**唯一 netif 是 loopif**，
//!   地址固定 `127.0.0.1/8`，`netif_loopif_init` 把
//!   `netif->output` 设成 `netif_loop_output_ipv4`，但 **`netif->mtu` 留 0**
//!   （见 `ip4_output_if_src` 里 `netif->mtu && ...` 的分片判断）。
//! - `ip4_route()` 被改成**无条件 `return netif_list`** —— 不做任何路由。
//! - `ip4_input_accept()` 被改成**无条件 `return 1`** —— 收下所有包。
//! - `udp_input` 里「取第一个 pcb 就 break」，所以 `UdpSocket::new()`
//!   拿到的是那个吃下所有 UDP 的默认 pcb。
//! - `udp_recv_cb` 被加了两个参数，把 `ip_current_dest_addr()` 也传上来 ——
//!   **因为默认 pcb 的地址跟包的目的地址无关，不传就拿不到原本的去向**。
//!
//! 结论：IP 的源/目的地址**必须由我们显式给出**。回包时
//! `SendHalf::send_to(data, src, dst)` 的两个地址就是 IP 头里真正的
//! 源/目的；`udp_sendto_if_src` 走的是 `ip_output_if_src`
//! （**不是**会把 any 替换成 netif 地址的 `ip_output_if`），所以原样生效。
//! 一旦给错（例如两头都填本地地址），内核查不到对应 socket，
//! **包会被静默丢弃** —— 没有报错、没有日志，只有「DNS 一直超时」。
//!
//! 还没做的：
//! - SOCKS5 只支持 NO_AUTH；目的地址 v4/v6 都吃；SOCKS5 服务端
//!   地址要求 IPv4。
//! - UDP relay 直连 SOCKS5 服务端的 UDP ASSOCIATE 端口（不做 CONNECT
//!   隧道里的 associate），且不做 FRAG 重组（QUIC/DNS 都不切分包）。
//!
//! FORWARDING_IMPLEMENTED = true：两半都到位了 ——
//! 本文件负责 TUN ↔ 本地 SOCKS5（lwIP，TCP + UDP 都实测过），
//! `meow_kernel.rs` 负责本地 SOCKS5 ↔ 订阅节点（内嵌 meow-rs，MIT）。
//!
//! ⚠️ 这个常量只表示「**原生层会转发**」，不表示「使用者一定有网」。
//! 如果节点清单还是出厂占位（没有真实节点），内核会正常启动、VPN 也显示
//! 已连接，但每个连接都指向没人监听的占位节点 —— 「已连接却打不开网页」。
//! 所以 Kotlin 侧把 Start 闸门设成 **两个条件都要满足**：
//! 本常量 + `LocalProxySetup.hasRealNodes()`。别只依赖这里。

use futures::{SinkExt, StreamExt};
use std::net::{SocketAddr, SocketAddrV4};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::net::TcpStream as TokioTcp;
use tokio::sync::mpsc;
use tokio::sync::Notify;

#[cfg(target_os = "android")]
use crate::protect;

static RUNNING: AtomicBool = AtomicBool::new(false);
// Notify 本身不是 Clone，用 Arc 共享给 stop() 和 run_thread
static SHUTDOWN: Mutex<Option<Arc<Notify>>> = Mutex::new(None);

pub const FORWARDING_IMPLEMENTED: bool = true;

const DEFAULT_SOCKS5: &str = "127.0.0.1:1080";
const SOCKS5_TIMEOUT: Duration = Duration::from_secs(10);

pub fn start(fd: RawFd, _dns_port: u16) -> Result<(), String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("tun2socks already running".into());
    }
    let socks5_str = std::env::var("GHBOOST_SOCKS5_ADDR").unwrap_or_else(|_| DEFAULT_SOCKS5.into());
    let socks5_addr: SocketAddr = socks5_str
        .parse()
        .map_err(|e| format!("bad SOCKS5 addr {socks5_str}: {e}"))?;
    let socks5_v4 = match socks5_addr {
        SocketAddr::V4(v) => v,
        SocketAddr::V6(_) => {
            RUNNING.store(false, Ordering::SeqCst);
            return Err("SOCKS5 addr must be IPv4 (got IPv6)".into());
        }
    };

    // dup 一份 fd：调用方（Java 侧）的 ParcelFileDescriptor 关掉时不能顺带把我们的撕了
    let owned_fd_raw = unsafe { libc::dup(fd) };
    if owned_fd_raw < 0 {
        RUNNING.store(false, Ordering::SeqCst);
        return Err(format!(
            "dup({fd}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    // 新的 shutdown Notify：存到全局让 stop() 能 notify_one
    let notify = Arc::new(Notify::new());
    *SHUTDOWN.lock().unwrap() = Some(Arc::clone(&notify));

    let join = std::thread::Builder::new()
        .name("ghboost-tun2socks".into())
        .spawn(move || run_thread(owned_fd_raw, socks5_v4, notify));

    if let Err(e) = join {
        unsafe {
            libc::close(owned_fd_raw);
        }
        *SHUTDOWN.lock().unwrap() = None;
        RUNNING.store(false, Ordering::SeqCst);
        return Err(format!("spawn thread: {e}"));
    }
    Ok(())
}

pub fn stop() {
    // TUN fd 由 run_thread 里的 AsyncFd<OwnedFd> 持有；这里只 notify
    // 让 run_until 收尾，run_thread 返回时 OwnedFd drop → fd 关闭。
    let notify = SHUTDOWN.lock().unwrap().take();
    if let Some(n) = notify {
        n.notify_one();
    }
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

fn cleanup() {
    RUNNING.store(false, Ordering::SeqCst);
    *SHUTDOWN.lock().unwrap() = None;
}

fn run_thread(fd: RawFd, socks5: SocketAddrV4, notify: Arc<Notify>) {
    // TUN fd 设成非阻塞，AsyncFd 的 readable/writable 才能 EAGAIN
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFL);
        if f >= 0 {
            libc::fcntl(fd, libc::F_SETFL, f | libc::O_NONBLOCK);
        }
    }

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("tun2socks: build runtime: {e}");
            cleanup();
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    let res: Result<(), String> = rt.block_on(local.run_until(async move {
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let tun = AsyncFd::new(owned).map_err(|e| format!("AsyncFd: {e}"))?;
        let (stack, tcp_listener, udp_socket) =
            lwip::NetStack::new().map_err(|e| format!("lwip NetStack::new: {e:?}"))?;
        let (stack_sink, stack_stream) = stack.split();

        // 三个任务在 LocalSet 上跑（不要求 Send，lwip 栈状态同线程）
        tokio::task::spawn_local(tun_io(tun, stack_sink, stack_stream));
        tokio::task::spawn_local(tcp_accept(tcp_listener, socks5));
        tokio::task::spawn_local(udp_drain(udp_socket, socks5));

        // 等 stop() 的 notify；期间不退出
        notify.notified().await;
        Ok::<(), String>(())
    }));
    if let Err(e) = res {
        eprintln!("tun2socks: {e}");
    }
    // run_until 返回 → LocalSet drop → 三个 spawn_local 任务取消
    // → 它们的 future drop → lwip 类型 drop；AsyncFd<OwnedFd> drop
    // → TUN fd 关闭
    cleanup();
}

// ── TUN ↔ lwip stack 双向搬运 ────────────────────────────────

async fn tun_io(
    tun: AsyncFd<OwnedFd>,
    stack_sink: futures::stream::SplitSink<lwip::NetStack, Vec<u8>>,
    stack_stream: futures::stream::SplitStream<lwip::NetStack>,
) {
    // pin_mut：不依赖 SplitSink/SplitStream 是否 Unpin
    futures::pin_mut!(stack_sink);
    futures::pin_mut!(stack_stream);
    loop {
        let read_fut = read_one_pkt(&tun);
        let write_fut = async {
            let pkt = match stack_stream.next().await {
                Some(Ok(p)) => p,
                _ => return Err::<(), ()>(()),
            };
            write_all_tun(&tun, &pkt).await
        };
        tokio::select! {
            r = read_fut => match r {
                Ok(pkt) => { if stack_sink.send(pkt).await.is_err() { return; } }
                Err(()) => return, // readable() 错（fd 关了）或堆栈错：收工
            },
            w = write_fut => { if w.is_err() { return; } }
        }
    }
}

async fn read_one_pkt(tun: &AsyncFd<OwnedFd>) -> Result<Vec<u8>, ()> {
    loop {
        let mut guard = tun.readable().await.map_err(|_| ())?;
        let fd = tun.as_raw_fd();
        let mut buf = [0u8; 65535];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EAGAIN) || e.raw_os_error() == Some(libc::EWOULDBLOCK)
            {
                guard.clear_ready();
                continue; // 假阳性（select readable 后 read EAGAIN），再等
            }
            return Err(());
        }
        if n == 0 {
            guard.clear_ready();
            continue;
        }
        guard.clear_ready();
        return Ok(buf[..n as usize].to_vec());
    }
}

async fn write_all_tun(tun: &AsyncFd<OwnedFd>, pkt: &[u8]) -> Result<(), ()> {
    let mut written = 0;
    while written < pkt.len() {
        let mut guard = tun.writable().await.map_err(|_| ())?;
        let fd = tun.as_raw_fd();
        let n =
            unsafe { libc::write(fd, pkt[written..].as_ptr() as *const _, pkt.len() - written) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EAGAIN) || e.raw_os_error() == Some(libc::EWOULDBLOCK)
            {
                guard.clear_ready();
                continue;
            }
            return Err(());
        }
        written += n as usize;
        guard.clear_ready();
    }
    Ok(())
}

// ── TCP 接受 → SOCKS5 出站 ──────────────────────────────────

async fn tcp_accept(mut tcp_listener: lwip::TcpListener, socks5: SocketAddrV4) {
    // 注意：lwip::TcpListener 的 Stream::Item 就是**裸元组**
    // `(TcpStream, local, remote)`，不是 Result（与 NetStack 的 Stream
    // 不同——后者 Item 才是 Result<Vec<u8>, io::Error>）。
    while let Some((stream, _local, remote)) = tcp_listener.next().await {
        tokio::task::spawn_local(handle_conn(stream, remote, socks5));
    }
}

async fn handle_conn(
    mut stream: lwip::TcpStream,
    remote: std::net::SocketAddr,
    socks5: SocketAddrV4,
) {
    // protect + connect 是阻塞 syscall，不能卡住单线程 reactor
    // （不然所有 TCP 都排队），丢进 blocking 线程池
    let std_stream = match tokio::task::spawn_blocking(move || protected_connect(socks5)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            eprintln!("tun2socks: connect {remote} via {socks5}: {e}");
            return; // lwip stream drop → RST
        }
        Err(_) => return, // blocking 任务 panic
    };
    let mut ts = match TokioTcp::from_std(std_stream) {
        Ok(t) => t,
        Err(_) => return,
    };
    if socks5_connect(&mut ts, &remote).await.is_err() {
        return;
    }
    // 双向 copy；任一端 EOF/错就收
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut ts).await;
}

/// 在 spawn_blocking 里跑：建 socket → protect → connect → 转 std TcpStream。
/// **protect 必须在 connect 之前**，否则出站 socket 会被 TUN 默认路由卷回隧道。
fn protected_connect(socks5: SocketAddrV4) -> Result<std::net::TcpStream, String> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(format!("socket: {}", std::io::Error::last_os_error()));
    }
    if let Err(e) = protect_fd(fd) {
        unsafe {
            libc::close(fd);
        }
        return Err(format!("protect: {e}"));
    }
    // SO_SNDTIMEO 给 connect 一个硬上限
    let tv = libc::timeval {
        tv_sec: SOCKS5_TIMEOUT.as_secs() as _,
        tv_usec: 0,
    };
    let r = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDTIMEO,
            &tv as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as _,
        )
    };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(format!("SO_SNDTIMEO: {e}"));
    }
    let sa = libc::sockaddr_in {
        sin_family: libc::AF_INET as _,
        sin_port: socks5.port().to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_be_bytes(socks5.ip().octets()),
        },
        sin_zero: [0; 8],
    };
    let r = unsafe {
        libc::connect(
            fd,
            &sa as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as _,
        )
    };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(format!("connect: {e}"));
    }
    let stream = unsafe { std::net::TcpStream::from_raw_fd(fd) };
    stream
        .set_nonblocking(true)
        .map_err(|e| format!("set_nonblocking: {e}"))?;
    Ok(stream)
}

#[cfg(target_os = "android")]
fn protect_fd(fd: RawFd) -> Result<(), String> {
    protect::protect(fd).map_err(|e| e.to_string())
}
#[cfg(not(target_os = "android"))]
fn protect_fd(_fd: RawFd) -> Result<(), String> {
    Ok(())
}

// ── SOCKS5 客户端（NO_AUTH + CONNECT，仅出站握手用） ─────────

async fn socks5_connect(ts: &mut TokioTcp, dst: &SocketAddr) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // 1. greeting
    ts.write_all(&[0x05, 0x01, 0x00])
        .await
        .map_err(|e| format!("socks5 greet write: {e}"))?;
    let mut h = [0u8; 2];
    ts.read_exact(&mut h)
        .await
        .map_err(|e| format!("socks5 greet read: {e}"))?;
    if h[0] != 0x05 {
        return Err(format!("socks5 ver: {}", h[0]));
    }
    if h[1] != 0x00 {
        return Err(format!(
            "socks5 auth method {} (only NO_AUTH=0 supported)",
            h[1]
        ));
    }
    // 2. CONNECT 请求
    let mut req = vec![0x05, 0x01, 0x00];
    match dst {
        SocketAddr::V4(v4) => {
            req.push(0x01);
            req.extend_from_slice(&v4.ip().octets());
            req.extend_from_slice(&v4.port().to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            req.push(0x04);
            req.extend_from_slice(&v6.ip().octets());
            req.extend_from_slice(&v6.port().to_be_bytes());
        }
    }
    ts.write_all(&req)
        .await
        .map_err(|e| format!("socks5 connect write: {e}"))?;
    // 3. 响应：ver(1) rep(1) rsv(1) atype(1) bnd...
    let mut head = [0u8; 4];
    ts.read_exact(&mut head)
        .await
        .map_err(|e| format!("socks5 connect read: {e}"))?;
    if head[0] != 0x05 {
        return Err(format!("socks5 resp ver: {}", head[0]));
    }
    if head[1] != 0x00 {
        return Err(format!("socks5 rep {} ({})", head[1], socks5_rep(head[1])));
    }
    // 消费 bnd addr
    match head[3] {
        0x01 => {
            let mut b = [0u8; 6];
            ts.read_exact(&mut b)
                .await
                .map_err(|e| format!("bnd v4: {e}"))?;
        }
        0x04 => {
            let mut b = [0u8; 18];
            ts.read_exact(&mut b)
                .await
                .map_err(|e| format!("bnd v6: {e}"))?;
        }
        0x03 => {
            let mut l = [0u8; 1];
            ts.read_exact(&mut l)
                .await
                .map_err(|e| format!("bnd len: {e}"))?;
            let mut b = vec![0u8; l[0] as usize + 2];
            ts.read_exact(&mut b)
                .await
                .map_err(|e| format!("bnd dom: {e}"))?;
        }
        a => return Err(format!("socks5 bnd atype: {a}")),
    }
    Ok(())
}

fn socks5_rep(c: u8) -> &'static str {
    match c {
        0x01 => "general SOCKS server failure",
        0x02 => "connection not allowed by ruleset",
        0x03 => "Network unreachable",
        0x04 => "Host unreachable",
        0x05 => "Connection refused",
        0x06 => "TTL expired",
        0x07 => "Command not supported",
        0x08 => "Address type not supported",
        _ => "unknown",
    }
}

// ── UDP：SOCKS5 UDP ASSOCIATE ───────────────────────────────

/// UDP 转发：每个「本地源地址」一个 SOCKS5 UDP relay。
///
/// 为什么不是「所有包共用一个 relay」：SOCKS5 的 UDP 响应里只有对端地址，
/// 拿不回来「这个包原本是哪个本地源端口发出的」，也就没法拼回 TUN 需要
/// 的完整四元组。所以按 src 分桶，每个 src 一个 relay，回包时把地址对调
/// 即可 —— 代价是每个本地 UDP 源多占一张 socket，但语义完全正确。
///
/// DNS 也能因此走通：lwip 栈里 DNS 的源是 TUN 上的 10.0.0.2:随机端口，
/// 它会自然成为这里的一个 src 桶。
///
/// ## 回包为什么必须显式给出「原始四元组」
///
/// lwip 的 tun2socks 模式（`lwipopts.h` 的 `TUN2SOCKS 1`）把内核改成了：
/// 本机不存在任何真实 IP，**唯一的 netif 是 loopif（127.0.0.1）**，
/// `ip4_route()` 无条件返回它。所以 `send_to(data, src, dst)` 里的
/// `src`/`dst` 是 IP 头里真正的源/目的地址，必须原样给出：
///   - `src` = 包应该在 TUN 上呈现的来源（也就是原来的远端）
///   - `dst` = 包应该送达的本地地址（也就是收到请求时 lwip 给的 `src`）
///
/// 一开始这里写成 `send_to(&data, &src, &src)`（两个参数都填本地地址），
/// 结果 IP 头变成 `10.0.0.2 -> 10.0.0.2`，内核拿到后查不到对应 socket，
/// **所有 UDP 回包被静默丢弃** —— DNS 永远超时。修法是把方向信息一路带下来。
async fn udp_drain(udp: Box<lwip::UdpSocket>, socks5: SocketAddrV4) {
    use std::collections::HashMap;

    // Box<UdpSocket>: Stream（Item=(Vec<u8>, src, dst)，裸元组）。
    // split 成两半：RecvHalf 读 TUN 进来的包，SendHalf 把回包写回栈。
    let (send_half, mut recv_half) = udp.split();

    // relay 任务把回包发到这里，本函数负责写回 lwip 栈（SendHalf 不是 Send，
    // 只能在同线程用，所以由这个循环独占持有）。
    //
    // 回包形状是 (数据, 来源, 去处)：来源 = 远端（成了回包的 src），
    // 去处 = 请求里 lwip 给的本地地址（成了回包的 dst）。
    let (replies_tx, mut replies_rx) =
        mpsc::unbounded_channel::<(Vec<u8>, SocketAddr, SocketAddr)>();

    // src → relay 发送端。relay 任务活着时它一直收包；
    let mut relays: HashMap<SocketAddr, mpsc::UnboundedSender<(Vec<u8>, SocketAddr)>> =
        HashMap::new();

    loop {
        tokio::select! {
            // TUN 侧来的新 UDP 包 → 交给对应 relay
            pkt = recv_half.next() => {
                let Some((data, src, dst)) = pkt else { return };
                // 清理已退出的 relay（它们 drop 了 receiver，send 会 Err）
                relays.retain(|_, tx| !tx.is_closed());
                let tx = match relays.get(&src) {
                    Some(tx) => tx.clone(),
                    None => match relay::spawn(src, socks5, replies_tx.clone()) {
                        Ok(tx) => {
                            relays.insert(src, tx.clone());
                            tx
                        }
                        Err(e) => {
                            eprintln!("tun2socks: udp relay for {src}: {e}");
                            continue;
                        }
                    },
                };
                if tx.send((data, dst)).is_err() {
                    relays.remove(&src);
                }
            }
            // relay 收回来的包 → 写回 lwip 栈
            rep = replies_rx.recv() => {
                let Some((data, from, to)) = rep else { continue };
                // 四元组必须原样给出，否则内核认不出这是谁的包（见函数头注释）
                if let Err(e) = send_half.send_to(&data, &from, &to) {
                    eprintln!("tun2socks: udp reply {from} -> {to}: {e}");
                }
            }
        }
    }
}

/// 一个 UDP relay：一条被 protect 的 UDP socket，直连 SOCKS5 服务端的
/// UDP ASSOCIATE 端口。DNS / QUIC 都从这里出去。
mod relay {
    use super::*;
    use std::net::UdpSocket as StdUdp;
    use tokio::net::UdpSocket as TokioUdp;
    use tokio::sync::mpsc;

    /// 回包缓存上限：一个 DNS 响应最多 4KB，QUIC 一个 datagram 也就 1.5KB 上下。
    const RECV_BUF: usize = 65_535;

    /// 起一个 relay 任务，返回「把包交给它」的发送端。
    pub fn spawn(
        local: SocketAddr,
        socks5: SocketAddrV4,
        replies: mpsc::UnboundedSender<(Vec<u8>, SocketAddr, SocketAddr)>,
    ) -> Result<mpsc::UnboundedSender<(Vec<u8>, SocketAddr)>, String> {
        let (tx, rx) = mpsc::unbounded_channel::<(Vec<u8>, SocketAddr)>();
        // 在 blocking 池里建 socket + protect + connect（与 TCP 出站同一条路）
        // 之后再交回 tokio —— 建 socket 是阻塞 syscall，不能卡单线程 reactor
        tokio::task::spawn_local(async move {
            let std_sock = match tokio::task::spawn_blocking(move || protected_udp(socks5)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    eprintln!("tun2socks: udp associate {local}: {e}");
                    return;
                }
                Err(_) => return,
            };
            let sock = match TokioUdp::from_std(std_sock) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("tun2socks: udp from_std {local}: {e}");
                    return;
                }
            };
            run(local, sock, rx, replies).await;
        });
        Ok(tx)
    }

    /// 建 UDP socket，protect 之后再 connect。
    ///
    /// connect 一个上层的 SOCKS5 CONNECT 隧道是做不到的（TCP 连接无法承载
    /// UDP），所以这里直连 SOCKS5 服务端的 UDP ASSOCIATE 端口 —— 前提是那个
    /// 服务端在 TUN 路由之外（服务器节点在公网，本来就在 TUN 之外），
    /// 且本 socket 已被 protect。
    fn protected_udp(socks5: SocketAddrV4) -> Result<StdUdp, String> {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return Err(format!("udp socket: {}", std::io::Error::last_os_error()));
        }
        if let Err(e) = protect_fd(fd) {
            unsafe { libc::close(fd) };
            return Err(format!("udp protect: {e}"));
        }
        // ⚠️ 刻意**不设** SO_RCVTIMEO。
        // UDP 是无连接的，recv 可能长时间没有回包（DNS 命中缓存、QUIC 空闲
        // 连接）。一旦 recv 超时，本 relay 就会退出、桶被清掉，下次发包还得
        // 重建 socket —— 既浪费又丢包。这里用「连接保持 + 上层 tokio 控制
        // 生命周期」的模型：relay 一直活着，直到上层不再往它这里发东西。
        let sa = libc::sockaddr_in {
            sin_family: libc::AF_INET as _,
            sin_port: socks5.port().to_be(),
            sin_addr: libc::in_addr {
                s_addr: u32::from_be_bytes(socks5.ip().octets()),
            },
            sin_zero: [0; 8],
        };
        let r = unsafe {
            libc::connect(
                fd,
                &sa as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as _,
            )
        };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(format!("udp connect: {e}"));
        }
        let sock = unsafe { StdUdp::from_raw_fd(fd) };
        sock.set_nonblocking(true)
            .map_err(|e| format!("udp set_nonblocking: {e}"))?;
        Ok(sock)
    }

    /// 收外发 → 封 SOCKS5 UDP 头 → 发；收回包 → 剥头 → 交给上层写回 TUN。
    ///
    /// `local` 是这条 relay 对应的**本地地址**（也就是 lwip 给的 `src`）。
    /// 回包时它会成为 IP 头的目的地址 —— 少了它，内核认不出这个包是谁的，
    /// 会被静默丢弃（详见 `udp_drain` 的文档注释）。
    async fn run(
        local: SocketAddr,
        sock: TokioUdp,
        mut rx: mpsc::UnboundedReceiver<(Vec<u8>, SocketAddr)>,
        replies: mpsc::UnboundedSender<(Vec<u8>, SocketAddr, SocketAddr)>,
    ) {
        let mut buf = vec![0u8; RECV_BUF];
        loop {
            tokio::select! {
                out = rx.recv() => {
                    let Some((data, dst)) = out else { break }; // 上层 drop 了
                    let pkt = super::socks5_udp_encode(&dst, &data);
                    if let Err(e) = sock.send(&pkt).await {
                        eprintln!("tun2socks: udp relay {local} send: {e}");
                        break;
                    }
                }
                r = sock.recv(&mut buf) => {
                    match r {
                        Ok(n) => {
                            match super::socks5_udp_decode(&buf[..n]) {
                                // 回包：来源=对端（成为 IP 头 src），
                                //       去处=本地（成为 IP 头 dst）
                                Some((data, from)) => {
                                    let _ = replies.send((data, from, local));
                                }
                                None => { /* 畸形包，丢 */ }
                            }
                        }
                        Err(e) => {
                            eprintln!("tun2socks: udp relay {local} recv: {e}");
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// 把 TUN 进来的 UDP 载荷按 SOCKS5 UDP 请求格式封装：
/// `RSV(2) FRAG(1) ATYP(1) DST.ADDR DST.PORT DATA`
fn socks5_udp_encode(dst: &SocketAddr, data: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(data.len() + 22);
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // RSV RSV FRAG(0=不切分)
    match dst {
        SocketAddr::V4(v4) => {
            pkt.push(0x01);
            pkt.extend_from_slice(&v4.ip().octets());
            pkt.extend_from_slice(&v4.port().to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            pkt.push(0x04);
            pkt.extend_from_slice(&v6.ip().octets());
            pkt.extend_from_slice(&v6.port().to_be_bytes());
        }
    }
    pkt.extend_from_slice(data);
    pkt
}

/// 拆 SOCKS5 UDP 响应，取出 (数据, 来源地址)。
/// 畸形或不支持的地址类型返回 None。
fn socks5_udp_decode(pkt: &[u8]) -> Option<(Vec<u8>, SocketAddr)> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    if pkt.len() < 4 || pkt[0] != 0x00 || pkt[1] != 0x00 {
        return None;
    }
    if pkt[2] != 0x00 {
        // FRAG != 0：要重组，不支持。QUIC/DNS 都不切分，直接丢。
        return None;
    }
    let mut o = 4;
    let ip: IpAddr = match pkt[3] {
        0x01 => {
            if pkt.len() < o + 4 + 2 {
                return None;
            }
            let a: [u8; 4] = pkt[o..o + 4].try_into().ok()?;
            o += 4;
            IpAddr::V4(Ipv4Addr::from(a))
        }
        0x04 => {
            if pkt.len() < o + 16 + 2 {
                return None;
            }
            let a: [u8; 16] = pkt[o..o + 16].try_into().ok()?;
            o += 16;
            IpAddr::V6(Ipv6Addr::from(a))
        }
        0x03 => {
            // 域名形态：UDP 响应里少见（我们是按 IP 发的），拿不到 IP 就整包丢，
            // 所以这里直接返回 None —— 不必再往下解析端口。
            return None;
        }
        _ => return None,
    };
    let port = u16::from_be_bytes(pkt[o..o + 2].try_into().ok()?);
    o += 2;
    Some((pkt[o..].to_vec(), SocketAddr::new(ip, port)))
}
