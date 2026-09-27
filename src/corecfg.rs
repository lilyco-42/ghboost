//! corecfg — 多内核配置生成（对标 NekoBox：mihomo / xray / sing-box · 全协议）。
//!
//! 分工：
//! - **mihomo**：不经本模块 —— 它的 proxy-provider 原生吃 v2rayN 链接 / base64 订阅，
//!   自己解析比我们再写一遍可靠（沿用 web.rs「一行协议解析都不写」的原则）。
//! - **xray / sing-box**：不会解 share URI，解析只能我们做。本模块负责
//!   「share URI / Clash YAML → 结构化节点 → 目标内核 JSON 配置」。
//!
//! 纯函数、零 IO：wasm 可编、`cargo test` 即验。配置是否为真内核接受，
//! 由 CI 里跑 `xray run -test` / `sing-box check` 对 fixture 兜底（见 workflows）。
//!
//! 协议矩阵与内核选择规则的权威说明见仓库根 `MULTI-CORE-PLAN.md`。

use std::collections::BTreeMap;

use base64::Engine;
use serde_json::json;

/// 三个可选内核。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoreKind {
    #[default]
    Mihomo,
    Xray,
    SingBox,
}

impl CoreKind {
    /// 解析 UI / CLI / FFI 传来的字串（大小写不敏感；singbox / sing-box / sing_box 都认）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "mihomo" | "clash" | "clash-meta" | "meta" => Some(Self::Mihomo),
            "xray" | "xray-core" => Some(Self::Xray),
            "sing-box" | "singbox" | "sing-box-core" => Some(Self::SingBox),
            _ => None,
        }
    }

    /// 稳定 id（配置、API、CLI 用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mihomo => "mihomo",
            Self::Xray => "xray",
            Self::SingBox => "sing-box",
        }
    }

    /// 展示名（面板 / 文案用）。
    pub fn display(self) -> &'static str {
        match self {
            Self::Mihomo => "Mihomo (Clash.Meta)",
            Self::Xray => "Xray-core",
            Self::SingBox => "sing-box",
        }
    }

    /// 该内核支持哪些协议（proto 为小写 id）。
    ///
    /// ❌ 项不是缺陷是事实：xray 上游没有 hysteria2/tuic/anytls 实现；
    /// mihomo 对 shadowtls/ssh 以实测为准，暂归 sing-box。
    pub fn supports(self, proto: &str) -> bool {
        match self {
            Self::Mihomo => matches!(
                proto,
                "ss" | "vmess"
                    | "vless"
                    | "trojan"
                    | "hysteria"
                    | "hysteria2"
                    | "tuic"
                    | "anytls"
                    | "wireguard"
                    | "snell"
                    | "socks"
                    | "http"
            ),
            Self::Xray => matches!(
                proto,
                "ss" | "vmess" | "vless" | "trojan" | "socks" | "http"
            ),
            Self::SingBox => matches!(
                proto,
                "ss" | "vmess"
                    | "vless"
                    | "trojan"
                    | "hysteria2"
                    | "tuic"
                    | "anytls"
                    | "shadowtls"
                    | "socks"
                    | "http"
                    | "ssh"
            ),
        }
    }

    /// 单节点的推荐内核（auto 规则，见 MULTI-CORE-PLAN.md）。
    pub fn auto_for(proto: &str) -> CoreKind {
        match proto {
            "hysteria2" | "tuic" | "anytls" | "shadowtls" | "ssh" => Self::SingBox,
            // hysteria1 被 sing-box 移除、xray 也没有 → 只剩 mihomo；snell/wireguard 同。
            "hysteria" | "snell" | "wireguard" => Self::Mihomo,
            // VLESS（含 Reality / XTLS-flow）在 xray 上实现最完整。
            "vless" => Self::Xray,
            // 规则引擎 + GEOIP,TW 直连是 mihomo 的主场。
            "ss" | "vmess" | "trojan" | "socks" | "http" => Self::Mihomo,
            _ => Self::SingBox,
        }
    }
}

/// 解析后的结构化节点（三内核共用的中间表示）。
#[derive(Debug, Clone, Default)]
pub struct ParsedNode {
    /// 节点名（URI fragment / Clash name；空则回落 server:port）。
    pub name: String,
    /// 小写协议 id：ss/vmess/vless/trojan/hysteria/hysteria2/tuic/socks/http/
    /// anytls/shadowtls/ssh/wireguard/snell
    pub proto: String,
    pub server: String,
    pub port: u16,
    /// uuid（vmess / vless / tuic）。
    pub uuid: Option<String>,
    /// 密码类（trojan / ss / hysteria2 / anytls / shadowtls / ssh …）。
    pub password: Option<String>,
    /// 用户名（socks / http / ssh；tuic 为 uuid 本体另存 `uuid`）。
    pub user: Option<String>,
    /// vless：none|tls|reality；vmess/trojan：tls|none（缺省按协议语义推断）。
    pub security: Option<String>,
    /// vless flow（xtls-rprx-vision 等）。
    pub flow: Option<String>,
    /// 传输层原始串：tcp|ws|grpc|h2|http|httpupgrade|kcp|quic …
    /// 注意 kcp/quic 的 host 是 v2rayN 的伪装类型字段、path 是 seed/key，不是域名/路径。
    /// quic 仅解析：xray 26+ 已移除该传输（发射报错），sing-box 需配 TLS。
    pub network: Option<String>,
    pub sni: Option<String>,
    pub alpn: Option<String>,
    /// uTLS 指纹（chrome / firefox / safari …）。
    pub fp: Option<String>,
    /// Reality public key。
    pub pbk: Option<String>,
    /// Reality short id。
    pub sid: Option<String>,
    /// Reality spiderX。
    pub spx: Option<String>,
    pub path: Option<String>,
    pub host: Option<String>,
    /// gRPC service name。
    pub service_name: Option<String>,
    /// vmess alterId。
    pub aid: u32,
    pub allow_insecure: bool,
    /// vless encryption（缺省 none）。
    pub encryption: Option<String>,
    /// 协议私有参数（ss 的 method/cipher、shadowsocks-obfs、tuic 的
    /// congestion_control、hysteria2 的 obfs …），key 全小写。
    pub extra: BTreeMap<String, String>,
}

impl ParsedNode {
    /// 展示名兜底。
    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            format!("{}:{}", self.server, self.port)
        } else {
            self.name.clone()
        }
    }

    fn ss_method(&self) -> Option<&String> {
        self.extra
            .get("method")
            .or_else(|| self.extra.get("cipher"))
    }
}

/// 整份订阅选一个内核（auto 模式）：全覆盖者优先 mihomo → sing-box → xray。
///
/// 顺序理由：mihomo 规则引擎最全（GEOIP,TW 直连、fake-ip），默认保它；
/// 协议超出 mihomo 覆盖时用 sing-box（三者协议面最广）；xray 只在
/// 前两者都不全覆盖而 xray 能覆盖时胜出（实践中几乎不会发生）。
pub fn auto_core(nodes: &[ParsedNode]) -> CoreKind {
    if nodes.is_empty() {
        return CoreKind::Mihomo;
    }
    if nodes.iter().all(|n| CoreKind::Mihomo.supports(&n.proto)) {
        return CoreKind::Mihomo;
    }
    if nodes.iter().all(|n| CoreKind::SingBox.supports(&n.proto)) {
        return CoreKind::SingBox;
    }
    if nodes.iter().all(|n| CoreKind::Xray.supports(&n.proto)) {
        return CoreKind::Xray;
    }
    // 混排且无人全覆盖：交给协议面最广的 sing-box，丢弃的节点单独汇报。
    CoreKind::SingBox
}

/// 按内核过滤节点，返回（保留，丢弃原因）。
pub fn filter_nodes(nodes: Vec<ParsedNode>, kind: CoreKind) -> (Vec<ParsedNode>, Vec<String>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for n in nodes {
        if kind.supports(&n.proto) {
            kept.push(n);
        } else {
            dropped.push(format!(
                "{}（{} 不支持 {} 协议）",
                n.display_name(),
                kind.as_str(),
                n.proto
            ));
        }
    }
    (kept, dropped)
}

/// 发射参数（监听端口按宿主环境传入：桌面 / Android 各自决定）。
#[derive(Debug, Clone)]
pub struct EmitOptions {
    pub socks_port: u16,
    pub http_port: u16,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            socks_port: 1080,
            http_port: 1081,
        }
    }
}

/// 生成目标内核配置（JSON 文本）。mihomo 不经发射 —— 它直接吃 provider。
pub fn emit(kind: CoreKind, nodes: &[ParsedNode], opts: &EmitOptions) -> Result<String, String> {
    match kind {
        CoreKind::Mihomo => Err("mihomo 走 proxy-provider 直接吃原始订阅，不经本模块发射".into()),
        CoreKind::Xray => emit_xray(nodes, opts),
        CoreKind::SingBox => emit_singbox(nodes, opts),
    }
}

// ─────────────────────────────────────────────────────────────
// share URI 解析
// ─────────────────────────────────────────────────────────────

/// [parse_line] 建模了的协议前缀（小写，不含 `://`）。
///
/// 与 [parse_line] 的 match 分支一一对应 —— 改那边就要改这里，否则
/// [modelled_link] 会开始替内核做它做不了的判断。
pub const MODELLED_SCHEMES: &[&str] = &[
    "ss",
    "vmess",
    "vless",
    "trojan",
    "hysteria",
    "hysteria2",
    "hy2",
    "hy",
    "tuic",
    "socks",
    "socks5",
    "socks5h",
    "http",
    "https",
    "wireguard",
    "wg",
    "anytls",
    "shadowtls",
    "ssh",
    "snell",
];

/// 掐掉行首的 UTF-8 BOM（U+FEFF）。
///
/// 为什么非做不可：Windows 上记事本 / PowerShell 的 `>` 重定向存出来的文本
/// 默认带 BOM，而 `str::trim()` **不**把 U+FEFF 当空白（Rust 认的空白是
/// Unicode White_Space，FEFF 是 Cf 类格式字符）。于是第一行的 scheme 变成
/// `\u{feff}ss`，[modelled_link] 判它「不是链接」、[parse_line] 也解不出来 ——
/// 节点**静默消失**，连 `dropped` 都不计（实测 2026-09-26：一份 21 行的清单
/// 被报成「kept 19, dropped 1 of 20」，少的那个数就是 BOM 吃掉的首行）。
/// 内核自己解析链接时是吃 BOM 的，所以只有我们这条路上会丢人。
pub fn strip_bom(line: &str) -> &str {
    line.trim().trim_start_matches('\u{feff}').trim()
}

/// 这行是不是我们**建模过**的节点链接。
///
/// 为什么不能直接拿「[parse_line] 解析不了」当「内核一定不认」：mihomo 支持的
/// 协议比我们建模的多（`ssr` / `juicity` / `mieru`），内核本来能用的节点会被
/// 连坐删掉 —— 那是自己制造新故障。只有**我们确实建模了**这个协议，才轮到
/// 「我们读不动 → 内核也读不动」这个推理（实测 mihomo v1.19.30 对
/// `ss://<uuid>@host?security=tls&encryption=none` 是整份 provider 拒收）。
pub fn modelled_link(line: &str) -> bool {
    match strip_bom(line).split_once("://") {
        Some((scheme, _)) => MODELLED_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// 解析一行 v2rayN / NekoBox 风格 share URI。
///
/// 支持：ss / vmess / vless / trojan / hysteria / hysteria2(hy2) / tuic /
/// socks(socks5/socks5h) / http(s) / anytls / shadowtls / ssh / wireguard(wg) / snell。
pub fn parse_line(raw: &str) -> Option<ParsedNode> {
    let line = strip_bom(raw);
    let (scheme, rest) = line.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let mut n = match scheme.as_str() {
        "ss" => parse_ss(rest)?,
        "vmess" => parse_vmess(rest)?,
        "hysteria2" | "hy2" => parse_std("hysteria2", rest)?,
        "hysteria" | "hy" => parse_std("hysteria", rest)?,
        "tuic" => parse_tuic(rest)?,
        "socks" | "socks5" | "socks5h" => parse_std("socks", rest)?,
        "http" | "https" => parse_std("http", rest)?,
        "wireguard" | "wg" => parse_std("wireguard", rest)?,
        "vless" | "trojan" | "anytls" | "shadowtls" | "ssh" | "snell" => parse_std(&scheme, rest)?,
        _ => return None,
    };
    if n.name.is_empty() {
        n.name = n.display_name();
    }
    Some(n)
}

/// URI 主体切分：`authority[?query][#frag]`。
///
/// authority **不**按 `/` 切 —— std base64 字母表含 `/`，ss 全体 base64 形态
/// 的主体里可能带斜杠，一切就解出残缺 host。
struct Parts {
    authority: String,
    query: String,
    frag: String,
}

fn parts_of(rest: &str) -> Parts {
    let (before_frag, frag) = match rest.split_once('#') {
        Some((a, b)) => (a, b.to_string()),
        None => (rest, String::new()),
    };
    let (authority, query) = match before_frag.split_once('?') {
        Some((a, q)) => (a, q.to_string()),
        None => (before_frag, String::new()),
    };
    Parts {
        // 尾斜杠**不**在这里 trim —— 旧式 ss:// 整段 authority 是 base64，末字符可能
        // 是合法的 '/'，trim 会吃数据；host 的尾斜杠交给 split_hostport 处理。
        authority: authority.to_string(),
        query,
        frag,
    }
}

/// query → 小写 key 的 map（value 与 key 都做 percent 解码）。
fn qmap(query: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = match kv.split_once('=') {
            Some((k, v)) => (k, v),
            None => (kv, ""),
        };
        m.insert(pd(k).to_ascii_lowercase(), pd(v));
    }
    m
}

fn pd(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .to_string()
}

/// 容错 base64：自动补齐 padding，std / url-safe 双轨试。
fn b64_flex(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let stripped = t.trim_end_matches('=');
    let padded = match stripped.len() % 4 {
        0 => stripped.to_string(),
        m => format!("{}{}", stripped, "=".repeat(4 - m)),
    };
    if let Ok(d) = base64::engine::general_purpose::STANDARD.decode(&padded) {
        if let Ok(x) = String::from_utf8(d) {
            return Some(x);
        }
    }
    if let Ok(d) = base64::engine::general_purpose::URL_SAFE.decode(&padded) {
        if let Ok(x) = String::from_utf8(d) {
            return Some(x);
        }
    }
    None
}

/// shadowsocks 加密方式白名单（小写规范形）。
///
/// 存在的理由：ss 的 method 认不出来的后果**不是这一行坏掉**，而是 mihomo 整份
/// file provider 初始化失败 → 0 节点（实测 v1.19.30：
/// `initial proxy provider subscription error: proxy 19 error: ss 162.159.1.33:443
/// cipher: ... unknown method`）。所以「我们解出来的 method 是不是真加密方式」
/// 必须是显式契约，不能靠「base64 恰好解不出 / 恰好没有冒号」这种巧合 ——
/// 那两条路一旦因为任何改动变了（比如 base64 改用 `from_utf8_lossy`），
/// 就会解出一个垃圾 method 混进配置，重新变成整批 0 节点且毫无线索。
///
/// **列表不是照抄文档，是逐个跑出来的**（mihomo v1.19.30 `mihomo -t -f`，
/// 2026-09-27 逐个 31 个候选跑完，见 FREEVPN-TEST 报告）。实测结果里有三处
/// 反直觉的，都写在这里以免下次有人「按常识」把它们加回去：
/// 1. **大小写敏感**：`AES-256-GCM` / `Chacha20-Ietf-Poly1305` 一律
///    `unknown method`。所以匹配必须大小写不敏感、同时**把 method 归一化成
///    小写**再进配置（[canon_cipher]）—— 只查不改等于放行致命值。
/// 2. `plain` / `rc4` / `dummy` / `bf-cfb` / `des-cfb` / `idea-cfb` /
///    `aes-*-ofb` / `chacha20-poly1305`（少 ietf）/ `camellia-128-cfb`
///    **都不支持**。`none` 才是「不加密」，没有 `plain` 这个别名。
/// 3. `xchacha20` 与 `aes-*-ccm` 支持，但 `2022-blake3-chacha8-poly1305`
///    也支持（2022 规范的 chacha8 那一支，容易漏）。
///
/// 代价取舍：白名单漏一个 = 那种节点被我们自己剔掉（而不是交给内核），
/// 与 [modelled_link] 里 ssr / juicity 的同款取舍。反过来白名单多一个
/// = 内核整份 provider 炸掉，代价大得多，所以这份名单按「实测通过」逐条列全。
const SS_CIPHERS: &[&str] = &[
    // AEAD（gcm / ccm）
    "aes-128-gcm",
    "aes-192-gcm",
    "aes-256-gcm",
    "aes-128-ccm",
    "aes-192-ccm",
    "aes-256-ccm",
    "chacha20-ietf-poly1305",
    "xchacha20-ietf-poly1305",
    // Shadowsocks 2022（密钥必须是定长 base64，长度不对是另一类错误，
    // 别和「method 不认识」混为一谈）
    "2022-blake3-aes-128-gcm",
    "2022-blake3-aes-256-gcm",
    "2022-blake3-chacha20-poly1305",
    "2022-blake3-chacha8-poly1305",
    // 旧式 stream
    "aes-128-cfb",
    "aes-192-cfb",
    "aes-256-cfb",
    "aes-128-ctr",
    "aes-192-ctr",
    "aes-256-ctr",
    "chacha20",
    "chacha20-ietf",
    "xchacha20",
    "rc4-md5",
    "none",
];

/// method / cipher 归一化成内核认的规范形（小写、去首尾空白）。
///
/// 必须**归一化后再存进配置**，不能只拿来比对：内核大小写敏感
/// （实测 `AES-256-GCM` 直接 `unknown method`），只查不改等于放行致命值 ——
/// 那正是这个白名单要消灭的整批 0 节点。
pub(crate) fn canon_cipher(m: &str) -> String {
    m.trim().to_ascii_lowercase()
}

/// method / cipher 是不是白名单里的加密方式（比的是归一化后的形态）。
pub(crate) fn known_cipher(m: &str) -> bool {
    SS_CIPHERS.contains(&canon_cipher(m).as_str())
}

/// `[2001:db8::1]:443` / `host:443` / 未括号 v6 末段端口。
fn split_hostport(s: &str) -> Option<(String, u16)> {
    // `host:443/`（订阅里偶见的尾斜杠）在这里吸收 —— authority 本体不能动。
    let s = s.trim().trim_end_matches('/');
    if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':')?.trim().parse().ok()?;
        if h.is_empty() || port == 0 {
            return None;
        }
        return Some((h.to_string(), port));
    }
    let (h, p) = s.rsplit_once(':')?;
    let port: u16 = p.trim().parse().ok()?;
    if h.is_empty() || port == 0 {
        return None;
    }
    Some((h.to_string(), port))
}

fn opt(s: Option<String>) -> Option<String> {
    s.filter(|x| !x.trim().is_empty())
}

/// 通用形态：`proto://userinfo@host:port[?query][#name]`。
///
/// userinfo 语义按协议分：
/// - vless / wireguard：uuid
/// - socks / http / ssh：`user[:pass]`（pass 可含冒号，user 不会）
/// - trojan / hysteria(2) / anytls / shadowtls / snell：整段即密码
fn parse_std(proto: &str, rest: &str) -> Option<ParsedNode> {
    let parts = parts_of(rest);
    let q = qmap(&parts.query);
    let (userinfo, hostport) = match parts.authority.rsplit_once('@') {
        Some((u, hp)) => (u, hp),
        None => ("", parts.authority.as_str()),
    };
    let (server, port) = split_hostport(hostport)?;

    let mut n = ParsedNode {
        name: pd(&parts.frag),
        proto: proto.to_string(),
        server,
        port,
        ..Default::default()
    };

    let userinfo = pd(userinfo);
    match proto {
        "vless" | "wireguard" => {
            n.uuid = opt(Some(userinfo));
        }
        "socks" | "http" | "ssh" => {
            if let Some((u, p)) = userinfo.split_once(':') {
                n.user = opt(Some(u.to_string()));
                n.password = opt(Some(p.to_string()));
            } else {
                n.user = opt(Some(userinfo));
            }
        }
        _ => {
            n.password = opt(Some(userinfo));
        }
    }

    apply_query(&mut n, &q);
    if proto == "vless" {
        n.encryption = opt(q
            .get("encryption")
            .cloned()
            .or_else(|| Some("none".to_string())));
    }
    apply_name_defaults(&mut n, proto);
    // 必备字段校验：没有 server/port 的解析结果是废节点，直接判失败。
    if n.server.is_empty() || n.port == 0 {
        return None;
    }
    Some(n)
}

/// 把 query 里的通用 / 协议私有参数落到结构体。
fn apply_query(n: &mut ParsedNode, q: &BTreeMap<String, String>) {
    for (k, v) in q {
        match k.as_str() {
            "security" => n.security = opt(Some(v.clone())),
            "type" | "net" => n.network = opt(Some(v.clone())),
            "sni" | "servername" | "server-name" => n.sni = opt(Some(v.clone())),
            "alpn" => n.alpn = opt(Some(v.clone())),
            "fp" | "fingerprint" | "client-fingerprint" => n.fp = opt(Some(v.clone())),
            "pbk" | "publickey" | "public-key" => n.pbk = opt(Some(v.clone())),
            "sid" | "shortid" | "short-id" => n.sid = opt(Some(v.clone())),
            "spx" | "spiderx" | "spider-x" => n.spx = opt(Some(v.clone())),
            "path" => n.path = opt(Some(v.clone())),
            "host" => n.host = opt(Some(v.clone())),
            "servicename" | "service-name" | "grpcservicename" => {
                n.service_name = opt(Some(v.clone()))
            }
            "flow" => n.flow = opt(Some(v.clone())),
            "encryption" => n.encryption = opt(Some(v.clone())),
            "allowinsecure" | "insecure" | "skip-cert-verify" => {
                n.allow_insecure = matches!(v.as_str(), "1" | "true" | "yes");
            }
            _ => {
                n.extra.insert(k.clone(), v.clone());
            }
        }
    }
    // shadowsocks 系列的加密方式也可能在 query（`?method=` / `?cipher=`）。
    if let Some(m) = q.get("method").or_else(|| q.get("cipher")) {
        if let Some(m) = opt(Some(m.clone())) {
            n.extra.insert("method".to_string(), m);
        }
    }
}

/// 名字与安全默认值的协议语义补全。
fn apply_name_defaults(n: &mut ParsedNode, proto: &str) {
    if n.name.is_empty() {
        n.name = n.display_name();
    }
    // trojan / tuic / hysteria2 / anytls / shadowtls / hysteria 语义上恒有 TLS。
    let tls_default = matches!(
        proto,
        "trojan" | "tuic" | "hysteria" | "hysteria2" | "anytls" | "shadowtls"
    );
    if tls_default && n.security.is_none() {
        n.security = Some("tls".to_string());
    }
    if proto == "vless" && n.security.is_none() {
        n.security = Some("none".to_string());
    }
    // ws 类传输没给 network 时按 ws 处理太激进 —— 缺省 tcp（显式 path 才升级）。
    if n.network.is_none() && n.path.is_some() {
        n.network = Some("ws".to_string());
    }
    // tls/reality 节点没 sni 时回落 host（ws+tls 场景 host 头通常就是域名）。
    // kcp/quic 例外：v2rayN 约定 host 是伪装类型（srtp 之类），当 sni 会发出去一个
    // 假域名，服务端证书校验必挂 —— 不回落。
    if n.sni.is_none()
        && matches!(n.security.as_deref(), Some("tls") | Some("reality"))
        && !matches!(
            n.network.as_deref(),
            Some("kcp") | Some("mkcp") | Some("quic")
        )
    {
        n.sni = n.host.clone();
    }
}

/// `ss://` 两种形态：
/// - SIP002：`ss://BASE64URL(method:password)@host:port[?plugin][#name]`
/// - 旧式：`ss://BASE64(method:password@host:port)[#name]`
fn parse_ss(rest: &str) -> Option<ParsedNode> {
    let parts = parts_of(rest);
    let q = qmap(&parts.query);

    let (method, password, hostport) = if let Some((u, hp)) = parts.authority.rsplit_once('@') {
        // SIP002：userinfo = base64(method:password)；也有人直接放明文 method:password
        //（2022-blake3 系常见）。':' 不在 base64 字母表 → 明文必然解码失败，天然分流。
        if let Some(dec) = b64_flex(u) {
            let (m, p) = dec.split_once(':')?;
            if !known_cipher(m) {
                return None;
            }
            (canon_cipher(m), p.to_string(), hp.to_string())
        } else {
            let (m, p) = u.split_once(':')?;
            if !known_cipher(m) {
                return None;
            }
            (canon_cipher(m), pd(p), hp.to_string())
        }
    } else {
        let decoded = b64_flex(&parts.authority)?;
        let (cred, hp) = decoded.split_once('@')?;
        let (m, p) = cred.split_once(':')?;
        if !known_cipher(m) {
            return None;
        }
        (canon_cipher(m), p.to_string(), hp.to_string())
    };
    let (server, port) = split_hostport(&hostport)?;

    let mut n = ParsedNode {
        name: pd(&parts.frag),
        proto: "ss".to_string(),
        server,
        port,
        password: Some(password),
        ..Default::default()
    };
    apply_query(&mut n, &q);
    // query 里的 `?method=` / `?cipher=` 同样要过白名单：apply_query 会把它写进
    // extra["method"]，一个内核不认的值足以让它在加载时炸掉整份 provider。
    // 必须在下面覆盖**之前**查 —— 覆盖后就分不清是谁给的了。
    let q_cipher = q.get("method").or_else(|| q.get("cipher"));
    if q_cipher.is_some_and(|c| !known_cipher(c)) {
        return None;
    }
    // userinfo 里的 method 权威（覆盖 query 可能给的 method/cipher 兜底）。
    n.extra.insert("method".to_string(), method);
    apply_name_defaults(&mut n, "ss");
    Some(n)
}

/// `vmess://BASE64(v2rayN JSON)`。
fn parse_vmess(rest: &str) -> Option<ParsedNode> {
    let body = rest.split('#').next().unwrap_or(rest);
    let body = body.split('?').next().unwrap_or(body);
    let dec = b64_flex(body)?;
    let v: serde_json::Value = serde_json::from_str(&dec).ok()?;

    let gs = |k: &str| -> Option<String> {
        opt(v.get(k).and_then(|x| x.as_str()).map(|s| s.to_string()))
    };
    let server = gs("add")?;
    let port = match v.get("port") {
        Some(serde_json::Value::Number(n)) => n.as_u64()? as u16,
        Some(serde_json::Value::String(s)) => s.trim().parse().ok()?,
        _ => return None,
    };
    let id = gs("id")?;
    let aid = match v.get("alterId") {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    };

    let mut n = ParsedNode {
        name: gs("ps").unwrap_or_default(),
        proto: "vmess".to_string(),
        server,
        port,
        uuid: Some(id),
        aid,
        ..Default::default()
    };
    if let Some(scy) = gs("scy").or_else(|| gs("security")) {
        n.extra.insert("scy".into(), scy);
    }
    n.network = gs("net");
    n.security = gs("tls");
    n.sni = gs("sni");
    n.alpn = gs("alpn");
    n.fp = gs("fp");
    n.path = gs("path");
    n.host = gs("host");
    n.allow_insecure = matches!(v.get("allowInsecure").and_then(|x| x.as_str()), Some("1"));
    apply_name_defaults(&mut n, "vmess");
    if n.name.is_empty() {
        n.name = n.display_name();
    }
    Some(n)
}

/// `tuic://uuid:password@host:port?congestion_control=…[#name]`。
fn parse_tuic(rest: &str) -> Option<ParsedNode> {
    let parts = parts_of(rest);
    let q = qmap(&parts.query);
    let (userinfo, hostport) = parts.authority.rsplit_once('@')?;
    let (server, port) = split_hostport(hostport)?;

    let mut n = ParsedNode {
        name: pd(&parts.frag),
        proto: "tuic".to_string(),
        server,
        port,
        ..Default::default()
    };
    match userinfo.rsplit_once(':') {
        Some((u, p)) => {
            n.uuid = opt(Some(pd(u)));
            n.password = opt(Some(pd(p)));
        }
        None => n.uuid = opt(Some(pd(userinfo))),
    }
    apply_query(&mut n, &q);
    apply_name_defaults(&mut n, "tuic");
    // clippy::question_mark（build-all 的 `-D warnings` 闸）：等价的 `?` 形式。
    n.uuid.as_ref()?;
    Some(n)
}

/// 整段文本 → 节点列表：整体 base64 先解一层 → Clash YAML 分流 → 否则逐行 URI。
///
/// 返回（节点，跳过原因）。跳过原因要带给界面 —— 「贴进去了但零节点」
/// 没有原因说明就等于软件坏了。
pub fn parse_subscription_text(text: &str) -> (Vec<ParsedNode>, Vec<String>) {
    let text = match crate::nodes::try_b64_decode(text.trim()) {
        Some(d) => d,
        None => text.to_string(),
    };
    if text.contains("\nproxies:") || text.starts_with("proxies:") {
        if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
            if let Some(arr) = v.get("proxies").and_then(|p| p.as_sequence()) {
                return clash_to_nodes(arr);
            }
        }
    }
    let mut nodes = Vec::new();
    let mut skipped = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if !l.contains("://") {
            continue;
        }
        match parse_line(l) {
            Some(n) => nodes.push(n),
            None => skipped.push(format!("无法解析：{}", truncate(l, 80))),
        }
    }
    (nodes, skipped)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

/// Clash YAML `proxies:` 序列 → 结构化节点。
fn clash_to_nodes(arr: &[serde_yaml::Value]) -> (Vec<ParsedNode>, Vec<String>) {
    let mut nodes = Vec::new();
    let mut skipped = Vec::new();
    for entry in arr {
        match clash_one(entry) {
            Some(n) => nodes.push(n),
            None => {
                let name = entry
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("(无名)");
                let ty = entry.get("type").and_then(|x| x.as_str()).unwrap_or("?");
                // ss 节点被丢掉只可能是 cipher 不被内核认（见 clash_one 里的
                // 白名单），报成「不支持的节点类型 ss」会把排障引到错的方向。
                let why = match ty {
                    "ss" => match ystr(entry, "cipher") {
                        Some(c) => format!("，cipher {c} 内核不认"),
                        None => String::new(),
                    },
                    _ => String::new(),
                };
                skipped.push(format!("不支持的 Clash 节点：{name}（{ty}{why}）"));
            }
        }
    }
    (nodes, skipped)
}

fn ystr(v: &serde_yaml::Value, k: &str) -> Option<String> {
    opt(v.get(k).and_then(|x| x.as_str()).map(|s| s.to_string()))
}

fn clash_one(entry: &serde_yaml::Value) -> Option<ParsedNode> {
    let ty = ystr(entry, "type")?.to_ascii_lowercase();
    let server = ystr(entry, "server")?;
    let port = entry.get("port")?.as_u64()? as u16;

    let mut n = ParsedNode {
        name: ystr(entry, "name").unwrap_or_default(),
        proto: ty.clone(),
        server,
        port,
        ..Default::default()
    };
    match ty.as_str() {
        "ss" => {
            n.password = ystr(entry, "password");
            // cipher 也要过白名单并归一化：Clash YAML 的 cipher 原样进 mihomo，
            // 一个内核不认的值（大小写错、ofb、plain…）会让**整份** provider
            // 初始化失败 —— 和 [parse_ss] 里是同一个坑，理由见 [SS_CIPHERS]。
            let cipher = ystr(entry, "cipher").unwrap_or_else(|| "aes-256-gcm".into());
            if !known_cipher(&cipher) {
                return None;
            }
            n.extra.insert("method".into(), canon_cipher(&cipher));
            n.security = ystr(entry, "tls");
            if let Some(pl) = ystr(entry, "plugin") {
                n.extra.insert("plugin".into(), pl);
            }
        }
        "vmess" => {
            n.uuid = ystr(entry, "uuid");
            n.aid = entry.get("alterId").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            n.network = ystr(entry, "network");
            if entry.get("tls").and_then(|x| x.as_bool()).unwrap_or(false) {
                n.security = Some("tls".into());
            }
            n.sni = ystr(entry, "servername").or_else(|| ystr(entry, "sni"));
            n.fp = ystr(entry, "client-fingerprint");
            if let Some(ws) = entry.get("ws-opts") {
                n.path = ystr(ws, "path");
                n.host = ws
                    .get("headers")
                    .and_then(|h| h.get("Host"))
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
            }
            if let Some(grpc) = entry.get("grpc-opts") {
                n.service_name = ystr(grpc, "grpc-service-name");
            }
            if let Some(sc) = ystr(entry, "cipher") {
                n.extra.insert("scy".into(), sc);
            }
        }
        "vless" => {
            n.uuid = ystr(entry, "uuid");
            n.flow = ystr(entry, "flow");
            n.network = ystr(entry, "network");
            if entry.get("tls").and_then(|x| x.as_bool()).unwrap_or(false) {
                n.security = Some("tls".into());
            }
            n.sni = ystr(entry, "servername").or_else(|| ystr(entry, "sni"));
            n.fp = ystr(entry, "client-fingerprint");
            n.encryption = Some("none".into());
            if let Some(ro) = entry.get("reality-opts") {
                n.pbk = ystr(ro, "public-key");
                n.sid = ystr(ro, "short-id");
                if n.pbk.is_some() {
                    n.security = Some("reality".into());
                }
            }
            if let Some(ws) = entry.get("ws-opts") {
                n.path = ystr(ws, "path");
            }
            if let Some(grpc) = entry.get("grpc-opts") {
                n.service_name = ystr(grpc, "grpc-service-name");
            }
        }
        "trojan" => {
            n.password = ystr(entry, "password");
            n.security = Some("tls".into());
            n.sni = ystr(entry, "sni").or_else(|| ystr(entry, "servername"));
            n.network = ystr(entry, "network");
            n.allow_insecure = entry
                .get("skip-cert-verify")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            if let Some(ws) = entry.get("ws-opts") {
                n.path = ystr(ws, "path");
            }
        }
        "hysteria2" | "hy2" => {
            n.proto = "hysteria2".into();
            n.password = ystr(entry, "password");
            n.security = Some("tls".into());
            n.sni = ystr(entry, "sni");
            n.allow_insecure = entry
                .get("skip-cert-verify")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            if let Some(obfs) = ystr(entry, "obfs") {
                n.extra.insert("obfs".into(), obfs);
            }
            if let Some(obfs_pw) = ystr(entry, "obfs-password") {
                n.extra.insert("obfs-password".into(), obfs_pw);
            }
        }
        "tuic" => {
            n.uuid = ystr(entry, "uuid");
            n.password = ystr(entry, "password");
            n.security = Some("tls".into());
            n.sni = ystr(entry, "sni");
            n.alpn = entry.get("alpn").and_then(|x| x.as_sequence()).map(|a| {
                a.iter()
                    .filter_map(|y| y.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            n.allow_insecure = entry
                .get("skip-cert-verify")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            if let Some(cc) =
                ystr(entry, "congestion-controller").or_else(|| ystr(entry, "congestion_control"))
            {
                n.extra.insert("congestion_control".into(), cc);
            }
        }
        "socks" | "http" | "ssh" | "wireguard" | "anytls" | "shadowtls" | "snell" | "hysteria" => {
            n.password = ystr(entry, "password");
            n.user = ystr(entry, "username").or_else(|| ystr(entry, "user"));
            if matches!(n.proto.as_str(), "wireguard") {
                n.uuid = ystr(entry, "public-key");
                n.password = ystr(entry, "private-key");
            }
            if ty == "snell" {
                n.password = ystr(entry, "psk");
            }
            n.sni = ystr(entry, "sni");
            n.security = Some("tls".into());
        }
        _ => return None,
    }
    let proto = n.proto.clone();
    apply_name_defaults(&mut n, &proto);
    Some(n)
}

// ─────────────────────────────────────────────────────────────
// xray 发射
// ─────────────────────────────────────────────────────────────

/// 内网 / 本机直连（不依赖 geoip.dat 的字面 CIDR，零外部数据文件要求）。
const PRIVATE_CIDRS: &[&str] = &[
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "100.64.0.0/10",
    "::1/128",
    "fc00::/7",
    "fe80::/10",
];

fn emit_xray(nodes: &[ParsedNode], opts: &EmitOptions) -> Result<String, String> {
    if nodes.is_empty() {
        return Err("没有可发射的节点".into());
    }
    let mut outbounds = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let tag = format!("node-{i}");
        outbounds.push(xray_outbound(n, &tag)?);
    }
    outbounds.push(json!({"tag": "direct", "protocol": "freedom", "settings": {}}));

    // 同端口时只发一个 socks 入站：两个 inbound 绑同一个 socket，xray 启动即死
    // （bind: Only one usage of each socket address，W10 实跑抓的）。实测 xray
    // v26.3.27 的 socks 入站兼容 HTTP 代理协议 —— 绝对 URI GET 与 CONNECT
    // 握手都在这一个口上正常应答；HTTP 入站只在两端口不同时才另起。
    let mut inbounds = vec![json!({
        "tag": "socks-in",
        "listen": "127.0.0.1",
        "port": opts.socks_port,
        "protocol": "socks",
        "settings": {"auth": "noauth", "udp": true}
    })];
    if opts.http_port != opts.socks_port {
        inbounds.push(json!({
            "tag": "http-in",
            "listen": "127.0.0.1",
            "port": opts.http_port,
            "protocol": "http",
            "settings": {}
        }));
    }

    let cfg = json!({
        "log": {"loglevel": "warning"},
        "inbounds": inbounds,
        // 代理服务器域名由内核自解析：`https+local` DoH 不进路由（防回环）、TLS 加密
        // 直连发出（抗局域网 DNS 劫持）—— xray 文档 Local Mode 语义。
        "dns": {"servers": ["https+local://1.1.1.1/dns-query", "https+local://8.8.8.8/dns-query"]},
        "outbounds": outbounds,
        "routing": {
            "rules": [
                {"type": "field", "ip": PRIVATE_CIDRS, "outboundTag": "direct"}
            ]
        }
    });
    serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())
}

fn xray_outbound(n: &ParsedNode, tag: &str) -> Result<serde_json::Value, String> {
    let err = || format!("xray 不支持协议 {}（节点 {}）", n.proto, n.display_name());
    let mut ob = match n.proto.as_str() {
        "ss" => {
            let method = n
                .ss_method()
                .cloned()
                .ok_or_else(|| format!("{} 缺少加密方式", n.display_name()))?;
            let password = n.password.clone().unwrap_or_default();
            json!({
                "tag": tag, "protocol": "shadowsocks",
                "settings": {"servers": [{
                    "address": n.server, "port": n.port,
                    "method": method, "password": password,
                    "network": "tcp,udp"
                }]}
            })
        }
        "vmess" => {
            let id = n
                .uuid
                .clone()
                .ok_or_else(|| format!("{} 缺少 uuid", n.display_name()))?;
            json!({
                "tag": tag, "protocol": "vmess",
                "settings": {"vnext": [{
                    "address": n.server, "port": n.port,
                    "users": [{
                        "id": id, "alterId": n.aid,
                        "security": n.extra.get("scy").cloned().unwrap_or_else(|| "auto".into())
                    }]
                }]}
            })
        }
        "vless" => {
            let id = n
                .uuid
                .clone()
                .ok_or_else(|| format!("{} 缺少 uuid", n.display_name()))?;
            let mut user = json!({
                "id": id,
                "encryption": n.encryption.clone().unwrap_or_else(|| "none".into())
            });
            if let Some(flow) = n.flow.clone() {
                user["flow"] = json!(flow);
            }
            json!({
                "tag": tag, "protocol": "vless",
                "settings": {"vnext": [{
                    "address": n.server, "port": n.port, "users": [user]
                }]}
            })
        }
        "trojan" => {
            let password = n
                .password
                .clone()
                .ok_or_else(|| format!("{} 缺少密码", n.display_name()))?;
            json!({
                "tag": tag, "protocol": "trojan",
                "settings": {"servers": [{
                    "address": n.server, "port": n.port, "password": [password]
                }]}
            })
        }
        "socks" | "http" => {
            let mut server = json!({"address": n.server, "port": n.port});
            if let Some(user) = n.user.clone() {
                server["users"] = json!([{
                    "user": user,
                    "pass": n.password.clone().unwrap_or_default()
                }]);
            }
            json!({"tag": tag, "protocol": n.proto, "settings": {"servers": [server]}})
        }
        _ => return Err(err()),
    };

    let has_transport = !matches!(n.network.as_deref().unwrap_or("tcp"), "tcp" | "" | "none");
    let security = effective_security(n);
    if has_transport || security != "none" {
        ob["streamSettings"] = xray_stream(n, &security)?;
    }
    Ok(ob)
}

/// 生效的传输安全：vless/trojan/vmess 读 security 字段，trojan/tuic 等默认 tls。
fn effective_security(n: &ParsedNode) -> String {
    match n.security.as_deref() {
        Some("reality") => "reality".into(),
        Some("tls") => "tls".into(),
        Some("none") => "none".into(),
        _ => {
            if matches!(
                n.proto.as_str(),
                "trojan" | "tuic" | "hysteria" | "hysteria2" | "anytls" | "shadowtls"
            ) {
                "tls".into()
            } else {
                "none".into()
            }
        }
    }
}

fn xray_network(raw: &str) -> &'static str {
    match raw {
        "h2" | "http" => "http",
        "ws" => "ws",
        "grpc" => "grpc",
        "httpupgrade" => "httpupgrade",
        "kcp" | "mkcp" => "kcp",
        "quic" => "quic",
        _ => "tcp",
    }
}

fn xray_stream(n: &ParsedNode, security: &str) -> Result<serde_json::Value, String> {
    let raw_net = n.network.as_deref().unwrap_or("tcp");
    let network = xray_network(raw_net);
    let mut ss = json!({"network": network});

    let sni = n
        .sni
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // kcp/quic 的 host 是 v2rayN 的伪装类型字段（如 srtp），不是域名，
            // 回落成 sni 会发一个假 serverName 出去 —— 不回落。
            if matches!(
                n.network.as_deref(),
                Some("kcp") | Some("mkcp") | Some("quic")
            ) {
                None
            } else {
                n.host.clone()
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| n.server.clone());

    match security {
        "tls" => {
            let mut tls = json!({"serverName": sni, "allowInsecure": n.allow_insecure});
            if let Some(alpn) = n.alpn.clone() {
                tls["alpn"] = json!(alpn
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>());
            }
            if let Some(fp) = n.fp.clone() {
                tls["fingerprint"] = json!(fp);
            }
            ss["security"] = json!("tls");
            ss["tlsSettings"] = tls;
        }
        "reality" => {
            let pbk = n
                .pbk
                .clone()
                .ok_or_else(|| format!("{} 缺少 Reality publicKey", n.display_name()))?;
            ss["security"] = json!("reality");
            ss["realitySettings"] = json!({
                "serverName": sni,
                "fingerprint": n.fp.clone().unwrap_or_else(|| "chrome".into()),
                "publicKey": pbk,
                "shortId": n.sid.clone().unwrap_or_default(),
                "spiderX": n.spx.clone().unwrap_or_default(),
            });
        }
        _ => {}
    }

    match network {
        "ws" => {
            let mut ws = json!({"path": n.path.clone().unwrap_or_else(|| "/".into())});
            if let Some(host) = n.host.clone() {
                ws["headers"] = json!({"Host": host});
            }
            ss["wsSettings"] = ws;
        }
        "grpc" => {
            ss["grpcSettings"] = json!({
                "serviceName": n.service_name
                    .clone()
                    .or_else(|| n.path.clone())
                    .unwrap_or_default()
            });
        }
        "http" => {
            let mut http = json!({"path": n.path.clone().unwrap_or_else(|| "/".into())});
            if let Some(host) = n.host.clone() {
                http["host"] = json!([host]);
            }
            ss["httpSettings"] = http;
        }
        "httpupgrade" => {
            let mut up = json!({"path": n.path.clone().unwrap_or_else(|| "/".into())});
            if let Some(host) = n.host.clone() {
                up["host"] = json!(host);
            }
            ss["httpupgradeSettings"] = up;
        }
        "kcp" => {
            // v2rayN 约定：mkcp 的 seed 放 path 字段、header 伪装类型放 host 字段
            // （URI 侧则落在 extra["seed"] / extra["headertype"]）。seed 不匹配服务端
            // 直接连不上，必须带上；旧实现只发 header，seed 丢了 = 节点静默死亡。
            let seed = n
                .extra
                .get("seed")
                .cloned()
                .or_else(|| n.path.clone())
                .filter(|s| !s.is_empty());
            let header = n
                .extra
                .get("header")
                .or_else(|| n.extra.get("headertype"))
                .cloned()
                .or_else(|| n.host.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "none".into());
            let mut kcp = json!({"header": {"type": header}});
            if let Some(seed) = seed {
                kcp["seed"] = json!(seed);
            }
            ss["kcpSettings"] = kcp;
        }
        "quic" => {
            // xray 26.x 起 QUIC 传输已整体移除（infra/conf/transport_internet.go:
            // PrintRemovedFeatureError("QUIC transport …", "XHTTP stream-one H3")），
            // 写 {"network":"quic"} 只会让 `xray run -test` 拒配置。明着报错，别发残货。
            return Err(format!(
                "{}：xray 26+ 已移除 QUIC 传输，请改用 mihomo 或换该节点的 ws/grpc 变体",
                n.display_name()
            ));
        }
        _ => {}
    }
    Ok(ss)
}

// ─────────────────────────────────────────────────────────────
// sing-box 发射
// ─────────────────────────────────────────────────────────────

fn emit_singbox(nodes: &[ParsedNode], opts: &EmitOptions) -> Result<String, String> {
    if nodes.is_empty() {
        return Err("没有可发射的节点".into());
    }
    let mut outbounds = Vec::new();
    let mut tags = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let tag = format!("node-{i}");
        outbounds.push(sb_outbound(n, &tag)?);
        tags.push(tag);
    }
    // urltest 择优 + selector 默认指向 urltest（对标 NekoBox 的自动测速选路）。
    let mut selector_list = vec![json!("auto")];
    selector_list.extend(tags.iter().map(|t| json!(t)));
    outbounds.insert(
        0,
        json!({"type": "selector", "tag": "select", "outbounds": selector_list}),
    );
    outbounds.insert(
        1,
        json!({
            "type": "urltest", "tag": "auto",
            "outbounds": tags,
            "url": "http://www.gstatic.com/generate_204",
            "interval": "5m"
        }),
    );
    outbounds.push(json!({"type": "direct", "tag": "direct"}));

    // 不写 dns 段：Android 上应用侧 DNS 由 tun2socks 内建 DoH（1054）应答，
    // 内核自身只在服务器域名为域名时需要解析，走系统解析器即可（见 MULTI-CORE-PLAN.md）。
    let cfg = json!({
        "log": {"level": "warn", "timestamp": false},
        "inbounds": [
            {
                "type": "mixed",
                "tag": "mixed-in",
                "listen": "127.0.0.1",
                "listen_port": opts.socks_port
            }
        ],
        "outbounds": outbounds,
        "route": {
            // ip_is_private（1.8+）一步覆盖 v4/v6 私有段，比手写 CIDR 短且全；
            // 不 sniff —— 当前没有消费 sniffed 数据的规则，白加首包延迟。
            "rules": [{"action": "route", "outbound": "direct", "ip_is_private": true}],
            "final": "select"
        }
    });
    serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())
}

fn sb_outbound(n: &ParsedNode, tag: &str) -> Result<serde_json::Value, String> {
    let err = || {
        format!(
            "sing-box 不支持协议 {}（节点 {}）",
            n.proto,
            n.display_name()
        )
    };
    let mut ob = match n.proto.as_str() {
        "ss" => {
            let method = n
                .ss_method()
                .cloned()
                .ok_or_else(|| format!("{} 缺少加密方式", n.display_name()))?;
            // sing-box 只认 "shadowsocks"；写 "ss" 内核直接 FATAL 拒配置
            // （unknown outbound type: ss —— W10 实跑抓的，单测只验结构没验内核认）。
            let mut o = json!({
                "type": "shadowsocks", "tag": tag,
                "method": method,
                "password": n.password.clone().unwrap_or_default()
            });
            if let Some(plugin) = n.extra.get("plugin") {
                o["plugin"] = json!(plugin);
            }
            if let Some(opts) = n
                .extra
                .get("plugin-opts")
                .or_else(|| n.extra.get("plugin_opts"))
            {
                o["plugin_opts"] = json!(opts);
            }
            o
        }
        "vmess" => {
            let id = n
                .uuid
                .clone()
                .ok_or_else(|| format!("{} 缺少 uuid", n.display_name()))?;
            let mut o = json!({"type": "vmess", "tag": tag, "uuid": id});
            if n.aid > 0 {
                o["alter_id"] = json!(n.aid);
            }
            if let Some(scy) = n.extra.get("scy") {
                o["security"] = json!(scy);
            }
            o
        }
        "vless" => {
            let id = n
                .uuid
                .clone()
                .ok_or_else(|| format!("{} 缺少 uuid", n.display_name()))?;
            let mut o = json!({"type": "vless", "tag": tag, "uuid": id});
            if let Some(flow) = n.flow.clone() {
                o["flow"] = json!(flow);
            }
            o
        }
        "trojan" => json!({
            "type": "trojan", "tag": tag,
            "password": n.password.clone().ok_or_else(|| format!("{} 缺少密码", n.display_name()))?
        }),
        "hysteria2" => {
            let mut o = json!({
                "type": "hysteria2", "tag": tag,
                "password": n.password.clone().unwrap_or_default()
            });
            if let Some(obfs) = n.extra.get("obfs") {
                let mut obfs_obj = json!({"type": obfs});
                if let Some(pw) = n.extra.get("obfs-password") {
                    obfs_obj["password"] = json!(pw);
                }
                o["obfs"] = obfs_obj;
            }
            o
        }
        "hysteria" => return Err(err()),
        "tuic" => {
            let mut o = json!({
                "type": "tuic", "tag": tag,
                "uuid": n.uuid.clone().ok_or_else(|| format!("{} 缺少 uuid", n.display_name()))?,
                "password": n.password.clone().unwrap_or_default()
            });
            if let Some(cc) = n.extra.get("congestion_control") {
                o["congestion_control"] = json!(cc);
            }
            if let Some(m) = n.extra.get("udp_relay_mode") {
                o["udp_relay_mode"] = json!(m);
            }
            o
        }
        "anytls" => json!({
            "type": "anytls", "tag": tag,
            "password": n.password.clone().ok_or_else(|| format!("{} 缺少密码", n.display_name()))?
        }),
        "shadowtls" => {
            let version: u8 = n
                .extra
                .get("version")
                .and_then(|v| v.parse().ok())
                .unwrap_or(3);
            json!({
                "type": "shadowtls", "tag": tag, "version": version,
                "password": n.password.clone().ok_or_else(|| format!("{} 缺少密码", n.display_name()))?
            })
        }
        "socks" | "http" => {
            let mut o = json!({"type": n.proto, "tag": tag});
            if let Some(u) = n.user.clone() {
                o["username"] = json!(u);
                o["password"] = json!(n.password.clone().unwrap_or_default());
            }
            o
        }
        "ssh" => {
            let mut o = json!({
                "type": "ssh", "tag": tag,
                "user": n.user.clone().or_else(|| Some("root".into())).unwrap_or_default()
            });
            if let Some(pw) = n.password.clone() {
                o["password"] = json!(pw);
            }
            if let Some(pk) = n.extra.get("private-key") {
                o["private_key"] = json!(pk);
            }
            o
        }
        "wireguard" | "snell" => return Err(err()),
        _ => return Err(err()),
    };

    ob["server"] = json!(n.server);
    ob["server_port"] = json!(n.port);

    // socks / http 出站没有 TLS / 传输层概念。
    if !matches!(n.proto.as_str(), "socks" | "http") {
        let security = effective_security(n);
        let needs_tls = security != "none"
            || matches!(
                n.proto.as_str(),
                "tuic" | "hysteria2" | "anytls" | "shadowtls"
            );
        if needs_tls {
            ob["tls"] = sb_tls(n, security == "reality")?;
        }
        if let Some(t) = sb_transport(n)? {
            ob["transport"] = t;
        }
    }
    Ok(ob)
}

fn sb_tls(n: &ParsedNode, reality: bool) -> Result<serde_json::Value, String> {
    let sni = n
        .sni
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // kcp/quic 的 host 是 v2rayN 的伪装类型字段（如 srtp），不是域名，
            // 回落成 sni 会发一个假 serverName 出去 —— 不回落。
            if matches!(
                n.network.as_deref(),
                Some("kcp") | Some("mkcp") | Some("quic")
            ) {
                None
            } else {
                n.host.clone()
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| n.server.clone());
    let mut tls = json!({"enabled": true, "server_name": sni, "insecure": n.allow_insecure});
    if let Some(alpn) = n.alpn.clone() {
        tls["alpn"] = json!(alpn
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>());
    }
    let fp = n.fp.clone().unwrap_or_else(|| "chrome".into());
    if reality || n.fp.is_some() {
        tls["utls"] = json!({"enabled": true, "fingerprint": fp});
    }
    if reality {
        let pbk = n
            .pbk
            .clone()
            .ok_or_else(|| format!("{} 缺少 Reality publicKey", n.display_name()))?;
        tls["reality"] = json!({
            "enabled": true,
            "public_key": pbk,
            "short_id": n.sid.clone().unwrap_or_default()
        });
    }
    Ok(tls)
}

/// sing-box 的 v2ray 传输层。不支持 = 显式报错，绝不静默按 tcp 发
/// （静默错配的表现是"节点死活连不上"，用户根本无从排查）。
fn sb_transport(n: &ParsedNode) -> Result<Option<serde_json::Value>, String> {
    if matches!(
        n.proto.as_str(),
        "hysteria" | "hysteria2" | "tuic" | "socks" | "http" | "ssh"
    ) {
        return Ok(None);
    }
    let raw = n.network.as_deref().unwrap_or("tcp");
    match raw {
        "ws" => {
            let mut ws =
                json!({"type": "ws", "path": n.path.clone().unwrap_or_else(|| "/".into())});
            if let Some(host) = n.host.clone() {
                ws["headers"] = json!({"Host": host});
            }
            Ok(Some(ws))
        }
        "grpc" => Ok(Some(json!({
            "type": "grpc",
            "service_name": n.service_name
                .clone()
                .or_else(|| n.path.clone())
                .unwrap_or_default()
        }))),
        "h2" | "http" => {
            let mut h =
                json!({"type": "http", "path": n.path.clone().unwrap_or_else(|| "/".into())});
            if let Some(host) = n.host.clone() {
                h["headers"] = json!({"Host": host});
            }
            Ok(Some(h))
        }
        "httpupgrade" => {
            let mut u = json!({
                "type": "httpupgrade",
                "path": n.path.clone().unwrap_or_else(|| "/".into())
            });
            if let Some(host) = n.host.clone() {
                u["headers"] = json!({"Host": host});
            }
            Ok(Some(u))
        }
        "quic" => {
            // sing-box 支持 quic 传输（1.11+，CI 钉 1.14.2）；QUIC 天然加密，
            // 不配 TLS sing-box 启动即报 "TLS is required"，这里提前用人话拦。
            if effective_security(n) == "none" {
                return Err(format!(
                    "{}：QUIC 传输必须启用 TLS（sing-box 不支持无加密 QUIC）",
                    n.display_name()
                ));
            }
            Ok(Some(json!({"type": "quic"})))
        }
        "kcp" | "mkcp" => Err(format!(
            "{}：sing-box 不支持 mKCP 传输，请改用 mihomo 或 xray",
            n.display_name()
        )),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit_ok(kind: CoreKind, nodes: &[ParsedNode]) -> serde_json::Value {
        let text = emit(kind, nodes, &EmitOptions::default()).expect("emit 应成功");
        serde_json::from_str(&text).expect("发射产物必须是合法 JSON")
    }

    #[test]
    fn parse_vless_reality() {
        let uri = "vless://11111111-2222-3333-4444-555555555555@reality.example.com:443\
                   ?encryption=none&security=reality&type=tcp&flow=xtls-rprx-vision\
                   &pbk=PUBKEY123&fp=chrome&sni=www.example.com&sid=abcd1234#Reality节点";
        let n = parse_line(uri).expect("应解析成功");
        assert_eq!(n.proto, "vless");
        assert_eq!(n.server, "reality.example.com");
        assert_eq!(n.port, 443);
        assert_eq!(n.security.as_deref(), Some("reality"));
        assert_eq!(n.flow.as_deref(), Some("xtls-rprx-vision"));
        assert_eq!(n.pbk.as_deref(), Some("PUBKEY123"));
        assert_eq!(n.fp.as_deref(), Some("chrome"));
        assert_eq!(n.sid.as_deref(), Some("abcd1234"));
        assert_eq!(n.sni.as_deref(), Some("www.example.com"));
        assert_eq!(n.name, "Reality节点");
        assert!(CoreKind::Xray.supports("vless"));
        assert_eq!(CoreKind::auto_for("vless"), CoreKind::Xray);
    }

    #[test]
    fn parse_vless_ws_tls() {
        let uri = "vless://uuid-1@1.2.3.4:8443?type=ws&security=tls&sni=cdn.x.com\
                   &path=%2Fws%3Fmode%3Dgun&host=cdn.x.com#WS节点";
        let n = parse_line(uri).unwrap();
        assert_eq!(n.network.as_deref(), Some("ws"));
        assert_eq!(n.path.as_deref(), Some("/ws?mode=gun"));
        assert_eq!(n.host.as_deref(), Some("cdn.x.com"));
        assert_eq!(n.sni.as_deref(), Some("cdn.x.com"));
    }

    #[test]
    fn parse_vmess_b64_json() {
        let json = serde_json::json!({
            "v": "2", "ps": "VMess节点", "add": "5.6.7.8", "port": "10086",
            "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "aid": "0",
            "scy": "auto", "net": "ws", "host": "h.example.com", "path": "/vmess",
            "tls": "tls", "sni": "s.example.com"
        })
        .to_string();
        let b64 = base64::engine::general_purpose::STANDARD.encode(json);
        let n = parse_line(&format!("vmess://{b64}")).unwrap();
        assert_eq!(n.proto, "vmess");
        assert_eq!(n.server, "5.6.7.8");
        assert_eq!(n.port, 10086);
        assert_eq!(n.aid, 0);
        assert_eq!(n.network.as_deref(), Some("ws"));
        assert_eq!(n.security.as_deref(), Some("tls"));
        assert_eq!(n.name, "VMess节点");
    }

    #[test]
    fn parse_vmess_dirty_rejected() {
        // 整体不是合法 base64 JSON → 应拒绝而不是吐半个节点。
        assert!(parse_line("vmess://not-valid-json!!!").is_none());
    }

    /// v2rayN 约定：mkcp 的 host=伪装类型、path=seed。xray 发射必须带上 seed
    /// （不匹配服务端连不上），且 host 不得污染成 sni。
    #[test]
    fn xray_vmess_kcp_seed_and_header() {
        let json = serde_json::json!({
            "v": "2", "ps": "KCP节点", "add": "7.7.7.7", "port": 9999,
            "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "aid": "0",
            "net": "kcp", "host": "srtp", "path": "my-seed", "type": "none"
        })
        .to_string();
        let b64 = base64::engine::general_purpose::STANDARD.encode(json);
        let n = parse_line(&format!("vmess://{b64}")).unwrap();
        assert_eq!(n.network.as_deref(), Some("kcp"));
        assert_eq!(n.host.as_deref(), Some("srtp"));
        assert_eq!(n.path.as_deref(), Some("my-seed"));

        let cfg = emit_ok(CoreKind::Xray, &[n]);
        let kcp = &cfg["outbounds"][0]["streamSettings"]["kcpSettings"];
        assert_eq!(kcp["header"]["type"], "srtp");
        assert_eq!(kcp["seed"], "my-seed");
    }

    /// xray 26.x 移除了 QUIC 传输（infra/conf PrintRemovedFeatureError），
    /// 必须报错而不是发 {"network":"quic"} 残配置让 run -test 拒。
    #[test]
    fn xray_quic_removed_rejected() {
        let json = serde_json::json!({
            "v": "2", "ps": "QUIC节点", "add": "8.8.4.4", "port": 443,
            "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "aid": "0",
            "net": "quic", "tls": "tls"
        })
        .to_string();
        let b64 = base64::engine::general_purpose::STANDARD.encode(json);
        let n = parse_line(&format!("vmess://{b64}")).unwrap();
        let err = emit(CoreKind::Xray, &[n], &EmitOptions::default()).unwrap_err();
        assert!(err.contains("QUIC"), "报错应说明 QUIC 被移除：{err}");
    }

    /// sing-box：quic 传输要 TLS（QUIC 天然加密）；mKCP 不支持必须报错，
    /// 不能静默按 tcp 发出去（那表现为"节点永远连不上"）。
    #[test]
    fn singbox_quic_transport_and_kcp_rejected() {
        let mk = |net: &str, tls: &str| {
            let json = serde_json::json!({
                "v": "2", "ps": "T", "add": "8.8.8.8", "port": 443,
                "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "aid": "0",
                "net": net, "tls": tls
            })
            .to_string();
            let b64 = base64::engine::general_purpose::STANDARD.encode(json);
            parse_line(&format!("vmess://{b64}")).unwrap()
        };

        let quic = mk("quic", "tls");
        let cfg = emit_ok(CoreKind::SingBox, &[quic]);
        // outbounds: [0]=selector [1]=urltest [2]=node-0
        assert_eq!(cfg["outbounds"][2]["transport"]["type"], "quic");
        // sni 不许被 v2rayN 伪装类型字段污染 —— quic 没给 host，这里用 server 兜底
        assert_eq!(cfg["outbounds"][2]["tls"]["server_name"], "8.8.8.8");

        let quic_plain = mk("quic", "");
        let err = emit(CoreKind::SingBox, &[quic_plain], &EmitOptions::default()).unwrap_err();
        assert!(err.contains("TLS"), "无加密 QUIC 应报 TLS 要求：{err}");

        let kcp = mk("kcp", "tls");
        let err = emit(CoreKind::SingBox, &[kcp], &EmitOptions::default()).unwrap_err();
        assert!(err.contains("mKCP"), "mKCP 应显式报错：{err}");
    }

    #[test]
    fn parse_ss_sip002_and_legacy() {
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode("aes-256-gcm:p@ss:word");
        let n = parse_line(&format!("ss://{b64}@10.0.0.1:8388#SS节点")).unwrap();
        assert_eq!(n.proto, "ss");
        assert_eq!(n.server, "10.0.0.1");
        assert_eq!(n.password.as_deref(), Some("p@ss:word"));
        assert_eq!(
            n.extra.get("method").map(String::as_str),
            Some("aes-256-gcm")
        );

        let legacy = base64::engine::general_purpose::STANDARD
            .encode("chacha20-ietf-poly1305:pw@10.0.0.2:8389");
        let n2 = parse_line(&format!("ss://{legacy}#旧式SS")).unwrap();
        assert_eq!(n2.server, "10.0.0.2");
        assert_eq!(n2.port, 8389);
        assert_eq!(
            n2.extra.get("method").map(String::as_str),
            Some("chacha20-ietf-poly1305")
        );
    }

    #[test]
    fn parse_ss_plaintext_userinfo() {
        // 2022-blake3 系常见：userinfo 直接是明文 method:password（password 可带百分号编码）。
        let n = parse_line("ss://aes-256-gcm:p%40w@9.9.9.9:8388#明文").unwrap();
        assert_eq!(
            n.extra.get("method").map(String::as_str),
            Some("aes-256-gcm")
        );
        assert_eq!(n.password.as_deref(), Some("p@w"));
        assert_eq!(n.server, "9.9.9.9");
    }

    /// `ss://` 链接最终落进配置的 method（`None` = 整行被丢掉）。
    ///
    /// 走这一层是为了让断言短：测试里真正要钉的是「内核最终看到的那个字符串」，
    /// 中间经过 `parse_line` → `extra["method"]` 之后才是它。
    fn ss_method_of(uri: &str) -> Option<String> {
        parse_line(uri).map(|n| n.extra["method"].clone())
    }

    /// 回归护栏：ss 的 method 认不出来时必须**整行丢掉**，不能带着垃圾值进配置。
    ///
    /// 第 2 条就是 v0.3.15 的 P1 实锤（`162.159.1.33`）。它以前之所以被剔掉
    /// 是靠两条巧合：base64 恰好解不出合法 UTF-8、UUID 里又恰好没有冒号。
    /// 任何一处改动都会让它解出一个垃圾 method 混进 provider，然后 mihomo
    /// 整份初始化失败 → 20 好 + 1 坏 = 0 节点，日志里只有一句
    /// `cipher: ... unknown method`。白名单把这个巧合换成显式契约。
    #[test]
    fn ss_的_method_认不出来必须整行丢掉() {
        // 1. userinfo 是 base64，但解出来不是加密方式
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode("not-a-cipher:pw");
        let u = format!("ss://{b64}@1.2.3.4:8388#假方法");
        assert!(ss_method_of(&u).is_none(), "解出非加密方式要丢掉整行");

        // 2. 真实毒行（P1 那条）：UUID 当 userinfo，里面根本没有冒号
        let poison = "ss://15298f41-e80b-463a-b85b-0c903258a1c8@162.159.1.33:443\
                      ?security=tls&encryption=none";
        assert!(ss_method_of(poison).is_none(), "P1 毒行必须被丢掉");

        // 3. 明文 userinfo，method 不认识
        assert!(ss_method_of("ss://whatever:pw@1.2.3.4:8388#x").is_none());

        // 4. 旧式整段 base64（`ss://BASE64(method:pw@host:port)`），method 不认识
        let enc = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let legacy = enc("nope:pw@1.2.3.4:8388");
        assert!(ss_method_of(&format!("ss://{legacy}#x")).is_none());

        // 5. query 里的 ?cipher= 也要过白名单（它会被写进 extra["method"]）
        let q = "ss://aes-256-gcm:pw@1.2.3.4:8388?cipher=bogus#x";
        assert!(ss_method_of(q).is_none(), "query 里的 cipher 也要过白名单");
    }

    /// 白名单不能反过来误伤真节点：白名单里的每一个都得能解析、且原样落盘。
    ///
    /// 直接遍历 `SS_CIPHERS` 而不是抄一份到测试里：以后往白名单加一个值，
    /// 这条测试自动就把它验一遍（少写一份名单就不会忘了同步）。
    #[test]
    fn ss_白名单里的加密方式都要能解析() {
        for c in SS_CIPHERS {
            let uri = format!("ss://{c}:pw@1.2.3.4:8388#{c}");
            assert_eq!(ss_method_of(&uri).as_deref(), Some(*c), "{c} 解析失败");
        }
    }

    /// 大小写 / 首尾空白必须**归一化**后再进配置，不能只查不改。
    ///
    /// 实测 mihomo v1.19.30 对 `AES-256-GCM` 回 `unknown method`：内核大小写敏感。
    /// 只查不改的话，这种节点会被"认出来"然后原样落盘，把整份 provider 炸掉 ——
    /// 正是这个白名单要消灭的故障换个入口再犯一次。
    #[test]
    fn ss_大写加密方式要归一化成内核认的小写() {
        for raw in ["AES-256-GCM", "Chacha20-Ietf-Poly1305", " aes-128-gcm "] {
            let got = ss_method_of(&format!("ss://{raw}:pw@1.2.3.4:8388#x"));
            let m = got.unwrap_or_default();
            assert!(SS_CIPHERS.contains(&m.as_str()), "{raw} 没被认下来");
            assert_eq!(m, m.to_ascii_lowercase(), "{raw} 没归一化：{m}");
        }
    }

    /// 白名单里**不能**有的：逐个 `mihomo -t -f` 跑出来的失败项。
    /// 这条测试的列表就是白名单的判据来源，别把它改松。
    #[test]
    fn ss_白名单不收_mihomo_实测不认的值() {
        let bad = [
            "plain",
            "rc4",
            "dummy",
            "aes-128-ofb",
            "bf-cfb",
            "des-cfb",
            "idea-cfb",
            "chacha20-poly1305",
            "camellia-128-cfb",
        ];
        for c in bad {
            assert!(!known_cipher(c), "{c} 内核不认，不能进白名单");
            let uri = format!("ss://{c}:pw@1.2.3.4:8388#x");
            assert!(ss_method_of(&uri).is_none(), "{c} 该被丢掉整行");
        }
        // 反面：`none` 是真加密方式（不加密），别跟 `plain` 混为一谈
        assert!(known_cipher("none"));
        assert!(!known_cipher("plain"), "plain 不是 none 的别名");
    }

    /// Clash YAML 那条入口也要过白名单 —— 它和 share URI 落到**同一份**
    /// file provider，只守 share URI 等于留了半个门。
    ///
    /// 顺带钉住两件事：跳过的 ss 节点要报出 cipher（报成「不支持的节点类型 ss」
    /// 会把排障引到错方向），以及大写 cipher 归一化成小写再存。
    #[test]
    fn clash_yaml_的_ss_cipher_同样过白名单并归一化() {
        // 缩进必须写在**字面量里面**（接在 `\n` 之后、行尾 `\` 之前）：
        // `\`+换行会吃掉下一行的**全部**前导空白，靠源码缩进拼出来的块状
        // YAML 会塌成非法标量流，serde_yaml 解析失败 → 一条都读不出来
        // （2026-09-27 CI 实测：这条测试曾因此报「实际 0」）。
        let yaml = "proxies:\n\
- name: 好节点\n  type: ss\n  server: 1.2.3.4\n  port: 8388\n  \
cipher: AES-256-GCM\n  password: pw\n\
- name: 坏节点\n  type: ss\n  server: 5.6.7.8\n  port: 8388\n  \
cipher: aes-128-ofb\n  password: pw\n";
        let (nodes, skipped) = parse_subscription_text(yaml);
        assert_eq!(nodes.len(), 1, "只该留一个节点，实际 {}", nodes.len());
        let m = nodes[0].extra["method"].clone();
        assert_eq!(m, "aes-256-gcm", "大写 cipher 必须归一化后再存");
        assert_eq!(skipped.len(), 1, "实际跳过：{skipped:?}");
        assert!(
            skipped[0].contains("aes-128-ofb"),
            "跳过原因要带出 cipher，实际：{}",
            skipped[0]
        );
    }

    #[test]
    fn parse_trojan_hy2_tuic() {
        let t = parse_line("trojan://pw@t.example.com:443?sni=t.example.com&type=ws#TJ").unwrap();
        assert_eq!(t.proto, "trojan");
        assert_eq!(t.password.as_deref(), Some("pw"));
        assert_eq!(t.security.as_deref(), Some("tls"));

        let h = parse_line(
            "hysteria2://secret@hy.example.com:8443?insecure=1\
                            &obfs=salamander&obfs-password=ob#HY2",
        )
        .unwrap();
        assert_eq!(h.proto, "hysteria2");
        assert_eq!(h.password.as_deref(), Some("secret"));
        assert!(h.allow_insecure);
        assert_eq!(h.extra.get("obfs").map(String::as_str), Some("salamander"));

        let u = parse_line(
            "tuic://11111111-2222-3333-4444-555555555555:pw@tu.example.com:443\
                            ?congestion_control=bbr&alpn=h3#TUIC",
        )
        .unwrap();
        assert_eq!(u.proto, "tuic");
        assert_eq!(
            u.uuid.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(
            u.extra.get("congestion_control").map(String::as_str),
            Some("bbr")
        );
    }

    #[test]
    fn parse_socks_http_ssh_v6() {
        let s = parse_line("socks5://user:pass@[2001:db8::1]:1080#SOCKS").unwrap();
        assert_eq!(s.proto, "socks");
        assert_eq!(s.server, "2001:db8::1");
        assert_eq!(s.user.as_deref(), Some("user"));
        assert_eq!(s.password.as_deref(), Some("pass"));

        let h = parse_line("http://1.2.3.4:8080#无认证").unwrap();
        assert_eq!(h.proto, "http");
        assert!(h.user.is_none());

        let ssh = parse_line("ssh://root:hunter2@srv.example.com:22#SSH").unwrap();
        assert_eq!(ssh.proto, "ssh");
        assert_eq!(ssh.user.as_deref(), Some("root"));
        assert!(CoreKind::SingBox.supports("ssh"));
        assert!(!CoreKind::Xray.supports("ssh"));
    }

    #[test]
    fn subscription_text_mix_and_skip_reasons() {
        let text = "vless://uuid@1.1.1.1:443?security=reality&pbk=K&sid=1#A\n\
                    vmess://!!!corrupt!!!\n\
                    trojan://pw@2.2.2.2:443#B";
        let (nodes, skipped) = parse_subscription_text(text);
        assert_eq!(nodes.len(), 2);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("无法解析"));
    }

    #[test]
    fn clash_yaml_ingested() {
        let yaml = "proxies:\n\
                    - name: A\n  type: ss\n  server: 9.9.9.9\n  port: 443\n  \
                      cipher: aes-128-gcm\n  password: pw\n\
                    - name: B\n  type: vmess\n  server: 8.8.8.8\n  port: 443\n  \
                      uuid: u-1\n  alterId: 0\n  network: ws\n  tls: true\n";
        let (nodes, skipped) = parse_subscription_text(yaml);
        assert_eq!(nodes.len(), 2);
        assert!(skipped.is_empty());
        assert_eq!(nodes[0].proto, "ss");
        assert_eq!(nodes[1].proto, "vmess");
        assert_eq!(nodes[1].security.as_deref(), Some("tls"));
    }

    #[test]
    fn base64_subscription_blob() {
        // try_b64_decode 有 40 字符下限（防短文本误判整块 base64），raw 得够长。
        let raw = "trojan://pw@3.3.3.3:443#CN-Home-01";
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        assert!(b64.len() >= 40, "raw 太短，进不了 base64 整块分支");
        let (nodes, _) = parse_subscription_text(&b64);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].proto, "trojan");
    }

    #[test]
    fn matrix_and_auto_pick() {
        assert!(CoreKind::Mihomo.supports("hysteria2"));
        assert!(CoreKind::SingBox.supports("hysteria2"));
        assert!(!CoreKind::Xray.supports("hysteria2"));
        assert_eq!(CoreKind::auto_for("hysteria2"), CoreKind::SingBox);
        assert_eq!(CoreKind::auto_for("ss"), CoreKind::Mihomo);
        assert_eq!(CoreKind::auto_for("vless"), CoreKind::Xray);
        assert_eq!(CoreKind::parse("SING_BOX"), Some(CoreKind::SingBox));
        assert_eq!(CoreKind::parse("singbox"), Some(CoreKind::SingBox));
        assert_eq!(CoreKind::parse("nope"), None);
    }

    #[test]
    fn auto_core_covers_all_or_widest() {
        let only_ss = vec![parse_line("ss://YWVzLTI1Ni1nY206cA@1.1.1.1:1#n").unwrap()];
        assert_eq!(auto_core(&only_ss), CoreKind::Mihomo);

        // mihomo/xray 都不吃 shadowtls、sing-box 全吃 → 必落 sing-box。
        let mixed = vec![
            parse_line("vless://u@1.1.1.1:443?security=reality&pbk=K#n").unwrap(),
            parse_line("shadowtls://pw@2.2.2.2:443?sni=x#n").unwrap(),
        ];
        assert_eq!(auto_core(&mixed), CoreKind::SingBox);

        let (kept, dropped) = filter_nodes(mixed, CoreKind::Xray);
        assert_eq!(kept.len(), 1);
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].contains("不支持 shadowtls"));
    }

    #[test]
    fn xray_emit_reality_and_rules() {
        let nodes = vec![
            parse_line(
                "vless://u@1.1.1.1:443?security=reality&pbk=K&sid=s&fp=chrome&sni=n\
                 &flow=xtls-rprx-vision#R",
            )
            .unwrap(),
            parse_line("trojan://pw@2.2.2.2:443#T").unwrap(),
        ];
        let cfg = emit_ok(CoreKind::Xray, &nodes);
        let outbounds = cfg["outbounds"].as_array().unwrap();
        // 两个节点 + freedom(direct)
        assert_eq!(outbounds.len(), 3);
        assert_eq!(outbounds[0]["protocol"], "vless");
        assert_eq!(outbounds[0]["streamSettings"]["security"], "reality");
        assert_eq!(
            outbounds[0]["streamSettings"]["realitySettings"]["publicKey"],
            "K"
        );
        assert_eq!(outbounds[1]["protocol"], "trojan");
        assert_eq!(outbounds[1]["streamSettings"]["security"], "tls");
        assert_eq!(outbounds[2]["protocol"], "freedom");
        assert_eq!(cfg["inbounds"][0]["port"], 1080);
        // 内网直连规则不依赖 geoip.dat
        assert!(cfg["routing"]["rules"][0]["ip"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "10.0.0.0/8"));
        // SOCKS 入站开 UDP
        assert_eq!(cfg["inbounds"][0]["settings"]["udp"], true);
    }

    #[test]
    fn xray_rejects_unsupported_proto() {
        let nodes = vec![parse_line("hysteria2://p@1.1.1.1:443#H").unwrap()];
        let err = emit(CoreKind::Xray, &nodes, &EmitOptions::default()).unwrap_err();
        assert!(err.contains("hysteria2"), "错误应点名协议：{err}");
    }

    #[test]
    fn emit_xray_same_port_uses_single_socks_inbound() {
        // W10 实跑抓的：同端口双 inbound → xray「Only one usage of each
        // socket address」启动即死。同端口时只应发一个 socks 入站。
        let nodes = vec![parse_line("vless://u@1.1.1.1:443#V").unwrap()];
        let same = EmitOptions {
            socks_port: 17890,
            http_port: 17890,
        };
        let text = emit(CoreKind::Xray, &nodes, &same).expect("emit 应成功");
        let cfg: serde_json::Value = serde_json::from_str(&text).expect("合法 JSON");
        let inbounds = cfg["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 1, "同端口只应发一个 socks 入站");
        assert_eq!(inbounds[0]["protocol"], "socks");
        assert_eq!(inbounds[0]["port"], 17890);

        // 端口不同（default 1080/1081）时 http 入站照旧单独起
        let cfg2 = emit_ok(CoreKind::Xray, &nodes);
        let ib2 = cfg2["inbounds"].as_array().unwrap();
        assert_eq!(ib2.len(), 2);
        assert_eq!(ib2[1]["protocol"], "http");
    }

    #[test]
    fn singbox_emit_selector_urltest_and_rules() {
        let nodes = vec![
            parse_line("ss://YWVzLTI1Ni1nY206cA@1.1.1.1:8388#S").unwrap(),
            parse_line("hysteria2://p@2.2.2.2:443?obfs=salamander&obfs-password=o#H").unwrap(),
            parse_line(
                "tuic://11111111-2222-3333-4444-555555555555:pw@3.3.3.3:443\
                 ?congestion_control=bbr#U",
            )
            .unwrap(),
        ];
        let cfg = emit_ok(CoreKind::SingBox, &nodes);
        let outbounds = cfg["outbounds"].as_array().unwrap();
        // selector + urltest + 3 节点 + direct
        assert_eq!(outbounds.len(), 6);
        assert_eq!(outbounds[0]["type"], "selector");
        assert_eq!(outbounds[0]["tag"], "select");
        assert_eq!(outbounds[0]["outbounds"][0], "auto");
        assert_eq!(outbounds[1]["type"], "urltest");
        // W10 实跑抓的：ss 出站类型必须是 "shadowsocks" —— "ss" 会让内核
        // FATAL 拒配置（unknown outbound type: ss）。
        assert_eq!(outbounds[2]["type"], "shadowsocks");
        assert_eq!(cfg["route"]["final"], "select");
        assert_eq!(cfg["inbounds"][0]["type"], "mixed");
        assert_eq!(cfg["inbounds"][0]["listen_port"], 1080);
        // hysteria2 obfs 映射
        assert_eq!(outbounds[3]["obfs"]["type"], "salamander");
        assert_eq!(outbounds[4]["congestion_control"], "bbr");
        // 内网直连（ip_is_private 一步覆盖 v4/v6 私有段）
        assert!(cfg["route"]["rules"][0]["ip_is_private"]
            .as_bool()
            .unwrap_or(false));
    }

    #[test]
    fn singbox_emit_reality_transport() {
        let nodes = vec![parse_line(
            "vless://u@1.1.1.1:443?security=reality&pbk=K&sid=ab&fp=chrome&sni=s\
             &type=ws&path=/p#R",
        )
        .unwrap()];
        let cfg = emit_ok(CoreKind::SingBox, &nodes);
        let ob = &cfg["outbounds"].as_array().unwrap()[2]; // selector,urltest 之后
        assert_eq!(ob["type"], "vless");
        assert_eq!(ob["tls"]["reality"]["public_key"], "K");
        assert_eq!(ob["tls"]["utls"]["fingerprint"], "chrome");
        assert_eq!(ob["transport"]["type"], "ws");
        assert_eq!(ob["transport"]["path"], "/p");
    }

    #[test]
    fn mihomo_emit_is_refused_with_reason() {
        let nodes = vec![parse_line("ss://YWVzLTI1Ni1nY206cA@1.1.1.1:1#n").unwrap()];
        let err = emit(CoreKind::Mihomo, &nodes, &EmitOptions::default()).unwrap_err();
        assert!(err.contains("proxy-provider"));
    }

    #[test]
    fn empty_nodes_refused() {
        assert!(emit(CoreKind::Xray, &[], &EmitOptions::default()).is_err());
        assert!(emit(CoreKind::SingBox, &[], &EmitOptions::default()).is_err());
    }
}
