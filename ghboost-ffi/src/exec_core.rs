//! exec 内核 —— Android 上以子进程跑官方 mihomo / xray / sing-box。
//!
//! ## 和 meow_kernel「必须内嵌」的结论矛盾吗
//!
//! 不矛盾。meow 内嵌的老理由是「`VpnService.protect(fd)` 只能保护本进程的
//! socket，子进程保护不了 → 出站被自己的 TUN 卷回 → 死循环」。现在
//! `Builder.addDisallowedApplication(自身包名)` 把**整个 App**（本进程 + 它
//! 派生的所有子进程）排除出 TUN，exec 内核的出站天然绕开隧道 —— 保护问题
//! 从根上消失。meow 的 protect 保留作另一条腿（MULTI-CORE-PLAN 决策 2；
//! 自排除在个别 ROM 是否生效，W10 实测确认）。
//!
//! ## 二进制从哪来
//!
//! W8 CI 把三内核官方预编译产物按 ABI 注入 jniLibs：
//!
//!     libmihomo.so  libxray.so  libsingbox.so
//!
//! 装包时 Android 解到 `applicationInfo.nativeLibraryDir`（Manifest 显式
//! `extractNativeLibs=true`，否则 AGP 8 默认原地加载、目录里没有真实文件，
//! exec 直接 ENOENT）。那里的文件带可执行位，运行时直接 exec。
//!
//! ## 配置从哪来
//!
//! - mihomo：直接吃现成的 `filesDir/mihomo/configs/`（与内嵌 meow 同一份
//!   YAML + provider，含 `dns.listen: 1053`，见 LocalProxySetup）。
//! - xray / sing-box：读同一份 provider → `corecfg::parse_subscription_text`
//!   → `filter_nodes`（按内核能力裁剪并记录丢弃原因）→ `corecfg::emit`
//!   → 落盘 JSON → `run -c`。协议与配置生成只在共享的 `corecfg`，这里零实现。
//!
//! ## DNS：exec 引擎的 1053 归谁
//!
//! tun2socks 把 TUN 里所有 UDP/53 转发到 `127.0.0.1:1053`（Kotlin 的
//! DNS_PORT）。mihomo 配置自带 `dns.listen` 由内核自己答；xray / sing-box
//! 的发射配置里没有 DNS 监听，由本模块的 [`dns_doh`] 占住 1053：查询按
//! RFC 8484（原始 DNS 报文透传、零解析）经 **SOCKS5 走隧道**问
//! 1.1.1.1 / 8.8.8.8 / 9.9.9.9 —— 抗局域网 DNS 劫持的性质与计划里
//! dokodemo-door 方案相同，但对三个内核统一、且共享的桌面 emit 配置
//! 一字节不改（MULTI-CORE-PLAN 决策 5 的收敛结果）。

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lilyco_ghboost::corecfg::{
    auto_core, emit, filter_nodes, parse_subscription_text, CoreKind, EmitOptions,
};
use serde_json::json;

/// jniLibs 注入名（W8 CI 按 ABI 落文件）。lib 前缀 / .so 后缀只是让 AGP
/// 把它们当 native 库打包解包，本质是可执行文件。
const LIBS: [(CoreKind, &str); 3] = [
    (CoreKind::Mihomo, "libmihomo.so"),
    (CoreKind::Xray, "libxray.so"),
    (CoreKind::SingBox, "libsingbox.so"),
];

/// 与 tun2socks.rs / Kotlin `DNS_PORT` 保持一致的两个口。
const SOCKS5_PORT: u16 = 1080;
const DNS_PORT: u16 = 1053;

/// 同步探活上限。子进程秒退（配置错 / 二进制坏）抓出来当错误返回；
/// 没死也没监听就先 Ok —— 冷启可能要几秒到几十秒（GeoIP 解析、模拟器
/// I/O 退化，meow 实测同款），由 tun2socks 的 LISTENING 等待循环兜住。
/// VpnService 主线程阻塞超过 5s 会 ANR，所以这个值不能放大。
const EARLY_PROBE: Duration = Duration::from_secs(3);

/// 子进程输出的环形尾巴 —— 秒退时把它当错误消息交出去（logcat 另有全量）。
const TAIL_CAP: usize = 40;

type Tail = Arc<Mutex<VecDeque<String>>>;

static RUNNING: AtomicBool = AtomicBool::new(false);
static LISTENING: AtomicBool = AtomicBool::new(false);
static CHILD: Mutex<Option<Child>> = Mutex::new(None);
static ENGINE: Mutex<Option<&'static str>> = Mutex::new(None);

/// exec 内核是否在跑（tun2socks 启动等待的半个条件）。
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// exec 内核是否已监听 1080（tun2socks 启动等待的另半个条件）。
pub fn is_listening() -> bool {
    LISTENING.load(Ordering::SeqCst)
}

/// 当前 exec 内核 id；没在跑为 None。状态栏用。
pub fn engine() -> Option<&'static str> {
    *ENGINE.lock().unwrap()
}

/// 三内核可用性 + auto 建议（JSON）。选择器灰显与状态栏用。
pub fn list(native_lib_dir: &str, config_root: &str) -> serde_json::Value {
    let dir = Path::new(native_lib_dir);
    let mut cores = Vec::new();
    for (kind, lib) in LIBS {
        let path = dir.join(lib);
        cores.push(json!({
            "id": kind.as_str(),
            "label": kind.display(),
            "available": path.is_file(),
            "path": path.to_string_lossy(),
        }));
    }
    json!({
        "cores": cores,
        "auto": resolve_auto(Path::new(config_root)).as_str(),
        "running": is_running(),
        "listening": is_listening(),
        "engine": engine(),
    })
}

/// 当前状态（状态栏轮询用）。
pub fn status() -> serde_json::Value {
    json!({
        "running": is_running(),
        "listening": is_listening(),
        "engine": engine(),
    })
}

/// 拉起 exec 内核。`engine` ∈ `auto | mihomo | xray | sing-box`
/// （`CoreKind::parse` 的别名全认）。返回实际选用的内核 id。
///
/// 同步只等 [`EARLY_PROBE`]，见其注释。
pub fn start(
    native_lib_dir: &str,
    engine: &str,
    config_root: &str,
) -> Result<&'static str, String> {
    stop(); // 漏杀的上一个实例会占着 1080 / 1053 —— 先清场

    let root = Path::new(config_root);
    let kind = match engine.trim() {
        "" | "auto" => resolve_auto(root),
        other => CoreKind::parse(other)
            .ok_or_else(|| format!("未知内核 {other}（可用 auto/mihomo/xray/sing-box）"))?,
    };
    let id = kind.as_str();

    let bin = kernel_bin(native_lib_dir, kind)?;
    let (args, need_doh) = prepare(kind, root)?;
    crate::logcat::info(&format!("exec_core: starting {id} ({})", bin.display()));

    let tail: Tail = Arc::new(Mutex::new(VecDeque::new()));
    let mut child = Command::new(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("拉起 {} 失败：{e}", bin.display()))?;
    if let Some(out) = child.stdout.take() {
        pipe_output(out, id, false, Arc::clone(&tail));
    }
    if let Some(err) = child.stderr.take() {
        pipe_output(err, id, true, Arc::clone(&tail));
    }

    *CHILD.lock().unwrap() = Some(child);
    RUNNING.store(true, Ordering::SeqCst);
    LISTENING.store(false, Ordering::SeqCst);
    *ENGINE.lock().unwrap() = Some(id);

    // DoH 中继要占 1053（xray / sing-box 的发射配置没有内核侧 DNS 监听）。
    // 失败即失败：DNS 全断比启动失败更难排查，宁可在这里炸出明确错误。
    if need_doh {
        if let Err(e) = dns_doh::start() {
            stop();
            return Err(e);
        }
    }

    monitor_spawn(Arc::clone(&tail));

    // 早期探活：抓秒退，把输出尾巴交出去。
    let t0 = Instant::now();
    loop {
        let why = match reap_child() {
            Probe::Running => None,
            Probe::Dead(why) => Some(why),
            Probe::Gone => Some("子进程已消失".to_string()),
        };
        if let Some(why) = why {
            let out = tail_text(&tail);
            stop();
            return Err(format!("{id} 起不来（{why}）；输出尾巴：\n{out}"));
        }
        if is_listening() {
            return Ok(id);
        }
        if t0.elapsed() >= EARLY_PROBE {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(id)
}

/// 停 exec 内核：先关闸（monitor 下一轮自退、不误报），再杀子进程并等它
/// 真的退出（否则 1080 还占着「第二次起不来」），收掉 DoH 中继，复位标志。
/// 幂等，任何状态下调都安全。
pub fn stop() {
    RUNNING.store(false, Ordering::SeqCst);
    let child = CHILD.lock().unwrap().take();
    if let Some(mut ch) = child {
        let _ = ch.kill();
        let _ = ch.wait();
        crate::logcat::info("exec_core: kernel stopped");
    }
    dns_doh::stop();
    LISTENING.store(false, Ordering::SeqCst);
    *ENGINE.lock().unwrap() = None;
}

/// auto 的落点：按节点协议矩阵选内核（与桌面 web / CLI 同一份规则引擎）。
fn resolve_auto(root: &Path) -> CoreKind {
    let provider = provider_path(root);
    if let Ok(text) = std::fs::read_to_string(&provider) {
        let (nodes, _) = parse_subscription_text(&text);
        if !nodes.is_empty() {
            return auto_core(&nodes);
        }
    }
    CoreKind::Mihomo
}

/// 节点清单路径（与 LocalProxySetup.providerFile 同一处，meow/mihomo 共用）。
fn provider_path(root: &Path) -> PathBuf {
    root.join("configs").join("providers").join("ghboost.yaml")
}

/// jniLibs 里该内核的可执行文件。
fn kernel_bin(native_lib_dir: &str, kind: CoreKind) -> Result<PathBuf, String> {
    let lib = LIBS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, l)| *l)
        .ok_or_else(|| format!("未知内核 {}", kind.as_str()))?;
    let path = Path::new(native_lib_dir).join(lib);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "内核 {} 未随包注入（{} 不存在）—— 需要 W8 CI 注入，或改用「內建」引擎",
            kind.display(),
            path.display()
        ))
    }
}

/// 落盘配置，返回（子进程参数，是否需要内建 DoH 中继）。
fn prepare(kind: CoreKind, root: &Path) -> Result<(Vec<String>, bool), String> {
    match kind {
        CoreKind::Mihomo => {
            // 与内嵌 meow 共用同一份配置；-d 指到配置文件所在目录，
            // provider 相对路径与 GeoIP 库按两种口径解析都落在这。
            let cfg_dir = root.join("configs");
            ensure_geo(&cfg_dir, root)?;
            let args = vec!["-d".to_string(), cfg_dir.to_string_lossy().into_owned()];
            Ok((args, false))
        }
        CoreKind::Xray => {
            let cfg = emit_config(kind, root, &root.join("xray"))?;
            let args = vec![
                "run".to_string(),
                "-c".to_string(),
                cfg.to_string_lossy().into_owned(),
            ];
            Ok((args, true))
        }
        CoreKind::SingBox => {
            let cfg = emit_config(kind, root, &root.join("sing-box"))?;
            let args = vec![
                "run".to_string(),
                "-c".to_string(),
                cfg.to_string_lossy().into_owned(),
            ];
            Ok((args, true))
        }
    }
}

/// provider → 解析 → 按内核过滤 → 发射 → 写 `<dir>/config.json`。
fn emit_config(kind: CoreKind, root: &Path, dir: &Path) -> Result<PathBuf, String> {
    let provider = provider_path(root);
    let text = std::fs::read_to_string(&provider)
        .map_err(|e| format!("读节点清单 {} 失败：{e}", provider.display()))?;
    let (parsed, _) = parse_subscription_text(&text);
    let (nodes, dropped) = filter_nodes(parsed, kind);
    for why in dropped {
        crate::logcat::error(&format!("{} 丢弃节点：{why}", kind.as_str()));
    }
    if nodes.is_empty() {
        return Err(format!(
            "节点清单里没有 {} 能用的节点：{}",
            kind.as_str(),
            provider.display()
        ));
    }
    let opts = EmitOptions {
        socks_port: SOCKS5_PORT,
        http_port: 7890,
    };
    let cfg_text = emit(kind, &nodes, &opts)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {} 失败：{e}", dir.display()))?;
    let cfg = dir.join("config.json");
    std::fs::write(&cfg, cfg_text).map_err(|e| format!("写 {} 失败：{e}", cfg.display()))?;
    Ok(cfg)
}

/// GeoIP / GeoSite 库复制一份进 `-d` 目录：mihomo 按配置目录找
/// Country.mmdb / geosite.dat，而 App 把库放 config 根（meow 的
/// `geodata.mmdb-path` 绝对路径读那）。缺哪个跳过哪个 —— 没打包时
/// 配置里本来也没有 GEO 规则（LocalProxySetup.rulesBlock 同判据）。
fn ensure_geo(cfg_dir: &Path, root: &Path) -> Result<(), String> {
    for name in ["Country.mmdb", "geosite.dat"] {
        let dst = cfg_dir.join(name);
        if dst.is_file() {
            continue;
        }
        let src = root.join(name);
        if !src.is_file() {
            continue;
        }
        std::fs::copy(&src, &dst).map_err(|e| format!("复制 {name} 到配置目录失败：{e}"))?;
    }
    Ok(())
}

/// 子进程输出 → logcat（全量）+ 环形尾巴（秒退时当错误消息）。
fn pipe_output<R>(mut src: R, id: &'static str, is_err: bool, tail: Tail)
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(src).lines().map_while(Result::ok) {
            let msg = format!("{id}: {line}");
            if is_err {
                crate::logcat::error(&msg);
            } else {
                crate::logcat::info(&msg);
            }
            let mut q = tail.lock().unwrap();
            if q.len() >= TAIL_CAP {
                q.pop_front();
            }
            q.push_back(msg);
        }
    });
}

/// 监控线程：盯子进程生死 + 探 1080。冷启可能几十秒（GeoIP 解析、模拟器
/// I/O 退化，meow 实测同款）—— 阻塞 VpnService 主线程既不可控也不该做，
/// 交给 tun2socks 启动时的等待循环配套。
fn monitor_spawn(tail: Tail) {
    let _ = std::thread::Builder::new()
        .name("ghboost-exec-core".into())
        .spawn(move || {
            loop {
                if !RUNNING.load(Ordering::SeqCst) {
                    // stop() 已经收尾了，一切已复位，直接走人
                    return;
                }
                match reap_child() {
                    Probe::Running => {}
                    // stop() 收走了子进程 —— 它自己负责复位，这里不误报
                    Probe::Gone => return,
                    Probe::Dead(why) => {
                        crate::logcat::error(&format!("exec_core: kernel {why}"));
                        break;
                    }
                }
                if !LISTENING.load(Ordering::SeqCst) && probe_1080() {
                    LISTENING.store(true, Ordering::SeqCst);
                    crate::logcat::info("exec_core: kernel listening on 1080");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // 走到这里 = 内核自己死了：不关闸的话 is_running() 会一直报 true，
            // 状态栏停在「还在跑」，且 DoH 中继还占着 1053。
            RUNNING.store(false, Ordering::SeqCst);
            LISTENING.store(false, Ordering::SeqCst);
            *ENGINE.lock().unwrap() = None;
            dns_doh::stop();
            crate::logcat::error(&format!("exec_core: output tail:\n{}", tail_text(&tail)));
        });
}

/// [`reap_child`] 的结果。
enum Probe {
    /// 还活着。
    Running,
    /// 已退出 / 出错（顺手把 [`CHILD`] 清了）。
    Dead(String),
    /// [`CHILD`] 是空的 —— stop() 收走了，或还没放进去。
    Gone,
}

/// 对 [`CHILD`] 做一次 try_wait：锁在本函数内做完，调用方拿到的只是
/// 所有权结论 —— 不把「match 锁内引用的同时改锁」这种借用边界问题
/// 扩散到每个调用点。
fn reap_child() -> Probe {
    let mut guard = CHILD.lock().unwrap();
    let ch = match guard.as_mut() {
        Some(ch) => ch,
        None => return Probe::Gone,
    };
    match ch.try_wait() {
        Ok(None) => Probe::Running,
        Ok(Some(status)) => {
            *guard = None;
            Probe::Dead(format!("已退出（{status}）"))
        }
        Err(e) => {
            *guard = None;
            Probe::Dead(format!("try_wait 出错：{e}"))
        }
    }
}

/// 1080 是否有人监听（200ms 上限，别把监控循环拖长）。
fn probe_1080() -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, SOCKS5_PORT));
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

fn tail_text(tail: &Tail) -> String {
    let q = tail.lock().unwrap();
    if q.is_empty() {
        "（无输出）".to_string()
    } else {
        q.iter().map(String::as_str).collect::<Vec<_>>().join("\n")
    }
}

/// 内建 DoH 中继（RFC 8484）：占 `127.0.0.1:1053`（tun2socks 的 DNS 转发
/// 目标口），把查询**原文透传**给 DoH 上游、响应回写 —— 没有一行 DNS 解析。
///
/// 上游经 SOCKS5（127.0.0.1:1080）走隧道发出：既抗局域网劫持（443+TLS，
/// UDP 53 的劫持链路够不着），又不依赖各内核的 DNS 配置差异。URL 一律
/// **IP 形式**（1.1.1.1 / 8.8.8.8 / 9.9.9.9，证书都带 IP SAN）—— 域名形式
/// 要先查 DNS 才能建 TLS，而 DNS 正是本模块要修的东西，会自举死锁。
///
/// 代际（GEN）管理生命周期：start 取新代、stop 递代，旧线程下一轮
/// 自见代际不符退出 —— 「停了马上再起」不会撞上一个还占着 1053 的旧线程。
mod dns_doh {
    use std::net::UdpSocket;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::{DNS_PORT, SOCKS5_PORT};

    const UPSTREAMS: [&str; 3] = [
        "https://1.1.1.1/dns-query",
        "https://8.8.8.8/dns-query",
        "https://9.9.9.9/dns-query",
    ];

    static GEN: AtomicUsize = AtomicUsize::new(0);
    /// 最近一条查询成功的上游 —— 下次从它开始试（sticky），单路挂掉不拖慢每条。
    static PREFERRED: AtomicUsize = AtomicUsize::new(0);

    /// 起中继。重复调用只会把旧线程顶掉（代际替换），不会双开。
    pub fn start() -> Result<(), String> {
        let my_gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
        let r = spawn(my_gen);
        if r.is_err() {
            // 没起来就再递一代，确保没有任何线程留在旧代里空转。
            GEN.fetch_add(1, Ordering::SeqCst);
        }
        r
    }

    /// 收中继（幂等）：递代让线程自退，socket 随线程 drop 释放。
    pub fn stop() {
        GEN.fetch_add(1, Ordering::SeqCst);
    }

    fn spawn(my_gen: usize) -> Result<(), String> {
        let sock = bind()?;
        let client = build_client()?;
        std::thread::Builder::new()
            .name("ghboost-dns-doh".into())
            .spawn(move || serve(sock, client, my_gen))
            .map_err(|e| format!("DoH 中继线程创建失败：{e}"))?;
        crate::logcat::info(&format!("dns_doh: up on 127.0.0.1:{DNS_PORT}"));
        Ok(())
    }

    fn bind() -> Result<Arc<UdpSocket>, String> {
        let addr = ("127.0.0.1", DNS_PORT);
        let sock = match UdpSocket::bind(addr) {
            Ok(s) => s,
            // 上一代线程最多 500ms（读超时轮询）后自退 —— 等它让位一次。
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                std::thread::sleep(Duration::from_millis(600));
                UdpSocket::bind(addr)
                    .map_err(|e| format!("绑定 127.0.0.1:{DNS_PORT} 失败：{e}"))?
            }
            Err(e) => return Err(format!("绑定 127.0.0.1:{DNS_PORT} 失败：{e}")),
        };
        sock.set_read_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| format!("DoH 中继 set_read_timeout：{e}"))?;
        Ok(Arc::new(sock))
    }

    fn build_client() -> Result<reqwest::blocking::Client, String> {
        let proxy_url = format!("socks5h://127.0.0.1:{SOCKS5_PORT}");
        let proxy = reqwest::Proxy::all(proxy_url.as_str())
            .map_err(|e| format!("SOCKS5 代理构建失败：{e}"))?;
        reqwest::blocking::Client::builder()
            .proxy(proxy)
            .timeout(Duration::from_secs(4))
            .build()
            .map_err(|e| format!("DoH 客户端构建失败：{e}"))
    }

    fn serve(sock: Arc<UdpSocket>, client: reqwest::blocking::Client, my_gen: usize) {
        let mut buf = [0u8; 4096];
        while GEN.load(Ordering::SeqCst) == my_gen {
            match sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    let q = buf[..n].to_vec();
                    let sock = Arc::clone(&sock);
                    let client = client.clone();
                    // 每条查询一个线程：DNS 突发量级（十/秒）下最简单也够用；
                    // 上游连接复用靠 client 连接池（同一条 SOCKS 隧道长连）。
                    std::thread::spawn(move || {
                        let resp = query(&client, &q).unwrap_or_else(|| servfail(&q));
                        if let Some(r) = resp {
                            let _ = sock.send_to(&r, from);
                        }
                    });
                }
                // 读超时是轮询代际的节拍，不是错误。
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                // 其它错误：socket 已不可用，收工。
                Err(_) => return,
            }
        }
    }

    /// 一条查询 → DoH 原文透传 → 响应报文。全路上游挂掉返回 None。
    fn query(client: &reqwest::blocking::Client, q: &[u8]) -> Option<Vec<u8>> {
        let start = PREFERRED.load(Ordering::Relaxed);
        for i in 0..UPSTREAMS.len() {
            let idx = (start + i) % UPSTREAMS.len();
            let resp = client
                .post(UPSTREAMS[idx])
                .header("content-type", "application/dns-message")
                .header("accept", "application/dns-message")
                .body(q.to_vec())
                .send();
            let Ok(resp) = resp else { continue };
            if !resp.status().is_success() {
                continue;
            }
            let Ok(bytes) = resp.bytes() else { continue };
            if bytes.len() < 12 {
                continue;
            }
            PREFERRED.store(idx, Ordering::Relaxed);
            return Some(bytes.to_vec());
        }
        None
    }

    /// 上游全挂时回 SERVFAIL：置 QR/RA、RCODE=2、清 ANCOUNT，问题段与
    /// EDNS 原样带回应答 —— 解析器立刻拿到失败，而不是干等超时。
    fn servfail(q: &[u8]) -> Option<Vec<u8>> {
        if q.len() < 12 {
            return None;
        }
        let mut r = q.to_vec();
        r[2] |= 0x80; // QR = 1
        r[3] = (r[3] & 0x70) | 0x80 | 0x02; // RA = 1, RCODE = SERVFAIL
        r[6] = 0;
        r[7] = 0; // ANCOUNT = 0
        Some(r)
    }
}
