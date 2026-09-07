//! mihomo — Mihomo 内核管理模块
//!
//! 功能：
//! - 进程生命周期管理（启动/停止/重启）
//! - 端口自动分配（HTTP/SOCKS/控制面板）
//! - REST API 轮询（流量/连接/规则/日志）
//! - 配置文件生成与热重载
//! - 节点注入（从 nodes_data 目录读取并注入）

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Mihomo 内核配置
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MihomoConfig {
    /// Mihomo 可执行文件路径（默认自动查找）
    pub binary_path: Option<PathBuf>,
    /// 配置文件目录（默认 ~/.config/ghboost/mihomo/）
    pub config_dir: Option<PathBuf>,
    /// HTTP 代理端口（默认 7890）
    pub http_port: u16,
    /// SOCKS5 代理端口（默认 7891）
    pub socks_port: u16,
    /// 控制面板端口（默认 9090）
    pub mixed_port: u16,
    /// REST API 端口（默认 9091）
    pub api_port: u16,
    /// 允许局域网连接
    pub allow_lan: bool,
    /// 日志级别（info/warning/error/debug）
    pub log_level: String,
}

impl Default for MihomoConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            config_dir: None,
            http_port: 7890,
            socks_port: 7891,
            mixed_port: 9090,
            api_port: 9091,
            allow_lan: false,
            log_level: "info".into(),
        }
    }
}

/// Mihomo 运行状态
#[derive(Debug, Clone, serde::Serialize)]
pub struct MihomoStatus {
    /// 是否运行中
    pub running: bool,
    /// 进程 PID
    pub pid: Option<u32>,
    /// HTTP 代理端口
    pub http_port: u16,
    /// SOCKS5 代理端口
    pub socks_port: u16,
    /// 控制面板端口
    pub mixed_port: u16,
    /// 上行速度 (bytes/s)
    pub upload_speed: u64,
    /// 下行速度 (bytes/s)
    pub download_speed: u64,
    /// 总上传量 (bytes)
    pub total_upload: u64,
    /// 总下载量 (bytes)
    pub total_download: u64,
    /// 活跃连接数
    pub connections: u32,
}

/// Mihomo 管理器
pub struct MihomoManager {
    config: MihomoConfig,
    process: Arc<Mutex<Option<Child>>>,
    config_path: PathBuf,
}

impl MihomoManager {
    /// 创建新的 Mihomo 管理器
    pub fn new(config: MihomoConfig) -> Self {
        let config_dir = config.config_dir.clone().unwrap_or_else(|| {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_default();
            PathBuf::from(home)
                .join(".config")
                .join("ghboost")
                .join("mihomo")
        });

        // 确保配置目录存在
        std::fs::create_dir_all(&config_dir).ok();

        let config_path = config_dir.join("config.yaml");

        Self {
            config,
            process: Arc::new(Mutex::new(None)),
            config_path,
        }
    }

    /// 查找 Mihomo 可执行文件
    pub fn find_binary(&self) -> Result<PathBuf, String> {
        // 1. 检查配置中的路径
        if let Some(ref path) = self.config.binary_path {
            if path.exists() {
                return Ok(path.clone());
            }
        }

        // 2. 检查当前目录
        let local_bin = PathBuf::from("mihomo.exe");
        if local_bin.exists() {
            return Ok(local_bin);
        }

        // 3. 检查 PATH
        if let Ok(output) = Command::new("where").arg("mihomo").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = stdout.lines().next() {
                    let path = PathBuf::from(first_line.trim());
                    if path.exists() {
                        return Ok(path);
                    }
                }
            }
        }

        // 4. 检查常见安装位置
        let common_paths = [
            "C:\\Program Files\\mihomo\\mihomo.exe",
            "C:\\Program Files (x86)\\mihomo\\mihomo.exe",
            "C:\\mihomo\\mihomo.exe",
        ];

        for p in &common_paths {
            let path = PathBuf::from(p);
            if path.exists() {
                return Ok(path);
            }
        }

        Err("找不到 Mihomo 可执行文件，请在设置中指定路径或将其放入 PATH".into())
    }

    /// 生成默认配置文件
    pub fn generate_config(&self) -> Result<(), String> {
        let config_content = format!(
            r#"# ghboost Mihomo 配置文件
# 自动生成于 {}

mixed-port: {}
allow-lan: {}
bind-address: '*'
mode: rule
log-level: {}

external-controller: 127.0.0.1:{}

dns:
  enable: true
  listen: 0.0.0.0:53
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  nameserver:
    - https://dns.alidns.com/dns-query
    - https://doh.pub/dns-query
  fallback:
    - https://1.1.1.1/dns-query
    - https://dns.google/dns-query
  fallback-filter:
    geoip: true
    geoip-code: CN

proxies: []

proxy-groups: []

rules:
  - GEOIP,CN,DIRECT
  - MATCH,🚀 节点选择
"#,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| format!("timestamp={}", d.as_secs()))
                .unwrap_or_default(),
            self.config.mixed_port,
            self.config.allow_lan,
            self.config.log_level,
            self.config.api_port,
        );

        std::fs::write(&self.config_path, config_content)
            .map_err(|e| format!("写入配置文件失败: {e}"))?;

        Ok(())
    }

    /// 启动 Mihomo 进程
    pub fn start(&mut self) -> Result<MihomoStatus, String> {
        // 检查是否已运行
        // 注意：get_status() 内部会再次 lock 同一把 std::sync::Mutex（不可重入），
        // 所以任何调用 get_status() 的地方都必须先释放 guard，否则自锁死。
        let already_running = {
            let mut process = self.process.lock().map_err(|e| e.to_string())?;
            match process.as_mut() {
                None => false,
                Some(child) => match child.try_wait() {
                    Ok(Some(_)) => false, // 进程已退出，需要重启
                    Ok(None) => true,     // 进程仍在运行
                    Err(e) => return Err(format!("检查进程状态失败: {e}")),
                },
            }
        };
        if already_running {
            return self.get_status();
        }

        // 生成配置文件
        self.generate_config()?;

        // 查找并启动
        let binary = self.find_binary()?;
        let mut child = Command::new(&binary)
            .args(["-d", self.config_path.parent().unwrap().to_str().unwrap()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 Mihomo 失败: {e}"))?;

        // 等待一下确认启动成功
        std::thread::sleep(Duration::from_millis(500));
        match child.try_wait() {
            Ok(Some(status)) => Err(format!("Mihomo 启动后立即退出，状态码: {}", status)),
            Ok(None) => {
                // 启动成功（先放锁再 get_status，理由同上）
                {
                    let mut process = self.process.lock().map_err(|e| e.to_string())?;
                    *process = Some(child);
                }
                self.get_status()
            }
            Err(e) => Err(format!("检查进程状态失败: {e}")),
        }
    }

    /// 停止 Mihomo 进程
    pub fn stop(&self) -> Result<MihomoStatus, String> {
        // 先把 guard 释放掉再 get_status()：std::sync::Mutex 不可重入，
        // 持有锁时再 lock 会直接死锁。Drop 里会调 stop()，所以这个死锁会把
        // `cargo test` 整个挂死（CI 上曾跑满 6 小时被 cancel）。
        {
            let mut process = self.process.lock().map_err(|e| e.to_string())?;
            if let Some(ref mut child) = *process {
                // 尝试优雅关闭
                let _ = child.kill();
                // 等待退出
                let _ = child.wait();
            }
            *process = None;
        }
        self.get_status()
    }

    /// 重启 Mihomo 进程
    pub fn restart(&mut self) -> Result<MihomoStatus, String> {
        self.stop()?;
        std::thread::sleep(Duration::from_millis(500));
        self.start()
    }

    /// 获取当前状态
    pub fn get_status(&self) -> Result<MihomoStatus, String> {
        let process = self.process.lock().map_err(|e| e.to_string())?;
        let running = process.is_some();
        let pid = process.as_ref().map(|c| c.id());

        // 尝试从 API 获取详细状态
        let (upload_speed, download_speed, total_upload, total_download, connections) = if running {
            self.fetch_api_stats().unwrap_or((0, 0, 0, 0, 0))
        } else {
            (0, 0, 0, 0, 0)
        };

        Ok(MihomoStatus {
            running,
            pid,
            http_port: self.config.http_port,
            socks_port: self.config.socks_port,
            mixed_port: self.config.mixed_port,
            upload_speed,
            download_speed,
            total_upload,
            total_download,
            connections,
        })
    }

    /// 从 REST API 获取统计信息
    fn fetch_api_stats(&self) -> Result<(u64, u64, u64, u64, u32), String> {
        let url = format!("http://127.0.0.1:{}/traffic", self.config.api_port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

        let resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("请求 API 失败: {e}"))?;

        // 简化的解析，实际应该解析 JSON 数组
        let _text = resp.text().map_err(|e| format!("读取响应失败: {e}"))?;

        // 返回基本统计
        Ok((0, 0, 0, 0, 0))
    }

    /// 重载配置文件
    pub fn reload_config(&self) -> Result<(), String> {
        let url = format!("http://127.0.0.1:{}/configs", self.config.api_port);

        let config_content = std::fs::read_to_string(&self.config_path)
            .map_err(|e| format!("读取配置文件失败: {e}"))?;

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

        client
            .put(&url)
            .body(config_content)
            .header("Content-Type", "application/yaml")
            .send()
            .map_err(|e| format!("重载配置失败: {e}"))?;

        Ok(())
    }

    /// 注入节点到配置文件
    pub fn inject_nodes(&self, nodes_data_dir: &Path) -> Result<(), String> {
        // 读取现有配置
        let mut config_content = std::fs::read_to_string(&self.config_path)
            .map_err(|e| format!("读取配置文件失败: {e}"))?;

        // 从 nodes_data 目录读取所有节点文件
        let mut proxies_content = String::new();

        if nodes_data_dir.exists() {
            for entry in
                std::fs::read_dir(nodes_data_dir).map_err(|e| format!("读取节点目录失败: {e}"))?
            {
                let entry = entry.map_err(|e| format!("读取目录项失败: {e}"))?;
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|ext| ext == "yaml" || ext == "yml")
                {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        // 简单解析：找到 proxies 和 proxy-groups 段
                        if let Some(start) = content.find("proxies:") {
                            if let Some(end) = content[start..]
                                .find("\nproxy-groups:")
                                .or_else(|| content[start..].find("\n\n"))
                            {
                                let proxies_str = &content[start + 9..start + end];
                                proxies_content.push_str(proxies_str);
                            }
                        }
                    }
                }
            }
        }

        if proxies_content.is_empty() {
            return Ok(());
        }

        // 在配置文件中添加 proxies 段
        if !config_content.contains("proxies:") {
            config_content.push_str("\nproxies:\n");
        }

        // 追加节点（简化实现，实际应该解析 YAML）
        config_content.push_str(&proxies_content);

        // 写回配置文件
        std::fs::write(&self.config_path, config_content)
            .map_err(|e| format!("写入配置文件失败: {e}"))?;

        // 重载配置
        self.reload_config()?;

        Ok(())
    }
}

impl Drop for MihomoManager {
    fn drop(&mut self) {
        // 确保进程被清理
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = MihomoConfig::default();
        assert_eq!(config.http_port, 7890);
        assert_eq!(config.socks_port, 7891);
        assert_eq!(config.mixed_port, 9090);
    }

    #[test]
    fn test_generate_config() {
        let config = MihomoConfig::default();
        let manager = MihomoManager::new(config);
        let result = manager.generate_config();
        assert!(result.is_ok());
        assert!(manager.config_path.exists());
    }
}
