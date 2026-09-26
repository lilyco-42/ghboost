//! 节点管理核心：自动扫描订阅源 → mihomo 内核精确测速 → 注入/导出（纯逻辑，不依赖 lilyco）
//!
//! 设计要点：
//! - **扫描** 只做"聚合 + 分类 + 去重"：把每个订阅源按格式拆成
//!   ① URI 文本行（`ss://`/`vless://`/...）或 ② Clash YAML 的 `proxies:` 块。
//!   去重到 **name 全局唯一**（`dedup_scanned`）—— 下游三处都按 name 硬匹配，
//!   同名多行会让 add 导出行数对不上 kept、导出的配置带重名被 mihomo 拒收。
//!   顺带把每个节点的来源订阅 URL 记进索引（`NodeInfo.source`）。
//! - **测速** 借用本机已有的 Mihomo 内核：把待测节点**内联**进临时 config 的
//!   `proxies:`（URI 行经 `corecfg::parse_line` 过滤/转换，Clash 块结构校验后
//!   原样保留），启动**独立实例**（独立端口 + external-controller + secret +
//!   `-d` 隔离），REST API 批量测延迟。绝不动用户正在跑的 Clash Verge。
//!   为什么内联而不用 proxy-provider（2026-09-25 实测 mihomo v1.19.30 两处硬伤）：
//!   ① file provider 是**原子解析**，一行坏节点（如 `ss://<uuid>@host` 没有
//!   method）把整个 provider 打死成 0；② provider 成员不进扁平 `/proxies` 表，
//!   `/proxies/{name}/delay` 恒 404（group `all` 里有、扁平表里没有），
//!   只有内联静态节点才能逐个测速。
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
    /// 来源订阅 URL（scan 时按行/名回填，test 据此补全 TestedNode.source）
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
    /// `output` 是否是用户显式指定的（而非默认值）。
    ///
    /// 显式指定时不可写就直接报错（别偷偷改路径）；用默认值时不可写
    /// 才回退到用户数据目录，见 `resolve_data_dir`。
    pub output_explicit: bool,
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
            output_explicit: false,
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
///
/// 面板"贴链接"功能也用它来判断用户贴的是不是一整坨 base64 订阅内容
/// （见 `web::classify_subscription`）—— 那里只需要"是或不是"，不需要解码结果。
pub fn try_b64_decode(text: &str) -> Option<String> {
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

/// 扫描产物去重：整行相同 → 同名 → 跨池同名，全部只留首条。
///
/// 维持一条下游全都依赖的不变量：**`nodes_uri.txt` + `nodes_clash.yaml` 的
/// name 全局唯一**。三处依赖它：
/// 1. mihomo 的 proxy name 不允许重复，重名整份配置拒收（内联 `proxies:` 尤其致命）；
/// 2. `add` 的 `nodes_good_uri.txt` 导出是按 name 硬匹配源行的 —— 同名多行会
///    让导出行数 > kept，且导出的配置带重名照样被拒；
/// 3. `test` 的索引回填按 name 建 map，重名会取错条目（source 串台）。
///
/// 同名跨池时 URI 池优先，与 `load_test_nodes` 的装载顺序一致。
fn dedup_scanned(uri_lines: &mut Vec<String>, clash_proxies: &mut Vec<serde_yaml::Value>) {
    uri_lines.sort();
    uri_lines.dedup();
    let mut seen: HashSet<String> = HashSet::new();
    uri_lines.retain(|l| match uri_name(l) {
        Some(n) => seen.insert(n),
        // 无名行测速/导出都用不上（都按 uri_name 硬匹配），但仍原样保留
        None => true,
    });
    clash_proxies.retain(|p| {
        if let Some(n) = p.get("name").and_then(|x| x.as_str()) {
            seen.insert(n.to_string())
        } else {
            false
        }
    });
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

/// 数据目录的兜底位置：`$HOME/.local/share/ghboost`（Windows 用 `%APPDATA%`）。
///
/// 只在用户**没显式指定** `--output`、且默认的 `./nodes_data` 不可写时才用。
fn fallback_data_dir() -> Option<PathBuf> {
    // Windows: %APPDATA%\ghboost
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(appdata).join("ghboost").join("nodes_data"));
    }
    // Unix: $XDG_DATA_HOME/ghboost 或 $HOME/.local/share/ghboost
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(xdg).join("ghboost").join("nodes_data"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("ghboost")
                .join("nodes_data"),
        );
    }
    None
}

/// 确保数据目录可用，返回**实际**使用的目录。
///
/// 为什么要兜底：默认值是相对路径 `nodes_data`，从只读目录（容器根目录、
/// `C:\Program Files\...`、只读挂载点）跑就会 `create_dir_all` 失败
/// （Linux 报 `read-only file system (os error 30)`、Windows 报 `os error 5`），
/// 而扫描本身并不依赖这个目录——直接报错 = 用户什么也拿不到。
///
/// 规则：
/// - 用户**显式**给了 `--output` → 失败就报错（附解决办法），不擅自改路径，
///   免得"我明明指定了却没写进去"更难查。
/// - 用的是**默认值**且不可写 → 落到 `fallback_data_dir()` 并提示一句。
fn resolve_data_dir(requested: &Path, explicit: bool) -> Result<PathBuf, String> {
    match std::fs::create_dir_all(requested) {
        Ok(()) => Ok(requested.to_path_buf()),
        Err(e) if explicit => Err(format!(
            "创建数据目录失败: {}（{e}）。请换一个可写的 --output 目录，\
             或去掉 --output 用默认目录。",
            requested.display()
        )),
        Err(e) => {
            let fb = fallback_data_dir().ok_or_else(|| {
                format!(
                    "创建数据目录失败: {}（{e}），且找不到可用的用户数据目录\
                     （APPDATA/XDG_DATA_HOME/HOME 都未设置）。请用 --output 指定一个可写目录。",
                    requested.display()
                )
            })?;
            std::fs::create_dir_all(&fb).map_err(|e2| {
                format!(
                    "创建数据目录失败: {}（{e}）；回退目录 {} 也不可写（{e2}）。\
                     请用 --output 指定一个可写目录。",
                    requested.display(),
                    fb.display()
                )
            })?;
            Ok(fb)
        }
    }
}

pub async fn scan_core(
    app: ScanParams,
    sink: &dyn Fn(&Event),
) -> Result<serde_json::Value, String> {
    let explicit = app.output_explicit;
    let data_dir = resolve_data_dir(&app.output, explicit)?;
    if data_dir != app.output {
        sink(&Event::Log {
            level: Level::Warn,
            message: format!(
                "默认目录 {} 不可写，已改用 {}",
                app.output.display(),
                data_dir.display()
            ),
        });
    }
    let data_dir = data_dir.as_path();

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
    // 行/名 → 来源 URL。索引的 source 字段靠它回填（可追溯到具体订阅），
    // 键在下面清洗名时同步换成清洗后的行/名。
    let mut uri_src: HashMap<String, String> = HashMap::new();
    let mut clash_src: HashMap<String, String> = HashMap::new();
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
            let (uris, clash) = classify(&text);
            for u in uris.into_iter().take(app.per_limit as usize) {
                uri_src.insert(u.clone(), url.clone());
                uri_lines.push(u);
            }
            for c in clash {
                if let Some(n) = c.get("name").and_then(|x| x.as_str()) {
                    clash_src.insert(n.to_string(), url.clone());
                }
                clash_proxies.push(c);
            }
        }
    }

    // 清理节点名：emoji/中文/空格会导致 mihomo REST 端点 `/proxies/{name}` 404，
    // 清理为 ASCII 安全名后再写入，测速才能精确匹配。来源映射同步换键。
    let mut uri_src_clean: HashMap<String, String> = HashMap::new();
    uri_lines = uri_lines
        .into_iter()
        .map(|l| {
            let cleaned = clean_uri_name(&l);
            if let Some(src) = uri_src.get(&l) {
                uri_src_clean.insert(cleaned.clone(), src.clone());
            }
            cleaned
        })
        .collect();
    let mut clash_src_clean: HashMap<String, String> = HashMap::new();
    for p in &mut clash_proxies {
        if let Some(n) = p.get("name").and_then(|x| x.as_str()) {
            let cleaned = clean_label(n);
            if let Some(src) = clash_src.get(n) {
                clash_src_clean.insert(cleaned.clone(), src.clone());
            }
            p["name"] = serde_yaml::Value::String(cleaned);
        }
    }

    // 去重：整行相同 → 同名 → 跨池同名
    dedup_scanned(&mut uri_lines, &mut clash_proxies);

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
            source: uri_src_clean.get(u).cloned().unwrap_or_default(),
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
        let source = clash_src_clean.get(&name).cloned().unwrap_or_default();
        index.push(NodeInfo {
            name,
            protocol: proto,
            source,
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

    // 待测节点内联装载：坏行只损失它自己（不再 provider 原子连坐），详见文件头
    let (nodes, stat) = load_test_nodes(dir, app.top)?;
    let kept = nodes.len();
    let skip = format!("{}/{}", stat.skip_uri, stat.skip_clash);
    sink(&Event::Log {
        level: Level::Info,
        message: format!("内联装载 {kept} 个待测节点（跳过 {skip}）"),
    });

    let mixed = free_port();
    let ctrl = free_port();
    let secret: String = {
        use rand::Rng;
        let b: [u8; 16] = rand::thread_rng().gen();
        b.iter().map(|x| format!("{x:02x}")).collect()
    };

    // 配置整体交给 serde_yaml 序列化：节点名里的引号/反斜杠/CJK 由它转义，
    // 手拼 YAML 在真实免费节点上迟早被一个带 `: #` 的名字打穿。
    let ctrl_addr = format!("127.0.0.1:{ctrl}");
    let mut cfg = serde_yaml::Mapping::new();
    cfg.insert("mixed-port".into(), mixed.into());
    cfg.insert("allow-lan".into(), false.into());
    cfg.insert("mode".into(), "rule".into());
    cfg.insert("log-level".into(), "warning".into());
    cfg.insert("external-controller".into(), ctrl_addr.into());
    cfg.insert("secret".into(), secret.as_str().into());
    let mut entries = Vec::with_capacity(nodes.len());
    for x in &nodes {
        entries.push(serde_yaml::Value::Mapping(x.entry.clone()));
    }
    cfg.insert("proxies".into(), serde_yaml::Value::Sequence(entries));
    let rules = vec![serde_yaml::Value::String("MATCH,DIRECT".into())];
    cfg.insert("rules".into(), serde_yaml::Value::Sequence(rules));
    let cfg_text = serde_yaml::to_string(&serde_yaml::Value::Mapping(cfg))
        .map_err(|e| format!("生成配置失败: {e}"))?;
    let cfg_path = tmp.join("config.yaml");
    std::fs::write(&cfg_path, cfg_text).map_err(|e| format!("写临时配置失败: {e}"))?;

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

    // 待测名单 = 刚装载的内联名单（装箱时已按 top 截断、名字与 add/索引一致）。
    // 不再查 /providers：provider 成员不进扁平 /proxies，旧实现按 provider
    // 名单去打 /proxies/{name}/delay 恒 404，测速永远 alive=0。
    let n = nodes.len();
    sink(&Event::Log {
        level: Level::Info,
        message: format!("实例就绪，共 {} 个节点待测", n),
    });
    sink(&Event::Started {
        total: Some(n as u64),
        message: Some("延迟测试".into()),
    });

    // 逐节点测延迟：名字来自内联装载（scan 已清洗为 ASCII 安全名），
    // 静态条目在扁平 /proxies 里，路由精确匹配。
    let sem = Arc::new(Semaphore::new(app.concurrency.max(1) as usize));
    let timeout = app.timeout_ms;
    let mut set = JoinSet::new();
    for (i, node) in nodes.iter().enumerate() {
        let sem = sem.clone();
        let client = client.clone();
        let base = base.clone();
        let auth = auth.clone();
        let name = node.name.clone();
        let proto = node.proto.clone();
        let url = app.test_url.clone();
        set.spawn(async move {
            let _p = sem.acquire().await;
            let ms = probe_delay(&client, &base, &auth, &name, &url, timeout).await;
            (i, name, proto, ms)
        });
    }

    let mut tested: Vec<TestedNode> = Vec::new();
    let mut done = 0u64;
    while let Some(res) = set.join_next().await {
        if let Ok((_i, name, proto, ms)) = res {
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
                protocol: proto,
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
            // 协议在装载时已定（比索引的 protocol_of 更准），索引只补 source
            if t.protocol.is_empty() {
                t.protocol = info.protocol.clone();
            }
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

// ── 内联装载（测速用） ──────────────────────────────────────────

/// 一个待测节点：API 探测用的名字 + 协议 id + 已验证的内联 Clash 条目。
struct InlineNode {
    name: String,
    proto: String,
    entry: serde_yaml::Mapping,
}

/// 装载统计（只记不炸：坏行数进日志，方便判断源数据质量）。
struct LoadStat {
    skip_uri: usize,
    skip_clash: usize,
}

/// 从 scan 产物装载 `top` 个待测节点，写进临时 config 的 `proxies:`。
///
/// 顺序：URI 行优先 —— `add` 的 good_uri 导出按 `uri_name` 匹配，URI 节点
/// 必须先进入 `--top` 名单才导出得出来；Clash 块补足余量。
/// 解析失败 / mihomo 不支持的协议只跳过该行，绝不连坐整个配置
/// （proxy-provider 原子失败的替代，见文件头注释）。
fn load_test_nodes(dir: &Path, top: u64) -> Result<(Vec<InlineNode>, LoadStat), String> {
    use crate::corecfg::{parse_line, CoreKind};

    let top = top.max(1) as usize;
    let mut out: Vec<InlineNode> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut skip_uri = 0usize;
    let mut skip_clash = 0usize;

    // ① URI 行
    if let Ok(txt) = std::fs::read_to_string(dir.join("nodes_uri.txt")) {
        for line in txt.lines() {
            if out.len() >= top {
                break;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some(parsed) = parse_line(line) else {
                skip_uri += 1;
                continue;
            };
            if !CoreKind::Mihomo.supports(&parsed.proto) {
                skip_uri += 1;
                continue;
            }
            // 名字必须与 add 导出 / 索引完全一致：都取扫描时已清洗的 uri_name；
            // 无 # 片段的行 add 导不出（它按 uri_name 硬匹配），测了也是浪费槽位
            let Some(name) = uri_name(line) else {
                skip_uri += 1;
                continue;
            };
            let Some(entry) = mihomo_entry(&parsed, &name) else {
                skip_uri += 1;
                continue;
            };
            if seen.insert(name.clone()) {
                out.push(InlineNode {
                    name,
                    proto: parsed.proto.clone(),
                    entry,
                });
            }
        }
    }

    // ② Clash 块补足（原样透传，只做结构校验 + 端口字符串纠偏）
    if let Ok(txt) = std::fs::read_to_string(dir.join("nodes_clash.yaml")) {
        let v: serde_yaml::Value = serde_yaml::from_str(&txt).unwrap_or_default();
        if let Some(arr) = v.get("proxies").and_then(|p| p.as_sequence()) {
            for p in arr {
                if out.len() >= top {
                    break;
                }
                let Some(entry) = clash_passthrough(p) else {
                    skip_clash += 1;
                    continue;
                };
                let name = entry
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let ptype = entry
                    .get("type")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                if seen.insert(name.clone()) {
                    out.push(InlineNode {
                        name,
                        proto: ptype,
                        entry,
                    });
                }
            }
        }
    }

    if out.is_empty() {
        return Err(format!(
            "数据目录 {} 没有可测节点（URI 全部解析失败且无 Clash 块），请先运行 scan",
            dir.display()
        ));
    }
    let stat = LoadStat {
        skip_uri,
        skip_clash,
    };
    Ok((out, stat))
}

/// Clash 块原样透传：结构校验 + 端口字符串纠偏。
///
/// 内联配置是「全有或全无」—— 缺 name/type/server 的条目会让 mihomo
/// 整份拒收；端口写成 `"443"` 这种字符串同样拒收，这里统一纠成整数。
fn clash_passthrough(p: &serde_yaml::Value) -> Option<serde_yaml::Mapping> {
    let mut m = p.as_mapping()?.clone();
    if m.get("name")?.as_str()?.trim().is_empty() {
        return None;
    }
    if m.get("type")?.as_str()?.trim().is_empty() {
        return None;
    }
    if m.get("server")?.as_str()?.trim().is_empty() {
        return None;
    }
    let port = m.get("port")?.clone();
    match port {
        serde_yaml::Value::Number(_) => {}
        serde_yaml::Value::String(s) => {
            let n: u16 = s.trim().parse().ok()?;
            m.insert("port".into(), n.into());
        }
        _ => return None,
    }
    Some(m)
}

/// 单个 URI 节点 → mihomo 内联条目。字段命名对齐真实数据：vless 用
/// `servername`，trojan/hysteria2 用 `sni`，TLS 类两种键都给
/// （nodes_clash.yaml 里 anytls 就是双键并存、内核照载不误）。
/// 必备字段缺失 / mihomo 不支持的协议 → None（只跳过这一行）。
fn mihomo_entry(n: &crate::corecfg::ParsedNode, name: &str) -> Option<serde_yaml::Mapping> {
    let mut m = serde_yaml::Mapping::new();
    if n.server.is_empty() || n.port == 0 || name.trim().is_empty() {
        return None;
    }
    m.insert("name".into(), name.into());
    m.insert("type".into(), clash_type(&n.proto)?.into());
    m.insert("server".into(), n.server.as_str().into());
    m.insert("port".into(), n.port.into());
    m.insert("udp".into(), true.into());

    // ── 协议必备字段（缺 = None 跳过；类型错会连累整份配置，绝不赌） ──
    match n.proto.as_str() {
        "ss" => {
            let cipher = n.extra.get("method").or_else(|| n.extra.get("cipher"))?;
            m.insert("cipher".into(), cipher.as_str().into());
            m.insert("password".into(), n.password.as_deref()?.into());
        }
        "vmess" => {
            m.insert("uuid".into(), n.uuid.as_deref()?.into());
            m.insert("alterId".into(), n.aid.into());
            let scy = n.extra.get("scy").map(String::as_str).unwrap_or("auto");
            m.insert("cipher".into(), scy.into());
        }
        "vless" => {
            m.insert("uuid".into(), n.uuid.as_deref()?.into());
            if let Some(flow) = n.flow.as_deref().filter(|s| !s.is_empty()) {
                m.insert("flow".into(), flow.into());
            }
        }
        "trojan" | "hysteria2" | "anytls" => {
            m.insert("password".into(), n.password.as_deref()?.into());
        }
        "tuic" => {
            m.insert("uuid".into(), n.uuid.as_deref()?.into());
            m.insert("password".into(), n.password.as_deref()?.into());
        }
        "socks" | "http" => {
            if let Some(u) = n.user.as_deref().filter(|s| !s.is_empty()) {
                m.insert("username".into(), u.into());
            }
            if let Some(p) = n.password.as_deref().filter(|s| !s.is_empty()) {
                m.insert("password".into(), p.into());
            }
        }
        // hysteria/snell/shadowtls/ssh/wireguard：URI 里低频且字段拿不准，
        // 跳过比赌一把强（赌错 = 内核整份拒收 = alive 归零）。
        _ => return None,
    }

    // hysteria2 的 obfs 是对象 {type,password}，不是字符串
    if n.proto == "hysteria2" {
        if let Some(obfs) = n.extra.get("obfs").filter(|s| !s.is_empty()) {
            let mut om = serde_yaml::Mapping::new();
            om.insert("type".into(), obfs.as_str().into());
            if let Some(pw) = n.extra.get("obfs-password").filter(|s| !s.is_empty()) {
                om.insert("password".into(), pw.as_str().into());
            }
            m.insert("obfs".into(), om.into());
        }
    }

    // ── TLS（trojan/tuic/hysteria2/anytls 由 parse 侧已置 security=tls） ──
    let tls = matches!(n.security.as_deref(), Some("tls") | Some("reality"));
    if tls {
        m.insert("tls".into(), true.into());
        if let Some(sni) = n.sni.as_deref().filter(|s| !s.is_empty()) {
            m.insert("servername".into(), sni.into());
            m.insert("sni".into(), sni.into());
        }
        if n.allow_insecure {
            m.insert("skip-cert-verify".into(), true.into());
        }
        if let Some(fp) = n.fp.as_deref().filter(|s| !s.is_empty()) {
            m.insert("client-fingerprint".into(), fp.into());
        }
        if let Some(alpn) = n.alpn.as_deref().filter(|s| !s.is_empty()) {
            let items: Vec<serde_yaml::Value> = alpn
                .split(',')
                .map(|s| serde_yaml::Value::String(s.trim().to_string()))
                .collect();
            m.insert("alpn".into(), serde_yaml::Value::Sequence(items));
        }
        if n.security.as_deref() == Some("reality") {
            if let Some(pbk) = n.pbk.as_deref().filter(|s| !s.is_empty()) {
                let sid = n.sid.as_deref().unwrap_or_default();
                let mut ro = serde_yaml::Mapping::new();
                ro.insert("public-key".into(), pbk.into());
                ro.insert("short-id".into(), sid.into());
                m.insert("reality-opts".into(), ro.into());
            }
        }
    }

    // ── 传输层：只发射 ws / grpc（真实 URI 里只有这两种）；h2/httpupgrade
    // 的 opts 键名拿不准，赌错类型会被内核拒收，宁可该节点不通。 ──
    match n.network.as_deref() {
        Some("ws") => {
            let mut opts = serde_yaml::Mapping::new();
            if let Some(path) = n.path.as_deref().filter(|s| !s.is_empty()) {
                opts.insert("path".into(), path.into());
            }
            if let Some(host) = n.host.as_deref().filter(|s| !s.is_empty()) {
                let mut headers = serde_yaml::Mapping::new();
                headers.insert("Host".into(), host.into());
                opts.insert("headers".into(), headers.into());
            }
            m.insert("network".into(), "ws".into());
            m.insert("ws-opts".into(), opts.into());
        }
        Some("grpc") => {
            m.insert("network".into(), "grpc".into());
            if let Some(svc) = n.service_name.as_deref().filter(|s| !s.is_empty()) {
                let mut opts = serde_yaml::Mapping::new();
                opts.insert("serviceName".into(), svc.into());
                m.insert("grpc-opts".into(), opts.into());
            }
        }
        _ => {}
    }

    Some(m)
}

/// 协议 id → mihomo `type` 字段（socks 要写成 socks5，其余同名）。
fn clash_type(proto: &str) -> Option<&'static str> {
    Some(match proto {
        "ss" => "ss",
        "vmess" => "vmess",
        "vless" => "vless",
        "trojan" => "trojan",
        "hysteria2" => "hysteria2",
        "tuic" => "tuic",
        "socks" => "socks5",
        "http" => "http",
        "anytls" => "anytls",
        _ => return None,
    })
}

/// 清洗一份外来节点清单（Android 导入 / 托盘订阅共用），返回**保证可被
/// mihomo 载入**的 `proxies:` YAML 文本。
///
/// 存在的原因：mihomo 的 `type: file` provider 对坏行是**原子**的 ——
/// `ss://<uuid>@host?security=tls&encryption=none` 这种（UUID 当 userinfo、没有
/// cipher）会让整个 provider 初始化失败，20 条好节点 + 1 条这种行 = 0 节点
/// （2026-09-26 实测 v1.19.30，日志 `initial proxy provider ... unknown method`）。
/// 症状是「VPN 显示已连接、每个请求都失败」，比直接报错更难排查。
///
/// 走的是和 `test_core` **同一条**已被实测跑通的路（`parse_line` → `mihomo_entry`）：
/// 解析不了 / 字段不全的行只丢自己。输入是 Clash `proxies:` 就结构校验后透传，
/// 是链接文本就逐行转成内联条目。同名只留首条（内联 proxies 重名整份拒收）。
///
/// `kept == 0` 时 `yaml` 为空串 —— 调用方**必须**保留用户原文（那份文件可能是
/// `proxy-providers: type: http` 的订阅配置，不是节点清单，覆写等于删掉用户的订阅）。
#[derive(Debug, Clone, Serialize)]
pub struct Sanitized {
    /// 可直接写进 provider 文件的 YAML；`kept == 0` 时为空串
    pub yaml: String,
    pub kept: usize,
    pub dropped: usize,
    pub total: usize,
}

pub fn sanitize_nodes_text(text: &str) -> Sanitized {
    use crate::corecfg::CoreKind;

    let mut entries: Vec<serde_yaml::Mapping> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut total = 0usize;
    let mut saw_proxies = false;

    // ① Clash `proxies:` 块：结构校验 + 端口纠偏后原样透传
    if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(text) {
        if let Some(arr) = v.get("proxies").and_then(|p| p.as_sequence()) {
            saw_proxies = true;
            for p in arr {
                total += 1;
                if let Some(entry) = clash_passthrough(p) {
                    let name = entry
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if seen.insert(name) {
                        entries.push(entry);
                    }
                }
            }
        }
    }

    // ② URI 链接文本：逐行 parse_line → mihomo 内联条目
    if !saw_proxies {
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            total += 1;
            let Some(parsed) = crate::corecfg::parse_line(l) else {
                continue;
            };
            if !CoreKind::Mihomo.supports(&parsed.proto) {
                continue;
            }
            let name = uri_name(l).unwrap_or_else(|| parsed.display_name());
            let cleaned = clean_label(&name);
            if cleaned.is_empty() || !seen.insert(cleaned.clone()) {
                continue;
            }
            if let Some(entry) = mihomo_entry(&parsed, &cleaned) {
                entries.push(entry);
            } else {
                seen.remove(&cleaned);
            }
        }
    }

    let kept = entries.len();
    let yaml = if kept == 0 {
        String::new()
    } else {
        let doc = serde_yaml::to_string(&serde_yaml::Value::Mapping(
            serde_yaml::Mapping::from_iter(vec![(
                serde_yaml::Value::from("proxies"),
                serde_yaml::Value::Sequence(
                    entries.into_iter().map(serde_yaml::Value::Mapping).collect(),
                ),
            )]),
        ))
        .unwrap_or_default()
    };
    Sanitized {
        yaml,
        kept,
        dropped: total.saturating_sub(kept),
        total,
    }
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
        good_uri = filter_uri_by_names(&txt, &alive_names);
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
                let mut taken: HashSet<String> = HashSet::new();
                for p in arr {
                    if let Some(n) = p.get("name").and_then(|x| x.as_str()) {
                        if alive_names.contains(n) && taken.insert(n.to_string()) {
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

/// 从 `nodes_uri.txt` 原文里挑出 name 命中 `names` 的行（`add` 的 good_uri 导出）。
///
/// 按 name 硬匹配，所以导出行数能对上 kept 的前提是源文件里 name 全局唯一
/// —— `dedup_scanned` 负责保证这条不变量，这里再兜一层重复行。
fn filter_uri_by_names(txt: &str, names: &HashSet<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for line in txt.lines() {
        let Some(name) = uri_name(line) else { continue };
        if names.contains(&name) && seen.insert(name) {
            out.push(line.to_string());
        }
    }
    out
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

    #[test]
    fn load_test_nodes_skips_broken_lines() {
        let dir = std::env::temp_dir().join("ghboost_load_test_nodes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 第一行就是当初打死整个 provider 的坏 ss：UUID 当 userinfo、没有 method
        let bad = "ss://15298f41-e80b-463a-b85b-0c903258a1c8@162.159.1.33:443";
        let bad = format!("{bad}?security=tls&encryption=none#meli_proxyy");
        let good = "vless://11111111-2222-3333-4444-555555555555@1.2.3.4:443";
        let good = format!("{good}?security=tls&type=ws&path=/ws#good_vless");
        std::fs::write(dir.join("nodes_uri.txt"), format!("{bad}\n{good}\n")).unwrap();
        // 一个正常 Clash 块（端口故意写成字符串）+ 一个缺 name 的坏块
        let ok = "proxies:\n  - name: clash_ok\n    type: ss\n    server: 5.6.7.8\n";
        let ok = format!("{ok}    port: \"8388\"\n    cipher: aes-256-gcm\n");
        let broken = "  - type: trojan\n    server: 9.9.9.9\n    port: 443\n    password: x\n";
        std::fs::write(dir.join("nodes_clash.yaml"), format!("{ok}{broken}")).unwrap();

        let (nodes, stat) = load_test_nodes(&dir, 10).unwrap();
        assert_eq!(nodes.len(), 2, "坏行只损失自己，好行保留");
        assert_eq!(nodes[0].name, "good_vless", "URI 名字与 add 导出一致");
        assert_eq!(nodes[0].proto, "vless");
        assert_eq!(nodes[1].name, "clash_ok");
        assert_eq!(nodes[1].proto, "ss");
        assert_eq!(stat.skip_uri, 1, "坏 ss 行计数跳过");
        assert_eq!(stat.skip_clash, 1, "缺 name 的块计数跳过");
        let port = nodes[1].entry.get("port").and_then(|v| v.as_u64());
        assert_eq!(port, Some(8388), "字符串端口被纠成整数");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_test_nodes_uri_first_and_top_truncates() {
        let dir = std::env::temp_dir().join("ghboost_load_top");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let uri = "vless://11111111-2222-3333-4444-555555555555@1.2.3.4:443#u1\n";
        std::fs::write(dir.join("nodes_uri.txt"), uri).unwrap();
        let clash = "proxies:\n  - name: c1\n    type: ss\n    server: 5.6.7.8\n    port: 8388\n";
        std::fs::write(dir.join("nodes_clash.yaml"), clash).unwrap();

        // top=1 → URI 优先（add 的 good_uri 导出依赖 URI 节点在名单内）
        let (nodes, stat) = load_test_nodes(&dir, 1).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "u1");
        assert_eq!(stat.skip_clash, 0, "被 top 截断不算跳过");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mihomo_entry_emits_reality_keys_and_guards() {
        let n = crate::corecfg::ParsedNode {
            name: "x".into(),
            proto: "vless".into(),
            server: "a.example.com".into(),
            port: 443,
            uuid: Some("11111111-2222-3333-4444-555555555555".into()),
            security: Some("reality".into()),
            sni: Some("cdn.example.com".into()),
            pbk: Some("pubkey".into()),
            flow: Some("xtls-rprx-vision".into()),
            ..Default::default()
        };
        let m = mihomo_entry(&n, "node-a").expect("vless reality emits");
        assert_eq!(m.get("type").and_then(|v| v.as_str()), Some("vless"));
        assert_eq!(
            m.get("servername").and_then(|v| v.as_str()),
            Some("cdn.example.com")
        );
        assert_eq!(
            m.get("flow").and_then(|v| v.as_str()),
            Some("xtls-rprx-vision")
        );
        let ro = m.get("reality-opts").and_then(|v| v.get("public-key"));
        assert_eq!(ro.and_then(|v| v.as_str()), Some("pubkey"));
        assert_eq!(m.get("tls").and_then(|v| v.as_bool()), Some(true));

        // 缺 uuid 必须整条跳过（None），不能发射半截条目
        let missing = crate::corecfg::ParsedNode {
            proto: "vless".into(),
            server: "a.example.com".into(),
            port: 443,
            ..Default::default()
        };
        assert!(mihomo_entry(&missing, "no-uuid").is_none());
        assert_eq!(clash_type("socks"), Some("socks5"));
        assert_eq!(clash_type("wireguard"), None);
    }

    #[test]
    fn dedup_scanned_keeps_names_globally_unique() {
        // 同名不同 server：实测 scan 产物里 `US-VPNine1` 出现过 3 条，
        // add 按 name 硬匹配导出会多写 2 行，且导出的配置带重名被 mihomo 拒收
        let mut uris: Vec<String> = vec![
            "ss://YWVzLTI1Ni1nY206cGFzcw==@1.1.1.1:8388#dup".into(),
            "ss://YWVzLTI1Ni1nY206cGFzcw==@2.2.2.2:8388#dup".into(),
            "ss://YWVzLTI1Ni1nY206cGFzcw==@3.3.3.3:8388#other".into(),
            "ss://YWVzLTI1Ni1nY206cGFzcw==@1.1.1.1:8388#other".into(),
        ];
        // clash：与 URI 同名的、自身重名的、缺 name 的
        let yaml = "proxies:\n\
                    - {name: dup, type: ss, server: 9.9.9.9, port: 1}\n\
                    - {name: c1, type: ss, server: 8.8.8.8, port: 2}\n\
                    - {name: c1, type: ss, server: 7.7.7.7, port: 3}\n\
                    - {type: ss, server: 6.6.6.6, port: 4}\n";
        let doc = serde_yaml::from_str::<serde_yaml::Value>(yaml).unwrap();
        let mut blocks: Vec<serde_yaml::Value> = doc["proxies"].as_sequence().unwrap().clone();

        dedup_scanned(&mut uris, &mut blocks);
        assert_eq!(uris.len(), 2, "同名只留首行");
        assert_eq!(uri_name(&uris[0]).as_deref(), Some("dup"));
        assert_eq!(uri_name(&uris[1]).as_deref(), Some("other"));
        let names: Vec<String> = blocks
            .iter()
            .map(|p| p["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(names, vec!["c1".to_string()], "跨池与自身重名都剔除");
    }

    #[test]
    fn filter_uri_by_names_exports_exactly_kept_lines() {
        let txt = "ss://YWVzLTI1Ni1nY206cGFzcw==@1.1.1.1:8388#a\n\
                   ss://YWVzLTI1Ni1nY206cGFzcw==@2.2.2.2:8388#b\n\
                   ss://YWVzLTI1Ni1nY206cGFzcw==@3.3.3.3:8388#c\n";
        let names: HashSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let got = filter_uri_by_names(txt, &names);
        assert_eq!(got.len(), 2, "导出行数 == kept");
        assert!(!got.iter().any(|l| l.contains("3.3.3.3")));

        // 旧数据目录（未按名去重）也不会导出重名行
        let dup = "ss://YWVzLTI1Ni1nY206cGFzcw==@1.1.1.1:8388#a\n\
                   ss://YWVzLTI1Ni1nY206cGFzcw==@2.2.2.2:8388#a\n";
        let got2 = filter_uri_by_names(dup, &names);
        assert_eq!(got2.len(), 1, "同名重复行只导出首条");
    }

    // ── sanitize_nodes_text（Android 导入 / 托盘订阅共用）──────────

    const POISON: &str = "ss://15298f41-e80b-463a-b85b-0c903258a1c8@162.159.1.33:443\
                           ?security=tls&encryption=none#meli_proxyy";

    #[test]
    fn sanitize_links_drops_the_kernel_poison_line() {
        // 实测 v1.19.30：这条行会让**整个** file provider 初始化失败（0 节点），
        // 而同批的其它行都是好的。洗完必须只剩好的那些。
        let good = "ss://YWVzLTI1Ni1nY206cGFzcw==@1.2.3.4:8388#S";
        let txt = format!("{good}\n{POISON}\n");
        let r = sanitize_nodes_text(&txt);
        assert_eq!(r.total, 2);
        assert_eq!(r.kept, 1, "坏行必须被剔掉");
        assert_eq!(r.dropped, 1);
        assert!(r.yaml.starts_with("proxies:"), "{}", r.yaml);
        assert!(!r.yaml.contains("162.159.1.33"), "坏行不能进 YAML: {}", r.yaml);
        assert!(r.yaml.contains("1.2.3.4"));
        // mihomo 内联 proxies 重名整份拒收 → 同名只留首条
        let dup = format!("{good}\nss://YWVzLTI1Ni1nY206cGFzcw==@5.6.7.8:8388#S\n");
        assert_eq!(sanitize_nodes_text(&dup).kept, 1, "同名只留首条");
    }

    #[test]
    fn sanitize_keeps_original_when_not_a_node_list() {
        // 出厂模板教用户填的就是这种：`proxy-providers: type: http` 的订阅配置。
        // 洗不出节点时必须报 kept=0（yaml 空串），让调用方保留原文 ——
        // 覆写等于替用户把订阅删了。
        let sub = "proxy-providers:\n  ghboost:\n    type: http\n    url: \"https://x/y\"\n";
        let r = sanitize_nodes_text(sub);
        assert_eq!(r.kept, 0);
        assert_eq!(r.dropped, 0, "不是节点清单就不算「丢节点」");
        assert!(r.yaml.is_empty());
    }

    #[test]
    fn sanitize_passthrough_clash_yaml_with_bad_entry() {
        // Clash 块走结构校验：缺 server/port 的条目丢掉，其余原样透传
        // （port 是字符串 "443" 这种也要纠偏 —— 真实订阅里到处都是）。
        let yaml = "proxies:\n\
                    - {name: a, type: ss, server: 1.2.3.4, port: '8388', cipher: aes-256-gcm, password: p}\n\
                    - {name: b, type: ss, server: 5.6.7.8, port: 9000, cipher: aes-256-gcm, password: p}\n\
                    - {name: broken, type: ss}\n";
        let r = sanitize_nodes_text(yaml);
        assert_eq!(r.kept, 2, "缺 server/port 的条目丢掉，其余透传");
        assert_eq!(r.dropped, 1);
        assert!(r.yaml.contains("port: 8388"), "字符串端口要纠偏: {}", r.yaml);
        assert!(!r.yaml.contains("'8388'"));
    }
}
