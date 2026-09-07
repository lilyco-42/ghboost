//! 节点管理核心：自动扫描订阅源 → mihomo 内核精确测速 → 注入/导出（纯逻辑，不依赖 lilyco）
//!
//! 设计要点：
//! - **扫描** 只做"聚合 + 分类 + 去重"：把每个订阅源按格式拆成
//!   ① URI 文本行（`ss://`/`vless://`/...）或 ② Clash YAML 的 `proxies:` 块。
//! - **测速** 借用本机已有的 Mihomo 内核：生成临时 config，用
//!   `proxy-providers`（`parse-type: v2ray` / `clash`）直接吃原始订阅，启动
//!   **独立实例**（独立端口 + external-controller + secret + `-d` 隔离），
//!   REST API 批量测延迟。绝不动用户正在跑的 Clash Verge。
//! - **添加** 默认只导出：把测过的可用节点写成 `nodes_good_uri.txt` +
//!   `nodes_good.yaml`。`apply` 才把它们注入用户当前激活的 local profile
//!   （备份原文件）。
//!
//! 对外暴露 `ScanParams`/`TestParams`/`AddParams` + `*_core` 异步函数，bin 与 cdylib 共用。

use std::collections::{HashMap, HashSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::rt::sync::Semaphore;
use crate::rt::task::JoinSet;
use base64::Engine;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use serde::Serialize;

use crate::{Event, Level};

/// free-VPN 仓库 README（订阅源索引）
pub const REPO_README: &str = "https://raw.githubusercontent.com/lilyco-42/free-VPN/main/README.md";

/// Mihomo 二进制候选路径（按存在性选第一个）
const MIHOMO_CANDIDATES: &[&str] = &[
    "C:\\Program Files\\Clash Verge\\verge-mihomo.exe",
    "C:\\Program Files\\Clash Verge\\verge-mihomo-alpha.exe",
    "/usr/local/bin/mihomo",
    "/usr/bin/mihomo",
    "verge-mihomo",
    "mihomo",
];

// ── 数据结构 ────────────────────────────────────────────────

/// 扫描出的单个节点索引（轻量，仅用于映射 name↔源行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub name: String,
    pub protocol: String,
    pub source: String,
    /// 原始 URI 行（如果是 URI 型），或空（Clash 型）
    pub raw: String,
}

#[derive(Debug, Serialize)]
pub struct ScanResult {
    pub sources_total: usize,
    pub sources_ok: usize,
    pub uri_nodes: usize,
    pub clash_nodes: usize,
    pub data_dir: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestedNode {
    pub name: String,
    /// 延迟毫秒；None 表示不可用
    pub delay_ms: Option<u128>,
    pub protocol: String,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct TestResult {
    pub tested: usize,
    pub alive: usize,
    pub best_ms: Option<u128>,
    pub data_dir: String,
}

#[derive(Debug, Serialize)]
pub struct AddResult {
    pub kept: usize,
    pub max_ms: u64,
    pub exported_uri: String,
    pub exported_yaml: String,
    pub applied: bool,
    pub apply_detail: Option<String>,
}

// ── 参数（FFI / bin 共用，JSON 反序列化，缺字段用 Default） ──

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ScanParams {
    /// 额外订阅源 URL（与仓库索引合并）
    pub source: Option<Vec<String>>,
    /// 是否也扫描 free-VPN 仓库 README 索引
    pub include_repo: bool,
    /// 最多处理的订阅源数
    pub max_sources: u64,
    /// 并发拉取数
    pub concurrency: u64,
    /// 单源最多取前 N 行节点
    pub per_limit: u64,
    /// 节点库数据目录
    pub output: PathBuf,
}

impl Default for ScanParams {
    fn default() -> Self {
        Self {
            source: None,
            include_repo: true,
            max_sources: 60,
            concurrency: 16,
            per_limit: 500,
            output: PathBuf::from("nodes_data"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TestParams {
    /// 节点库数据目录（需先 scan）
    pub input: PathBuf,
    /// 最多测试节点数
    pub top: u64,
    /// 测速并发
    pub concurrency: u64,
    /// 单个节点测速超时（毫秒）
    pub timeout_ms: u64,
    /// 测速用的探测 URL
    pub test_url: String,
    /// mihomo 二进制路径（默认自动探测）
    pub mihomo: Option<PathBuf>,
}

impl Default for TestParams {
    fn default() -> Self {
        Self {
            input: PathBuf::from("nodes_data"),
            top: 300,
            concurrency: 32,
            timeout_ms: 8000,
            test_url: "https://www.gstatic.com/generate_204".to_string(),
            mihomo: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AddParams {
    /// 测试结果数据目录
    pub input: PathBuf,
    /// 保留延迟最低的 N 个节点
    pub keep: u64,
    /// 延迟上限（毫秒），超过的丢弃；0=不限
    pub max_ms: u64,
    /// 写入用户当前激活的 local profile（带备份）。默认只导出
    pub apply: bool,
    /// 目标 profile 路径（默认自动定位当前激活的 Clash Verge local profile）
    pub profile: Option<PathBuf>,
}

impl Default for AddParams {
    fn default() -> Self {
        Self {
            input: PathBuf::from("nodes_data"),
            keep: 20,
            max_ms: 0,
            apply: false,
            profile: None,
        }
    }
}

// ── 工具函数 ────────────────────────────────────────────────

/// 拿到一个当前空闲的 TCP 端口
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok().map(|a| a.port()))
        .unwrap_or(19090)
}

/// 探测 mihomo 二进制
fn find_mihomo() -> Option<PathBuf> {
    for c in MIHOMO_CANDIDATES {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    // PATH 里再找一次
    if let Ok(path) = which_mihomo() {
        return Some(path);
    }
    None
}

fn which_mihomo() -> Result<PathBuf, ()> {
    let out = Command::new("where")
        .arg("mihomo")
        .output()
        .or_else(|_| Command::new("which").arg("mihomo").output())
        .map_err(|_| ())?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(first) = s.lines().next() {
            return Ok(PathBuf::from(first.trim()));
        }
    }
    Err(())
}

/// 整体 base64 解码（仅当整块都是 base64 字符时）
fn try_b64_decode(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() < 40 {
        return None;
    }
    let all_b64 = t
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=');
    if !all_b64 {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(t).ok()?;
    String::from_utf8(decoded).ok()
}

/// 从一行 URI 提取节点名（# 之后，URL decode）
pub fn uri_name(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // vmess:// 是整体 base64 JSON，name 在 ps 字段
    if let Some(rest) = line.strip_prefix("vmess://") {
        if let Some(dec) = try_b64_decode(rest.split('#').next().unwrap_or(rest)) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&dec) {
                if let Some(ps) = v.get("ps").and_then(|x| x.as_str()) {
                    return Some(ps.to_string());
                }
            }
        }
        // 退化：vmess://BASE64#name
        if let Some((_, frag)) = line.split_once('#') {
            return Some(url_decode(frag));
        }
        return None;
    }
    // 其它协议：name 在 # 之后
    if let Some((_, frag)) = line.split_once('#') {
        return Some(url_decode(frag));
    }
    None
}

fn url_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .to_string()
}

fn protocol_of(line: &str) -> &'static str {
    if line.starts_with("ss://") {
        "ss"
    } else if line.starts_with("vless://") {
        "vless"
    } else if line.starts_with("vmess://") {
        "vmess"
    } else if line.starts_with("trojan://") {
        "trojan"
    } else if line.starts_with("socks") {
        "socks"
    } else if line.starts_with("http://") || line.starts_with("https://") {
        "http"
    } else {
        "other"
    }
}

/// 把一个订阅源文本拆成 URI 行 + Clash proxies 块
///
/// 顺序：整体 base64 先解码一次（不递归，避免双重 base64 死循环）→
/// 再判是否 Clash YAML（含 `proxies:`）→ 否则按 URI 文本逐行提取。
fn classify(text: &str) -> (Vec<String>, Vec<serde_yaml::Value>) {
    // 1) 整体 base64？只解码一层
    let text = match try_b64_decode(text) {
        Some(d) => d,
        None => text.to_string(),
    };
    // 2) Clash YAML（含 proxies: 且是合法流）
    if text.contains("\nproxies:") || text.starts_with("proxies:") {
        if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
            if let Some(arr) = v.get("proxies").and_then(|p| p.as_sequence()) {
                let clash: Vec<serde_yaml::Value> = arr
                    .iter()
                    .filter(|x| x.get("name").is_some())
                    .cloned()
                    .collect();
                if !clash.is_empty() {
                    return (vec![], clash);
                }
            }
        }
    }
    // 3) URI 文本：逐行取协议行（并清洗常见格式污染）
    let mut uris = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("ss://")
            || l.starts_with("vless://")
            || l.starts_with("vmess://")
            || l.starts_with("trojan://")
            || l.starts_with("socks://")
            || l.starts_with("socks5://")
            || l.starts_with("http://")
            || l.starts_with("https://")
        {
            if let Some(clean) = clean_uri_line(l) {
                uris.push(clean);
            }
        }
    }
    (uris, vec![])
}

/// 清洗单行 URI：免费订阅脏数据极多，且 mihomo 的 file provider 遇到
/// 畸形 vmess 行会 **整批 fatal**（不是逐行跳过），导致 0 节点。
/// 所以扫描阶段就把明显畸形的行过滤掉：
/// - vmess://BASE64：必须能 base64 解码成 UTF-8 JSON，且含合法 add/port/alterId，
///   否则丢弃（覆盖 `==vless` 尾缀拼接、alterId 是二进制 `\x02` 等情况）。
/// - 其它协议：mihomo 是逐行 warning 跳过，不致命，原样保留。
fn clean_uri_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(body) = line.strip_prefix("vmess://") {
        // 取 # 之前的 base64 主体
        let b = body.split('#').next().unwrap_or(body);
        let dec = decode_vmess(b)?;
        let v: serde_json::Value = serde_json::from_str(&dec).ok()?;
        // add 必须非空字符串
        v.get("add")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())?;
        // port 必须是数字（或数字字符串）
        match v.get("port") {
            Some(serde_json::Value::Number(_)) => {}
            Some(serde_json::Value::String(s)) => {
                if s.parse::<u16>().is_err() {
                    return None;
                }
            }
            _ => return None,
        }
        // alterId 必须是数字（或数字字符串）
        match v.get("alterId") {
            Some(serde_json::Value::Number(_)) => {}
            Some(serde_json::Value::String(s)) => {
                if s.parse::<i64>().is_err() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(line.to_string())
}

/// 尝试标准 / URL-safe base64 解码 vmess 主体
fn decode_vmess(b: &str) -> Option<String> {
    if let Ok(d) = base64::engine::general_purpose::STANDARD.decode(b) {
        if let Ok(s) = String::from_utf8(d) {
            return Some(s);
        }
    }
    if let Ok(d) = base64::engine::general_purpose::URL_SAFE.decode(b) {
        if let Ok(s) = String::from_utf8(d) {
            return Some(s);
        }
    }
    None
}

/// 清理代理名：仅保留 ASCII 字母数字与 `-_.`，其余（emoji/中文/空格/符号）删除。
/// mihomo 的 `/proxies/{name}` 端点对 emoji/中文名会 404，清理为 ASCII 后才能用 URL 路由精确匹配。
fn clean_label(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect()
}

/// 清理单行 URI 的 #name 部分（协议/参数不变），返回新 URI
fn clean_uri_name(line: &str) -> String {
    if let Some(pos) = line.rfind('#') {
        let (head, rest) = line.split_at(pos);
        let cleaned = clean_label(&rest[1..]);
        if cleaned.is_empty() {
            head.to_string()
        } else {
            format!("{head}#{cleaned}")
        }
    } else {
        line.to_string()
    }
}

/// 从 README 提取 raw.githubusercontent 订阅链接（剥掉 # 片段）
fn extract_repo_sources(readme: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in readme.lines() {
        for m in line.split_whitespace() {
            let m = m.trim_matches(|c| "*`<>()".contains(c));
            if m.starts_with("https://raw.githubusercontent.com/")
                || m.starts_with("https://gist.githubusercontent.com/")
                || m.starts_with("https://cdn.jsdelivr.net/")
                || m.starts_with("https://fastly.jsdelivr.net/")
            {
                // 剥掉 # 片段（subs-check 工具标记）
                let url = m.split('#').next().unwrap_or(m).to_string();
                if !url.is_empty() {
                    out.push(url);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 抓订阅源文本（直连，禁用系统代理）
async fn fetch_text(client: &reqwest::Client, url: &str) -> Option<String> {
    match client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.text().await.ok(),
        _ => None,
    }
}

// ── 命令 1：节点自动扫描 ───────────────────────────────────

pub async fn scan_core(
    app: ScanParams,
    sink: &dyn Fn(&Event),
) -> Result<serde_json::Value, String> {
    let data_dir = &app.output;
    std::fs::create_dir_all(data_dir).map_err(|e| format!("创建数据目录失败: {e}"))?;

    let builder = crate::http_builder();
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder.no_proxy().timeout(Duration::from_secs(30));
    let client = builder
        .user_agent("ghboost/0.1")
        .build()
        .map_err(|e| format!("http 客户端失败: {e}"))?;

    // 收集源 URL
    let mut sources: Vec<String> = Vec::new();
    if app.include_repo {
        if let Some(readme) = fetch_text(&client, REPO_README).await {
            let mut repo = extract_repo_sources(&readme);
            sink(&Event::Log {
                level: Level::Info,
                message: format!("仓库索引提取到 {} 个订阅源", repo.len()),
            });
            sources.append(&mut repo);
        } else {
            sink(&Event::Log {
                level: Level::Warn,
                message: "仓库 README 抓取失败，跳过（可手动 --source 指定）".to_string(),
            });
        }
    }
    if let Some(extra) = &app.source {
        sources.extend(extra.iter().cloned());
    }
    sources.sort();
    sources.dedup();
    if sources.is_empty() {
        return Err("没有任何订阅源可扫描".into());
    }
    let sources: Vec<String> = sources.into_iter().take(app.max_sources as usize).collect();
    let total = sources.len() as u64;
    sink(&Event::Log {
        level: Level::Info,
        message: format!("共 {} 个源，开始并发扫描", total),
    });
    sink(&Event::Started {
        total: Some(total),
        message: Some("扫描订阅源".into()),
    });

    // 并发拉取 + 分类
    let sem = Arc::new(Semaphore::new(app.concurrency.max(1) as usize));
    let mut set = JoinSet::new();
    for (i, url) in sources.iter().enumerate() {
        let sem = sem.clone();
        let client = client.clone();
        let url = url.clone();
        set.spawn(async move {
            let _p = sem.acquire().await;
            let text = fetch_text(&client, &url).await;
            (i, url, text)
        });
    }

    let mut uri_lines: Vec<String> = Vec::new();
    let mut clash_proxies: Vec<serde_yaml::Value> = Vec::new();
    let mut used_sources: HashSet<String> = HashSet::new();
    let mut sources_ok = 0usize;

    while let Some(res) = set.join_next().await {
        if let Ok((i, url, text)) = res {
            sink(&Event::Tick {
                current: i as u64,
                total: Some(total),
                message: format!("扫描 {}", &url[..url.len().min(48)]),
            });
            let Some(text) = text else { continue };
            sources_ok += 1;
            used_sources.insert(url.clone());
            let (uris, clash) = classify(&text);
            for u in uris.into_iter().take(app.per_limit as usize) {
                uri_lines.push(u);
            }
            clash_proxies.extend(clash);
        }
    }

    // 清理节点名：emoji/中文/空格会导致 mihomo REST 端点 `/proxies/{name}` 404，
    // 清理为 ASCII 安全名后再写入，测速才能精确匹配。
    uri_lines = uri_lines.into_iter().map(|l| clean_uri_name(&l)).collect();
    for p in &mut clash_proxies {
        if let Some(n) = p.get("name").and_then(|x| x.as_str()) {
            p["name"] = serde_yaml::Value::String(clean_label(n));
        }
    }

    // 去重
    uri_lines.sort();
    uri_lines.dedup();
    // clash 按 name 去重
    let mut seen: HashSet<String> = HashSet::new();
    clash_proxies.retain(|p| {
        if let Some(n) = p.get("name").and_then(|x| x.as_str()) {
            seen.insert(n.to_string())
        } else {
            false
        }
    });

    // 写磁盘
    let uri_path = data_dir.join("nodes_uri.txt");
    std::fs::write(&uri_path, uri_lines.join("\n") + "\n")
        .map_err(|e| format!("写 {} 失败: {e}", uri_path.display()))?;
    let clash_path = data_dir.join("nodes_clash.yaml");
    let clash_doc = serde_yaml::to_string(&serde_yaml::Value::Mapping(
        serde_yaml::Mapping::from_iter(vec![(
            serde_yaml::Value::from("proxies"),
            serde_yaml::Value::Sequence(clash_proxies.clone()),
        )]),
    ))
    .map_err(|e| format!("序列化 clash 失败: {e}"))?;
    std::fs::write(&clash_path, clash_doc)
        .map_err(|e| format!("写 {} 失败: {e}", clash_path.display()))?;

    // 索引：name↔源行，供测速/添加映射
    let mut index: Vec<NodeInfo> = Vec::new();
    for u in &uri_lines {
        let name = uri_name(u).unwrap_or_else(|| u.clone());
        let proto = protocol_of(u).to_string();
        index.push(NodeInfo {
            name: name.clone(),
            protocol: proto,
            source: "".into(),
            raw: u.clone(),
        });
    }
    for c in &clash_proxies {
        let name = c
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_string();
        let proto = c
            .get("type")
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_string();
        index.push(NodeInfo {
            name,
            protocol: proto,
            source: "".into(),
            raw: String::new(),
        });
    }
    let index_path = data_dir.join("nodes_index.json");
    std::fs::write(&index_path, serde_json::to_string_pretty(&index).unwrap())
        .map_err(|e| format!("写索引失败: {e}"))?;

    sink(&Event::Log {
        level: Level::Info,
        message: format!(
            "扫描完成：{} 源成功 / URI 节点 {} / Clash 节点 {}",
            sources_ok,
            uri_lines.len(),
            clash_proxies.len()
        ),
    });

    let result = ScanResult {
        sources_total: total as usize,
        sources_ok,
        uri_nodes: uri_lines.len(),
        clash_nodes: clash_proxies.len(),
        data_dir: data_dir.display().to_string(),
    };
    sink(&Event::Done {
        output: serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        elapsed_ms: 0,
    });
    Ok(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null))
}

// ── 命令 2：节点测试（mihomo 内核） ─────────────────────────

pub async fn test_core(
    app: TestParams,
    sink: &dyn Fn(&Event),
) -> Result<serde_json::Value, String> {
    let dir = &app.input;
    let uri_path = dir.join("nodes_uri.txt");
    let clash_path = dir.join("nodes_clash.yaml");
    if !uri_path.exists() && !clash_path.exists() {
        return Err(format!(
            "数据目录 {} 没有 nodes_uri.txt / nodes_clash.yaml，请先运行 scan",
            dir.display()
        ));
    }

    let mihomo = app
        .mihomo
        .clone()
        .or_else(find_mihomo)
        .ok_or_else(|| {
            "未找到 Mihomo 内核（C:\\Program Files\\Clash Verge\\verge-mihomo.exe 或 PATH 中的 mihomo）。\n\
             请先用 Clash Verge / 手动安装 Mihomo，或 --mihomo 指定路径"
                .to_string()
        })?;
    sink(&Event::Log {
        level: Level::Info,
        message: format!("使用内核: {}", mihomo.display()),
    });

    // 临时工作目录（独立于用户 Clash Verge，绝不干扰）
    let tmp = dir.join("mihomo_test");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;

    // mihomo 的 file provider 路径必须在 -d home 目录内（安全限制），
    // 所以把节点文件复制进临时 home，provider 改用相对/内部路径。
    let uri_in_tmp = tmp.join("nodes_uri.txt");
    let clash_in_tmp = tmp.join("nodes_clash.yaml");
    if uri_path.exists() {
        std::fs::copy(&uri_path, &uri_in_tmp).map_err(|e| format!("复制 URI 失败: {e}"))?;
    }
    if clash_path.exists() {
        std::fs::copy(&clash_path, &clash_in_tmp).map_err(|e| format!("复制 Clash 失败: {e}"))?;
    }

    let mixed = free_port();
    let ctrl = free_port();
    let secret: String = {
        use rand::Rng;
        let b: [u8; 16] = rand::thread_rng().gen();
        b.iter().map(|x| format!("{x:02x}")).collect()
    };

    // 动态生成 providers。
    // 注意：mihomo 的 file provider 路径必须是相对 -d home 的文件名
    //（安全限制，绝对路径会被静默拒绝，导致 0 节点）。文件已复制到 tmp 内，
    // 这里只用文件名。
    let uri_rel = uri_in_tmp
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clash_rel = clash_in_tmp
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut providers = String::new();
    if uri_in_tmp.exists() {
        providers.push_str(&format!(
            "  uri_nodes:\n    type: file\n    path: {}\n    provider-type: Proxy\n    parse-type: v2ray\n    health-check:\n      enable: true\n      url: {}\n      interval: 600\n",
            uri_rel,
            app.test_url
        ));
    }
    if clash_in_tmp.exists() {
        providers.push_str(&format!(
            "  clash_nodes:\n    type: file\n    path: {}\n    provider-type: Proxy\n    parse-type: clash\n    health-check:\n      enable: true\n      url: {}\n      interval: 600\n",
            clash_rel,
            app.test_url
        ));
    }
    let mut uses = Vec::new();
    if uri_path.exists() {
        uses.push("uri_nodes");
    }
    if clash_path.exists() {
        uses.push("clash_nodes");
    }

    let cfg = format!(
        "mixed-port: {mixed}\n\
         allow-lan: false\n\
         mode: rule\n\
         log-level: warning\n\
         external-controller: 127.0.0.1:{ctrl}\n\
         secret: \"{secret}\"\n\
         profile:\n  store-selected: false\n\
         proxy-providers:\n{providers}\
         proxy-groups:\n  - name: ALL\n    type: select\n    use:\n      - {uses}\n\
         rules:\n  - MATCH,ALL\n",
        mixed = mixed,
        ctrl = ctrl,
        secret = secret,
        providers = providers,
        uses = uses.join("\n      - ")
    );
    let cfg_path = tmp.join("config.yaml");
    std::fs::write(&cfg_path, cfg).map_err(|e| format!("写临时配置失败: {e}"))?;

    // 启动独立 mihomo 实例（stderr 落日志，便于失败时诊断；stdout 丢弃）
    sink(&Event::Log {
        level: Level::Info,
        message: format!("启动独立实例 (mixed:{mixed} ctrl:{ctrl})..."),
    });
    let log_path = tmp.join("mihomo.log");
    let log_file = std::fs::File::create(&log_path).map_err(|e| format!("创建日志失败: {e}"))?;
    let log_dup = log_file
        .try_clone()
        .map_err(|e| format!("克隆日志失败: {e}"))?;
    let mut child = Command::new(&mihomo)
        .arg("-d")
        .arg(&tmp)
        .arg("-f")
        .arg(&cfg_path)
        .stdout(Stdio::from(log_dup))
        .stderr(Stdio::from(log_file))
        .spawn()
        .map_err(|e| format!("启动 mihomo 失败: {e}（可能端口被占，重试）"))?;

    let base = format!("http://127.0.0.1:{ctrl}");
    let auth = format!("Bearer {secret}");
    let builder = crate::http_builder();
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder.no_proxy().timeout(Duration::from_secs(600));
    let client = builder
        .build()
        .map_err(|e| format!("rest 客户端失败: {e}"))?;

    // 等待就绪
    let ready = wait_ready(&client, &base, &auth).await;
    if !ready {
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        let _ = child.kill();
        return Err(format!("mihomo 启动后 30s 内未就绪。日志:\n{log}"));
    }

    // 拿待测节点名（已是 ASCII 安全名，scan 阶段清理过），top 限制
    let members = get_all_members(&client, &base, &auth).await;
    let members: Vec<String> = members.into_iter().take(app.top as usize).collect();
    let n = members.len();
    sink(&Event::Log {
        level: Level::Info,
        message: format!("实例就绪，共 {} 个节点待测", n),
    });
    sink(&Event::Started {
        total: Some(n as u64),
        message: Some("延迟测试".into()),
    });

    // 逐节点测延迟：节点名是 ASCII，URL 路由精确匹配，不会 404。
    let sem = Arc::new(Semaphore::new(app.concurrency.max(1) as usize));
    let timeout = app.timeout_ms;
    let mut set = JoinSet::new();
    for (i, name) in members.iter().enumerate() {
        let sem = sem.clone();
        let client = client.clone();
        let base = base.clone();
        let auth = auth.clone();
        let name = name.clone();
        let url = app.test_url.clone();
        set.spawn(async move {
            let _p = sem.acquire().await;
            let ms = probe_delay(&client, &base, &auth, &name, &url, timeout).await;
            (i, name, ms)
        });
    }

    let mut tested: Vec<TestedNode> = Vec::new();
    let mut done = 0u64;
    while let Some(res) = set.join_next().await {
        if let Ok((_i, name, ms)) = res {
            done += 1;
            if done.is_multiple_of(25) {
                sink(&Event::Tick {
                    current: done,
                    total: Some(n as u64),
                    message: format!("已测 {done}/{n}"),
                });
            }
            tested.push(TestedNode {
                name,
                delay_ms: ms,
                protocol: String::new(),
                source: String::new(),
            });
        }
    }
    // 按延迟升序
    tested.sort_by_key(|t| t.delay_ms.unwrap_or(u128::MAX));
    sink(&Event::Log {
        level: Level::Info,
        message: format!("收到 {} 个节点测速结果", tested.len()),
    });

    // 关实例
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&tmp);

    // 用索引补全 protocol/source
    let index_path = dir.join("nodes_index.json");
    let index: Vec<NodeInfo> = std::fs::read_to_string(&index_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let idx_map: HashMap<String, &NodeInfo> = index.iter().map(|n| (n.name.clone(), n)).collect();
    for t in tested.iter_mut() {
        if let Some(info) = idx_map.get(&t.name) {
            t.protocol = info.protocol.clone();
            t.source = info.source.clone();
        }
    }

    let alive: Vec<&TestedNode> = tested.iter().filter(|t| t.delay_ms.is_some()).collect();
    let best = alive.iter().filter_map(|t| t.delay_ms).min();
    sink(&Event::Log {
        level: Level::Info,
        message: format!(
            "测试完成：{} 测 / {} 可用 / 最优 {}ms",
            tested.len(),
            alive.len(),
            best.unwrap_or(0)
        ),
    });

    let out_path = dir.join("nodes_tested.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&tested).unwrap())
        .map_err(|e| format!("写结果失败: {e}"))?;

    let result = TestResult {
        tested: tested.len(),
        alive: alive.len(),
        best_ms: if alive.is_empty() {
            None
        } else {
            Some(best.unwrap_or(0))
        },
        data_dir: dir.display().to_string(),
    };
    sink(&Event::Done {
        output: serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        elapsed_ms: 0,
    });
    Ok(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null))
}

async fn wait_ready(client: &reqwest::Client, base: &str, auth: &str) -> bool {
    for _ in 0..60 {
        if let Ok(r) = client
            .get(format!("{base}/version"))
            .header("Authorization", auth)
            .send()
            .await
        {
            if r.status().is_success() {
                return true;
            }
        }
        crate::rt::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn get_all_members(client: &reqwest::Client, base: &str, auth: &str) -> Vec<String> {
    // select 组的 `all` 字段不会展开 provider 成员，必须直接从
    // `/providers/proxies` 拿每个 provider 的节点名列表。
    // 注意 mihomo 默认还有一个 `default` provider（含 DIRECT/REJECT 等），
    // 我们只取真实订阅 provider 的节点；用 `provider-name` 字段稳健识别。
    // provider 节点是异步加载的，启动就绪后可能还没展开，所以重试几次。
    for _ in 0..20 {
        let mut names: Vec<String> = Vec::new();
        if let Ok(r) = client
            .get(format!("{base}/providers/proxies"))
            .header("Authorization", auth)
            .send()
            .await
        {
            if let Ok(j) = r.json::<serde_json::Value>().await {
                if let Some(providers) = j.get("providers").and_then(|p| p.as_object()) {
                    for (_k, prov) in providers {
                        // 跳过 mihomo 内置的 default provider（DIRECT/REJECT 等）
                        if prov.get("name").and_then(|x| x.as_str()) == Some("default") {
                            continue;
                        }
                        if let Some(proxies) = prov.get("proxies").and_then(|p| p.as_array()) {
                            for p in proxies {
                                if let Some(n) = p.get("name").and_then(|x| x.as_str()) {
                                    names.push(n.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        if !names.is_empty() {
            return names;
        }
        crate::rt::time::sleep(Duration::from_millis(800)).await;
    }
    vec![]
}

async fn probe_delay(
    client: &reqwest::Client,
    base: &str,
    auth: &str,
    name: &str,
    test_url: &str,
    timeout_ms: u64,
) -> Option<u128> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static DIAG: AtomicU32 = AtomicU32::new(0);
    let enc = utf8_percent_encode(name, NON_ALPHANUMERIC).to_string();
    let url = format!("{base}/proxies/{enc}/delay?url={test_url}&timeout={timeout_ms}");
    let r = client.get(&url).header("Authorization", auth).send().await;
    if let Ok(r) = r {
        let status = r.status();
        if let Ok(j) = r.json::<serde_json::Value>().await {
            if let Some(d) = j.get("delay").and_then(|x| x.as_u64()) {
                return Some(d as u128);
            }
            if DIAG.fetch_add(1, Ordering::SeqCst) < 5 {
                eprintln!("[probe-err] name={:?} status={} body={}", name, status, j);
            }
        } else if DIAG.fetch_add(1, Ordering::SeqCst) < 5 {
            eprintln!("[probe-err] name={:?} status={} (非 JSON)", name, status);
        }
    } else if DIAG.fetch_add(1, Ordering::SeqCst) < 5 {
        eprintln!("[probe-err] name={:?} 请求失败", name);
    }
    None
}

// ── 命令 3：节点添加 ───────────────────────────────────────

pub async fn add_core(app: AddParams, sink: &dyn Fn(&Event)) -> Result<serde_json::Value, String> {
    let dir = &app.input;
    let tested_path = dir.join("nodes_tested.json");
    let raw = std::fs::read_to_string(&tested_path)
        .map_err(|_| format!("找不到 {}，请先运行 test", tested_path.display()))?;
    let tested: Vec<TestedNode> =
        serde_json::from_str(&raw).map_err(|e| format!("解析测试结果失败: {e}"))?;

    // 筛可用 + 排序 + 限量
    let mut alive: Vec<&TestedNode> = tested.iter().filter(|t| t.delay_ms.is_some()).collect();
    alive.sort_by_key(|t| t.delay_ms.unwrap());
    if app.max_ms > 0 {
        alive.retain(|t| t.delay_ms.unwrap() <= app.max_ms as u128);
    }
    alive.truncate(app.keep as usize);
    if alive.is_empty() {
        return Err("没有可用节点（全部测速失败）。换网络或放宽条件重试 scan/test".into());
    }
    let alive_names: HashSet<String> = alive.iter().map(|t| t.name.clone()).collect();
    sink(&Event::Log {
        level: Level::Info,
        message: format!("保留 {} 个最优可用节点", alive.len()),
    });

    // 导出：从 nodes_uri.txt 过滤可用 name 的行
    let uri_path = dir.join("nodes_uri.txt");
    let mut good_uri: Vec<String> = Vec::new();
    if uri_path.exists() {
        let txt = std::fs::read_to_string(&uri_path).unwrap_or_default();
        for line in txt.lines() {
            if let Some(name) = uri_name(line) {
                if alive_names.contains(&name) {
                    good_uri.push(line.to_string());
                }
            }
        }
    }
    let good_uri_path = dir.join("nodes_good_uri.txt");
    std::fs::write(&good_uri_path, good_uri.join("\n") + "\n")
        .map_err(|e| format!("写可用 URI 失败: {e}"))?;

    // 导出：从 nodes_clash.yaml 过滤可用 name 的块
    let clash_path = dir.join("nodes_clash.yaml");
    let mut good_clash: Vec<serde_yaml::Value> = Vec::new();
    if clash_path.exists() {
        let txt = std::fs::read_to_string(&clash_path).unwrap_or_default();
        if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&txt) {
            if let Some(arr) = v.get("proxies").and_then(|p| p.as_sequence()) {
                for p in arr {
                    if let Some(n) = p.get("name").and_then(|x| x.as_str()) {
                        if alive_names.contains(n) {
                            good_clash.push(p.clone());
                        }
                    }
                }
            }
        }
    }
    let good_clash_path = dir.join("nodes_good.yaml");
    let doc = serde_yaml::to_string(&serde_yaml::Value::Mapping(serde_yaml::Mapping::from_iter(
        vec![(
            serde_yaml::Value::from("proxies"),
            serde_yaml::Value::Sequence(good_clash.clone()),
        )],
    )))
    .map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(&good_clash_path, doc).map_err(|e| format!("写可用 clash 失败: {e}"))?;

    let mut result = AddResult {
        kept: alive.len(),
        max_ms: app.max_ms,
        exported_uri: good_uri_path.display().to_string(),
        exported_yaml: good_clash_path.display().to_string(),
        applied: false,
        apply_detail: None,
    };

    // --apply：注入用户 profile
    if app.apply {
        let profile = resolve_profile(app.profile.clone())?;
        let detail = inject_profile(&profile, &good_uri_path, &good_clash)?;
        result.applied = true;
        sink(&Event::Log {
            level: Level::Info,
            message: detail.clone(),
        });
        result.apply_detail = Some(detail);
    } else {
        sink(&Event::Log {
            level: Level::Info,
            message: "未注入（默认只导出）。加 apply 才写入 Clash Verge 当前 profile".to_string(),
        });
    }

    sink(&Event::Done {
        output: serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        elapsed_ms: 0,
    });
    Ok(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null))
}

/// 定位当前激活的 Clash Verge local profile
fn resolve_profile(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    // Clash Verge Rev 配置目录
    let base = dirs_config().ok_or_else(|| "找不到 Clash Verge 配置目录".to_string())?;
    let profiles_yaml = base.join("profiles.yaml");
    if !profiles_yaml.exists() {
        return Err(format!(
            "找不到 {}，无法自动定位 profile",
            profiles_yaml.display()
        ));
    }
    let txt = std::fs::read_to_string(&profiles_yaml)
        .map_err(|e| format!("读 profiles.yaml 失败: {e}"))?;
    let v: serde_yaml::Value =
        serde_yaml::from_str(&txt).map_err(|e| format!("解析 profiles.yaml 失败: {e}"))?;
    let current = v
        .get("current")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "profiles.yaml 无 current 字段".to_string())?;
    // 在 items 里找 uid == current，type == local，取其 file
    if let Some(items) = v.get("items").and_then(|x| x.as_sequence()) {
        for it in items {
            if it.get("uid").and_then(|x| x.as_str()) == Some(current) {
                if let Some(file) = it.get("file").and_then(|x| x.as_str()) {
                    return Ok(base.join("profiles").join(file));
                }
            }
        }
    }
    Err(format!("profile current={current} 未找到对应 local 文件"))
}

fn dirs_config() -> Option<PathBuf> {
    // Windows: %APPDATA%\io.github.clash-verge-rev.clash-verge-rev
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let p = PathBuf::from(&appdata).join("io.github.clash-verge-rev.clash-verge-rev");
        if p.exists() {
            return Some(p);
        }
    }
    // Linux/macOS
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(&home)
            .join(".config")
            .join("io.github.clash-verge-rev.clash-verge-rev");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// 把可用节点注入 profile（备份 + 追加 proxy-providers + 扩展 PROXY 组）
fn inject_profile(
    profile: &Path,
    good_uri: &Path,
    good_clash: &[serde_yaml::Value],
) -> Result<String, String> {
    // 备份
    let bak = profile.with_extension("yaml.bak");
    std::fs::copy(profile, &bak).map_err(|e| format!("备份失败: {e}"))?;

    let txt = std::fs::read_to_string(profile).map_err(|e| format!("读 profile 失败: {e}"))?;
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(&txt).map_err(|e| format!("解析失败: {e}"))?;

    let profiles_dir = profile.parent().unwrap().to_path_buf();
    // 把 good_uri 复制到 profiles 目录（mihomo -d 用相对/绝对都可，用绝对最稳）
    let uri_in_profile = profiles_dir.join("ghboost_nodes_uri.txt");
    std::fs::copy(good_uri, &uri_in_profile).map_err(|e| format!("复制 URI 失败: {e}"))?;
    let uri_abs = uri_in_profile.canonicalize().unwrap_or(uri_in_profile);

    // 追加 proxy-providers（若已存在 ghboost-nodes 则替换）
    let providers = doc
        .get_mut("proxy-providers")
        .and_then(|p| p.as_mapping_mut())
        .cloned()
        .unwrap_or_default();
    let mut providers = providers;
    providers.insert(
        serde_yaml::Value::from("ghboost-nodes"),
        serde_yaml::Value::Mapping(serde_yaml::Mapping::from_iter(vec![
            (
                serde_yaml::Value::from("type"),
                serde_yaml::Value::from("file"),
            ),
            (
                serde_yaml::Value::from("path"),
                serde_yaml::Value::from(uri_abs.to_string_lossy().to_string()),
            ),
            (
                serde_yaml::Value::from("provider-type"),
                serde_yaml::Value::from("Proxy"),
            ),
            (
                serde_yaml::Value::from("parse-type"),
                serde_yaml::Value::from("v2ray"),
            ),
            (
                serde_yaml::Value::from("health-check"),
                serde_yaml::Value::Mapping(serde_yaml::Mapping::from_iter(vec![
                    (
                        serde_yaml::Value::from("enable"),
                        serde_yaml::Value::from(true),
                    ),
                    (
                        serde_yaml::Value::from("url"),
                        serde_yaml::Value::from("https://www.gstatic.com/generate_204"),
                    ),
                    (
                        serde_yaml::Value::from("interval"),
                        serde_yaml::Value::from(600),
                    ),
                ])),
            ),
        ])),
    );
    doc.as_mapping_mut().unwrap().insert(
        serde_yaml::Value::from("proxy-providers"),
        serde_yaml::Value::Mapping(providers),
    );

    // 追加 clash 节点到 proxies（去重 name）
    let proxies = doc
        .get_mut("proxies")
        .and_then(|p| p.as_sequence_mut())
        .cloned()
        .unwrap_or_default();
    let mut proxies = proxies;
    let mut existing: HashSet<String> = proxies
        .iter()
        .filter_map(|p| {
            p.get("name")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let mut added_names: Vec<String> = Vec::new();
    for c in good_clash {
        if let Some(n) = c.get("name").and_then(|x| x.as_str()) {
            if existing.insert(n.to_string()) {
                proxies.push(c.clone());
                added_names.push(n.to_string());
            }
        }
    }
    doc.as_mapping_mut().unwrap().insert(
        serde_yaml::Value::from("proxies"),
        serde_yaml::Value::Sequence(proxies),
    );

    // 扩展 PROXY 组（加 clash 节点名 + provider 名）
    if let Some(groups) = doc
        .get_mut("proxy-groups")
        .and_then(|g| g.as_sequence_mut())
    {
        for g in groups.iter_mut() {
            if g.get("name").and_then(|x| x.as_str()) == Some("PROXY") {
                if let Some(arr) = g.get_mut("proxies").and_then(|p| p.as_sequence_mut()) {
                    for c in &added_names {
                        arr.push(serde_yaml::Value::from(c.clone()));
                    }
                    arr.push(serde_yaml::Value::from("ghboost-nodes"));
                }
            }
        }
    }

    let out = serde_yaml::to_string(&doc).map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(profile, out).map_err(|e| format!("写回失败: {e}"))?;

    Ok(format!(
        "已注入 {} 个 Clash 节点 + ghboost-nodes provider（URI 型）。\n\
         备份: {}\n请在 Clash Verge 切到 PROXY 组选择新节点，或重启内核生效",
        added_names.len(),
        bak.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_of_detects_scheme() {
        assert_eq!(protocol_of("ss://x"), "ss");
        assert_eq!(protocol_of("vless://x"), "vless");
        assert_eq!(protocol_of("vmess://x"), "vmess");
        assert_eq!(protocol_of("trojan://x"), "trojan");
        assert_eq!(protocol_of("socks5://x"), "socks");
        assert_eq!(protocol_of("http://x"), "http");
        assert_eq!(protocol_of("ftp://x"), "other");
    }

    #[test]
    fn url_decode_handles_percent() {
        assert_eq!(url_decode("a%20b"), "a b");
        assert_eq!(url_decode("hello"), "hello");
    }

    #[test]
    fn try_b64_decode_requires_length_and_valid_chars() {
        assert!(try_b64_decode("short").is_none());
        let long = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 50]);
        assert!(try_b64_decode(&long).is_some());
        assert!(try_b64_decode("!!! not base64 !!!").is_none());
    }

    #[test]
    fn uri_name_prefers_vmess_ps_and_fragment() {
        let json = r#"{"ps":"节点A","add":"1.2.3.4","port":443,"alterId":0}"#;
        let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        let uri = format!("vmess://{}#节点A", b64);
        assert_eq!(uri_name(&uri), Some("节点A".to_string()));

        let vless = "vless://uuid@1.2.3.4:443?type=tcp#我的节点";
        assert_eq!(uri_name(vless), Some("我的节点".to_string()));

        let trojan = "trojan://pw@1.2.3.4:443#T Node";
        assert_eq!(uri_name(trojan), Some("T Node".to_string()));
    }

    #[test]
    fn clean_uri_line_validates_vmess() {
        let json = r#"{"ps":"n","add":"1.2.3.4","port":443,"alterId":0}"#;
        let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        assert!(clean_uri_line(&format!("vmess://{}#n", b64)).is_some());

        // 空 add 应被丢弃
        let bad = r#"{"ps":"n","add":"","port":443,"alterId":0}"#;
        let badb64 = base64::engine::general_purpose::STANDARD.encode(bad.as_bytes());
        assert!(clean_uri_line(&format!("vmess://{}#n", badb64)).is_none());

        // 非数字端口应被丢弃
        let bad2 = r#"{"ps":"n","add":"1.2.3.4","port":"abc","alterId":0}"#;
        let bad2b64 = base64::engine::general_purpose::STANDARD.encode(bad2.as_bytes());
        assert!(clean_uri_line(&format!("vmess://{}#n", bad2b64)).is_none());

        // 非 vmess 原样保留
        assert!(clean_uri_line("vless://x@1.2.3.4:443#n").is_some());
    }

    #[test]
    fn classify_splits_clash_and_uris() {
        let yaml = "proxies:\n  - name: n1\n    type: ss\n    server: 1.1.1.1\n    port: 8388\n";
        let (uris, clash) = classify(yaml);
        assert!(uris.is_empty(), "clash yaml yields no uri lines");
        assert_eq!(clash.len(), 1);

        let json = r#"{"ps":"n","add":"1.2.3.4","port":443,"alterId":0}"#;
        let vme = format!(
            "vmess://{}#n1",
            base64::engine::general_purpose::STANDARD.encode(json.as_bytes())
        );
        let text = format!("{}\nvless://u@1.2.3.4:443#n2\n noises \n", vme);
        let (uris2, _) = classify(&text);
        assert_eq!(uris2.len(), 2, "vmess + vless extracted");
    }

    #[test]
    fn extract_repo_sources_dedup_and_strip_fragment() {
        let readme = "see https://raw.githubusercontent.com/a/b/main/x.yaml#subs and \
                      https://raw.githubusercontent.com/a/b/main/x.yaml (dup) plus \
                      https://cdn.jsdelivr.net/gh/c/d@main/y.yml";
        let srcs = extract_repo_sources(readme);
        assert_eq!(srcs.len(), 2, "deduped to 2 unique");
        assert!(srcs
            .iter()
            .any(|s| s.starts_with("https://raw.githubusercontent.com")));
        assert!(srcs
            .iter()
            .any(|s| s.starts_with("https://cdn.jsdelivr.net")));
        assert!(srcs.iter().all(|s| !s.contains('#')), "fragments stripped");
    }

    #[test]
    fn decode_vmess_handles_standard_and_urlsafe() {
        let json = r#"{"ps":"n","add":"1.2.3.4","port":443,"alterId":0}"#;
        let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        assert!(decode_vmess(&b64).is_some());
        let urlsafe = base64::engine::general_purpose::URL_SAFE.encode(json.as_bytes());
        assert!(decode_vmess(&urlsafe).is_some());
        assert!(decode_vmess("!!!not base64!!!").is_none());
    }
}
