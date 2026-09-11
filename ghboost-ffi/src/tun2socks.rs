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
//! 还没做的：
//! - UDP：现在只 poll-and-drop。要走通 DNS / QUIC 得接 SOCKS5 UDP
//!   ASSOCIATE（一个保护过的 UDP socket 转发所有 UDP 包）。
//! - SOCKS5 只支持 NO_AUTH；目的地址 v4/v6 都吃；SOCKS5 服务端
//!   地址要求 IPv4。
//!
//! FORWARDING_IMPLEMENTED 留 false：lwip 接进来了，但板上要有一个
//! 真能用的本地 SOCKS5 代理（mihomo on Android：把订阅节点协议
//! VLESS/Trojan/SS 转成 SOCKS5 喂给这里，并且它自己的出站也要
//! 被 protect 保护），否则放开 Start 之后 UI 写「VPN 已连接」但
//! 所有连接都 ECONNREFUSED，比完全断网还难查。条件齐了把下面
//! 这个 `false` 改成 `true` 即可，Kotlin 不用动。

use futures::{SinkExt, StreamExt};
use std::net::{SocketAddr, SocketAddrV4};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::net::TcpStream as TokioTcp;
use tokio::sync::Notify;

#[cfg(target_os = "android")]
use crate::protect;

static RUNNING: AtomicBool = AtomicBool::new(false);
static SHUTDOWN: Mutex<Option<Notify>> = Mutex::new(None);

pub const FORWARDING_IMPLEMENTED: bool = false;

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
    let notify = Notify::new();
    *SHUTDOWN.lock().unwrap() = Some(notify.clone());

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

fn run_thread(fd: RawFd, socks5: SocketAddrV4, notify: Notify) {
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
        let (mut stack_sink, mut stack_stream) = stack.split();

        // 三个 !Send 任务在 LocalSet 上跑（lwip 类型不能跨线程）
        tokio::task::spawn_local(tun_io(tun, stack_sink, stack_stream));
        tokio::task::spawn_local(tcp_accept(tcp_listener, socks5));
        tokio::task::spawn_local(udp_drain(udp_socket));

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

async fn tun_io<Sink, Stream, E>(
    tun: AsyncFd<OwnedFd>,
    mut stack_sink: Sink,
    mut stack_stream: Stream,
) where
    Sink: futures::Sink<Vec<u8>, Error = E> + Unpin,
    Stream: futures::Stream<Item = Result<Vec<u8>, E>> + Unpin,
{
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

async fn tcp_accept<TL>(mut tcp_listener: TL, socks5: SocketAddrV4)
where
    TL: futures::Stream + Unpin,
{
    while let Some(ev) = tcp_listener.next().await {
        let ev = match ev {
            Ok(v) => v,
            Err(e) => {
                eprintln!("tun2socks tcp_listener: {e:?}");
                continue;
            }
        };
        let (stream, _local, remote): (
            lwip::TcpStream,
            std::net::SocketAddr,
            std::net::SocketAddr,
        ) = ev;
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

// ── UDP：现在只 poll-and-drop，TODO SOCKS5 UDP ASSOCIATE ─────

async fn udp_drain(mut udp: Box<lwip::UdpSocket>) {
    // Box<UdpSocket>: Stream（Item=(Vec<u8>, src, dst)）。Pin 住以免
    // UdpSocket 自身是否 Unpin 影响 .next()（Pin<Box<_>> 永远 Unpin）。
    let mut udp = std::pin::Pin::from(udp);
    while let Some(_pkt) = udp.next().await {
        // TODO: SOCKS5 UDP ASSOCIATE（一个被 protect 的 UDP socket
        // 转发所有 UDP 包）。没做之前 DNS 走不通，TCP 仍能走 IP 直连。
    }
}
