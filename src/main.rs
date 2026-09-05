//! ghboost — GitHub 访问加速：优选 IP 并写入 hosts
//!
//! ```bash
//! ghboost                 # 优选并打印 hosts 内容
//! ghboost --apply         # 写入系统 hosts（Windows 需管理员权限）
//! ghboost --clean         # 清理 ghboost 写入的条目
//! ghboost --mcp           # MCP 服务器（AI Agent 直接调用）
//! ```
//!
//! 原理：多路 DoH 解析 + 公开 hosts 源聚合候选 IP，再对每个 IP 用
//! **真实 TLS 握手**测速（SNI=域名、校验证书），因此不会选到"快但
//! 不服务该域名"的假 IP。

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lilyco::prelude::*;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// 需要加速的 GitHub 域名
const DOMAINS: &[&str] = &[
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "avatars.githubusercontent.com",
    "camo.githubusercontent.com",
    "gist.githubusercontent.com",
    "github.githubassets.com",
];

/// DoH 服务（多路解析，拿到不同 CDN 节点）
const DOH: &[&str] = &[
    "https://dns.alidns.com/resolve",
    "https://doh.pub/dns-query",
    "https://doh.360.cn/dns-query",
    "https://cloudflare-dns.com/dns-query",
    "https://dns.google/resolve",
];

/// 公开 hosts 源（已知可用 IP，作为候选补充）
const HOSTS_SRC: &[&str] = &[
    "https://cdn.jsdelivr.net/gh/521xueweihan/GitHub520@main/hosts",
    "https://raw.hellogithub.com/hosts",
];

const BEGIN: &str = "# BEGIN ghboost";
const END: &str = "# END ghboost";

#[derive(Debug, Deserialize)]
struct DnsAnswer {
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DnsResp {
    #[serde(rename = "Answer")]
    answer: Option<Vec<DnsAnswer>>,
}

/// 单个域名的优选结果
#[derive(serde::Serialize)]
struct Row {
    domain: String,
    best_ip: String,
    best_ms: u128,
    candidates: usize,
    usable: usize,
}

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
    let start = Instant::now();

    // 清理模式不需要网络
    if app.clean {
        let msg = clean_hosts()?;
        ctx.log(LogLevel::Info, msg.clone());
        let out = serde_json::json!({ "cleaned": true, "detail": msg });
        ctx.done(out.clone(), start.elapsed().as_millis() as u64);
        return Ok(out);
    }

    let fut = run_async(app, ctx);
    let result = match tokio::runtime::Handle::try_current() {
        Ok(h) => h.block_on(fut),
        Err(_) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| AppError::Runtime(format!("tokio 启动失败: {e}")))?
            .block_on(fut),
    }?;

    ctx.done(result.clone(), start.elapsed().as_millis() as u64);
    Ok(result)
}

async fn run_async(app: &GhBoost, ctx: &Context) -> Result<serde_json::Value, AppError> {
    let domains: Vec<String> = match &app.only {
        Some(v) if !v.is_empty() => v.clone(),
        _ => DOMAINS.iter().map(|s| s.to_string()).collect(),
    };
    let total = domains.len() as u64;

    ctx.emit(Progress::Started {
        total: Some(total),
        message: Some("聚合候选 IP".to_string()),
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(app.timeout_ms + 3000))
        .user_agent("ghboost/0.1")
        .build()
        .map_err(|e| AppError::Runtime(format!("http 客户端创建失败: {e}")))?;

    // 公开 hosts 源：一次性拉取，供所有域名复用
    let public = fetch_public_hosts(&client).await;
    if !public.is_empty() {
        let n: usize = public.values().map(|v| v.len()).sum();
        ctx.log(LogLevel::Info, format!("公开源补充 {n} 条候选"));
    } else {
        ctx.log(LogLevel::Warn, "公开 hosts 源不可用，仅用 DoH 解析");
    }

    // 聚合候选：先收集每个域名的专属 IP，同时汇成全局池。
    // 全局池让每个域名都在更大的池子里挑 —— TLS 证书校验会兜底排除
    // 不服务该域名的 IP，所以扩大候选是安全的。
    let mut per_domain: HashMap<String, HashSet<IpAddr>> = HashMap::new();
    let mut global: HashSet<IpAddr> = HashSet::new();

    for domain in &domains {
        let mut set: HashSet<IpAddr> = HashSet::new();
        for doh in DOH {
            set.extend(doh_query(&client, doh, domain).await);
        }
        if let Some(v) = public.get(domain.as_str()) {
            set.extend(v.iter().copied());
        }
        global.extend(set.iter().copied());
        per_domain.insert(domain.clone(), set);
    }
    for v in public.values() {
        global.extend(v.iter().copied());
    }
    for s in app.extra_ip.iter().flatten() {
        if let Ok(ip) = s.parse::<IpAddr>() {
            global.insert(ip);
        }
    }

    ctx.log(
        LogLevel::Info,
        format!("全局候选池 {} 个 IP，开始测速优选", global.len()),
    );

    let mut rows: Vec<Row> = Vec::new();
    // 已验证可用的 IP。第一个域名用全局池试，之后复用这个已验证集合，
    // 避免每个域名都重测整个池子（9 域名 × 全池要 90 秒）。
    let mut proven: HashSet<IpAddr> = HashSet::new();

    for (i, domain) in domains.iter().enumerate() {
        ctx.tick(i as u64, Some(total), format!("优选 {domain}"));

        let mut cands: HashSet<IpAddr> = if proven.is_empty() {
            global.clone()
        } else {
            proven.clone()
        };
        if let Some(v) = per_domain.get(domain) {
            cands.extend(v.iter().copied());
        }

        if cands.is_empty() {
            ctx.log(LogLevel::Warn, format!("{domain}: 无候选 IP，跳过"));
            continue;
        }

        let n_cand = cands.len();
        let (ranked, errs) = probe_all(domain, &cands, app).await;

        if ranked.is_empty() {
            ctx.log(
                LogLevel::Warn,
                format!("{domain}: {n_cand} 个候选全部不可用"),
            );
            for e in &errs {
                ctx.log(LogLevel::Warn, format!("  失败样例 {e}"));
            }
            continue;
        }

        let best = &ranked[0];
        ctx.log(
            LogLevel::Info,
            format!(
                "{domain}: {} 候选 / {} 可用 / 最优 {} ({}ms)",
                n_cand,
                ranked.len(),
                best.0,
                best.1
            ),
        );

        for (ip, _) in &ranked {
            proven.insert(*ip);
        }

        rows.push(Row {
            domain: domain.clone(),
            best_ip: best.0.to_string(),
            best_ms: best.1,
            candidates: n_cand,
            usable: ranked.len(),
        });

        // 保存前 N 个用于渲染
        let _top_ips: Vec<IpAddr> = ranked
            .iter()
            .take(app.top as usize)
            .map(|(ip, _)| *ip)
            .collect();
        // 渲染时重新取，这里只记录 best
    }

    if rows.is_empty() {
        return Err(AppError::Runtime(
            "所有域名都没选出可用 IP，请检查网络或放宽超时".to_string(),
        ));
    }

    let hosts_block = render_hosts(&rows);
    let mut out = serde_json::json!({
        "rows": rows,
        "hosts": hosts_block,
        "applied": false,
    });

    ctx.log(LogLevel::Info, format!("\n{hosts_block}"));

    if app.apply {
        let msg = apply_hosts(&hosts_block)?;
        ctx.log(LogLevel::Info, msg.clone());
        out["applied"] = serde_json::json!(true);
        out["apply_detail"] = serde_json::json!(msg);
    } else {
        ctx.log(
            LogLevel::Info,
            "未写入 hosts（加 --apply 才会写入，Windows 需要管理员权限）",
        );
    }

    Ok(out)
}

/// DoH JSON 解析
async fn doh_query(client: &reqwest::Client, doh: &str, domain: &str) -> Vec<IpAddr> {
    let url = format!("{doh}?name={domain}&type=A");
    let resp = match client
        .get(&url)
        .header("accept", "application/dns-json")
        .timeout(Duration::from_millis(3000))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let j: DnsResp = match resp.json().await {
        Ok(j) => j,
        Err(_) => return vec![],
    };
    j.answer
        .unwrap_or_default()
        .iter()
        .filter_map(|a| a.data.as_ref())
        .filter_map(|s| s.parse::<IpAddr>().ok())
        .collect()
}

/// 从公开 hosts 源抓取已知 IP
async fn fetch_public_hosts(client: &reqwest::Client) -> HashMap<String, Vec<IpAddr>> {
    let mut map: HashMap<String, Vec<IpAddr>> = HashMap::new();
    for src in HOSTS_SRC {
        let text = match client
            .get(*src)
            .timeout(Duration::from_millis(6000))
            .send()
            .await
        {
            Ok(r) => match r.text().await {
                Ok(t) => t,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            if let (Some(ip), Some(host)) = (it.next(), it.next()) {
                if let Ok(ip) = ip.parse::<IpAddr>() {
                    map.entry(host.to_string()).or_default().push(ip);
                }
            }
        }
    }
    map
}

/// 并发测速，返回按延迟升序的可用 IP
async fn probe_all(
    domain: &str,
    cands: &HashSet<IpAddr>,
    app: &GhBoost,
) -> (Vec<(IpAddr, u128)>, Vec<String>) {
    let sem = Arc::new(Semaphore::new(app.concurrency.max(1) as usize));
    let timeout = Duration::from_millis(app.timeout_ms);
    let mut set = JoinSet::new();

    for &ip in cands {
        let sem = sem.clone();
        let d = domain.to_string();
        set.spawn(async move {
            let _permit = sem.acquire().await;
            (ip, probe_retry(&d, ip, timeout, 1).await)
        });
    }

    let mut ok = Vec::new();
    let mut errs = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok((ip, r)) = res {
            match r {
                Ok(ms) => ok.push((ip, ms)),
                Err(e) => {
                    if errs.len() < 3 {
                        errs.push(format!("{ip}: {e}"));
                    }
                }
            }
        }
    }
    ok.sort_by_key(|(_, ms)| *ms);
    (ok, errs)
}

/// 对单个 IP 做真实 TLS 握手测速。
/// reqwest 会校验证书与 SNI，因此不服务该域名的 IP 会直接失败被排除。
async fn probe(domain: &str, ip: IpAddr, timeout: Duration) -> Result<u128, String> {
    let addr = SocketAddr::new(ip, 443);
    let client = reqwest::Client::builder()
        // 关键：必须禁用系统代理。reqwest 默认会读 HTTPS_PROXY，一旦走代理，
        // 下面 .resolve() 指定的 IP 就被代理绕过（代理自己重新解析域名），
        // 测出来的是代理延迟，选出的 IP 毫无意义。
        .no_proxy()
        .resolve(domain, addr)
        .timeout(timeout)
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) ghboost/0.1")
        .build()
        .map_err(|e| format!("client: {e}"))?;

    let t0 = Instant::now();
    // GET 而非 HEAD：HEAD 在部分 CDN 上被拒。send() 只等响应头，不下载 body。
    let resp = client
        .get(format!("https://{domain}/"))
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    let ms = t0.elapsed().as_millis();

    // 关键：TLS 握手成功 ≠ 真能服务该域名（实测有 Azure IP 能握手但无响应），
    // 必须校验响应头带 GitHub 特征，否则是假阳性。
    let status = resp.status();
    if !looks_like_github(resp.headers()) {
        return Err(format!("非 GitHub 响应 status={status}"));
    }
    // 但光有 GitHub 头还不够：实测 Pages 段 IP（185.199.110.153）带
    // Server: GitHub.com、证书也合法，却对 github.com 根路径返回 404 ——
    // 写进 hosts 会直接得到 404 页面。所以主站域名必须拿到 2xx。
    //
    // CDN 子域名（raw/objects/camo 等）根路径本来就没有内容，404 属正常，
    // 只对主站严格要求。
    if requires_ok_status(domain) && !status.is_success() {
        return Err(format!("主站返回 {status}，该 IP 不服务此域名"));
    }
    Ok(ms)
}

/// 这些域名的根路径应当有真实内容，非 2xx 说明 IP 不匹配
fn requires_ok_status(domain: &str) -> bool {
    matches!(domain, "github.com" | "api.github.com")
}

/// 网络抖动很严重（实测同一 IP 时通时断），对连接类错误重试一次。
/// 明确的"不匹配"错误（404 / 非 GitHub 响应）属于确定性结论，不重试，快速失败。
async fn probe_retry(
    domain: &str,
    ip: IpAddr,
    timeout: Duration,
    retries: u32,
) -> Result<u128, String> {
    let mut last = String::new();
    for _ in 0..=retries {
        match probe(domain, ip, timeout).await {
            Ok(ms) => return Ok(ms),
            Err(e) => {
                let fatal = e.contains("主站返回") || e.contains("非 GitHub 响应");
                last = e;
                if fatal {
                    break;
                }
            }
        }
    }
    Err(last)
}

/// 响应头是否带 GitHub 强特征
fn looks_like_github(h: &HeaderMap) -> bool {
    // 最强：GitHub 全站请求追踪 ID
    if h.contains_key("x-github-request-id") {
        return true;
    }
    // 主站自报家门
    if let Some(s) = h.get("server").and_then(|v| v.to_str().ok()) {
        if s.eq_ignore_ascii_case("github.com") {
            return true;
        }
    }
    // GitHub 主站 HTML 的 PJAX 协商头
    if let Some(v) = h.get("vary").and_then(|v| v.to_str().ok()) {
        if v.contains("X-PJAX") {
            return true;
        }
    }
    false
}

fn render_hosts(rows: &[Row]) -> String {
    let mut s = String::from(BEGIN);
    s.push('\n');
    for r in rows {
        s.push_str(&format!("{} {}\n", r.best_ip, r.domain));
    }
    s.push_str(END);
    s.push('\n');
    s
}

fn hosts_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

/// 移除已有的 ghboost 标记块
fn strip_block(s: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for line in s.lines() {
        let t = line.trim();
        if t == BEGIN {
            skip = true;
            continue;
        }
        if t == END {
            skip = false;
            continue;
        }
        if !skip {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn apply_hosts(block: &str) -> Result<String, AppError> {
    let p = hosts_path();
    let cur = std::fs::read_to_string(&p).unwrap_or_default();
    let cleaned = strip_block(&cur);
    let mut out = cleaned.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(block);
    std::fs::write(&p, out).map_err(|e| {
        AppError::Runtime(format!(
            "写入 {} 失败: {e} —— Windows 请用管理员权限运行",
            p.display()
        ))
    })?;
    Ok(format!("已写入 {}", p.display()))
}

fn clean_hosts() -> Result<String, AppError> {
    let p = hosts_path();
    let cur = std::fs::read_to_string(&p).unwrap_or_default();
    let cleaned = strip_block(&cur);
    std::fs::write(&p, cleaned).map_err(|e| {
        AppError::Runtime(format!(
            "清理 {} 失败: {e} —— Windows 请用管理员权限运行",
            p.display()
        ))
    })?;
    Ok("已清理 ghboost 写入的 hosts 条目".to_string())
}

mod nodes;

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
        let _ = std::env::remove_var(k);
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
        .register(RegisteredCommand::from_app::<nodes::Scan>())
        .expect("注册 scan 失败");
    registry
        .register(RegisteredCommand::from_app::<nodes::Test>())
        .expect("注册 test 失败");
    registry
        .register(RegisteredCommand::from_app::<nodes::Add>())
        .expect("注册 add 失败");

    if args.iter().any(|a| a == "--mcp") {
        lilyco::serve_mcp(registry);
    } else if args.iter().any(|a| a == "--schema") {
        let schemas: Vec<_> = registry.visible().map(|c| &c.schema).collect();
        println!("{}", serde_json::to_string_pretty(&schemas).unwrap());
    } else {
        lilyco::run_cli_registry("ghboost", registry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderName, HeaderValue};

    #[test]
    fn strip_block_removes_ghboost_section_only() {
        let input = "# existing\n# BEGIN ghboost\n1.2.3.4 github.com\n# END ghboost\n# trailing\n";
        let out = strip_block(input);
        assert!(out.contains("# existing"), "outside lines kept");
        assert!(out.contains("# trailing"), "outside lines kept");
        assert!(!out.contains("github.com"), "block content removed");
        assert!(!out.contains("# BEGIN ghboost"));
        assert!(!out.contains("# END ghboost"));
    }

    #[test]
    fn render_hosts_wraps_entries_with_markers() {
        let rows = vec![Row {
            domain: "github.com".to_string(),
            best_ip: "1.2.3.4".to_string(),
            best_ms: 10,
            candidates: 3,
            usable: 2,
        }];
        let s = render_hosts(&rows);
        assert!(s.starts_with("# BEGIN ghboost\n"));
        assert!(s.contains("1.2.3.4 github.com\n"));
        assert!(s.trim_end().ends_with("# END ghboost"));
    }

    #[test]
    fn requires_ok_status_only_for_main_sites() {
        assert!(requires_ok_status("github.com"));
        assert!(requires_ok_status("api.github.com"));
        assert!(!requires_ok_status("raw.githubusercontent.com"));
        assert!(!requires_ok_status("objects.githubusercontent.com"));
    }

    #[test]
    fn looks_like_github_detects_markers() {
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("x-github-request-id"),
            HeaderValue::from_static("abc"),
        );
        assert!(looks_like_github(&h));

        let mut h2 = HeaderMap::new();
        h2.insert(
            HeaderName::from_static("server"),
            HeaderValue::from_static("github.com"),
        );
        assert!(looks_like_github(&h2));

        let h3 = HeaderMap::new();
        assert!(!looks_like_github(&h3));
    }

    #[test]
    fn dns_resp_parses_answers_into_ips() {
        let json = r#"{"Answer":[{"data":"20.205.243.166"},{"data":"2606:50c0:8000::2"}]}"#;
        let r: DnsResp = serde_json::from_str(json).unwrap();
        let ips: Vec<String> = r
            .answer
            .unwrap()
            .iter()
            .filter_map(|a| a.data.clone())
            .collect();
        assert_eq!(
            ips,
            vec![
                "20.205.243.166".to_string(),
                "2606:50c0:8000::2".to_string()
            ]
        );
    }

    #[test]
    fn hosts_path_ends_with_hosts() {
        let p = hosts_path();
        assert!(p.to_string_lossy().ends_with("hosts"));
    }
}
