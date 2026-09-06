//! ghboost — GitHub 访问加速 + 免费节点扫描/测速/注入（lilyco 框架 bin）
//!
//! 核心逻辑全部在 `ghboost` lib crate（`hosts` / `nodes` 模块），本文件只负责：
//! - 用 lilyco 把命令暴露为 CLI / TUI / Web / MCP；
//! - 把 lilyco `App` 参数转换成 lib 的 `*Params`；
//! - 通过 sink 把 lib 事件回传给 lilyco 的进度/日志。

use std::path::PathBuf;

use lilyco::prelude::*;

use ghboost::deploy;
use ghboost::hosts;
use ghboost::nodes;
use ghboost::proxy;
use ghboost::webview::{self, WebView, HINT_NONE};
use ghboost::{run_blocking, Event, Level};

/// GitHub 访问加速 — 优选 IP 并写入 hosts
#[derive(App)]
#[app(run = "boost")]
struct GhBoost {
    /// 单个 IP 测速超时（毫秒）
    #[arg(default = 3000, range = 200..=15000)]
    timeout_ms: u64,

    /// 并发测速数（过高会导致大量连接失败，实测 16 较稳）
    #[arg(default = 16, range = 1..=64)]
    concurrency: u64,

    /// 每个域名保留的最优 IP 数
    #[arg(default = 1, range = 1..=5)]
    top: u64,

    /// 额外候选 IP（可多次指定）
    extra_ip: Option<Vec<String>>,

    /// 只处理指定域名（可多次指定，默认全部）
    only: Option<Vec<String>>,

    /// 写入系统 hosts（Windows 需管理员权限）
    apply: bool,

    /// 清理 ghboost 已写入的 hosts 条目
    clean: bool,
}

fn boost(app: &GhBoost, ctx: &Context) -> Result<serde_json::Value, AppError> {
    let bp = hosts::BoostParams {
        timeout_ms: app.timeout_ms,
        concurrency: app.concurrency,
        top: app.top,
        extra_ip: app.extra_ip.clone(),
        only: app.only.clone(),
        apply: app.apply,
        clean: app.clean,
    };
    let sink = make_sink(ctx);
    run_blocking(hosts::boost_core(bp, &sink)).map_err(AppError::Runtime)
}

/// 节点自动扫描
#[derive(App)]
#[app(
    name = "scan",
    about = "自动扫描 free-VPN 等订阅源，解析去重导出节点库",
    run = "run_scan"
)]
struct Scan {
    /// 额外订阅源 URL（可多次指定，与仓库索引合并）
    source: Option<Vec<String>>,
    /// 是否也扫描 free-VPN 仓库 README 索引（默认开）
    #[arg(default = true)]
    include_repo: bool,
    /// 最多处理的订阅源数（源很多，限量避免过慢）
    #[arg(default = 60, range = 1..=200)]
    max_sources: u64,
    /// 并发拉取数
    #[arg(default = 16, range = 1..=64)]
    concurrency: u64,
    /// 单源最多取前 N 行节点（避免巨型源卡死）
    #[arg(default = 500, range = 1..=5000)]
    per_limit: u64,
    /// 节点库数据目录（默认 ./nodes_data）
    #[arg(default = "nodes_data")]
    output: PathBuf,
}

fn run_scan(app: &Scan, ctx: &Context) -> Result<serde_json::Value, AppError> {
    let sp = nodes::ScanParams {
        source: app.source.clone(),
        include_repo: app.include_repo,
        max_sources: app.max_sources,
        concurrency: app.concurrency,
        per_limit: app.per_limit,
        output: app.output.clone(),
    };
    let sink = make_sink(ctx);
    run_blocking(nodes::scan_core(sp, &sink)).map_err(AppError::Runtime)
}

/// 节点测试（mihomo 内核）
#[derive(App)]
#[app(
    name = "test",
    about = "启动独立 Mihomo 实例，对扫描出的节点做真实延迟测试",
    run = "run_test"
)]
struct Test {
    /// 节点库数据目录（默认 ./nodes_data，需先 scan）
    #[arg(default = "nodes_data")]
    input: PathBuf,
    /// 最多测试节点数（太多 mihomo 启动慢，默认 300）
    #[arg(default = 300, range = 10..=3000)]
    top: u64,
    /// 测速并发
    #[arg(default = 32, range = 1..=128)]
    concurrency: u64,
    /// 单个节点测速超时（毫秒）
    #[arg(default = 8000, range = 1000..=30000)]
    timeout_ms: u64,
    /// 测速用的探测 URL（代表能否访问墙外）
    #[arg(default = "https://www.gstatic.com/generate_204")]
    test_url: String,
    /// mihomo 二进制路径（默认自动探测）
    mihomo: Option<PathBuf>,
}

fn run_test(app: &Test, ctx: &Context) -> Result<serde_json::Value, AppError> {
    let tp = nodes::TestParams {
        input: app.input.clone(),
        top: app.top,
        concurrency: app.concurrency,
        timeout_ms: app.timeout_ms,
        test_url: app.test_url.clone(),
        mihomo: app.mihomo.clone(),
    };
    let sink = make_sink(ctx);
    run_blocking(nodes::test_core(tp, &sink)).map_err(AppError::Runtime)
}

/// 节点导出 / 注入
#[derive(App)]
#[app(
    name = "add",
    about = "把测过的可用节点导出订阅，或 --apply 注入 Clash Verge",
    run = "run_add"
)]
struct Add {
    /// 测试结果数据目录（默认 ./nodes_data）
    #[arg(default = "nodes_data")]
    input: PathBuf,
    /// 保留延迟最低的 N 个节点
    #[arg(default = 20, range = 1..=1000)]
    keep: u64,
    /// 延迟上限（毫秒），超过的丢弃；0=不限
    #[arg(default = 0, range = 0..=60000)]
    max_ms: u64,
    /// 写入用户当前激活的 local profile（带备份）。默认只导出
    apply: bool,
    /// 目标 profile 路径（默认自动定位当前激活的 Clash Verge local profile）
    profile: Option<PathBuf>,
}

fn run_add(app: &Add, ctx: &Context) -> Result<serde_json::Value, AppError> {
    let ap = nodes::AddParams {
        input: app.input.clone(),
        keep: app.keep,
        max_ms: app.max_ms,
        apply: app.apply,
        profile: app.profile.clone(),
    };
    let sink = make_sink(ctx);
    run_blocking(nodes::add_core(ap, &sink)).map_err(AppError::Runtime)
}

/// 一键部署服务器（支持多种协议）
#[derive(App)]
#[app(
    name = "deploy",
    about = "一键部署代理服务器（VLESS-Reality / VLESS-WS / Trojan / Shadowsocks / Hysteria2）",
    run = "run_deploy"
)]
struct Deploy {
    /// 服务器 IP 地址
    host: String,
    /// SSH 端口（默认 22）
    #[arg(default = 22)]
    port: u16,
    /// SSH 用户名（默认 root）
    #[arg(default = "root")]
    user: String,
    /// SSH 密码（可选，优先使用密钥）
    password: Option<String>,
    /// SSH 私钥路径（可选）
    key_path: Option<PathBuf>,
    /// 协议类型：vless-reality, vless-ws, trojan, shadowsocks, hysteria2
    #[arg(default = "vless-reality")]
    protocol: String,
    /// 服务端口（不填则自动分配）
    port_out: Option<u16>,
    /// 域名（Reality/TLS 需要，默认 www.microsoft.com）
    domain: Option<String>,
    /// 不安装 BBR 加速
    no_bbr: bool,
    /// 不配置防火墙
    no_firewall: bool,
}

fn run_deploy(app: &Deploy, _ctx: &Context) -> Result<serde_json::Value, AppError> {
    let protocol = match app.protocol.as_str() {
        "vless-reality" => deploy::Protocol::VlessReality,
        "vless-ws" => deploy::Protocol::VlessWs,
        "trojan" => deploy::Protocol::Trojan,
        "shadowsocks" => deploy::Protocol::Shadowsocks,
        "hysteria2" => deploy::Protocol::Hysteria2,
        _ => {
            return Err(AppError::Runtime(format!(
                "不支持的协议: {}。支持: vless-reality, vless-ws, trojan, shadowsocks, hysteria2",
                app.protocol
            )))
        }
    };

    let dp = deploy::DeployParams {
        host: app.host.clone(),
        port: app.port,
        user: app.user.clone(),
        password: app.password.clone(),
        key_path: app.key_path.clone(),
        protocol,
        port_out: app.port_out,
        domain: app.domain.clone(),
        install_bbr: !app.no_bbr,
        configure_firewall: !app.no_firewall,
    };

    run_blocking(deploy::deploy_core(dp))
        .map(|r| serde_json::to_value(&r).unwrap_or(serde_json::Value::Null))
        .map_err(AppError::Runtime)
}

/// 把 lib 的 `Event` 映射回 lilyco 的进度/日志
fn make_sink<'a>(ctx: &'a Context) -> impl Fn(&Event) + 'a {
    move |e: &Event| match e {
        Event::Started { total, message } => {
            ctx.emit(Progress::Started {
                total: *total,
                message: message.clone(),
            });
        }
        Event::Tick {
            current,
            total,
            message,
        } => {
            ctx.tick(*current, *total, message.clone());
        }
        Event::Log { level, message } => {
            let lvl = match level {
                Level::Info => LogLevel::Info,
                Level::Warn => LogLevel::Warn,
                Level::Error => LogLevel::Error,
            };
            ctx.log(lvl, message.clone());
        }
        Event::Done { output, elapsed_ms } => {
            ctx.done(output.clone(), *elapsed_ms);
        }
    }
}

/// 启动原生 WebView GUI（无外部浏览器依赖）
fn launch_gui(_registry: Registry) {
    let mut wv = WebView::new(cfg!(debug_assertions))
        .expect("WebView 创建失败 — Windows 需安装 WebView2 运行时");

    wv.set_title("ghboost").unwrap();
    wv.set_size(900, 600, HINT_NONE).unwrap();

    // 加载内嵌 HTML
    let html = include_str!("gui.html");
    wv.set_html(html).unwrap();

    // 绑定 run_command：JS → Rust 桥接
    // JS 调用: window.run_command(JSON.stringify({cmd:"boost", args:{...}}))
    wv.bind("run_command", |_id: String, req: String| {
        // req 是 JSON 数组包裹的字符串参数，解析第一个元素
        let payload: serde_json::Value =
            serde_json::from_str(&req).unwrap_or(serde_json::Value::Null);
        // webview_bind 的 req 格式是 JSON 数组 ["string"]，取第一个
        let cmd_json = payload
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let parsed: serde_json::Value =
            serde_json::from_str(cmd_json).unwrap_or(serde_json::Value::Null);
        let cmd = parsed["cmd"].as_str().unwrap_or("").to_string();
        let args = parsed["args"].clone();

        // 立即返回 "started"，实际工作在后台线程
        std::thread::spawn(move || {
            let result = execute_command(&cmd, &args);
            match result {
                Ok(val) => {
                    let json_str = serde_json::to_string(&val).unwrap_or_default();
                    let elapsed = 0u64; // TODO: track real elapsed
                    let _ = webview::eval_global(&format!(
                        "push_done({},{})",
                        serde_json::json!(json_str),
                        elapsed
                    ));
                }
                Err(e) => {
                    let _ = webview::eval_global(&format!("push_error({})", serde_json::json!(e)));
                }
            }
        });

        // 立即 resolve JS promise
        serde_json::json!({"status": "started"}).to_string()
    })
    .unwrap();

    // 绑定 set_proxy / unset_proxy：JS → Rust 系统代理控制
    wv.bind("set_proxy", |_id: String, req: String| {
        let payload: serde_json::Value =
            serde_json::from_str(&req).unwrap_or(serde_json::Value::Null);
        let params = payload
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let config: proxy::ProxyConfig = if params.is_empty() {
            proxy::ProxyConfig::default()
        } else {
            serde_json::from_str(params).unwrap_or_default()
        };

        match proxy::set_proxy(&config) {
            Ok(state) => {
                let _ = webview::eval_global(&format!(
                    "proxy_set_result({})",
                    serde_json::json!({"ok": true, "state": format!("{:?}", state)})
                ));
            }
            Err(e) => {
                let _ = webview::eval_global(&format!(
                    "proxy_set_result({})",
                    serde_json::json!({"ok": false, "error": e})
                ));
            }
        }
        "".to_string()
    })
    .unwrap();

    wv.bind("unset_proxy", |_id: String, _req: String| {
        match proxy::unset_proxy() {
            Ok(state) => {
                let _ = webview::eval_global(&format!(
                    "proxy_set_result({})",
                    serde_json::json!({"ok": true, "state": format!("{:?}", state)})
                ));
            }
            Err(e) => {
                let _ = webview::eval_global(&format!(
                    "proxy_set_result({})",
                    serde_json::json!({"ok": false, "error": e})
                ));
            }
        }
        "".to_string()
    })
    .unwrap();

    wv.bind("get_proxy_status", |_id: String, _req: String| {
        let state = proxy::get_proxy_status();
        let _ = webview::eval_global(&format!(
            "proxy_status_result({})",
            serde_json::json!({"state": format!("{:?}", state)})
        ));
        "".to_string()
    })
    .unwrap();

    // 启动消息循环（阻塞直到窗口关闭）
    wv.run().expect("WebView 运行失败");
}

/// 根据命令名分发到对应的 core 函数
fn execute_command(cmd: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    // 创建一个把事件推送到 WebView 的 sink
    let sink = |e: &Event| {
        let js = match e {
            Event::Started { total, message } => {
                let t = total.unwrap_or(0);
                let m = message.as_deref().unwrap_or("...");
                format!(
                    "push_log(\"info\",\"{}\")",
                    escape_js(&format!("started ({t} total): {m}"))
                )
            }
            Event::Tick {
                current,
                total,
                message,
            } => {
                format!(
                    "push_progress({},{},{})",
                    current,
                    total.unwrap_or(0),
                    serde_json::json!(message)
                )
            }
            Event::Log { level, message } => {
                let lvl = match level {
                    Level::Info => "info",
                    Level::Warn => "warn",
                    Level::Error => "error",
                };
                format!("push_log(\"{}\",{})", lvl, serde_json::json!(message))
            }
            Event::Done { output, elapsed_ms } => {
                let json_str = serde_json::to_string(output).unwrap_or_default();
                format!("push_done({},{})", serde_json::json!(json_str), elapsed_ms)
            }
        };
        let _ = webview::eval_global(&js);
    };

    match cmd {
        "boost" => {
            let bp = hosts::BoostParams {
                timeout_ms: args["timeout_ms"].as_u64().unwrap_or(3000),
                concurrency: args["concurrency"].as_u64().unwrap_or(16),
                top: args["top"].as_u64().unwrap_or(1),
                extra_ip: None,
                only: None,
                apply: args["apply"].as_bool().unwrap_or(false),
                clean: args["clean"].as_bool().unwrap_or(false),
            };
            run_blocking(hosts::boost_core(bp, &sink))
        }
        "scan" => {
            let sp = nodes::ScanParams {
                source: None,
                include_repo: true,
                max_sources: args["max_sources"].as_u64().unwrap_or(60),
                concurrency: args["concurrency"].as_u64().unwrap_or(16),
                per_limit: args["per_limit"].as_u64().unwrap_or(500),
                output: PathBuf::from(args["output"].as_str().unwrap_or("nodes_data")),
            };
            run_blocking(nodes::scan_core(sp, &sink))
        }
        "test" => {
            let tp = nodes::TestParams {
                input: PathBuf::from(args["input"].as_str().unwrap_or("nodes_data")),
                top: args["top"].as_u64().unwrap_or(300),
                concurrency: args["concurrency"].as_u64().unwrap_or(32),
                timeout_ms: args["timeout_ms"].as_u64().unwrap_or(8000),
                test_url: args["test_url"]
                    .as_str()
                    .unwrap_or("https://www.gstatic.com/generate_204")
                    .to_string(),
                mihomo: None,
            };
            run_blocking(nodes::test_core(tp, &sink))
        }
        "add" => {
            let ap = nodes::AddParams {
                input: PathBuf::from(args["input"].as_str().unwrap_or("nodes_data")),
                keep: args["keep"].as_u64().unwrap_or(20),
                max_ms: args["max_ms"].as_u64().unwrap_or(0),
                apply: args["apply"].as_bool().unwrap_or(false),
                profile: None,
            };
            run_blocking(nodes::add_core(ap, &sink))
        }
        "deploy" => {
            let protocol = match args["protocol"].as_str().unwrap_or("vless-reality") {
                "vless-reality" => deploy::Protocol::VlessReality,
                "vless-ws" => deploy::Protocol::VlessWs,
                "trojan" => deploy::Protocol::Trojan,
                "shadowsocks" => deploy::Protocol::Shadowsocks,
                "hysteria2" => deploy::Protocol::Hysteria2,
                _ => return Err("不支持的协议".to_string()),
            };
            let dp = deploy::DeployParams {
                host: args["host"].as_str().unwrap_or("").to_string(),
                port: args["port"].as_u64().unwrap_or(22) as u16,
                user: args["user"].as_str().unwrap_or("root").to_string(),
                password: args["password"].as_str().map(|s| s.to_string()),
                key_path: args["key_path"].as_str().map(PathBuf::from),
                protocol,
                port_out: args["port_out"].as_u64().map(|p| p as u16),
                domain: args["domain"].as_str().map(|s| s.to_string()),
                install_bbr: args["install_bbr"].as_bool().unwrap_or(true),
                configure_firewall: args["configure_firewall"].as_bool().unwrap_or(true),
            };
            run_blocking(deploy::deploy_core(dp))
                .map(|r| serde_json::to_value(&r).unwrap_or(serde_json::Value::Null))
        }
        _ => Err(format!("unknown command: {cmd}")),
    }
}

/// 转义字符串用于 JS 字面量
fn escape_js(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// 清掉 WorkBuddy 等环境注入的代理变量。
/// 否则 reqwest 默认走 `HTTPS_PROXY=127.0.0.1:55995` 假代理，直连被劫持；
/// 同时 mihomo 子进程也会继承该代理，测速结果失真。
fn clear_proxy_env() {
    for k in [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        std::env::remove_var(k);
    }
}

fn main() {
    clear_proxy_env();
    let args: Vec<String> = std::env::args().collect();
    let mut registry = Registry::new();
    registry
        .register(RegisteredCommand::from_app::<GhBoost>())
        .expect("注册 boost 失败");
    registry
        .register(RegisteredCommand::from_app::<Scan>())
        .expect("注册 scan 失败");
    registry
        .register(RegisteredCommand::from_app::<Test>())
        .expect("注册 test 失败");
    registry
        .register(RegisteredCommand::from_app::<Add>())
        .expect("注册 add 失败");
    registry
        .register(RegisteredCommand::from_app::<Deploy>())
        .expect("注册 deploy 失败");

    if args.iter().any(|a| a == "--mcp") {
        lilyco::serve_mcp(registry);
    } else if args.iter().any(|a| a == "--gui") {
        launch_gui(registry);
    } else if args.iter().any(|a| a == "--schema") {
        let schemas: Vec<_> = registry.visible().map(|c| &c.schema).collect();
        println!("{}", serde_json::to_string_pretty(&schemas).unwrap());
    } else {
        lilyco::run_cli_registry("ghboost", registry);
    }
}
