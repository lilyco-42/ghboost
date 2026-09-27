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
use std::time::{Duration, Instant};

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
    /// 这个 manager 当前用的是哪个 mixed 端口。
    ///
    /// 换端口重新订阅时必须**换掉 manager**：`api_port` 是创建时定死的，沿用旧的
    /// manager 会把 reload 请求发到旧端口，而新配置里声明的是新端口 ——
    /// 故障形态是"改了端口再订阅，节点数变成 0"。
    pub fn mixed_port(&self) -> u16 {
        self.config.mixed_port
    }

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

    /// 配置文件路径（外部要按 mihomo 原生格式覆写配置时用它）
    ///
    /// 典型用法：`start()` → 写入订阅配置 → `reload_config()`。
    /// 顺序不能反：`start()` 内部会先调 `generate_config()` 覆盖一份默认配置。
    pub fn config_path(&self) -> &std::path::Path {
        &self.config_path
    }

    /// 内核日志路径（stdout/stderr 都落这儿，与配置同目录）。
    ///
    /// 和 `coreman::kernel.log` 同一个约定：诊断只看得到内核原话才算闭环。
    pub fn kernel_log_path(&self) -> PathBuf {
        self.config_path.with_file_name("kernel.log")
    }

    /// kernel.log 尾部若干行（内核致命错误的原话）。
    ///
    /// 单行截断到 200 字符：mihomo 会把整份配置的错误堆在一行里（实测 provider
    /// 报错带 400+ 字符），不截的话「尾部 20 行」可能就是一堵墙。
    pub fn log_tail(&self, lines: usize) -> String {
        match std::fs::read_to_string(self.kernel_log_path()) {
            Ok(s) => {
                let all: Vec<String> = s
                    .lines()
                    .map(|l| {
                        let l = l.trim_end();
                        if l.chars().count() > 200 {
                            l.chars().take(200).collect::<String>() + "…（已截断）"
                        } else {
                            l.to_string()
                        }
                    })
                    .collect();
                let skip = all.len().saturating_sub(lines);
                all[skip..].join("\n")
            }
            Err(e) => format!("（读内核日志失败: {e}）"),
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
  # 刻意**不开** `listen: 0.0.0.0:53`：53 是特权端口，非管理员进程绑定即失败，
  # 内核会直接 fatal 退出。普通代理模式用不到它（只有 TUN 模式才需要），
  # 真要开也应该在 TUN 模式里单独开成高位端口。
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

# 必须**先定义**规则里引用的 group，否则 mihomo 直接 fatal：
#   "rules[1] [MATCH,🚀 节点选择] error: proxy [🚀 节点选择] not found"
# 即默认配置（还没导入任何节点时）也必须是自洽的，否则内核一次都起不来。
proxy-groups:
  - name: "🚀 节点选择"
    type: select
    proxies:
      - DIRECT

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
        // 内核的 stdout/stderr 落 kernel.log（与配置同目录，和 coreman.rs 同款做法）。
        // 原来是 `Stdio::piped()` 接管之后**从不读** —— 两条后果：
        //   1. 诊断全丢：`initial proxy provider subscription error: ... unknown method`
        //      这类致命信息一条都拿不到（v0.3.15 的 P1「20 好 + 1 坏 = 0 节点」只能
        //      手工搭一份 mihomo 才复现出原文），用户只看到"已连接但每个请求都失败"；
        //   2. **输出超 64KB 会永久阻塞**：内核往管道写、没人读 → 管道写满 →
        //      内核卡死在 write 上，整个代理静默死掉。
        // 直接重定向到文件，两个问题一起消失，也不用为它开读取线程。
        let log_out = std::fs::File::create(self.kernel_log_path())
            .map_err(|e| format!("建内核日志失败: {e}"))?;
        let log_err = log_out
            .try_clone()
            .map_err(|e| format!("克隆内核日志句柄失败: {e}"))?;
        let mut child = Command::new(&binary)
            .args(["-d", self.config_path.parent().unwrap().to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_out))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("启动 Mihomo 失败: {e}"))?;

        // 等待一下确认启动成功
        std::thread::sleep(Duration::from_millis(500));
        match child.try_wait() {
            Ok(Some(status)) => Err(format!(
                "Mihomo 启动后立即退出，状态码: {status}。内核日志尾部：\n{}",
                self.log_tail(20)
            )),
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

    /// 构造 `PUT /configs` 的请求体。
    ///
    /// 必须是 JSON `{"path": "", "payload": "<yaml>"}`，**不能**直接把 YAML 当请求体：
    /// 内核（v1.19.30 实测）对裸 YAML 一律回 `HTTP 400 {"message":"Body invalid"}`。
    /// 单独拆出来是为了能写单测钉死这个形状 —— 这段一旦被"简化"回裸 YAML，
    /// 订阅会退化成静默 0 节点，非常难查。
    fn reload_body(config_content: &str) -> String {
        serde_json::json!({ "path": "", "payload": config_content }).to_string()
    }

    /// 等内核的 RESTful API 真正开始监听。
    ///
    /// `start()` 只固定睡 500ms，而内核冷启动到 "RESTful API listening" 实测要
    /// 1.6~2.5s（慢机器更久）。不等就发 PUT 会撞 connection refused，
    /// 故障形态是"点订阅报网络错"，而且只在慢机器上复现 —— 最难查的那一种。
    fn wait_api_ready(&self, budget: Duration) -> Result<(), String> {
        let url = format!("http://127.0.0.1:{}/version", self.config.api_port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(800))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

        let deadline = Instant::now() + budget;
        let mut last = String::from("从未成功连上");
        while Instant::now() < deadline {
            match client.get(&url).send() {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => last = format!("HTTP {}", r.status()),
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(format!(
            "内核的 API 一直没起来（最后一次：{last}）。\n\
             可能是端口 {} 被别的内核占了，或内核启动失败。",
            self.config.api_port
        ))
    }

    /// 重载配置文件
    pub fn reload_config(&self) -> Result<(), String> {
        // 顺序上必须先等 API 就绪：见 wait_api_ready 的说明。
        self.wait_api_ready(Duration::from_secs(10))?;

        // 两个坑，都是 v1.19.30 上实测出来的：
        // 1. force=true 是必需的：不带这个参数时 mihomo 只接受**部分字段**热更新，
        //    新增的 proxy-provider / rules 会被静默忽略，表现为"订阅导进去了但没节点"。
        // 2. 请求体必须是 JSON {"path","payload} 而不是裸 YAML（见 reload_body）。
        //    更致命的是：reqwest 的 send() 只有**网络层**失败才返回 Err，
        //    4xx/5xx 是 Ok —— 所以必须自己检查状态码，否则内核拒绝配置这件事
        //    会被完全吞掉，外面只看到"没有节点"，谁都想不到是这里。
        let url = format!(
            "http://127.0.0.1:{}/configs?force=true",
            self.config.api_port
        );

        let config_content = std::fs::read_to_string(&self.config_path)
            .map_err(|e| format!("读取配置文件失败: {e}"))?;

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

        let resp = client
            .put(&url)
            .header("Content-Type", "application/json")
            .body(Self::reload_body(&config_content))
            .send()
            .map_err(|e| format!("重载配置失败: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            // 内核会在 body 里说清是哪一行不认识，这是排障的唯一线索，一定要带出来。
            let detail = resp.text().unwrap_or_default();
            let brief: String = detail.chars().take(300).collect();
            return Err(format!("内核拒绝了新的配置（HTTP {status}）。\n{brief}"));
        }

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

    /// 回归护栏：请求体必须是 JSON {"path","payload"}，不是裸 YAML。
    /// 2026-09-10 踩过：发裸 YAML 时内核回 400，而 send() 不把 4xx 当错误，
    /// 结果订阅永远解析出 0 个节点，且没有任何报错。
    #[test]
    fn reload_请求体必须是_json_包裹而不是裸_yaml() {
        let yaml = "mixed-port: 7890\nproxies: []\n";
        let body = MihomoManager::reload_body(yaml);

        // 它得是能被内核解析的 JSON，而不是一段以 mixed-port 开头的 YAML 文本
        assert!(
            !body.starts_with("mixed-port"),
            "请求体不能是裸 YAML: {body}"
        );

        let v: serde_json::Value =
            serde_json::from_str(&body).expect("reload 请求体必须是合法 JSON");
        assert_eq!(v["path"], "");
        assert_eq!(v["payload"], yaml);
    }

    /// YAML 里带引号、反斜杠、换行时，JSON 转义必须把它们完整保住。
    #[test]
    fn reload_请求体里的_yaml_特殊字符不能被转义破坏() {
        let yaml = "path: 'C:/Users/me/nodes.txt'\nname: \"a\\\\b\"\n";
        let body = MihomoManager::reload_body(yaml);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["payload"], yaml);
    }

    /// kernel.log 的尾部抽取 + 单行截断。
    ///
    /// 这条尾巴是 P1「20 好 + 1 坏 = 0 节点」唯一的诊断线索
    /// （`initial proxy provider subscription error: proxy 19 error: ss ... unknown method`），
    /// 而内核会把整段配置的错误堆进一行（实测 400+ 字符）—— 不截断的话
    /// 「最近 20 行」就是一堵墙，真正的原因反而被埋在后面。
    #[test]
    fn kernel_log_尾部按行数截断且单行超长要截断() {
        let dir = std::env::temp_dir().join(format!("ghb-klog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = MihomoManager::new(MihomoConfig {
            config_dir: Some(dir.clone()),
            ..Default::default()
        });

        // 日志还不存在时不能 panic 也不能空字串糊弄，得说清读失败
        let missing = mgr.log_tail(5);
        assert!(
            missing.starts_with("（读内核日志失败"),
            "日志缺失时要说明原因，实际: {missing}"
        );

        let long = "x".repeat(500);
        let body = format!("第一行\n第二行\n{long}\n最后一行\n");
        std::fs::write(mgr.kernel_log_path(), body).unwrap();

        let tail = mgr.log_tail(2);
        let got: Vec<&str> = tail.lines().collect();
        assert_eq!(got.len(), 2, "只要尾部 2 行，实际: {tail}");
        assert!(got[1].ends_with("…（已截断）"), "超长行要截断: {}", got[1]);
        assert!(got[1].chars().count() <= 210, "截断后仍要短");
        // 短行原样保留（只去行尾空白，不许动内容）
        let all = mgr.log_tail(10);
        assert!(
            all.lines().any(|l| l == "最后一行"),
            "尾部窗口够大时要能看到最后一行: {all}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// kernel.log 必须与 config.yaml 同目录（coreman 的同名约定）。
    ///
    /// 钉死这条是因为「日志和配置分家」是这类实现最常见的退化方式：
    /// 用户按文档去 config.yaml 旁边找日志找不到，就等于没有。
    #[test]
    fn kernel_log_与配置同目录() {
        let dir = std::env::temp_dir().join(format!("ghb-klog2-{}", std::process::id()));
        let mgr = MihomoManager::new(MihomoConfig {
            config_dir: Some(dir.clone()),
            ..Default::default()
        });
        assert_eq!(
            mgr.kernel_log_path(),
            dir.join("kernel.log"),
            "kernel.log 必须在配置同目录"
        );
        assert_eq!(
            mgr.kernel_log_path().parent().unwrap(),
            mgr.config_path().parent().unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
