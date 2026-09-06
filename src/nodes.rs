//! 节点模型、订阅解析（URI 列表 / 整体 base64 / Clash YAML）与本地库存取。
//!
//! 设计要点：
//! - 扫描出的每个节点都保留**原始串 raw**（URI 或 clash-yaml 块）。
//!   `test` 阶段直接把 raw 喂给 mihomo 的 file proxy-provider，零字段损失。
//! - mihomo 的 file provider 原生吃「URI 列表」和「整体 base64」，但**不吃**
//!   clash 标准 yaml（proxies: 数组）。所以 clash-yaml 源需要先转成 URI，
//!   转不出的节点跳过并记日志。

use std::collections::HashSet;
use std::path::PathBuf;

use base64::Engine;
use serde::{Deserialize, Serialize};

/// 支持的协议（用于识别 URI 行）
const SCHEMES: &[&str] = &[
    "vless://",
    "vmess://",
    "trojan://",
    "ss://",
    "hysteria2://",
    "hysteria://",
    "tuic://",
    "socks5://",
    "socks://",
    "http://",
    "https://",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    /// 稳定去重 id（raw 的 sha1）
    pub id: String,
    /// 协议：vless / vmess / trojan / ss / hysteria2 / tuic / socks5 / http ...
    pub proto: String,
    /// 显示名（来自 URI 的 #tag 或 clash 的 name）
    pub name: String,
    pub server: String,
    pub port: u16,
    /// 原始串：URI 或 clash-yaml 块（mihomo 直接吃）
    pub raw: String,
    /// 来自哪个订阅源
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodesDB {
    pub updated_at: String,
    pub sources: Vec<String>,
    /// 已抓取的订阅源 URL（去重后，便于增量）
    pub fetched: Vec<String>,
    pub nodes: Vec<Node>,
}

impl NodesDB {
    pub fn load() -> NodesDB {
        let p = data_path();
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => NodesDB::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = data_path();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(self)?)
    }

    /// 合并新节点，按 id 去重
    pub fn merge(&mut self, incoming: Vec<Node>) {
        let mut seen: HashSet<String> = self.nodes.iter().map(|n| n.id.clone()).collect();
        for n in incoming {
            if seen.insert(n.id.clone()) {
                self.nodes.push(n);
            }
        }
    }
}

pub fn data_path() -> PathBuf {
    // 项目目录下的 data/nodes.json
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("data");
    p.push("nodes.json");
    p
}

/// 整体解析一个订阅源内容。自动探测三种格式。
pub fn parse_subscription(text: &str, source: &str) -> Vec<Node> {
    // 1) 整体 base64？解码后若含 :// 则按 URI 列表处理
    if let Some(decoded) = try_base64(text) {
        if decoded.contains("://") {
            return parse_uris(&decoded, source);
        }
    }
    // 2) clash yaml（proxies: / proxy-groups:）
    if text.contains("proxies:") || text.contains("proxy-groups:") {
        if let Some(v) = parse_clash_yaml(text, source) {
            return v;
        }
    }
    // 3) 默认按 URI 列表
    parse_uris(text, source)
}

/// 逐行提取协议 URI
fn parse_uris(text: &str, source: &str) -> Vec<Node> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(node) = parse_one_uri(line, source) {
            out.push(node);
        }
    }
    out
}

/// 从一行里提取第一个协议 URI。
/// 行内可能夹带 clash 风格注释 " # note"，需裁掉（URI 的 #tag 不含 " # "）。
fn parse_one_uri(line: &str, source: &str) -> Option<Node> {
    let lower = line.to_ascii_lowercase();
    // 找第一个出现的 scheme
    let mut at: Option<(usize, &str)> = None;
    for s in SCHEMES {
        if let Some(i) = lower.find(s) {
            match at {
                Some((best, _)) if i >= best => {}
                _ => at = Some((i, *s)),
            }
        }
    }
    let (start, scheme) = at?;
    let mut rest = &line[start..];
    // 裁掉尾部 " # 注释"（clash 注释，区别于 URI 的 #tag）
    if let Some(pos) = rest.rfind(" # ") {
        rest = &rest[..pos];
    }
    rest = rest.trim_end();
    if rest.is_empty() {
        return None;
    }
    let proto = scheme.trim_end_matches("://").to_string();

    // 取 #tag 作为显示名（percent-decode 还原 emoji）
    let (body, raw_name) = match rest.split_once('#') {
        Some((b, n)) => (b, n),
        None => (rest, ""),
    };
    let name = percent_decode(raw_name);

    // 解析 host:port（忽略 userinfo 与 query）
    let after = &body[scheme.len() + 3..]; // 去掉 "scheme://"
    let (hostport, _q) = after.split_once('?').unwrap_or((after, ""));
    let (hp, _user) = hostport.rsplit_once('@').unwrap_or((hostport, ""));
    let (host, port_s) = hp.rsplit_once(':').unwrap_or(("", ""));
    let host = host.trim_matches('[').trim_matches(']').to_string();
    let port: u16 = port_s.parse().unwrap_or(0);

    let id = sha1_hex(rest);

    Some(Node {
        id,
        proto,
        name,
        server: host,
        port,
        raw: rest.to_string(),
        source: source.to_string(),
    })
}

/// 解析 clash yaml 订阅：提取 proxies 数组，逐条转 URI。
/// 转不出的节点跳过（记日志由调用方处理）。
fn parse_clash_yaml(text: &str, source: &str) -> Option<Vec<Node>> {
    let doc: serde_yaml::Value = serde_yaml::from_str(text).ok()?;
    let proxies = doc.get("proxies")?.as_sequence()?;
    let mut out = Vec::new();
    for p in proxies {
        if let Some(uri) = yaml_proxy_to_uri(p) {
            let name = proxy_name(p);
            let server = p
                .get("server")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let port = p.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let proto = uri_proto(&uri);
            out.push(Node {
                id: sha1_hex(&uri),
                proto,
                name,
                server,
                port,
                raw: uri,
                source: source.to_string(),
            });
        }
    }
    Some(out)
}

fn proxy_name(p: &serde_yaml::Value) -> String {
    p.get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string()
}

fn uri_proto(uri: &str) -> String {
    uri.split("://").next().unwrap_or("").to_string()
}

/// clash yaml 的单个 proxy 块 → v2ray URI。
/// 覆盖 ss / trojan / vless / vmess / socks5 / http(s)。其余返回 None。
fn yaml_proxy_to_uri(p: &serde_yaml::Value) -> Option<String> {
    let ty = p.get("type")?.as_str()?;
    let name = proxy_name(p);
    let server = p.get("server")?.as_str()?;
    let port = p.get("port")?.as_u64()?;
    let enc_name = percent_encode(&name);
    match ty {
        "ss" => {
            let method = p.get("cipher").or_else(|| p.get("method"))?.as_str()?;
            let pass = p.get("password")?.as_str()?;
            let userinfo = base64::engine::general_purpose::STANDARD
                .encode(format!("{method}:{pass}"));
            Some(format!(
                "ss://{userinfo}@{server}:{port}#{enc_name}"
            ))
        }
        "trojan" => {
            let pass = p.get("password")?.as_str()?;
            let mut q = vec![];
            if let Some(sni) = p.get("sni").and_then(|v| v.as_str()) {
                q.push(format!("sni={sni}"));
            }
            if p.get("skip-cert-verify").and_then(|v| v.as_bool()) == Some(true) {
                q.push("allowInsecure=1".into());
            }
            let q = if q.is_empty() {
                String::new()
            } else {
                format!("?{}", q.join("&"))
            };
            Some(format!("trojan://{pass}@{server}:{port}{q}#{enc_name}"))
        }
        "vless" => {
            let id = p.get("uuid").or_else(|| p.get("id"))?.as_str()?;
            let mut q = vec!["encryption=none".to_string()];
            let security = if p.get("tls").and_then(|v| v.as_bool()) == Some(true) {
                "tls"
            } else {
                "none"
            };
            q.push(format!("security={security}"));
            let net = p.get("network").and_then(|v| v.as_str()).unwrap_or("tcp");
            q.push(format!("type={net}"));
            if net == "ws" {
                if let Some(opts) = p.get("ws-opts") {
                    if let Some(path) = opts.get("path").and_then(|v| v.as_str()) {
                        q.push(format!("path={}", percent_encode(path)));
                    }
                    if let Some(host) = opts
                        .get("headers")
                        .and_then(|h| h.get("Host"))
                        .and_then(|v| v.as_str())
                    {
                        q.push(format!("host={host}"));
                    }
                }
            }
            if let Some(sni) = p.get("servername").or_else(|| p.get("sni")).and_then(|v| v.as_str()) {
                q.push(format!("sni={sni}"));
            }
            if let Some(fp) = p.get("client-fingerprint").and_then(|v| v.as_str()) {
                q.push(format!("fp={fp}"));
            }
            Some(format!(
                "vless://{id}@{server}:{port}?{}#{enc_name}",
                q.join("&")
            ))
        }
        "vmess" => {
            let id = p.get("uuid").or_else(|| p.get("id"))?.as_str()?;
            let aid = p.get("alterId").and_then(|v| v.as_u64()).unwrap_or(0);
            let scy = p.get("cipher").unwrap_or(&serde_yaml::Value::from("auto")).as_str().unwrap_or("auto").to_string();
            let net = p.get("network").and_then(|v| v.as_str()).unwrap_or("tcp").to_string();
            let tls = if p.get("tls").and_then(|v| v.as_bool()) == Some(true) {
                "tls"
            } else {
                ""
            };
            let (host, path) = match net.as_str() {
                "ws" => {
                    let opts = p.get("ws-opts");
                    let host = opts
                        .and_then(|o| o.get("headers"))
                        .and_then(|h| h.get("Host"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let path = opts
                        .and_then(|o| o.get("path"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    (host, path)
                }
                _ => ("".to_string(), "".to_string()),
            };
            let sni = p
                .get("servername")
                .or_else(|| p.get("sni"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let obj = serde_json::json!({
                "v": "2",
                "ps": name,
                "add": server,
                "port": port,
                "id": id,
                "aid": aid,
                "scy": scy,
                "net": net,
                "type": "none",
                "host": host,
                "path": path,
                "tls": tls,
                "sni": sni,
            });
            let b64 = base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_string(&obj).ok()?);
            Some(format!("vmess://{b64}#{enc_name}"))
        }
        "socks5" | "socks" => {
            let user = p.get("username").and_then(|v| v.as_str()).unwrap_or("");
            let pass = p.get("password").and_then(|v| v.as_str()).unwrap_or("");
            let auth = if !user.is_empty() {
                format!("{user}:{pass}@")
            } else {
                String::new()
            };
            Some(format!(
                "socks5://{auth}{server}:{port}#{enc_name}"
            ))
        }
        "http" | "https" => {
            let user = p.get("username").and_then(|v| v.as_str()).unwrap_or("");
            let pass = p.get("password").and_then(|v| v.as_str()).unwrap_or("");
            let auth = if !user.is_empty() {
                format!("{user}:{pass}@")
            } else {
                String::new()
            };
            Some(format!(
                "{ty}://{auth}{server}:{port}#{enc_name}"
            ))
        }
        _ => None,
    }
}

/// 尝试整体 base64 解码（宽松：忽略空白、容忍非 base64 字符则返回 None）
fn try_base64(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() < 16 {
        return None;
    }
    // 去掉可能的空白与换行
    let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    // base64 标准字母表
    if compact.chars().any(|c| !c.is_ascii_alphanumeric() && c != '+' && c != '/' && c != '=') {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(&compact).ok()?;
    String::from_utf8(decoded).ok()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn sha1_hex(s: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(s.as_bytes());
    let d = h.finalize();
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从 README 文本里提取订阅源 URL（去重、剥掉 # 片段）
pub fn extract_source_urls(readme: &str) -> Vec<String> {
    let mut set: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let bytes = readme.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 找 "https://"
        if bytes[i..].starts_with(b"https://") {
            // 截到空白/引号/反引号/尖括号为止
            let mut j = i;
            while j < bytes.len() {
                match bytes[j] {
                    b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'\'' | b'`' | b'<' | b'>' | b')'
                    | b']' | b'}' => break,
                    _ => j += 1,
                }
            }
            let mut url = readme[i..j].to_string();
            if let Some(p) = url.find('#') {
                url.truncate(p);
            }
            if !url.is_empty() && seen.insert(url.clone()) {
                set.push(url);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    set
}

/// 把一个 URI 节点转成 clash-yaml 的 proxy 块（缩进 2 空格，供 `proxies:` 下列表项）。
/// 覆盖 ss / trojan / vless / vmess / socks5 / http。转不出返回 None。
pub fn node_to_clash_yaml_block(node: &Node) -> Option<String> {
    let raw = &node.raw;
    match node.proto.as_str() {
        "ss" => {
            // ss://base64(method:pass)@host:port 或 ss://method:pass@host:port
            let after = raw.strip_prefix("ss://")?;
            let (hp, _tag) = split_tag(after);
            let (userinfo, hostport) = hp.rsplit_once('@')?;
            let (method, pass) = decode_ss_userinfo(userinfo);
            let (server, port) = host_port(hostport)?;
            Some(format!(
                "  - name: {}\n    type: ss\n    server: {}\n    port: {}\n    cipher: {}\n    password: {}\n",
                yaml_str(&node.name), server, port, method, yaml_str(&pass)
            ))
        }
        "trojan" => {
            let after = raw.strip_prefix("trojan://")?;
            let (hp, _tag) = split_tag(after);
            let (pass, hostport) = hp.rsplit_once('@')?;
            let (server, port) = host_port(hostport)?;
            let q = query_of(hp);
            let sni = q.get("sni").cloned().unwrap_or_default();
            let allow = q.contains_key("allowInsecure") || q.contains_key("allowInsecureCrt");
            let mut s = format!(
                "  - name: {}\n    type: trojan\n    server: {}\n    port: {}\n    password: {}\n",
                yaml_str(&node.name), server, port, yaml_str(pass)
            );
            if !sni.is_empty() {
                s.push_str(&format!("    sni: {}\n", yaml_str(&sni)));
            }
            if allow {
                s.push_str("    skip-cert-verify: true\n");
            }
            Some(s)
        }
        "vless" => {
            let after = raw.strip_prefix("vless://")?;
            let (hp, _tag) = split_tag(after);
            let (uuid, hostport) = hp.rsplit_once('@')?;
            let (server, port) = host_port(hostport)?;
            let q = query_of(hp);
            let security = q.get("security").cloned().unwrap_or_else(|| "none".into());
            let net = q.get("type").cloned().unwrap_or_else(|| "tcp".into());
            let sni = q.get("sni").cloned().unwrap_or_default();
            let fp = q.get("fp").cloned().unwrap_or_default();
            let mut s = format!(
                "  - name: {}\n    type: vless\n    server: {}\n    port: {}\n    uuid: {}\n    network: {}\n    tls: {}\n    udp: true\n",
                yaml_str(&node.name), server, port, uuid, net,
                if security == "tls" || security == "reality" { "true" } else { "false" }
            );
            if net == "ws" {
                let path = q.get("path").cloned().unwrap_or_default();
                let host = q.get("host").cloned().unwrap_or_else(|| sni.clone());
                s.push_str("    ws-opts:/n      path: ");
                s.push_str(&yaml_str(&path));
                s.push_str("\n      headers:/n        Host: ");
                s.push_str(&yaml_str(&if host.is_empty() { sni.clone() } else { host }));
                s.push('\n');
            }
            if !sni.is_empty() {
                s.push_str(&format!("    servername: {}\n", yaml_str(&sni)));
            }
            if !fp.is_empty() {
                s.push_str(&format!("    client-fingerprint: {}\n", yaml_str(&fp)));
            }
            Some(s)
        }
        "vmess" => {
            let after = raw.strip_prefix("vmess://")?;
            let b64 = split_tag(after).0;
            let json = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
            let v: serde_json::Value = serde_json::from_slice(&json).ok()?;
            let ps = v.get("ps").and_then(|x| x.as_str()).unwrap_or(&node.name).to_string();
            let add = v.get("add").and_then(|x| x.as_str())?;
            let port = v.get("port").and_then(|x| x.as_u64())? as u16;
            let id = v.get("id").and_then(|x| x.as_str())?;
            let aid = v.get("aid").and_then(|x| x.as_u64()).unwrap_or(0);
            let scy = v.get("scy").and_then(|x| x.as_str()).unwrap_or("auto");
            let net = v.get("net").and_then(|x| x.as_str()).unwrap_or("tcp");
            let tls = v.get("tls").and_then(|x| x.as_str()).unwrap_or("");
            let host = v.get("host").and_then(|x| x.as_str()).unwrap_or("");
            let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
            let mut s = format!(
                "  - name: {}\n    type: vmess\n    server: {}\n    port: {}\n    uuid: {}\n    alterId: {}\n    cipher: {}\n    network: {}\n    tls: {}\n    udp: true\n",
                yaml_str(&ps), add, port, id, aid, scy, net,
                if tls == "tls" { "true" } else { "false" }
            );
            if net == "ws" {
                s.push_str("    ws-opts:/n      path: ");
                s.push_str(&yaml_str(path));
                s.push_str("\n      headers:/n        Host: ");
                s.push_str(&yaml_str(host));
                s.push('\n');
            }
            Some(s)
        }
        "socks5" | "socks" => {
            let after = raw.strip_prefix(node.proto.as_str()).and_then(|s| s.strip_prefix("://"))?;
            let (hp, _tag) = split_tag(after);
            let (auth, hostport) = match hp.rsplit_once('@') {
                Some((a, h)) => (a, h),
                None => ("", hp),
            };
            let (server, port) = host_port(hostport)?;
            let mut s = format!(
                "  - name: {}\n    type: socks5\n    server: {}\n    port: {}\n",
                yaml_str(&node.name), server, port
            );
            if !auth.is_empty() {
                let (u, p) = auth.split_once(':').unwrap_or((auth, ""));
                s.push_str(&format!("    username: {}\n    password: {}\n", yaml_str(u), yaml_str(p)));
            }
            Some(s)
        }
        "http" | "https" => {
            let after = raw.strip_prefix(node.proto.as_str()).and_then(|s| s.strip_prefix("://"))?;
            let (hp, _tag) = split_tag(after);
            let (auth, hostport) = match hp.rsplit_once('@') {
                Some((a, h)) => (a, h),
                None => ("", hp),
            };
            let (server, port) = host_port(hostport)?;
            let mut s = format!(
                "  - name: {}\n    type: {}\n    server: {}\n    port: {}\n",
                yaml_str(&node.name), node.proto, server, port
            );
            if !auth.is_empty() {
                let (u, p) = auth.split_once(':').unwrap_or((auth, ""));
                s.push_str(&format!("    username: {}\n    password: {}\n", yaml_str(u), yaml_str(p)));
            }
            Some(s)
        }
        "tuic" => {
            // tuic://uuid:password@host:port?sni=&alpn=
            let after = raw.strip_prefix("tuic://")?;
            let (hp, _tag) = split_tag(after);
            let (auth, hostport) = hp.rsplit_once('@')?;
            let (uuid, pass) = auth.split_once(':').unwrap_or((auth, ""));
            let (server, port) = host_port(hostport)?;
            let q = query_of(hp);
            let sni = q.get("sni").cloned().unwrap_or_default();
            let alpn = q.get("alpn").cloned().unwrap_or_default();
            let mut s = format!(
                "  - name: {}\n    type: tuic\n    server: {}\n    port: {}\n    uuid: {}\n    password: {}\n    alpn: [\"h3\"]\n",
                yaml_str(&node.name), server, port, uuid, yaml_str(pass)
            );
            if !sni.is_empty() {
                s.push_str(&format!("    sni: {}\n", yaml_str(&sni)));
            }
            if !alpn.is_empty() {
                s.push_str(&format!("    alpn: [{}]\n", alpn.split(',').map(|a| format!("\"{}\"", a.trim())).collect::<Vec<_>>().join(", ")));
            }
            Some(s)
        }
        "hysteria2" | "hysteria" => {
            // hysteria2://password@host:port?sni=&insecure=
            let after = raw.strip_prefix(node.proto.as_str()).and_then(|s| s.strip_prefix("://"))?;
            let (hp, _tag) = split_tag(after);
            let (pass, hostport) = hp.rsplit_once('@')?;
            let (server, port) = host_port(hostport)?;
            let q = query_of(hp);
            let sni = q.get("sni").cloned().unwrap_or_default();
            let insecure = q.contains_key("insecure") || q.contains_key("allowInsecure");
            let mut s = format!(
                "  - name: {}\n    type: hysteria2\n    server: {}\n    port: {}\n    password: {}\n",
                yaml_str(&node.name), server, port, yaml_str(pass)
            );
            if !sni.is_empty() {
                s.push_str(&format!("    sni: {}\n", yaml_str(&sni)));
            }
            if insecure {
                s.push_str("    skip-cert-verify: true\n");
            }
            Some(s)
        }
        _ => None,
    }
}

fn split_tag(s: &str) -> (&str, &str) {
    match s.split_once('#') {
        Some((b, t)) => (b, t),
        None => (s, ""),
    }
}

fn query_of(hp: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let q = match hp.split_once('?') {
        Some((_, q)) => q,
        None => return map,
    };
    for kv in q.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            map.insert(k.to_string(), percent_decode(v));
        }
    }
    map
}

fn host_port(hp: &str) -> Option<(String, u16)> {
    // 先去掉 query（host:port?type=ws&...），否则 port 解析会带上 query 导致失败
    let hp = hp.split('?').next().unwrap_or(hp).trim_end_matches('/');
    let (h, p) = hp.rsplit_once(':')?;
    let port = p.parse().ok()?;
    Some((h.trim_matches('[').trim_matches(']').to_string(), port))
}

fn decode_ss_userinfo(userinfo: &str) -> (String, String) {
    if let Ok(d) = base64::engine::general_purpose::STANDARD.decode(userinfo) {
        if let Ok(s) = String::from_utf8(d) {
            if let Some((m, p)) = s.split_once(':') {
                return (m.to_string(), p.to_string());
            }
        }
    }
    // 明文 method:password
    if let Some((m, p)) = userinfo.split_once(':') {
        return (m.to_string(), p.to_string());
    }
    (userinfo.to_string(), String::new())
}

/// 给 YAML 标量加引号并转义（节点名可能含 emoji/`:`/特殊字符）
fn yaml_str(s: &str) -> String {
    let escaped = s.replace('"', "\\\"");
    format!("\"{escaped}\"")
}

// ─────────────────────────────────────────────────────────────
// 核心能力（lib 契约）：scan_core / test_core / add_core
// 由 bin（lilyco App）或 cdylib（C ABI）调用，通过 sink 回传事件。
// ─────────────────────────────────────────────────────────────

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::mihomo::Mihomo;
use crate::{Event, Level};

/// free-VPN 仓库 README 索引（自动扫描的默认源）
const FREE_VPN_README: &str = "https://raw.githubusercontent.com/lilyco-42/free-VPN/main/README.md";

/// 扫描参数（由 lilyco App / C ABI 反序列化得到）
#[derive(serde::Deserialize)]
pub struct ScanParams {
    pub source: Option<Vec<String>>,
    pub include_repo: bool,
    pub max_sources: u64,
    pub concurrency: u64,
    pub per_limit: u64,
    pub output: PathBuf,
}

/// 测试参数
#[derive(serde::Deserialize)]
pub struct TestParams {
    pub input: PathBuf,
    pub top: u64,
    pub concurrency: u64,
    pub timeout_ms: u64,
    pub test_url: String,
    pub mihomo: Option<PathBuf>,
}

/// 导出 / 注入参数
#[derive(serde::Deserialize)]
pub struct AddParams {
    pub input: PathBuf,
    pub keep: u64,
    pub max_ms: u64,
    pub apply: bool,
    pub profile: Option<PathBuf>,
}

/// 单条测速结果
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct TestResult {
    pub node: Node,
    pub delay_ms: u64,
}

fn emit(sink: &dyn Fn(&Event), e: Event) {
    sink(&e);
}

fn log(sink: &dyn Fn(&Event), level: Level, msg: String) {
    emit(sink, Event::Log { level, message: msg });
}

/// 扫描 free-VPN 索引 + 额外源，抓订阅解析去重，落库 nodes.json
pub async fn scan_core(sp: ScanParams, sink: &dyn Fn(&Event)) -> Result<serde_json::Value, String> {
    let start = Instant::now();
    let client = reqwest::Client::builder()
        .no_proxy() // 直连，避免工作区代理污染
        .timeout(Duration::from_secs(20))
        .user_agent("ghboost/0.2")
        .build()
        .map_err(|e| format!("http 客户端创建失败: {e}"))?;

    // 1) 收集源 URL
    let mut urls: Vec<String> = Vec::new();
    let mut repo_err: Option<String> = None;
    if sp.include_repo {
        match fetch_text(&client, FREE_VPN_README).await {
            Ok(t) => {
                let extracted = extract_source_urls(&t);
                log(sink, Level::Info, format!("free-VPN 索引解析出 {} 个订阅源", extracted.len()));
                urls.extend(extracted);
            }
            Err(e) => {
                repo_err = Some(e.clone());
                log(sink, Level::Warn, format!("free-VPN README 抓取失败: {e}，跳过仓库索引"));
            }
        }
    }
    if let Some(extra) = &sp.source {
        urls.extend(extra.iter().cloned());
    }
    if urls.is_empty() {
        if repo_err.is_some() && sp.source.is_none() {
            return Err(format!(
                "没有可用订阅源：free-VPN README 抓取失败（{}），且未提供 --source。\
请确认本机可访问 GitHub raw（如 Clash Verge 已启动并选好节点、7890 端口在监听），\
或改用 `scan --source <订阅URL>` 直接指定源。注意：本工具不借用任何外部 HTTP 代理，\
需要本机直连或经 Clash 直连 GitHub。",
                repo_err.unwrap()
            ));
        }
        return Err("没有可用订阅源（include_repo 与 source 都为空或失败）".into());
    }
    urls.truncate(sp.max_sources as usize);
    let n_src = urls.len();

    emit(sink, Event::Started { total: Some(n_src as u64), message: Some("扫描订阅源".into()) });

    // 2) 并发抓源 + 解析
    let sem = Arc::new(Semaphore::new(sp.concurrency.max(1) as usize));
    let mut set: JoinSet<(String, usize, Vec<Node>)> = JoinSet::new();
    for url in &urls {
        let sem = sem.clone();
        let client = client.clone();
        let u = url.clone();
        let per = sp.per_limit;
        set.spawn(async move {
            let _p = sem.acquire().await;
            let text = fetch_text(&client, &u).await.unwrap_or_default();
            // per_limit：URI 列表源截断行数（base64 源通常不大，不截断）
            let limited = if text.lines().count() as u64 > per {
                text.lines().take(per as usize).collect::<Vec<_>>().join("\n")
            } else {
                text
            };
            let nodes = if limited.trim().is_empty() {
                vec![]
            } else {
                parse_subscription(&limited, &u)
            };
            (u, nodes.len(), nodes)
        });
    }

    let mut db = NodesDB::default();
    let mut ok_src = 0usize;
    let mut i = 0usize;
    while let Some(r) = set.join_next().await {
        i += 1;
        if let Ok((url, n, nodes)) = r {
            if n > 0 {
                ok_src += 1;
                db.merge(nodes);
            } else {
                log(sink, Level::Warn, format!("源无节点: {url}"));
            }
        }
        emit(sink, Event::Tick { current: i as u64, total: Some(n_src as u64), message: format!("扫描源 {i}/{n_src}") });
    }

    // 3) 落库
    let db_path = sp.output.join("nodes.json");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    db.updated_at = chrono_now();
    db.sources = urls.clone();
    std::fs::write(&db_path, serde_json::to_string_pretty(&db).map_err(|e| format!("序列化失败: {e}"))?)
        .map_err(|e| format!("写库失败: {e}"))?;

    // 协议分布
    let mut dist: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for n in &db.nodes {
        *dist.entry(n.proto.clone()).or_default() += 1;
    }

    let out = serde_json::json!({
        "sources_total": n_src,
        "sources_ok": ok_src,
        "nodes_total": db.nodes.len(),
        "proto_dist": dist,
        "db_path": db_path.to_string_lossy(),
        "elapsed_ms": start.elapsed().as_millis() as u64,
    });
    log(sink, Level::Info, format!("扫描完成：{} 源 / {} 节点", n_src, db.nodes.len()));
    emit(sink, Event::Done { output: out.clone(), elapsed_ms: start.elapsed().as_millis() as u64 });
    Ok(out)
}

/// 用独立 mihomo 实例测节点延迟，落库 tests.json
pub async fn test_core(tp: TestParams, sink: &dyn Fn(&Event)) -> Result<serde_json::Value, String> {
    let start = Instant::now();
    let db_path = tp.input.join("nodes.json");
    let db: NodesDB = match std::fs::read_to_string(&db_path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("读节点库失败: {e}"))?,
        Err(_) => return Err(format!("未找到节点库 {}，请先运行 scan", db_path.display())),
    };
    if db.nodes.is_empty() {
        return Err("节点库为空，无法测试".into());
    }

    // 取前 top 个（按库顺序）
    let top = if tp.top == 0 { db.nodes.len() as u64 } else { tp.top };
    let batch_nodes: Vec<Node> = db.nodes.iter().take(top as usize).cloned().collect();
    let n = batch_nodes.len();

    emit(sink, Event::Started { total: Some(n as u64), message: Some("启动 Mihomo 测速".into()) });

    // 单次全量加载（独立实例，按节点名 n{index} 路由）
    // 注意：Mihomo::start 内含阻塞式等待（轮询 controller 端口），必须放进
    // spawn_blocking 的专用阻塞线程，否则会在 lilyco 已有的 tokio runtime 内
    // 触发 "Cannot drop a runtime in a context where blocking is not allowed"。
    log(sink, Level::Info, format!("启动独立 Mihomo 实例，加载 {} 个节点…", n));
    let mh = {
        let batch = batch_nodes.clone();
        let bin = tp.mihomo.clone();
        match tokio::task::spawn_blocking(move || Mihomo::start(&batch, &bin)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => return Err(format!("Mihomo 启动失败: {e}")),
            Err(e) => return Err(format!("Mihomo 启动线程异常: {e}")),
        }
    };

    log(sink, Level::Info, "并发测速中…".into());
    let raw = mh.test_all(&tp.test_url, tp.timeout_ms, tp.concurrency as usize).await;

    // 收集可用节点（delay 命中）
    let mut results: Vec<TestResult> = Vec::new();
    let mut dead = 0usize;
    for (i, delay) in raw {
        match delay {
            Some(d) => {
                if let Some(node) = batch_nodes.get(i) {
                    results.push(TestResult { node: node.clone(), delay_ms: d });
                }
            }
            None => dead += 1,
        }
    }
    results.sort_by_key(|r| r.delay_ms);

    // 落库
    let tests_path = tp.input.join("tests.json");
    let tests_json = serde_json::json!({
        "test_url": tp.test_url,
        "timeout_ms": tp.timeout_ms,
        "updated_at": chrono_now(),
        "results": results,
    });
    std::fs::write(&tests_path, serde_json::to_string_pretty(&tests_json).map_err(|e| format!("序列化失败: {e}"))?)
        .map_err(|e| format!("写测试结果失败: {e}"))?;

    let out = serde_json::json!({
        "tested": n,
        "alive": results.len(),
        "dead": dead,
        "best_ms": results.first().map(|r| r.delay_ms).unwrap_or(0),
        "worst_alive_ms": results.last().map(|r| r.delay_ms).unwrap_or(0),
        "tests_path": tests_path.to_string_lossy(),
        "elapsed_ms": start.elapsed().as_millis() as u64,
        "top5": results.iter().take(5).map(|r| serde_json::json!({"name": r.node.name, "proto": r.node.proto, "delay_ms": r.delay_ms})).collect::<Vec<_>>(),
    });
    log(sink, Level::Info, format!("测速完成：{} 测 / {} 可用 / {} 死", n, results.len(), dead));
    emit(sink, Event::Done { output: out.clone(), elapsed_ms: start.elapsed().as_millis() as u64 });
    Ok(out)
}

/// 导出 / 注入 Top N 可用节点
pub async fn add_core(ap: AddParams, sink: &dyn Fn(&Event)) -> Result<serde_json::Value, String> {
    let start = Instant::now();
    let tests_path = ap.input.join("tests.json");
    let raw: serde_json::Value = match std::fs::read_to_string(&tests_path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("读测试结果失败: {e}"))?,
        Err(_) => return Err(format!("未找到测试结果 {}，请先运行 test", tests_path.display())),
    };
    let mut results: Vec<TestResult> = serde_json::from_value(raw.get("results").cloned().unwrap_or(serde_json::Value::Null))
        .map_err(|e| format!("解析结果失败: {e}"))?;
    // 按延迟升序，过滤上限
    results.sort_by_key(|r| r.delay_ms);
    if ap.max_ms > 0 {
        results.retain(|r| r.delay_ms <= ap.max_ms);
    }
    results.truncate(ap.keep as usize);
    if results.is_empty() {
        return Err("没有满足延迟条件的可用节点".into());
    }

    emit(sink, Event::Started { total: Some(results.len() as u64), message: Some("导出节点".into()) });

    if ap.apply {
        // 写入 Clash Verge 当前激活的 local profile（带备份）
        let profile = match &ap.profile {
            Some(p) => p.clone(),
            None => clash_verge_active_profile().ok_or_else(|| "无法定位 Clash Verge 配置目录".to_string())?,
        };
        let mut blocks = Vec::new();
        let mut failed = 0usize;
        for r in &results {
            match node_to_clash_yaml_block(&r.node) {
                Some(b) => blocks.push(b),
                None => failed += 1,
            }
        }
        if blocks.is_empty() {
            return Err("所有候选节点都无法转换为 Clash 配置（协议不支持）".into());
        }
        let yaml = format!("proxies:/n{}", blocks.concat());
        // 备份 + 追加 proxies
        let backup = profile.with_extension(format!("yaml.bak.{}", now_ts()));
        std::fs::copy(&profile, &backup).map_err(|e| format!("备份 profile 失败: {e}"))?;
        let existing = std::fs::read_to_string(&profile).unwrap_or_default();
        let merged = merge_clash_proxies(&existing, &yaml);
        std::fs::write(&profile, merged).map_err(|e| format!("写 profile 失败: {e}"))?;
        let out = serde_json::json!({
            "applied": true,
            "count": blocks.len(),
            "failed_proto": failed,
            "profile": profile.to_string_lossy(),
            "backup": backup.to_string_lossy(),
            "note": "已追加到 Clash Verge 当前 profile 的 proxies，切换/重载该配置后生效",
        });
        log(sink, Level::Info, format!("已注入 {} 个节点到 {}", blocks.len(), profile.display()));
        emit(sink, Event::Done { output: out.clone(), elapsed_ms: start.elapsed().as_millis() as u64 });
        Ok(out)
    } else {
        // 默认：导出订阅文件（URI 列表 + base64）
        let uris: Vec<String> = results.iter().map(|r| r.node.raw.clone()).collect();
        let uri_path = ap.input.join("ghboost_sub.txt");
        std::fs::write(&uri_path, uris.join("\n")).map_err(|e| format!("写订阅失败: {e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(uris.join("\n"));
        let b64_path = ap.input.join("ghboost_sub.b64.txt");
        std::fs::write(&b64_path, &b64).map_err(|e| format!("写 base64 订阅失败: {e}"))?;
        let out = serde_json::json!({
            "applied": false,
            "count": results.len(),
            "uri_sub": uri_path.to_string_lossy(),
            "b64_sub": b64_path.to_string_lossy(),
            "top": results.iter().take(5).map(|r| serde_json::json!({"name": r.node.name, "delay_ms": r.delay_ms})).collect::<Vec<_>>(),
            "note": "已导出订阅文件，在 Clash Verge / v2rayN 等客户端「订阅」里导入即可",
        });
        log(sink, Level::Info, format!("已导出 {} 个优质节点订阅", results.len()));
        emit(sink, Event::Done { output: out.clone(), elapsed_ms: start.elapsed().as_millis() as u64 });
        Ok(out)
    }
}

// ── 工具 ──

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    match client.get(url).send().await {
        Ok(r) => r.text().await.map_err(|e| format!("读取响应体失败: {e}")),
        Err(e) => Err(format!("请求失败: {e}")),
    }
}

/// 定位 Clash Verge 当前激活的 local profile 文件路径。
/// 读 profiles.yaml 的 current 字段，拼出 profiles/<current>.yaml。
fn clash_verge_active_profile() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    let dir = PathBuf::from(appdata).join("io.github.clash-verge-rev.clash-verge-rev");
    let profiles_yaml = dir.join("profiles.yaml");
    let text = std::fs::read_to_string(&profiles_yaml).ok()?;
    let v: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let current = v.get("current")?.as_str()?;
    // current 指向 profiles/<uid>.yaml
    let p = dir.join("profiles").join(format!("{current}.yaml"));
    if p.exists() { Some(p) } else { None }
}

/// 把新增 proxies 合并进已有 clash yaml（保留其它段，proxies 追加去重）
fn merge_clash_proxies(existing: &str, append: &str) -> String {
    let mut out = String::new();
    let mut appended = false;
    for line in existing.lines() {
        out.push_str(line);
        out.push('\n');
        if !appended && line.trim_start().starts_with("proxies:") {
            // 在 proxies: 之后插入新增块
            out.push_str(append.strip_prefix("proxies:/n").unwrap_or(append));
            out.push('\n');
            appended = true;
        }
    }
    if !appended {
        out.push('\n');
        out.push_str(append);
    }
    out
}

fn chrono_now() -> String {
    // 不引入 chrono 依赖：用系统时间戳
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
