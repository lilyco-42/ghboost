//! 命令分发 + 事件序列化 —— 与传输层解耦
//!
//! GUI 有两个宿主（原生 WebView / 浏览器 Web 控制台），它们都必须执行同一套
//! core 逻辑、只是把进度事件送到不同地方。所以这里只认一个 `Fn(&Event)` sink：
//!
//! - WebView：`webview_sink()`（cfg feature=webview）
//! - Web 控制台：SSE，直接发 `event_to_json` 的结果
//! - CLI / TUI：lilyco 自己的 `ctx.tick/log/done`（走 `make_sink`，在 main.rs）

use std::path::PathBuf;

use crate::deploy;
use crate::hosts;
use crate::nodes;
use crate::{run_blocking, Event, Level};

/// 把 `Event` 序列化成与传输层无关的 JSON。
pub fn event_to_json(e: &Event) -> serde_json::Value {
    match e {
        Event::Started { total, message } => serde_json::json!({
            "type": "started",
            "total": total.unwrap_or(0),
            "message": message.as_deref().unwrap_or("..."),
        }),
        Event::Tick {
            current,
            total,
            message,
        } => serde_json::json!({
            "type": "tick",
            "current": current,
            "total": total.unwrap_or(0),
            "message": message,
        }),
        Event::Log { level, message } => serde_json::json!({
            "type": "log",
            "level": match level {
                Level::Info => "info",
                Level::Warn => "warn",
                Level::Error => "error",
            },
            "message": message,
        }),
        Event::Done { output, elapsed_ms } => serde_json::json!({
            "type": "done",
            "result": serde_json::to_string(output).unwrap_or_default(),
            "elapsed_ms": elapsed_ms,
        }),
    }
}

/// 根据命令名分发到对应的 core 函数。
///
/// `sink` 决定事件去哪里（WebView eval / SSE / 丢弃），本函数与传输层无关。
pub fn execute_command_with_sink(
    cmd: &str,
    args: &serde_json::Value,
    sink: &(dyn Fn(&Event) + Send + Sync),
) -> Result<serde_json::Value, String> {
    match cmd {
        "boost" => {
            let bp = hosts::BoostParams {
                timeout_ms: args["timeout_ms"].as_u64().unwrap_or(3000),
                concurrency: args["concurrency"].as_u64().unwrap_or(16),
                top: args["top"].as_u64().unwrap_or(1),
                extra_ip: None,
                // `only` 让调用方决定加速范围：CLI 默认不管（全部 GitHub 域名），
                // 桌面版要连同 Google / YouTube 一起加速就传这个数组。
                only: args["only"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .filter(|v| !v.is_empty()),
                apply: args["apply"].as_bool().unwrap_or(false),
                clean: args["clean"].as_bool().unwrap_or(false),
            };
            run_blocking(hosts::boost_core(bp, sink))
        }
        "scan" => {
            let sp = nodes::ScanParams {
                source: None,
                include_repo: true,
                max_sources: args["max_sources"].as_u64().unwrap_or(60),
                concurrency: args["concurrency"].as_u64().unwrap_or(16),
                per_limit: args["per_limit"].as_u64().unwrap_or(500),
                output: PathBuf::from(args["output"].as_str().unwrap_or("nodes_data")),
                // JSON 调用方显式传了 output 就尊重它（不可写则报错）
                output_explicit: args["output"].as_str().is_some(),
            };
            run_blocking(nodes::scan_core(sp, sink))
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
            run_blocking(nodes::test_core(tp, sink))
        }
        "add" => {
            let ap = nodes::AddParams {
                input: PathBuf::from(args["input"].as_str().unwrap_or("nodes_data")),
                keep: args["keep"].as_u64().unwrap_or(20),
                max_ms: args["max_ms"].as_u64().unwrap_or(0),
                apply: args["apply"].as_bool().unwrap_or(false),
                profile: None,
            };
            run_blocking(nodes::add_core(ap, sink))
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
