//! Web 控制台 —— 监听回环的 axum 服务，复用仓库自带的 `src/gui.html`
//!
//! 设计约束（都是踩过的坑）：
//! 1. **零外部请求**：不引任何 CDN/字体/脚本。ghboost 的受众是外网连通性差的人，
//!    本地控制台一旦依赖公网资源，页面会在他们机器上静默退化成"点了没反应"
//!    （事件绑定在外部脚本的回调里，脚本没加载 = 回调不执行）。
//! 2. **只听 127.0.0.1**：本地 GUI 不需要暴露到局域网；Host 非回环直接 403，
//!    顺带防 DNS rebinding。
//! 3. **命令分发与传输层解耦**：`execute_command_with_sink` 只认一个 `Fn(&Event)`，
//!    WebView 和 SSE 两条路各传各的 sink，共用同一套 core 逻辑。
//! 4. **浏览器打不开也要能用**：`webbrowser::open` 失败只打印 URL，不让进程退出
//!    （headless / SSH / Windows Server Core 是真实场景）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{
    sse::{Event as SseEvent, KeepAlive, Sse},
    Html, IntoResponse, Response,
};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::dispatch::{event_to_json, execute_command_with_sink};
use crate::proxy;
use crate::Event;

/// 会话表：session_id → 事件接收端。
///
/// `/run` 建立会话并起线程，`/progress/{id}` 通过 SSE 把事件流出去。
/// 每个 `Receiver` 只能被一个 SSE 连接消费（取走即消失），天然防止重复消费。
///
/// 用 `std::sync::mpsc` 而不是 tokio 的：命令线程在 `run_blocking` 里
/// **自建 runtime 并在当前线程 block_on**，此后该线程处于 runtime 上下文中，
/// tokio channel 的 `blocking_send` 会 panic
/// （"Cannot block the current thread from within a runtime"）。
/// 原生 channel 的 `send` 没有这个限制。
type Sessions = Arc<Mutex<HashMap<String, std::sync::mpsc::Receiver<Value>>>>;

struct State {
    sessions: Sessions,
    /// 首页 HTML。`serve_at` 可以换成另一套皮（见该函数注释）。
    html: String,
}

/// 找一个可用端口：从 `preferred` 开始，被占用就 +1，最多试 10 次。
///
/// 这里 bind 完立刻 drop，axum 会重新 bind 同一个端口——中间有极小的
/// TOCTOU 窗口，但对本地 GUI 工具来说，比起直接 unwrap 恐慌掉要友好得多
/// （用户连开两次 exe 是很常见的操作）。
pub fn pick_port(preferred: u16) -> Result<u16, String> {
    for offset in 0..10 {
        let port = preferred + offset;
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(format!(
        "127.0.0.1 上 {preferred}..{} 端口都被占用（用 LILYCO_PORT 换一个）",
        preferred + 9
    ))
}

/// 启动 Web 控制台（阻塞，直到收到 Ctrl-C）。
pub async fn serve(port: u16) -> Result<(), String> {
    serve_at(port, include_str!("gui.html")).await
}

/// 用自定义首页启动控制台 —— 同一套路由，换一层皮。
///
/// 为什么要能换皮：`src/gui.html` 是**开发者控制台**（5 个面板、一堆参数），
/// 公开发行版要的是"傻瓜式"单页（一个大按钮 + 三个站点灯）。路由/命令分发
/// 完全共用，只有首页不同。
pub async fn serve_at(port: u16, html: &str) -> Result<(), String> {
    let state = Arc::new(State {
        sessions: Arc::new(Mutex::new(HashMap::new())),
        html: html.to_string(),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/run", post(run_handler))
        .route("/progress/{id}", get(progress_handler))
        .route("/proxy/set", post(proxy_set))
        .route("/proxy/unset", post(proxy_unset))
        .route("/proxy/status", get(proxy_status))
        .route("/api/quit", post(api_quit))
        .route("/api/info", get(api_info))
        .route("/api/update", get(api_update))
        .route("/api/check", get(api_check))
        .route("/api/elevate", post(api_elevate))
        .route("/api/proxy/subscribe", post(api_proxy_subscribe))
        .route("/api/proxy/stop", post(api_proxy_stop))
        .route("/api/proxy/state", get(api_proxy_state))
        .layer(axum::middleware::from_fn(guard_loopback_mw))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("绑定 127.0.0.1:{port} 失败：{e}"))?;

    let url = format!("http://127.0.0.1:{port}");
    eprintln!("ghboost Web 控制台：{url}");
    eprintln!("（浏览器将自动打开；按 Ctrl-C 退出）");

    // 开浏览器：失败只提示，不退出——服务本身仍然可用
    if let Err(e) = webbrowser::open(&url) {
        eprintln!("  自动打开浏览器失败（{e}），请手动打开上面的地址");
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            eprintln!("\n正在关闭…");
        })
        .await
        .map_err(|e| format!("axum serve 失败：{e}"))
}

/// 首页：仓库自带的 gui.html，注入一行标记让页面知道走 HTTP 传输层。
///
/// WebView 模式下没有这个标记，页面会退回 `window.run_command` 的 bind 调用。
async fn index(axum::extract::State(state): axum::extract::State<Arc<State>>) -> Html<String> {
    // 必须在最前面执行，早于页面内联脚本解析
    const SHIM: &str = "<script>window.__GH_HTTP__=1;</script>\n";
    Html(format!("{SHIM}{}", state.html))
}

/// 只允许回环访问，挡掉 DNS rebinding / 局域网扫描。
///
/// 做成**全局中间件**而不是逐 handler 加——漏一个 handler 就是一个洞
/// （第一版就漏了 /proxy/*，实测非回环 Host 照样 200）。
async fn guard_loopback_mw(
    headers: axum::http::HeaderMap,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // 去掉可能的 userinfo@，再去掉 :port
    let host = host.rsplit_once('@').map(|x| x.1).unwrap_or(host);
    let authority = host.rsplit_once(':').map(|x| x.0).unwrap_or(host);
    let ok = matches!(authority, "127.0.0.1" | "localhost" | "[::1]" | "::1");
    if !ok {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    // ── 同源问题：localhost 和 127.0.0.1 在浏览器眼里是**两个 origin** ──
    // 页面用 localhost 打开、请求打到 127.0.0.1（或反过来）就算跨域。
    // 简单 GET（/api/check、/api/info）不发预检，所以照常成功；
    // 但带 `Content-Type: application/json` 的 **POST /run** 会先发
    // OPTIONS 预检，axum 没有对应 handler → 405 → fetch 直接 reject，
    // 前端只看到一句毫无信息量的 "Failed to fetch"。
    // 表现就是"能检测、点加速就失败"，极难排查。这里统一放行。
    if request.method() == axum::http::Method::OPTIONS {
        return (
            StatusCode::NO_CONTENT,
            [
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS"),
                (header::ACCESS_CONTROL_ALLOW_HEADERS, "Content-Type"),
            ],
        )
            .into_response();
    }

    let mut resp = next.run(request).await;
    // 只在回环内可达（`*` 此时是安全的：非回环 Host 上面已被 403 挡掉）
    resp.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        axum::http::HeaderValue::from_static("*"),
    );
    resp
}

#[derive(serde::Deserialize)]
struct RunRequest {
    cmd: String,
    #[serde(default)]
    args: Value,
}

async fn run_handler(
    axum::extract::State(state): axum::extract::State<Arc<State>>,
    Json(req): Json<RunRequest>,
) -> Response {
    let sid = format!("{:x}", rand_u64());
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    state.sessions.lock().await.insert(sid.clone(), rx);

    // 命令必须在**不带 tokio 上下文的原生线程**里跑。
    // 不能用 `tokio::task::spawn_blocking`：它的线程仍然带 runtime 上下文，
    // `run_blocking` 里 `Handle::try_current()` 会成功，于是走 `h.block_on(fut)`
    // → panic "Cannot block the current thread from within a runtime"
    // → 连结束帧都发不出来，前端永远卡在 Running。
    // 换成 std::thread 后 try_current() 返回 Err，run_blocking 自建 runtime，正常。
    let cmd = req.cmd;
    let args = req.args;
    std::thread::spawn(move || {
        // catch_unwind：即使 core 里 panic，也要给前端一个结束帧，别让它转圈到底
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let sink = |e: &Event| {
                let _ = tx.send(event_to_json(e));
            };
            execute_command_with_sink(&cmd, &args, &sink)
        }));

        // 结束帧：done 或 error，前端据此收尾
        let last = match outcome {
            Ok(Ok(v)) => serde_json::json!({
                "type": "done",
                "result": serde_json::to_string(&v).unwrap_or_default(),
                "elapsed_ms": 0,
            }),
            Ok(Err(e)) => serde_json::json!({ "type": "error", "message": e }),
            Err(_) => serde_json::json!({
                "type": "error",
                "message": "内部错误（线程 panic），详见控制台 stderr"
            }),
        };
        let _ = tx.send(last);
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({ "session_id": sid })),
    )
        .into_response()
}

async fn progress_handler(
    axum::extract::State(state): axum::extract::State<Arc<State>>,
    Path(id): Path<String>,
) -> Response {
    let rx = state.sessions.lock().await.remove(&id);
    let Some(rx) = rx else {
        return (StatusCode::NOT_FOUND, "unknown session").into_response();
    };

    // 原生 channel 只能同步 `try_recv`，所以用轮询（50ms ≈ 20Hz）。
    // 本地 UI 场景下这点延迟完全无感，换来的是命令线程不受 tokio 上下文约束。
    let stream = async_stream::stream! {
        loop {
            match rx.try_recv() {
                Ok(v) => yield Ok::<_, std::convert::Infallible>(
                    SseEvent::default().data(v.to_string())
                ),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                // 发送端已 drop：命令线程结束（或 panic 后 unwind），流到此为止
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ── 系统代理：与 WebView 的 set_proxy/unset_proxy/get_proxy_status 同一套 ──

async fn proxy_set(Json(cfg): Json<proxy::ProxyConfig>) -> Json<Value> {
    Json(match proxy::set_proxy(&cfg) {
        Ok(state) => serde_json::json!({ "ok": true, "state": format!("{state:?}") }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    })
}

async fn proxy_unset() -> Json<Value> {
    Json(match proxy::unset_proxy() {
        Ok(state) => serde_json::json!({ "ok": true, "state": format!("{state:?}") }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    })
}

async fn proxy_status() -> Json<Value> {
    Json(serde_json::json!({ "state": format!("{:?}", proxy::get_proxy_status()) }))
}

// ── 桌面壳（托盘）需要的三个控制面接口 ──
//
// 为什么要这些：托盘图标在 Win11 会被折进「^」溢出区，小白根本找不到，
// 更别说点"退出"。浏览器里的按钮是**唯一确定能被看见**的逃生通道。

/// 退出程序。
///
/// 先回 200 再退，否则 axum 还没把响应写回 socket 进程就没了，
/// 前端会收到 `net::ERR_EMPTY_RESPONSE`（看起来像"点了没反应"）。
async fn api_quit() -> Json<Value> {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(250));
        std::process::exit(0);
    });
    Json(serde_json::json!({ "ok": true }))
}

/// 以管理员/root 权限重启自己，然后退出当前进程。
///
/// 为什么不整包都设 `requireAdministrator`：那样每次开机自启都会弹 UAC，
/// 小白会以为中毒。正确做法（Watt Toolkit / dev-sidecar 都这么干）是
/// **平时按普通权限跑，只有真要写 hosts 时才提权一次**。
///
/// 实现上没用 ShellExecute/PowerShell 之外的 FFI：走 `Command` 不需要给
/// 这个 crate 加 windows-sys 依赖，代价是 Windows 上会闪一下 powershell。
async fn api_elevate() -> Json<Value> {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    };

    // 提权重启后要回到同一个端口，否则浏览器里的页面会指向已死的端口。
    // 但 `--no-browser` 必须丢掉：用户是**从页面上点的提权**，他的标签页
    // 马上就会随着旧进程退出而失效，新进程必须再开一次浏览器，否则他会
    // 面对一个点不动的死页面。
    let mut args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--no-browser")
        .collect();

    let spawned = if cfg!(windows) {
        let exe_s = exe.to_string_lossy().replace('\'', "''");
        // 单引号里包住路径，避免空格/中文路径被拆开
        let arg_list = args
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let ps = if arg_list.is_empty() {
            format!("Start-Process -FilePath '{exe_s}' -Verb RunAs")
        } else {
            format!("Start-Process -FilePath '{exe_s}' -ArgumentList @({arg_list}) -Verb RunAs")
        };
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .spawn()
    } else if cfg!(target_os = "macos") {
        let cmd = if args.is_empty() {
            format!("'{}'", exe.to_string_lossy())
        } else {
            format!("'{}' {}", exe.to_string_lossy(), args.join(" "))
        };
        let script = format!("do shell script \"{cmd}\" with administrator privileges");
        std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn()
    } else {
        args.insert(0, exe.to_string_lossy().to_string());
        // pkexec 有图形化授权框；没有就退回 sudo -A（需要 SUDO_ASKPASS）
        std::process::Command::new("pkexec").args(&args).spawn()
    };

    match spawned {
        Ok(_) => {
            // 让新进程先起来再自杀：UAC 对话框期间旧进程还活着也无所谓，
            // 端口冲突时它会自己顺延一个（见 tray 的 pick_port）。
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(400));
                std::process::exit(0);
            });
            Json(serde_json::json!({ "ok": true }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// 运行环境信息：前端据此决定要不要弹"以管理员身份重启"。
async fn api_info() -> Json<Value> {
    Json(serde_json::json!({
        "admin": crate::hosts::is_admin(),
        "os": std::env::consts::OS,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ────────────────────────────── 版本更新提醒 ──────────────────────────────
//
// 客户端永远免费、靠订阅赚钱 —— 用户跑着旧版本，等于修过的 bug 他还在踩，
// 转化就断了。所以面板必须能告诉他"有新版"。
//
// 三条硬约束（都是这文件开头那套原则的延续）：
// 1. **页面零外部请求**：轮询 GitHub 必须由 Rust 侧做，不能让浏览器去 fetch
//    api.github.com —— 那样在缺根证书 / 被拦的环境里会静默失败，
//    也违背了"本地控制台不依赖公网"这条底线。
// 2. **绝不能挡住面板**：结果缓存 6 小时；过期时本次请求直接回旧值，
//    刷新丢到后台线程。网络不通只是"没有提示"，不是报错。
// 3. **只在真的更新时提示**：逐段比较数字，tag 带后缀（如 `0.3.5-beta`）
//    非数字段按 0 处理，不会被误判成更新。

static UPDATE_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<(Instant, Value)>>> =
    std::sync::OnceLock::new();

const UPDATE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const RELEASES_LATEST_API: &str = "https://api.github.com/repos/lilyco-42/ghboost/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/lilyco-42/ghboost/releases/latest";

/// 新版检查。`checked=false` 表示"这次还没查到"，UI 什么都不显示。
async fn api_update() -> Json<Value> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let cached: Option<(Instant, Value)> = UPDATE_CACHE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|g| g.as_ref().cloned());

    if let Some((at, val)) = &cached {
        if at.elapsed() < UPDATE_TTL {
            return Json(val.clone());
        }
    }

    // 过期：先回旧值（没有就回"未检查"），刷新另起线程，页面不等它。
    let me = current.clone();
    std::thread::spawn(move || {
        let val = match fetch_latest_version() {
            Some(latest) => serde_json::json!({
                "checked": true,
                "current": me,
                "latest": latest,
                "has_update": is_newer(&latest, &me),
                "url": RELEASES_PAGE,
            }),
            // 查不到（没网 / 被拦 / 限流）就标"查过了但没有更新"，下次再试。
            None => serde_json::json!({ "checked": true, "current": me, "has_update": false }),
        };
        if let Ok(mut g) = UPDATE_CACHE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
        {
            *g = Some((Instant::now(), val));
        }
    });

    Json(cached.map(|(_, v)| v).unwrap_or_else(
        || serde_json::json!({ "checked": false, "current": current, "has_update": false }),
    ))
}

/// 拉 GitHub 上最新 release 的 tag（去掉前导 `v`）。任何失败都当"查不到"。
fn fetch_latest_version() -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(6))
        // GitHub API 强制要求 User-Agent，没有会直接 403。
        .user_agent(concat!("ghboost/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let body: Value = client
        .get(RELEASES_LATEST_API)
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .ok()?;
    Some(
        body.get("tag_name")?
            .as_str()?
            .trim()
            .trim_start_matches('v')
            .to_string(),
    )
}

/// 逐段比较数字；非数字段按 0（所以 `0.3.5-beta` 不会被判成比 `0.3.4` 新）。
fn is_newer(latest: &str, current: &str) -> bool {
    let nums = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| p.trim().parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (a, b) = (nums(latest), nums(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

// ────────────────────────────── 代理设置 ──────────────────────────────
//
// 面向的是**已有节点/订阅、只差一个傻瓜式开关**的用户（台湾市场的主要形态：
// 自建或购买节点在台湾完全合法，Clash Verge / v2rayN 之类的工具是公开商品）。

/// 内置的 mihomo 内核进程。全局唯一，重启订阅时复用同一个。
static MIHOMO: std::sync::OnceLock<std::sync::Mutex<Option<crate::mihomo::MihomoManager>>> =
    std::sync::OnceLock::new();

fn mihomo_slot() -> &'static std::sync::Mutex<Option<crate::mihomo::MihomoManager>> {
    MIHOMO.get_or_init(|| std::sync::Mutex::new(None))
}

/// 内核与订阅配置的存放目录（%LOCALAPPDATA%\ghboost\mihomo）
fn ghboost_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    let base = std::env::var("LOCALAPPDATA").or_else(|_| std::env::var("APPDATA"));
    #[cfg(not(windows))]
    let base = std::env::var("XDG_DATA_HOME")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.local/share")));
    let dir = base
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("ghboost");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 内置内核的预期位置（`install.ps1 -WithKernel` 会把文件放到这里）
fn kernel_path() -> std::path::PathBuf {
    let dir = ghboost_dir().join("bin");
    let name = if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    };
    let want = dir.join(name);
    if !want.exists() {
        // 自愈：见 normalize_kernel_name 的说明。
        // 这一段不是防御性冗余 —— 安装脚本的 PowerShell 在 CI 里根本执行不到，
        // 唯一能真正被编译、被回归测试覆盖的正规化逻辑只有这里。
        normalize_kernel_name(&dir, name);
    }
    want
}

/// 把 `mihomo-windows-amd64-compatible.exe` 这类档名统一成 `mihomo.exe`。
///
/// 上游发行包解出来的就是那个带后缀的名字，而程式找的是精确档名。
/// 不正规化的故障形态极其糟糕：包里明明有内核，程式却报「找不到内核」，
/// 用户完全无从下手 —— 而且它在开发机上不会复现（本机那份是手动改过名的）。
#[cfg(windows)]
fn normalize_kernel_name(dir: &std::path::Path, want: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let os_name = entry.file_name();
        let n = os_name.to_string_lossy();
        if n.eq_ignore_ascii_case(want) {
            continue;
        }
        if n.starts_with("mihomo") && n.ends_with(".exe") {
            eprintln!("[ghboost] 内核档名不规范（{n}），已自动改名为 {want}");
            let _ = std::fs::rename(entry.path(), dir.join(want));
            return;
        }
    }
}

#[cfg(not(windows))]
fn normalize_kernel_name(_dir: &std::path::Path, _want: &str) {}

/// 用户贴进来的东西属于哪一种。
///
/// 台湾市场两种形态都很常见：买订阅给一条 `https://` 网址，
/// 或者机场直接给一串 `vless://` 链接（甚至一整坨 base64）。
/// 两种都得能直接贴 —— 让用户先去搞懂差别，就不是傻瓜式了。
enum NodeSource {
    /// 订阅网址：交给 mihomo 的 http provider 自己去拉、自己定时更新
    Url(String),
    /// 节点链接原文：单条 / 多条 / base64 一大坨都算，落盘后由 file provider 读
    Links(String),
}

/// 认得出 `vless://` 这类前缀的链接
const LINK_SCHEMES: &[&str] = &[
    "vless://",
    "vmess://",
    "ss://",
    "ssr://",
    "trojan://",
    "hysteria://",
    "hysteria2://",
    "hy2://",
    "tuic://",
    "snell://",
    "socks://",
    "socks5://",
    "http://",
    "https://",
    "wireguard://",
    "anytls://",
    "juicity://",
    "mieru://",
    "ssh://",
];

/// 判断输入是订阅网址还是节点链接。
///
/// 只做**分流**，不做解析 —— 解析是内核的活，自己写就是造轮子 + 永远追不上新协议。
/// 实测（mihomo v1.19.30）：`type: file` provider 能吃原始链接、能吃 base64、
/// 能吃五种协议混排，全部正确解析，所以这里只要别误杀就行。
fn classify_subscription(input: &str) -> Result<NodeSource, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("请先贴上订阅网址或节点链接。".to_string());
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return Ok(NodeSource::Url(s.to_string()));
    }

    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();

    if lines.iter().all(|l| {
        let low = l.to_ascii_lowercase();
        LINK_SCHEMES.iter().any(|p| low.starts_with(*p))
    }) {
        return Ok(NodeSource::Links(lines.join("\n")));
    }

    // 一整坨看不懂的东西：很可能就是 base64 订阅内容。mihomo 自己会解，
    // 这里只负责放行，别把用户输入判死。
    // 判定直接复用扫描模块里那个 —— 它比"长得像 base64"更严格，
    // 会真的解一次并校验 UTF-8，误放行率更低。
    if lines.len() == 1 && crate::nodes::try_b64_decode(lines[0]).is_some() {
        return Ok(NodeSource::Links(lines[0].to_string()));
    }

    Err("看不出这是订阅网址还是节点链接。\n\
         订阅网址要以 https:// 开头；\n\
         节点链接要以 vless:// / vmess:// / ss:// / trojan:// 这类开头，多条就一行一条；\n\
         也可以直接把整段 base64 订阅内容贴进来。"
        .to_string())
}

/// 订阅网址：http provider，内核自己去拉
fn http_provider(url: &str) -> String {
    format!(
        "proxy-providers:\n  subscription:\n    type: http\n    url: \"{url}\"\n    \
         interval: 86400\n    path: ./subscription.yaml\n    health-check:\n      \
         enable: true\n      url: https://www.gstatic.com/generate_204\n      interval: 300\n"
    )
}

/// 节点链接：file provider，读我们刚落盘的那份文本
fn file_provider(path: &std::path::Path) -> String {
    // 路径里的反斜杠在 YAML 双引号里是**转义符**（`\U`、`\n` …），
    // `C:\Users\...` 会被吃成 `C:Users...`，表现为"链接贴进去了却零节点"。
    // 统一成正斜杠 + 单引号，两个坑一起绕开（单引号里反斜杠才是字面量）。
    let p = path.to_string_lossy().replace('\\', "/");
    format!(
        "proxy-providers:\n  subscription:\n    type: file\n    path: '{p}'\n    \
         health-check:\n      enable: true\n      \
         url: https://www.gstatic.com/generate_204\n      interval: 300\n"
    )
}

/// 订阅配置 —— 关键点：**一行协议解析都不写**。
///
/// mihomo 原生支持 `proxy-providers` 直接吃订阅 URL 或节点文件，自己解析
/// vless / vmess / ss / trojan / hysteria2 / tuic，还自带健康检查。
/// 自己写解析器 = 重复造轮子 + 永远追不上新协议 + 解析错就是连不上。
///
/// 规则按台湾用户调过：`GEOIP,TW,DIRECT` 让 PTT / 露天 / 蝦皮 / 各家网银
/// 走直连 —— 这些站点绕一圈出去反而更慢，有些网银还会因为异地登录被挡。
fn subscription_config(provider: &str, mixed: u16, api: u16) -> String {
    format!(
        r#"# ghboost 生成 · 由 mihomo 内核直接使用
mixed-port: {mixed}
allow-lan: false
bind-address: '*'
mode: rule
log-level: info
external-controller: 127.0.0.1:{api}

dns:
  enable: true
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  nameserver:
    - https://1.1.1.1/dns-query
    - https://dns.google/dns-query
  fallback:
    - https://dns.alidns.com/dns-query
    - https://doh.pub/dns-query
  fallback-filter:
    geoip: true
    geoip-code: TW

{provider}
proxy-groups:
  - name: "節點選擇"
    type: select
    use:
      - subscription
  - name: "手動切換"
    type: select
    proxies:
      - DIRECT
      - "節點選擇"

rules:
  - GEOIP,TW,DIRECT
  - GEOIP,LAN,DIRECT
  - MATCH,節點選擇
"#
    )
}

/// 问内核要"到底认出几个节点"。
///
/// 没有这一步，链接填错的故障形态是**代理开了但全都连不上**，
/// 用户只会以为软件坏了。宁可多等两秒，也要把错误说清楚。
fn provider_node_count(api_port: u16) -> usize {
    let url = format!("http://127.0.0.1:{api_port}/providers/proxies/subscription");
    let Ok(resp) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .and_then(|c| c.get(&url).send())
    else {
        return 0;
    };
    let Ok(v) = resp.json::<serde_json::Value>() else {
        return 0;
    };
    v.get("proxies")
        .and_then(|p| p.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

/// 轮询内核直到节点解析出来（或超时）。返回 (节点数, 等待毫秒)。
fn wait_for_nodes(api_port: u16, budget: Duration) -> usize {
    let step = Duration::from_millis(300);
    let mut waited = Duration::ZERO;
    loop {
        let n = provider_node_count(api_port);
        if n > 0 || waited >= budget {
            return n;
        }
        std::thread::sleep(step);
        waited += step;
    }
}

#[derive(serde::Deserialize)]
struct SubscribeRequest {
    url: String,
    #[serde(default = "default_mixed")]
    mixed_port: u16,
}

fn default_mixed() -> u16 {
    7890
}

/// 导入订阅并启动内核 + 打开系统代理。
async fn api_proxy_subscribe(Json(req): Json<SubscribeRequest>) -> Json<Value> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(do_subscribe(&req.url, req.mixed_port));
    });
    Json(match rx.await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(_) => serde_json::json!({ "ok": false, "error": "内核线程异常退出" }),
    })
}

fn do_subscribe(input: &str, mixed_port: u16) -> Result<Value, String> {
    use crate::mihomo::{MihomoConfig, MihomoManager};

    let source = classify_subscription(input)?;

    let api_port = mixed_port + 10;
    let cfg = MihomoConfig {
        // 显式指定随包/随安装脚本放好的内核，别去 PATH 里碰运气。
        // 路径固定，缺了就给一句能照抄的命令，绝不静默失败。
        binary_path: Some(kernel_path()),
        config_dir: Some(ghboost_dir().join("mihomo")),
        http_port: mixed_port,
        socks_port: mixed_port,
        mixed_port,
        api_port,
        allow_lan: false,
        log_level: "info".to_string(),
    };

    if !kernel_path().exists() {
        // 内核现在是随发行包附上的，所以这里的处置不再是「去 GitHub 下载」——
        // 那对目标用户（正是连 GitHub 都不顺的人）是自相矛盾的。
        return Err("找不到内置内核。请重新执行一次安装包里的 install.bat；\
             若仍失败，确认安装目录下存在 bin\\mihomo.exe 与 mihomo\\country.mmdb。"
            .to_string());
    }

    // 先探端口：7890 极可能被 Clash Verge / v2rayN 占着。
    // 不先说清楚的话，用户只看到一句"Mihomo 启动后立即退出"，完全不知道该怎么办。
    if std::net::TcpListener::bind(("127.0.0.1", mixed_port)).is_err() {
        return Err(format!(
            "端口 {mixed_port} 已被占用。\n\
             如果你已经在跑 Clash Verge / v2rayN，直接用「用现成代理」填它的端口即可；\n\
             想用内置内核就换一个端口（比如 {}）重试。",
            mixed_port + 100
        ));
    }

    let mut slot = mihomo_slot().lock().map_err(|e| e.to_string())?;
    // 端口变了就必须换一个新的 manager：manager 的 api_port 是创建时定死的，
    // 沿用旧的会把 reload 请求打到旧端口，而新配置声明的是新端口 ——
    // 表现为"面板上改了端口再订阅，节点数变成 0"（面板端口是用户可改的）。
    let needs_new = match slot.as_ref() {
        None => true,
        Some(m) => m.mixed_port() != mixed_port,
    };
    if needs_new {
        if let Some(old) = slot.take() {
            let _ = old.stop();
        }
        *slot = Some(MihomoManager::new(cfg));
    }
    let mgr = slot.as_mut().unwrap();

    // 顺序固定：start() 内部会 generate_config() 写一份默认配置，
    // 所以必须先启动、再覆写、再 reload。
    mgr.start()?;

    // file provider 要读的那份文本，和配置放在同一个目录（内核的 -d 就是这里）。
    let is_links = matches!(source, NodeSource::Links(_));
    let provider = match &source {
        NodeSource::Url(u) => http_provider(u),
        NodeSource::Links(text) => {
            let path = mgr
                .config_path()
                .parent()
                .map(|d| d.join("nodes.txt"))
                .ok_or_else(|| "配置文件路径异常".to_string())?;
            std::fs::write(&path, format!("{text}\n"))
                .map_err(|e| format!("写入节点文件失败: {e}"))?;
            file_provider(&path)
        }
    };

    std::fs::write(
        mgr.config_path(),
        subscription_config(&provider, mixed_port, api_port),
    )
    .map_err(|e| format!("写入订阅配置失败: {e}"))?;
    mgr.reload_config()?;

    // 等内核把节点解析出来，顺便把"链接贴错了"变成一句人话。
    // file provider 是同步解析的，给短预算；http 要真去联网拉，给长一点。
    let nodes = wait_for_nodes(
        api_port,
        if is_links {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(12)
        },
    );
    if nodes == 0 && is_links {
        // 链接是**同步**解析的，0 个就说明输入一定有问题。
        // 关键：必须把内核和系统代理一起收掉。留着的话系统代理指向一个没有节点的
        // 内核，故障形态是"点了开启代理以后整个网都断了"—— 比没开还糟。
        if let Some(m) = slot.as_mut() {
            let _ = m.stop();
        }
        *slot = None;
        let _ = crate::proxy::unset_proxy();
        return Err("内核没能认出这些链接里的任何节点。\n\
             请确认整条链接是完整的（从 vless:// 一路到 #备注都要复制到）。\n\
             一次贴了很多条的话，先只贴一条试试 —— \n\
             只要有一条格式坏掉，内核会把整批都丢掉（不是跳过那一条）。"
            .to_string());
    }
    if nodes == 0 {
        // 网址的情况不判死：可能是对方服务器慢，也可能订阅要带特殊 header。
        // 内核留着（也许过会儿就拉到了），但**不接管系统代理** ——
        // 接管了就是把用户所有流量丢进黑洞。
        return Ok(serde_json::json!({
            "ok": true,
            "mixed_port": mixed_port,
            "api_port": api_port,
            "nodes": 0,
            "proxy_ok": false,
            "system_proxy": "未接管：这个订阅网址一个节点都没拉到。\
                             它可能要浏览器才能打开，或已经失效。",
        }));
    }

    // 内核起来了就算成功 —— 系统代理是"顺手帮你开"，
    // 它失败（比如组策略禁改代理）不该把整次订阅导入判成失败。
    let (proxy_ok, proxy_state) = match crate::proxy::set_proxy(&crate::proxy::ProxyConfig {
        host: "127.0.0.1".into(),
        port: mixed_port,
        socks_port: Some(mixed_port),
        bypass: "localhost,127.0.0.1,::1,<local>".into(),
    }) {
        Ok(s) => (true, format!("{s:?}")),
        Err(e) => (false, e),
    };

    Ok(serde_json::json!({
        "ok": true,
        "mixed_port": mixed_port,
        "api_port": api_port,
        "nodes": nodes,
        "proxy_ok": proxy_ok,
        "system_proxy": proxy_state,
    }))
}

/// 停内核 + 关系统代理
async fn api_proxy_stop() -> Json<Value> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(do_stop());
    });
    Json(match rx.await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(_) => serde_json::json!({ "ok": false, "error": "内核线程异常退出" }),
    })
}

fn do_stop() -> Result<Value, String> {
    let mut slot = mihomo_slot().lock().map_err(|e| e.to_string())?;
    if let Some(mgr) = slot.as_mut() {
        let _ = mgr.stop();
    }
    *slot = None;
    let state = crate::proxy::unset_proxy()?;
    Ok(serde_json::json!({ "ok": true, "system_proxy": format!("{state:?}") }))
}

/// 内核 + 系统代理的合并状态（前端据此决定按钮显示"开启"还是"关闭"）
async fn api_proxy_state() -> Json<Value> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let (kernel, mixed) = match mihomo_slot().lock() {
            Ok(slot) => match slot.as_ref() {
                Some(mgr) => match mgr.get_status() {
                    Ok(s) => (s.running, s.mixed_port),
                    Err(_) => (false, 0),
                },
                None => (false, 0),
            },
            Err(_) => (false, 0),
        };
        let _ = tx.send((kernel, mixed, crate::proxy::get_proxy_status()));
    });
    let (kernel, mixed, system) =
        rx.await
            .unwrap_or((false, 0, crate::proxy::ProxyState::Disabled));
    Json(serde_json::json!({
        "kernel": kernel,
        "mixed_port": mixed,
        "system": format!("{system:?}"),
    }))
}

/// 一键检测的默认站点 —— 用户点完"加速"最关心的三个。
const CHECK_SITES: &[(&str, &str)] = &[
    ("Google", "https://google.hk/"),
    ("YouTube", "https://www.youtube.com/"),
    ("GitHub", "https://github.com/"),
];

/// 连通性检测：DNS → TCP → HTTPS，返回状态码与耗时。
///
/// 放在**裸 std 线程**里跑（和 `/run` 同一个理由）：`reqwest::blocking` 会自建
/// runtime，在带 tokio 上下文的线程里建 runtime 有踩坑风险。
/// 用 `oneshot` 把结果传回 async，避免任何 block_on。
async fn api_check() -> Json<Value> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            CHECK_SITES
                .iter()
                .map(|(name, url)| check_one(name, url))
                .collect::<Vec<_>>(),
        );
    });
    let sites = rx.await.unwrap_or_default();
    Json(serde_json::json!({ "sites": sites }))
}

fn check_one(name: &str, url: &str) -> Value {
    use std::net::ToSocketAddrs;

    let host = url
        .split("//")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("");
    // DNS 解析：拿到 IP 才知道加速到底生效没有（hosts 生效 = IP 变成我们写的那个）
    let ip = (host, 443u16)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
        .map(|s| s.ip().to_string());

    let t0 = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        // 忽略系统代理：我们要测的是**裸连**，不是经过 Clash 之后的假象
        .no_proxy()
        .build();

    let mut out = match client {
        Err(e) => serde_json::json!({ "site": name, "ok": false, "error": e.to_string() }),
        Ok(c) => match c.get(url).send() {
            Ok(r) => {
                let status = r.status().as_u16();
                serde_json::json!({
                    "site": name,
                    "ok": status < 500,
                    "http": status,
                    "ms": t0.elapsed().as_millis() as u64,
                })
            }
            Err(e) => serde_json::json!({
                "site": name,
                "ok": false,
                "error": e.to_string(),
                "ms": t0.elapsed().as_millis() as u64,
            }),
        },
    };
    if let Some(obj) = out.as_object_mut() {
        if let Some(ip) = ip {
            obj.insert("ip".into(), Value::String(ip));
        }
    }
    out
}

/// 会话 id，够随机即可（只在本机回环用，不是安全边界）
fn rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 这些用例锁的是「用户贴什么都能被正确分流」。
    // 之前 README 和面板都写着"vless / vmess / ss / trojan 都行"，
    // 代码却硬性要求 http(s) 开头 —— 测试和实现得一起对上，不然文档就是假的。

    #[test]
    fn 订阅网址走_http_provider() {
        match classify_subscription("https://sub.example.com/a?b=1").unwrap() {
            NodeSource::Url(u) => assert_eq!(u, "https://sub.example.com/a?b=1"),
            other => panic!("应该判成网址，实际: {}", kind(&other)),
        }
    }

    #[test]
    fn 单条节点链接要认() {
        // 台湾用户最典型的形态：机场直接给一条 vless 链接，没有订阅网址
        let s = "vless://b5f6bc1e-6f4a-4d0e-9c1a-1f2e3d4c5b6a@1.2.3.4:443?type=ws#HK-01";
        match classify_subscription(s).unwrap() {
            NodeSource::Links(t) => assert!(t.starts_with("vless://")),
            other => panic!("应该判成节点链接，实际: {}", kind(&other)),
        }
    }

    #[test]
    fn 多条混协议链接要认() {
        let input =
            "vless://a@b:443#V\nvmess://eyJ2IjoiMiJ9\nss://YWVzLTEyOC1nY206cGFzcw==@e.com:8388#S";
        match classify_subscription(input).unwrap() {
            NodeSource::Links(t) => assert_eq!(t.lines().count(), 3),
            other => panic!("应该判成节点链接，实际: {}", kind(&other)),
        }
    }

    #[test]
    fn 整段_base64_要认() {
        // 一串看起来完全不像链接的东西：base64 字母表里没有 ':'
        // 注意必须带正确的 padding —— 判定用的是 STANDARD 解码，不是"长得像"
        let blob = "dmxlc3M6Ly9iNWY2YmMxZS02ZjRhLTRkMGUtOWMxYS0xZjJlM2Q0YzViNmFAMS4yLjMuNDo0NDM/\
                    dHlwZT13cyNISy0wMQ==";
        match classify_subscription(blob).unwrap() {
            NodeSource::Links(t) => assert_eq!(t, blob),
            other => panic!("应该判成节点链接，实际: {}", kind(&other)),
        }
    }

    #[test]
    fn 看不懂的输入要报错而不是放行() {
        for bad in ["随便打几个字", "ftp://example.com/a", "12345"] {
            assert!(
                classify_subscription(bad).is_err(),
                "{bad} 应该被拒绝 —— 放行只会让内核解析出 0 个节点，用户无从判断哪里错了"
            );
        }
        assert!(classify_subscription("   ").is_err());
    }

    #[test]
    fn 节点文件路径不能被转义吃掉() {
        let cfg = subscription_config(
            &file_provider(std::path::Path::new(r"C:\Users\me\nodes.txt")),
            7890,
            7900,
        );
        // 反斜杠配双引号 = YAML 转义序列，C:\Users 会被吃成 C:Users，
        // 表现是"链接贴进去了却零节点"。这里必须看到正斜杠 + 单引号。
        assert!(
            cfg.contains("path: 'C:/Users/me/nodes.txt'"),
            "实际配置:\n{cfg}"
        );
        assert!(cfg.contains("type: file"));
    }

    fn kind(s: &NodeSource) -> &'static str {
        match s {
            NodeSource::Url(_) => "Url",
            NodeSource::Links(_) => "Links",
        }
    }
}
