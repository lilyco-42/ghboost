//! proxy — 跨平台系统代理设置
//!
//! 支持平台：
//! - Windows: 注册表修改 (HKCU\...\Internet Settings)
//! - macOS: networksetup 命令
//! - Linux: gsettings (GNOME) + 环境变量
//! - Android: VPN Service（需要 App 层配合）
//! - iOS: NetworkExtension（需要 App 层配合）

/// 代理配置
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProxyConfig {
    /// HTTP 代理地址 (e.g., "127.0.0.1")
    pub host: String,
    /// HTTP 代理端口 (e.g., 7890)
    pub port: u16,
    /// SOCKS5 代理端口 (可选，与 HTTP 代理同端口则为 None)
    pub socks_port: Option<u16>,
    /// 绕过列表 (e.g., "localhost,127.0.0.1,::1")
    pub bypass: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 7890,
            socks_port: Some(7891),
            bypass: "localhost,127.0.0.1,::1,<local>".into(),
        }
    }
}

/// 代理状态
#[derive(Debug, Clone, PartialEq)]
pub enum ProxyState {
    Enabled,
    Disabled,
    Error(String),
}

/// 设置系统代理（自动选择当前平台）
pub fn set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
    #[cfg(target_os = "windows")]
    let st = windows_set_proxy(config);
    #[cfg(target_os = "macos")]
    let st = macos_set_proxy(config);
    #[cfg(target_os = "linux")]
    let st = linux_set_proxy(config);
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let st: Result<ProxyState, String> = Err(format!(
        "系统代理设置不支持当前平台: {}",
        std::env::consts::OS
    ));

    // 归属记账必须与「真的改成功了」同步：只有 Enabled 才记。
    // 反过来记账（失败的设置也记）会让 stop 去关别人的代理 —— 那正是本模块
    // 要消灭的故障。
    if matches!(st, Ok(ProxyState::Enabled)) {
        remember_owned(&format!("{}:{}", config.host, config.port));
    }
    st
}

/// 关闭系统代理
pub fn unset_proxy() -> Result<ProxyState, String> {
    #[cfg(target_os = "windows")]
    let st = windows_unset_proxy();
    #[cfg(target_os = "macos")]
    let st = macos_unset_proxy();
    #[cfg(target_os = "linux")]
    let st = linux_unset_proxy();
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let st: Result<ProxyState, String> = Err(format!(
        "系统代理设置不支持当前平台: {}",
        std::env::consts::OS
    ));

    // 关失败时**保留**记账：代理很可能还开着且仍是我们开的，
    // 下次 stop 还得去收拾它。
    if st.is_ok() {
        forget_owned();
    }
    st
}

/// 获取当前代理状态
pub fn get_proxy_status() -> ProxyState {
    #[cfg(target_os = "windows")]
    {
        windows_get_proxy_status()
    }

    #[cfg(target_os = "macos")]
    {
        macos_get_proxy_status()
    }

    #[cfg(target_os = "linux")]
    {
        linux_get_proxy_status()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        ProxyState::Error("不支持的平台".into())
    }
}

// ═══════════════════════════════════════════════════════════
// 归属记账：「现在这个系统代理是 ghboost 开的吗」
// ═══════════════════════════════════════════════════════════
//
// 为什么需要：以前「停内核」无条件 unset_proxy()，会把**别的代理软件**的系统
// 代理一起关掉。本机实测：Clash Verge 监听 7897，ghboost 一停它的代理就失效了，
// 而且全程没有任何提示 —— 用户视角就是「用了 ghboost 之后我的代理软件坏了」。
//
// 记账写文件而不是只放内存：崩在「代理开着」的状态上时，下次启动仍认得这是
// 自己开的，会去收拾它。只放内存的话重启后归属丢失 = 留下一个指向死端口的
// 系统代理（整机断网，而且 ghboost 自己都说不出它开过）。
//
// 关之前还要核对当前 `ProxyServer` 仍指向我们开的那一次：中途用户换了端口、
// 或干脆换了别的代理软件，那就不再是我们的东西，不能动。

fn owned_path() -> std::path::PathBuf {
    crate::web::ghboost_dir().join("proxy.owned")
}

fn remember_owned(endpoint: &str) {
    let _ = std::fs::write(owned_path(), endpoint);
}

fn forget_owned() {
    let _ = std::fs::remove_file(owned_path());
}

/// 记账里的 endpoint；`None` = 没有记账 = 按「不是我们开的」处理。
pub fn owned_endpoint() -> Option<String> {
    let s = std::fs::read_to_string(owned_path()).ok()?;
    let t = s.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// 当前系统代理指向哪里；读不出来（或形状不认识）返回 `None` = 核对不了。
pub fn current_endpoint() -> Option<String> {
    #[cfg(target_os = "windows")]
    let raw = platform::current_endpoint();
    #[cfg(target_os = "macos")]
    let raw = platform::current_endpoint();
    #[cfg(target_os = "linux")]
    let raw = platform::current_endpoint();
    // Android / iOS 等目标没有本模块的平台实现，核对不了就是核对不了。
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let raw: Option<String> = None;
    raw.as_deref().and_then(parse_proxy_server)
}

/// 只在「系统代理确实是 ghboost 自己开的那一次」时才关掉它。
///
/// - `Ok(Some(state))` —— 关掉了
/// - `Ok(None)` —— **没动**：不是我们开的，或中途被别人换掉了
///
/// 「停内核」这类收自己摊子的场景用它；用户显式点「关闭系统代理」仍然走
/// `unset_proxy()`（那个必须无条件执行 —— 用户明确要求了）。
pub fn unset_owned_proxy() -> Result<Option<ProxyState>, String> {
    let Some(mine) = owned_endpoint() else {
        return Ok(None);
    };
    // 核对不了（macOS / Linux / 读失败）时按记账走；核对得了就必须一致。
    if let Some(cur) = current_endpoint() {
        if !cur.eq_ignore_ascii_case(&mine) {
            forget_owned();
            return Ok(None);
        }
    }
    Ok(Some(unset_proxy()?))
}

/// `ProxyServer` 原始值 → `host:port`。
///
/// Windows 上我们自己写进去的形状是 `http=H:P;https=H:P`（见 windows_impl），
/// 但别的软件可能写裸 `H:P`，也可能写 `socks=...` —— 认不出来就返回 `None`：
/// 宁可不下结论，也不要把别人的代理误判成自己的。
fn parse_proxy_server(raw: &str) -> Option<String> {
    let v = raw.trim();
    if v.is_empty() {
        return None;
    }
    if v.contains('=') {
        let http = v.split(';').find_map(|seg| {
            let seg = seg.trim();
            seg.strip_prefix("http=")
                .or_else(|| seg.strip_prefix("HTTP="))
        })?;
        return normalize_endpoint(http);
    }
    normalize_endpoint(v)
}

fn normalize_endpoint(s: &str) -> Option<String> {
    let t = s.trim().trim_matches('"').to_ascii_lowercase();
    let (host, port) = t.rsplit_once(':')?;
    if host.is_empty() || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{host}:{port}"))
}

// ═══════════════════════════════════════════════════════════
// Windows 实现
// ═══════════════════════════════════════════════════════════

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use std::process::Command;

    // 注意：reg.exe 的 key 必须是**一个**参数 `HKCU\Software\...` —— 拆成
    // "HKCU" + "Software\..." 两个位置参数会直接报「无效语法」并打印 usage、
    // 静默不写注册表（W10 实跑抓的：proxy_ok:false，错误正文里是 "REG ADD /?"）。
    const INTERNET_SETTINGS_KEY: &str =
        r#"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings"#;

    pub fn set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
        let proxy_server = format!(
            "http={}:{};https={}:{}",
            config.host, config.port, config.host, config.port
        );

        // 设置代理服务器
        let output = Command::new("reg")
            .args([
                "add",
                INTERNET_SETTINGS_KEY,
                "/v",
                "ProxyEnable",
                "/t",
                "REG_DWORD",
                "/d",
                "1",
                "/f",
            ])
            .output()
            .map_err(|e| format!("reg add ProxyEnable 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        let output = Command::new("reg")
            .args([
                "add",
                INTERNET_SETTINGS_KEY,
                "/v",
                "ProxyServer",
                "/t",
                "REG_SZ",
                "/d",
                &proxy_server,
                "/f",
            ])
            .output()
            .map_err(|e| format!("reg add ProxyServer 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        // 设置绕过列表
        if !config.bypass.is_empty() {
            let output = Command::new("reg")
                .args([
                    "add",
                    INTERNET_SETTINGS_KEY,
                    "/v",
                    "ProxyOverride",
                    "/t",
                    "REG_SZ",
                    "/d",
                    &config.bypass,
                    "/f",
                ])
                .output()
                .map_err(|e| format!("reg add ProxyOverride 失败: {e}"))?;

            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).to_string());
            }
        }

        // 通知系统代理已更改
        let _ = Command::new("powershell")
            .args([
                "-Command",
                "[System.Net.WebRequest]::DefaultWebProxy = [System.Net.WebRequest]::GetSystemWebProxy()",
            ])
            .output();

        Ok(ProxyState::Enabled)
    }

    pub fn unset_proxy() -> Result<ProxyState, String> {
        let output = Command::new("reg")
            .args([
                "add",
                INTERNET_SETTINGS_KEY,
                "/v",
                "ProxyEnable",
                "/t",
                "REG_DWORD",
                "/d",
                "0",
                "/f",
            ])
            .output()
            .map_err(|e| format!("reg add ProxyEnable 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        Ok(ProxyState::Disabled)
    }

    pub fn get_proxy_status() -> ProxyState {
        let output = Command::new("reg")
            .args(["query", INTERNET_SETTINGS_KEY, "/v", "ProxyEnable"])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.contains("0x1") {
                    ProxyState::Enabled
                } else {
                    ProxyState::Disabled
                }
            }
            Err(e) => ProxyState::Error(e.to_string()),
        }
    }

    /// `ProxyServer` 的原始值。输出形如
    /// `    ProxyServer    REG_SZ    http=127.0.0.1:7897;https=127.0.0.1:7897`
    pub fn current_endpoint() -> Option<String> {
        let out = Command::new("reg")
            .args(["query", INTERNET_SETTINGS_KEY, "/v", "ProxyServer"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .lines()
            .find(|l| l.contains("ProxyServer"))
            .and_then(|l| l.split_whitespace().nth(2))
            .map(|v| v.to_string())
    }
}

#[cfg(target_os = "windows")]
use windows_impl as platform;

// ═══════════════════════════════════════════════════════════
// macOS 实现
// ═══════════════════════════════════════════════════════════

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::*;
    // 必须显式导入：windows_impl / linux_impl 各有自己的 `use std::process::Command`，
    // 而 `use super::*` 不会带进来（proxy.rs 顶层没有导入 Command）。
    // 缺这一行会让 macOS 目标在 cargo build 时报 9 个 E0433 "cannot find type Command"，
    // 但因为整个模块是 #[cfg(target_os="macos")]，Linux/Windows 的 CI 永远发现不了。
    use std::process::Command;

    fn default_network_service() -> Result<String, String> {
        let output = Command::new("networksetup")
            .arg("-listallnetworkservices")
            .output()
            .map_err(|e| format!("networksetup 失败: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        // 第二行是默认网络服务（跳过标题行和分隔线）
        stdout
            .lines()
            .nth(2)
            .map(|s| s.trim().to_string())
            .ok_or_else(|| "无法获取默认网络服务".into())
    }

    pub fn set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
        let service = default_network_service()?;

        // 设置 HTTP 代理
        let output = Command::new("networksetup")
            .args([
                "-setwebproxy",
                &service,
                &config.host,
                &config.port.to_string(),
            ])
            .output()
            .map_err(|e| format!("networksetup -setwebproxy 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        // 设置 HTTPS 代理
        let output = Command::new("networksetup")
            .args([
                "-setsecurewebproxy",
                &service,
                &config.host,
                &config.port.to_string(),
            ])
            .output()
            .map_err(|e| format!("networksetup -setsecurewebproxy 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        // 设置 SOCKS 代理
        if let Some(socks_port) = config.socks_port {
            let output = Command::new("networksetup")
                .args([
                    "-setsocksfirewallproxy",
                    &service,
                    &config.host,
                    &socks_port.to_string(),
                ])
                .output()
                .map_err(|e| format!("networksetup -setsocksfirewallproxy 失败: {e}"))?;

            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).to_string());
            }
        }

        // 设置绕过列表
        let bypass_args: Vec<&str> = config.bypass.split(',').map(|s| s.trim()).collect();
        let mut cmd = Command::new("networksetup");
        cmd.args(["-setproxybypassdomains", &service]);
        cmd.args(&bypass_args);

        let output = cmd
            .output()
            .map_err(|e| format!("networksetup -setproxybypassdomains 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        Ok(ProxyState::Enabled)
    }

    pub fn unset_proxy() -> Result<ProxyState, String> {
        let service = default_network_service()?;

        // 关闭 HTTP 代理
        let _ = Command::new("networksetup")
            .args(["-setwebproxystate", &service, "off"])
            .output();

        // 关闭 HTTPS 代理
        let _ = Command::new("networksetup")
            .args(["-setsecurewebproxystate", &service, "off"])
            .output();

        // 关闭 SOCKS 代理
        let _ = Command::new("networksetup")
            .args(["-setsocksfirewallproxystate", &service, "off"])
            .output();

        Ok(ProxyState::Disabled)
    }

    pub fn get_proxy_status() -> ProxyState {
        let service = match default_network_service() {
            Ok(s) => s,
            Err(e) => return ProxyState::Error(e),
        };

        let output = Command::new("networksetup")
            .args(["-getwebproxystate", &service])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.contains("Enabled") {
                    ProxyState::Enabled
                } else {
                    ProxyState::Disabled
                }
            }
            Err(e) => ProxyState::Error(e.to_string()),
        }
    }

    /// 核对不了就返回 `None`：macOS 要按网络服务逐个问 `networksetup`，
    /// 代价不值当。此时归属判断退回「只认记账」。
    pub fn current_endpoint() -> Option<String> {
        None
    }
}

#[cfg(target_os = "macos")]
use macos_impl as platform;

// ═══════════════════════════════════════════════════════════
// Linux 实现 (GNOME gsettings)
// ═══════════════════════════════════════════════════════════

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;
    use std::process::Command;

    pub fn set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
        // 设置模式为手动
        let output = Command::new("gsettings")
            .args(["set", "org.gnome.system.proxy", "mode", "'manual'"])
            .output()
            .map_err(|e| format!("gsettings set mode 失败: {e}"))?;

        if !output.status.success() {
            // 尝试 KDE 方案
            return kde_set_proxy(config);
        }

        // 设置 HTTP 代理
        let output = Command::new("gsettings")
            .args([
                "set",
                "org.gnome.system.proxy.http",
                "host",
                &format!("'{}'", config.host),
            ])
            .output()
            .map_err(|e| format!("gsettings set http host 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        let output = Command::new("gsettings")
            .args([
                "set",
                "org.gnome.system.proxy.http",
                "port",
                &config.port.to_string(),
            ])
            .output()
            .map_err(|e| format!("gsettings set http port 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        // 设置 HTTPS 代理
        let output = Command::new("gsettings")
            .args([
                "set",
                "org.gnome.system.proxy.https",
                "host",
                &format!("'{}'", config.host),
            ])
            .output()
            .map_err(|e| format!("gsettings set https host 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        let output = Command::new("gsettings")
            .args([
                "set",
                "org.gnome.system.proxy.https",
                "port",
                &config.port.to_string(),
            ])
            .output()
            .map_err(|e| format!("gsettings set https port 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        // 设置 SOCKS 代理
        if let Some(socks_port) = config.socks_port {
            let _ = Command::new("gsettings")
                .args([
                    "set",
                    "org.gnome.system.proxy.socks",
                    "host",
                    &format!("'{}'", config.host),
                ])
                .output();

            let _ = Command::new("gsettings")
                .args([
                    "set",
                    "org.gnome.system.proxy.socks",
                    "port",
                    &socks_port.to_string(),
                ])
                .output();
        }

        // 设置绕过列表
        let bypass_list: Vec<String> = config
            .bypass
            .split(',')
            .map(|s| format!("'{}'", s.trim()))
            .collect();
        let bypass_str = format!("[{}]", bypass_list.join(","));

        let output = Command::new("gsettings")
            .args(["set", "org.gnome.system.proxy", "ignore-hosts", &bypass_str])
            .output()
            .map_err(|e| format!("gsettings set ignore-hosts 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        Ok(ProxyState::Enabled)
    }

    fn kde_set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
        // KDE 使用 kwriteconfig5
        let http_proxy = format!("{}:{}", config.host, config.port);

        let output = Command::new("kwriteconfig5")
            .args([
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                "httpProxy",
                &http_proxy,
            ])
            .output()
            .map_err(|e| format!("kwriteconfig5 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        let output = Command::new("kwriteconfig5")
            .args([
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                "httpsProxy",
                &http_proxy,
            ])
            .output()
            .map_err(|e| format!("kwriteconfig5 https 失败: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }

        // 设置 SOCKS
        if let Some(socks_port) = config.socks_port {
            let socks_proxy = format!("{}:{}", config.host, socks_port);
            let _ = Command::new("kwriteconfig5")
                .args([
                    "--file",
                    "kioslaverc",
                    "--group",
                    "Proxy Settings",
                    "--key",
                    "socksProxy",
                    &socks_proxy,
                ])
                .output();
        }

        // 启用代理
        let _ = Command::new("kwriteconfig5")
            .args([
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                "ProxyType",
                "1",
            ])
            .output();

        Ok(ProxyState::Enabled)
    }

    pub fn unset_proxy() -> Result<ProxyState, String> {
        // GNOME
        let output = Command::new("gsettings")
            .args(["set", "org.gnome.system.proxy", "mode", "'none'"])
            .output();

        if output.is_ok() {
            return Ok(ProxyState::Disabled);
        }

        // KDE
        let _ = Command::new("kwriteconfig5")
            .args([
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                "ProxyType",
                "0",
            ])
            .output();

        Ok(ProxyState::Disabled)
    }

    pub fn get_proxy_status() -> ProxyState {
        let output = Command::new("gsettings")
            .args(["get", "org.gnome.system.proxy", "mode"])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.contains("manual") {
                    ProxyState::Enabled
                } else {
                    ProxyState::Disabled
                }
            }
            Err(e) => ProxyState::Error(e.to_string()),
        }
    }

    /// 核对不了就返回 `None`（见 macos_impl 的同款说明）。
    pub fn current_endpoint() -> Option<String> {
        None
    }
}

#[cfg(target_os = "linux")]
use linux_impl as platform;

// ═══════════════════════════════════════════════════════════
// 统一接口（委托给平台实现）
// ═══════════════════════════════════════════════════════════

#[cfg(target_os = "windows")]
fn windows_set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
    platform::set_proxy(config)
}

#[cfg(target_os = "windows")]
fn windows_unset_proxy() -> Result<ProxyState, String> {
    platform::unset_proxy()
}

#[cfg(target_os = "windows")]
fn windows_get_proxy_status() -> ProxyState {
    platform::get_proxy_status()
}

#[cfg(target_os = "macos")]
fn macos_set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
    platform::set_proxy(config)
}

#[cfg(target_os = "macos")]
fn macos_unset_proxy() -> Result<ProxyState, String> {
    platform::unset_proxy()
}

#[cfg(target_os = "macos")]
fn macos_get_proxy_status() -> ProxyState {
    platform::get_proxy_status()
}

#[cfg(target_os = "linux")]
fn linux_set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
    platform::set_proxy(config)
}

#[cfg(target_os = "linux")]
fn linux_unset_proxy() -> Result<ProxyState, String> {
    platform::unset_proxy()
}

#[cfg(target_os = "linux")]
fn linux_get_proxy_status() -> ProxyState {
    platform::get_proxy_status()
}

// ═══════════════════════════════════════════════════════════
// Android/iOS 特殊处理（需要 App 层配合）
// ═══════════════════════════════════════════════════════════

// Android 系统代理设置说明：
// Android 没有全局系统代理 API，需要通过以下方式之一：
// 1. VPN Service: 创建本地 VPN 拦截所有流量（推荐，无需 root）
// 2. Root + iptables: 使用 iptables 重定向流量（需要 root）
// 3. WifiManager API: 仅对当前 WiFi 设置代理（需要 Android API）
// 对于 WebView-based 应用，可以使用 WebView.setProxy() 或在 App 层设置代理。
//
// iOS 系统代理设置说明：
// iOS 没有公开的系统代理 API，需要通过以下方式之一：
// 1. NEPacketTunnelProvider: 使用 NetworkExtension 框架（推荐）
// 2. Configuration Profile: 安装描述文件设置代理
// 对于 WebView-based 应用，可以使用 WKWebViewConfiguration 的代理设置。
//
// Android/iOS 的 set_proxy/unset_proxy/get_proxy_status 已在上方通用函数中
// 通过 #[cfg(not(any(...)))] 分支处理，无需重复定义。

// ═══════════════════════════════════════════════════════════
// 环境变量辅助（跨平台通用）
// ═══════════════════════════════════════════════════════════

/// 设置进程级代理环境变量（影响子进程）
pub fn set_env_proxy(config: &ProxyConfig) {
    let http_proxy = format!("http://{}:{}", config.host, config.port);
    let https_proxy = format!("http://{}:{}", config.host, config.port);
    let all_proxy = if let Some(socks_port) = config.socks_port {
        format!("socks5://{}:{}", config.host, socks_port)
    } else {
        http_proxy.clone()
    };

    std::env::set_var("HTTP_PROXY", &http_proxy);
    std::env::set_var("HTTPS_PROXY", &https_proxy);
    std::env::set_var("ALL_PROXY", &all_proxy);
    std::env::set_var("http_proxy", &http_proxy);
    std::env::set_var("https_proxy", &https_proxy);
    std::env::set_var("all_proxy", &all_proxy);
    std::env::set_var("NO_PROXY", &config.bypass);
    std::env::set_var("no_proxy", &config.bypass);
}

/// 清除进程级代理环境变量
pub fn unset_env_proxy() {
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        std::env::remove_var(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ProxyServer` 的形状各种各样，认错一个 = 把别人的代理当成自己的关掉。
    /// 这里只钉死「能认的」和「必须认不出的」，认不出时上层会保守地不动手。
    #[test]
    fn parse_proxy_server_认得我们自己写进去的形状() {
        // 我们自己 set_proxy 写进去的就是这个形状（见 windows_impl）
        assert_eq!(
            parse_proxy_server("http=127.0.0.1:7897;https=127.0.0.1:7897").as_deref(),
            Some("127.0.0.1:7897")
        );
        // 别的软件常写的裸值
        assert_eq!(
            parse_proxy_server("127.0.0.1:7890").as_deref(),
            Some("127.0.0.1:7890")
        );
        // 大小写 / 空白 / 引号
        assert_eq!(
            parse_proxy_server("  HTTP=LocalHost:1080 ; https=x ").as_deref(),
            Some("localhost:1080")
        );
    }

    #[test]
    fn parse_proxy_server_认不出的必须返回_none() {
        for raw in [
            "",
            "   ",
            "socks=127.0.0.1:1080",       // 只有 socks：核对不了
            "proxy.company.local",          // 没有端口
            "127.0.0.1:",                  // 端口为空
            "127.0.0.1:abc",               // 端口不是数字
            "http=;https=127.0.0.1:7897",  // http 段是空的
        ] {
            assert_eq!(
                parse_proxy_server(raw),
                None,
                "不该被认出来: {raw:?}"
            );
        }
    }

    /// 归属比较必须忽略大小写（`LOCALHOST` vs `localhost` 是同一个东西）。
    #[test]
    fn endpoint_比较忽略大小写() {
        assert!("LOCALHOST:7890".eq_ignore_ascii_case("localhost:7890"));
        assert!(!"localhost:7890".eq_ignore_ascii_case("localhost:7897"));
    }
}
