//! ghboost 桌面外壳 —— 托盘状态灯 + 一键加速 + 傻瓜式 Web 面板
//!
//! ## 为什么是「托盘 + 浏览器」而不是自绘窗口
//! 同类工具（dev-sidecar / Watt Toolkit）都是 Electron 壳里套一个本地网页。
//! 我们不引入 WebView/浏览器内核，直接用**系统默认浏览器**打开本机的
//! axum 控制台：零原生依赖、LTSC/Server Core 不会白屏、跨平台一致。
//! 托盘只负责「一直有个东西在 + 状态一眼可见 + 一定能退出」。
//!
//! ## 三个必须记住的坑（都是实测出来的）
//! 1. **Windows 必须有 Win32 消息泵**，且要和创建托盘图标的线程是同一个。
//!    没有泵 → 托盘窗口的 WndProc 永远不被调用 → 菜单点击（包括"退出"）
//!    根本不会送达 → 表现就是"托盘关不掉"。见 [`win::drain`]。
//! 2. **退出必须 `std::process::exit`**：控制台跑在另一个线程里 `block_on`，
//!    `main` 返回后它不一定会被收掉，进程会吊着。
//! 3. **命令必须跑在裸 `std::thread` 上**，不能用 `tokio::spawn_blocking`
//!    （`run_blocking` 内部走 `Handle::try_current()`，会撞上
//!    "Cannot block the current thread from within a runtime" 双重 panic）。

// release 版不弹黑色控制台窗口 —— 小白看到黑框会以为程序出错了。
// debug 版保留控制台，方便 `--selftest` / `--quit` 看输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ghboost::{dispatch, hosts, run_blocking};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Working,
    Ok,
    Err,
}

impl State {
    fn rgb(self) -> (u8, u8, u8) {
        match self {
            State::Idle => (136, 135, 128),
            State::Working => (239, 159, 39),
            State::Ok => (99, 153, 34),
            State::Err => (226, 75, 74),
        }
    }
    fn label(self) -> &'static str {
        match self {
            State::Idle => "待机",
            State::Working => "处理中",
            State::Ok => "已加速",
            State::Err => "失败",
        }
    }
}

/// 生成一个纯色圆形图标（32x32 RGBA，边缘做一点抗锯齿）。
///
/// 程序生成而不是打包 .ico：省掉素材文件，而且颜色就是状态灯本身。
fn make_icon(s: State) -> Icon {
    const S: usize = 32;
    let (r, g, b) = s.rgb();
    let c = (S as f32 - 1.0) / 2.0;
    let mut rgba = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = (dx * dx + dy * dy).sqrt();
            if d <= c + 0.5 {
                let i = (y * S + x) * 4;
                let a = ((c - d + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
                rgba[i] = r;
                rgba[i + 1] = g;
                rgba[i + 2] = b;
                rgba[i + 3] = a;
            }
        }
    }
    Icon::from_rgba(rgba, S as u32, S as u32).expect("生成图标失败")
}

enum Msg {
    Progress(String),
    Done(Result<String, String>),
}

// ────────────────────────────── 本机实例管理 ──────────────────────────────

fn state_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var("LOCALAPPDATA").or_else(|_| std::env::var("APPDATA"));
    #[cfg(not(windows))]
    let base = std::env::var("XDG_DATA_HOME")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.local/share")));

    let dir = base
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("ghboost");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn port_file() -> PathBuf {
    state_dir().join("console.port")
}

/// 追踪日志。GUI 子系统下没有控制台，panic/println 全都看不见，
/// 排障只能靠这个文件。路径：%LOCALAPPDATA%\ghboost\trace.log
fn trace(s: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir().join("trace.log"))
    {
        let _ = writeln!(f, "[{}] {s}", chrono_like());
    }
}

/// 不想为一行时间戳引入 chrono
fn chrono_like() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{d} pid={}", std::process::id())
}

/// 已有实例在跑就返回它的端口（用 TCP 探活，避免读到僵尸端口文件）
fn running_port() -> Option<u16> {
    let s = std::fs::read_to_string(port_file()).ok()?;
    let port: u16 = s.trim().parse().ok()?;
    let addr = format!("127.0.0.1:{port}");
    TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(400))
        .map(|_| port)
        .ok()
}

/// 给本机控制台发一个最小 HTTP 请求（不想为一个 POST 引入 reqwest）
fn post_local(port: u16, path: &str) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let Ok(mut s) = TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(800))
    else {
        return false;
    };
    use std::io::{Read, Write};
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let _ = s.read(&mut buf);
    true
}

// ────────────────────────────── 控制台 ──────────────────────────────

static CONSOLE_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

/// 启动傻瓜式控制台（只有大按钮和三盏灯），并打开浏览器。
///
/// 用的是 ghboost 同一套 axum 路由，只换了首页 HTML：
/// `src/gui.html` 是给开发者看的（5 个面板 + 一堆参数），
/// 这里 `src/panel.html` 是给小白看的。
/// 保证控制台在跑，返回它的 URL。已经在跑就直接返回。
///
/// 注意 `--no-browser` 只影响"弹不弹窗口"，**服务永远要起**：
/// 开机自启的场景下，用户点托盘『打开面板』时希望是秒开的，
/// 而不是现起一个服务再等它绑定端口。
pub fn ensure_console() -> Result<String, String> {
    if let Some(&p) = CONSOLE_PORT.get() {
        return Ok(format!("http://127.0.0.1:{p}"));
    }
    match ghboost::web::pick_port(8619) {
        Ok(p) => {
            let _ = CONSOLE_PORT.set(p);
            let _ = std::fs::write(port_file(), p.to_string());
            let url = format!("http://127.0.0.1:{p}");
            std::thread::spawn(move || {
                // 把域名列表注入页面：只维护 Rust 里这一份，
                // 避免 JS 与 Rust 两份列表悄悄漂移。
                let html = include_str!("panel.html").replace(
                    "\"__ONLY__\"",
                    &serde_json::to_string(&PUBLIC_DOMAINS).unwrap_or_else(|_| "null".into()),
                );
                if let Err(e) = run_blocking(ghboost::web::serve_at(p, &html)) {
                    eprintln!("控制台启动失败: {e}");
                }
            });
            Ok(url)
        }
        Err(e) => Err(format!("无法绑定端口: {e}")),
    }
}

/// 起控制台 + 开浏览器（跨平台由 webbrowser crate 负责）
fn open_panel() -> String {
    match ensure_console() {
        Err(e) => e,
        Ok(url) => {
            if webbrowser::open(&url).is_ok() {
                format!("控制台已启动: {url}")
            } else {
                format!("控制台在 {url}（自动打开失败，请手动访问）")
            }
        }
    }
}

// ────────────────────────────── 后台任务 ──────────────────────────────

/// 在裸 std 线程里跑异步核心逻辑（见文件头第 3 条）。
fn spawn_job(job: hosts::BoostParams, tx: Arc<Mutex<Sender<Msg>>>) {
    std::thread::spawn(move || {
        let sink_tx = tx.clone();
        let sink = move |e: &ghboost::Event| {
            let v = dispatch::event_to_json(e);
            let line = match v["type"].as_str().unwrap_or("") {
                "log" => v["message"].as_str().unwrap_or("").to_string(),
                "tick" => format!("测速 {}/{}", v["current"], v["total"].as_u64().unwrap_or(0)),
                _ => String::new(),
            };
            if !line.is_empty() {
                let _ = sink_tx.lock().unwrap().send(Msg::Progress(line));
            }
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_blocking(hosts::boost_core(job, &sink))
        }));
        let msg = match outcome {
            Ok(Ok(v)) => Msg::Done(Ok(v.to_string())),
            Ok(Err(e)) => Msg::Done(Err(e)),
            Err(_) => Msg::Done(Err("内部错误（线程 panic）".to_string())),
        };
        let _ = tx.lock().unwrap().send(msg);
    });
}

/// 公开版要加速的域名。
///
/// 前 9 个是 `ghboost::hosts::DOMAINS`（开源仓的核心场景：GitHub）。
/// 后面这组是小白最常报"打不开"的：
///   - `google.com` / `www.google.com` / `google.hk` —— 搜索与跳转
///   - `youtube.com` / `www.youtube.com` —— 主页与播放页
///   - `fonts.googleapis.com` / `ajax.googleapis.com` —— 国内网页引用最多、
///     卡住会导致整页白屏干等 30 秒的两个，性价比最高
///
/// 刻意**不含** `googlevideo.com`：那是视频 CDN，IP 段变动极快，
/// 写死反而会变慢、甚至拖垮播放。hosts 能做到哪一步就做到哪一步，
/// 剩下的（视频）属于代理产品的职责范围，不混进来。
pub const PUBLIC_DOMAINS: &[&str] = &[
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "avatars.githubusercontent.com",
    "camo.githubusercontent.com",
    "gist.githubusercontent.com",
    "github.githubassets.com",
    "google.com",
    "www.google.com",
    "google.hk",
    "youtube.com",
    "www.youtube.com",
    "fonts.googleapis.com",
    "ajax.googleapis.com",
];

fn boost_params(apply: bool, clean: bool) -> hosts::BoostParams {
    hosts::BoostParams {
        timeout_ms: 3000,
        concurrency: 16,
        top: 1,
        extra_ip: None,
        only: Some(PUBLIC_DOMAINS.iter().map(|s| s.to_string()).collect()),
        apply,
        clean,
    }
}

// ────────────────────────────── Windows 消息泵 ──────────────────────────────
//
// 这是「托盘关不掉」的根因所在：tray-icon 的官方约束是
//   "On Windows and Linux, an event loop must be running on the thread,
//    on Windows, a win32 event loop … you have to create the tray icon
//    on the same thread as the event loop."
// 托盘窗口是这个线程创建的，点击消息也投递到这个线程。不抽消息队列，
// WndProc 就永远不会被调用，菜单点击全部石沉大海。
#[cfg(windows)]
mod win {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_QUIT,
    };

    /// 抽干当前线程的消息队列。返回 false 表示收到了 WM_QUIT（该退出了）。
    pub fn drain() -> bool {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message == WM_QUIT {
                    return false;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        true
    }
}

#[cfg(windows)]
fn pump() -> bool {
    win::drain()
}

#[cfg(not(windows))]
fn pump() -> bool {
    std::thread::sleep(Duration::from_millis(80));
    true
}

#[cfg(windows)]
const TICK: Duration = Duration::from_millis(16); // ~60Hz，够灵敏且不吃 CPU
#[cfg(not(windows))]
const TICK: Duration = Duration::from_millis(0);

// ────────────────────────────── 自检 ──────────────────────────────

/// 无头自检：跑一遍完整的「加速」链路并打印结果。
fn selftest() {
    println!("[权限] 可写 hosts: {}", hosts::is_admin());
    let (tx, rx) = channel::<Msg>();
    let tx = Arc::new(Mutex::new(tx));
    spawn_job(boost_params(true, false), tx.clone());
    loop {
        match rx.recv() {
            Ok(Msg::Progress(line)) => println!("[进度] {line}"),
            Ok(Msg::Done(Ok(v))) => {
                println!("[成功] {v}");
                break;
            }
            Ok(Msg::Done(Err(e))) => {
                println!("[失败] {e}");
                break;
            }
            Err(_) => break,
        }
    }
}

// ────────────────────────────── 主流程 ──────────────────────────────

/// 收摊：先摘掉托盘图标（否则图标会残留在通知区直到鼠标划过），
/// 再显式结束进程 —— 控制台线程还在 block_on，等它自己退出是等不到的。
fn shutdown(tray: TrayIcon) -> ! {
    drop(tray);
    let _ = std::fs::remove_file(port_file());
    std::process::exit(0);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    trace(&format!("start args={args:?}"));
    // 把 panic 也写进 trace.log：GUI 子系统下 stderr 是黑洞
    std::panic::set_hook(Box::new(|info| {
        trace(&format!("PANIC: {info}"));
    }));

    // `--quit`：给"托盘点不动"留的命令行后门，也方便安装脚本 uninstall 前收尾
    if has("--quit") {
        match running_port() {
            Some(p) if post_local(p, "/api/quit") => println!("已请求退出（端口 {p}）"),
            Some(p) => println!("端口 {p} 有响应但退出失败"),
            None => println!("没有检测到运行中的 ghboost"),
        }
        return;
    }

    if has("--selftest") {
        selftest();
        return;
    }

    // 已经在跑：只把浏览器再开一次，绝不启第二个实例
    // （小白会反复双击图标，启第二个只会让端口漂移、状态互相打架）
    if let Some(p) = running_port() {
        let url = format!("http://127.0.0.1:{p}");
        let _ = webbrowser::open(&url);
        return;
    }

    let admin = hosts::is_admin();
    trace(&format!("admin={admin}"));

    let menu = Menu::new();
    let it_boost = MenuItem::with_id("boost", "一键加速", true, None);
    let it_clean = MenuItem::with_id("clean", "还原 hosts", true, None);
    let sep1 = PredefinedMenuItem::separator();
    let it_panel = MenuItem::with_id("panel", "打开面板", true, None);
    let it_admin = MenuItem::with_id("admin", "以管理员身份重启", true, None);
    let sep2 = PredefinedMenuItem::separator();
    let it_quit = MenuItem::with_id("quit", "退出", true, None);
    menu.append(&it_boost).expect("append");
    menu.append(&it_clean).expect("append");
    menu.append(&sep1).expect("append");
    menu.append(&it_panel).expect("append");
    if !admin {
        // 有权限就别占菜单位置
        menu.append(&it_admin).expect("append");
    }
    menu.append(&sep2).expect("append");
    menu.append(&it_quit).expect("append");

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(make_icon(State::Idle))
        .with_tooltip("ghboost · 待机")
        // 左键＝打开面板（小白的第一直觉），右键＝菜单。
        // 默认是左键也弹菜单，那样"点一下看看是什么"会先撞出一堆选项。
        .with_menu_on_left_click(false)
        .build()
        .expect("托盘创建失败");
    trace("tray built");

    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
    let tx = Arc::new(Mutex::new(tx));

    // 启动即开面板。`--no-browser` 给开机自启用：服务照起，只是不弹窗口。
    let mut detail = if has("--no-browser") {
        match ensure_console() {
            Ok(url) => format!("待机（控制台在 {url}，点托盘『打开面板』）"),
            Err(e) => e,
        }
    } else {
        open_panel()
    };
    trace(&format!("console: {detail}"));
    if !admin {
        detail = "未获得管理员权限，写入 hosts 会失败".to_string();
    }

    let mut state = State::Idle;
    let mut busy = false;
    let mut rendered: Option<(State, String)> = None;
    trace("entering loop");

    loop {
        // ── 1. 消息泵（Windows 上这是托盘能响应的前提）──
        if !pump() {
            trace("pump said quit");
            break;
        }

        // ── 2. 托盘图标被点击 ──
        // 能收到这个事件本身就证明消息泵在正常工作（它产生于窗口过程，
        // 不抽消息队列就永远不会有）。所以这也是「退出」能不能用的同一条链路。
        if let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = ev
            {
                trace("tray left click -> open panel");
                detail = open_panel();
            }
        }

        // ── 3. 菜单事件 ──
        if let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = ev.id();
            if id == it_quit.id() {
                shutdown(tray);
            } else if id == it_panel.id() {
                detail = open_panel();
            } else if id == it_admin.id() {
                if let Some(p) = CONSOLE_PORT.get() {
                    // 走 /api/elevate：它会拉起提权进程，然后把自己结束掉
                    let _ = post_local(*p, "/api/elevate");
                    detail = "正在请求提权，请在弹窗里选『是』…".to_string();
                }
            } else if busy {
                detail = "正在处理，请稍候…".to_string();
            } else if id == it_boost.id() {
                state = State::Working;
                busy = true;
                detail = "正在测速选优…".to_string();
                spawn_job(boost_params(true, true), tx.clone());
            } else if id == it_clean.id() {
                state = State::Working;
                busy = true;
                detail = "正在还原 hosts…".to_string();
                spawn_job(boost_params(false, true), tx.clone());
            }
        }

        // ── 3. 工作线程回传 ──
        match rx.try_recv() {
            Ok(Msg::Progress(line)) => detail = line,
            Ok(Msg::Done(res)) => {
                busy = false;
                match res {
                    Ok(v) => {
                        state = State::Ok;
                        detail = v;
                    }
                    Err(e) => {
                        state = State::Err;
                        // 失败必须**说出来**。小白看不到 stderr，
                        // 他唯一能看到的就是这行 tooltip。
                        detail = if e.contains("拒绝访问")
                            || e.to_lowercase().contains("permission")
                            || e.to_lowercase().contains("denied")
                        {
                            format!("需要管理员权限（托盘『以管理员身份重启』）：{e}")
                        } else {
                            e
                        };
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                trace("rx disconnected");
                break;
            }
        }

        // ── 4. 只在变化时刷新（Windows tooltip 上限 127 字符）──
        let mut tip = format!("ghboost · {} · {}", state.label(), detail);
        if tip.chars().count() > 120 {
            tip = format!("{}…", tip.chars().take(119).collect::<String>());
        }
        if rendered.as_ref() != Some(&(state, tip.clone())) {
            let _ = tray.set_tooltip(Some(tip.clone()));
            let _ = tray.set_icon(Some(make_icon(state)));
            rendered = Some((state, tip));
        }

        std::thread::sleep(TICK);
    }

    // 走到这里说明收到了 WM_QUIT 或通道断开 —— 同样要显式收摊
    shutdown(tray);
}
