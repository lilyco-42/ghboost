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
    let state = Arc::new(State {
        sessions: Arc::new(Mutex::new(HashMap::new())),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/run", post(run_handler))
        .route("/progress/{id}", get(progress_handler))
        .route("/proxy/set", post(proxy_set))
        .route("/proxy/unset", post(proxy_unset))
        .route("/proxy/status", get(proxy_status))
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
async fn index() -> Html<String> {
    // 必须在最前面执行，早于页面内联脚本解析
    const SHIM: &str = "<script>window.__GH_HTTP__=1;</script>\n";
    Html(format!("{SHIM}{}", include_str!("gui.html")))
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
    if ok {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, "Forbidden").into_response()
    }
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

/// 会话 id，够随机即可（只在本机回环用，不是安全边界）
fn rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}
