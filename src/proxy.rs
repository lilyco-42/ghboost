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
    {
        windows_set_proxy(config)
    }

    #[cfg(target_os = "macos")]
    {
        macos_set_proxy(config)
    }

    #[cfg(target_os = "linux")]
    {
        linux_set_proxy(config)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Err(format!(
            "系统代理设置不支持当前平台: {}",
            std::env::consts::OS
        ))
    }
}

/// 关闭系统代理
pub fn unset_proxy() -> Result<ProxyState, String> {
    #[cfg(target_os = "windows")]
    {
        windows_unset_proxy()
    }

    #[cfg(target_os = "macos")]
    {
        macos_unset_proxy()
    }

    #[cfg(target_os = "linux")]
    {
        linux_unset_proxy()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Err(format!(
            "系统代理设置不支持当前平台: {}",
            std::env::consts::OS
        ))
    }
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
// Windows 实现
// ═══════════════════════════════════════════════════════════

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use std::process::Command;

    const INTERNET_SETTINGS_KEY: &str =
        r#"Software\Microsoft\Windows\CurrentVersion\Internet Settings"#;

    pub fn set_proxy(config: &ProxyConfig) -> Result<ProxyState, String> {
        let proxy_server = format!(
            "http={}:{};https={}:{}",
            config.host, config.port, config.host, config.port
        );

        // 设置代理服务器
        let output = Command::new("reg")
            .args([
                "add",
                "HKCU",
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
                "HKCU",
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
                    "HKCU",
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
                "HKCU",
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
            .args(["query", "HKCU", INTERNET_SETTINGS_KEY, "/v", "ProxyEnable"])
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
}

#[cfg(target_os = "windows")]
use windows_impl as platform;

// ═══════════════════════════════════════════════════════════
// macOS 实现
// ═══════════════════════════════════════════════════════════

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::*;

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
}

#[cfg(target_os = "macos")]
use macos_impl as platform;

// ═══════════════════════════════════════════════════════════
// Linux 实现 (GNOME gsettings)
// ═══════════════════════════════════════════════════════════

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;

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

/// Android 系统代理设置说明：
/// Android 没有全局系统代理 API，需要通过以下方式之一：
/// 1. **VPN Service**: 创建本地 VPN 拦截所有流量（推荐，无需 root）
/// 2. **Root + iptables**: 使用 iptables 重定向流量（需要 root）
/// 3. **WifiManager API**: 仅对当前 WiFi 设置代理（需要 Android API）
///
/// 对于 WebView-based 应用，可以使用 `WebView.setProxy()` 或在 App 层设置代理。
///
/// iOS 系统代理设置说明：
/// iOS 没有公开的系统代理 API，需要通过以下方式之一：
/// 1. **NEPacketTunnelProvider**: 使用 NetworkExtension 框架（推荐）
/// 2. **Configuration Profile**: 安装描述文件设置代理
///
/// 对于 WebView-based 应用，可以使用 `WKWebViewConfiguration` 的代理设置。

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
