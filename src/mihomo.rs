//! mihomo 内核后端：启动一个**独立**实例（专用进程 + 专用目录 + 专用端口 +
//! 随机 secret），用 REST API 批量测节点延迟。绝不动用户正在跑的 Clash Verge。
//!
//! 关键经验（已逐一验证）：
//! - verge-mihomo.exe 必须传 **Windows 风格路径**（正斜杠），Unix 路径 `/tmp/...`
//!   会被忽略并回退到默认 home 配置。
//! - 认证头是 `Authorization: Bearer <raw_secret>`（不是 base64）。
//! - file proxy-provider 的 `path` 里的反斜杠在 YAML 里是转义符，必须用正斜杠。
//! - file provider 吃「URI 列表」和「整体 base64」，但**不吃** clash 标准 yaml。
//! - 单节点 `GET /proxies/{name}/delay?url=&timeout=` 成功返回 `{"delay":ms}`，
//!   失败返回 `{"message":"..."}` 且没有 delay 字段。

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rand::Rng;
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::nodes::Node;

/// 探测 mihomo 内核二进制。优先级：
/// 1) 调用方显式指定（`TestParams.mihomo`）
/// 2) 环境变量 `GHBOOST_MIHOMO`
/// 3) **随 ghboost 分发的内置内核**：与当前可执行文件同目录的 `mihomo` /
///    `mihomo.exe`，或按目标三元组命名的 `mihomo-<triple>` /
///    `mihomo-<triple>.exe`（Release 里的命名）
/// 4) 操作系统标准安装路径（macOS：`/opt/homebrew/bin`、`/usr/local/bin`；
///    Linux：`/usr/local/bin`、`/usr/bin`；Windows：Clash Verge 安装路径）
/// 5) PATH 里的 `mihomo`
///
/// 这样 ghboost 在任意平台都能「自带内核」运行 `test`：只要把 Release 里对应
/// 目标的 `mihomo-<triple>` 与 ghboost 放同目录即可，无需用户另装 Clash Verge。
fn mihomo_bin(bin_override: &Option<PathBuf>) -> Option<PathBuf> {
    if let Some(b) = bin_override {
        if b.exists() {
            return Some(b.clone());
        }
    }
    if let Ok(v) = std::env::var("GHBOOST_MIHOMO") {
        let p = PathBuf::from(v);
        if p.exists() {
            return Some(p);
        }
    }
    // 随 ghboost 自带的内核（CLI 场景：与可执行文件同目录）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(b) = bundled_in(dir) {
                return Some(b);
            }
        }
    }
    // 操作系统标准安装路径（含 Windows 的 Clash Verge）
    for c in system_paths() {
        if c.exists() {
            return Some(c);
        }
    }
    // 退路：PATH
    which::which("mihomo").ok()
}

/// 在目录里找随 ghboost 分发的 mihomo 内核。命名规则与 Release 一致：
/// 裸 `mihomo` / `mihomo.exe`，或按目标三元组的 `mihomo-<triple>` /
/// `mihomo-<triple>.exe`。
fn bundled_in(dir: &Path) -> Option<PathBuf> {
    let plain = dir.join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    });
    if plain.is_file() {
        return Some(plain);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let s = e.file_name().to_string_lossy().into_owned();
        let lower = s.to_ascii_lowercase();
        let matched = if cfg!(windows) {
            lower.starts_with("mihomo-") && lower.ends_with(".exe")
        } else {
            s.starts_with("mihomo-") && !s.contains('.')
        };
        if matched && e.path().is_file() {
            return Some(e.path());
        }
    }
    None
}

/// 各操作系统的 mihomo 标准安装路径。
fn system_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        v.push(PathBuf::from("/opt/homebrew/bin/mihomo"));
        v.push(PathBuf::from("/usr/local/bin/mihomo"));
    } else if cfg!(target_os = "windows") {
        v.push(PathBuf::from(
            r"C:\Program Files\Clash Verge\verge-mihomo.exe",
        ));
        v.push(PathBuf::from(
            r"C:\Program Files\Clash Verge\verge-mihomo-alpha.exe",
        ));
        v.push(PathBuf::from(
            r"C:\Program Files (x86)\Clash Verge\verge-mihomo.exe",
        ));
    } else {
        v.push(PathBuf::from("/usr/local/bin/mihomo"));
        v.push(PathBuf::from("/usr/bin/mihomo"));
    }
    v
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn rand_secret() -> String {
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect()
}

/// 把节点 URI 的 #tag 改写成 `n{index}`，保证 mihomo 节点名无特殊字符
/// （路径段路由安全），同时保留连接参数不变。
fn rewrite_uri_name(raw: &str, index: usize) -> String {
    let safe = format!("n{index}");
    match raw.split_once('#') {
        Some((body, _)) => format!("{body}#{safe}"),
        None => format!("{raw}#{safe}"),
    }
}

pub struct Mihomo {
    pub dir: PathBuf,
    pub ctrl_port: u16,
    pub secret: String,
    pub child: Option<Child>,
    /// 顺序节点名（mihomo 内部名），与 batch 下标对应
    pub names: Vec<String>,
}

impl Mihomo {
    /// 启动一个包含给定节点的独立实例。
    /// `bin_override` 可指定 mihomo 二进制路径（否则自动探测）。
    /// 内部按 `n{index}` 生成 mihomo 节点名（保证路径段路由安全）。
    pub fn start(batch: &[Node], bin_override: &Option<PathBuf>) -> Result<Mihomo, String> {
        let bin = mihomo_bin(bin_override).ok_or_else(|| {
            "找不到 mihomo 二进制（Clash Verge 的 verge-mihomo.exe 或 PATH 里的 mihomo）"
                .to_string()
        })?;

        let dir =
            std::env::temp_dir().join(format!("ghboost_mh_{}", rand::thread_rng().gen::<u32>()));
        fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;

        let nodes_file = dir.join("nodes.txt");
        let mut content = String::new();
        for (i, n) in batch.iter().enumerate() {
            content.push_str(&rewrite_uri_name(&n.raw, i));
            content.push('\n');
        }
        fs::write(&nodes_file, content).map_err(|e| format!("写节点文件失败: {e}"))?;

        let http_port = free_port();
        let ctrl_port = free_port();
        let secret = rand_secret();

        // mihomo 是 Windows 程序：路径用正斜杠，反斜杠在 YAML 里是转义符
        let dir_win = to_win_path(&dir);
        let cfg_win = format!("{dir_win}/config.yaml");
        let nodes_win = to_win_path(&nodes_file);

        let cfg = format!(
            "mixed-port: {http_port}\n\
             external-controller: 127.0.0.1:{ctrl_port}\n\
             secret: \"{secret}\"\n\
             mode: direct\n\
             log-level: error\n\
             proxy-providers:\n\
               nodes:\n\
                 type: file\n\
                 path: \"{nodes_win}\"\n\
                 health-check:\n\
                   enable: false\n\
             proxy-groups:\n\
               - name: ALL\n\
                 type: select\n\
                 use: [nodes]\n\
             rules:\n\
               - MATCH,DIRECT\n"
        );
        fs::write(dir.join("config.yaml"), &cfg).map_err(|e| format!("写配置失败: {e}"))?;

        let child = Command::new(&bin)
            .args(["-d", &dir_win, "-f", &cfg_win])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 mihomo 失败: {e}"))?;

        let names: Vec<String> = (0..batch.len()).map(|i| format!("n{i}")).collect();
        let mh = Mihomo {
            dir,
            ctrl_port,
            secret,
            child: Some(child),
            names,
        };

        // 等待 controller 起来 + provider 加载完（ALL 组节点数达标）
        mh.wait_ready(batch.len(), Duration::from_secs(25))?;
        Ok(mh)
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.ctrl_port)
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.secret)
    }

    /// 轮询直到 controller 可连且 provider 加载稳定（节点数连续不再增长）。
    /// 不强制要求达到 `expect`——部分节点 URI 解析失败时 ALL 会永久少于 expect，
    /// 此时应在「加载稳定」或超时后返回已加载的节点，而不是无限等待。
    /// 用 reqwest::blocking 轮询；调用方已把 start 包进 spawn_blocking，线程内无
    /// tokio runtime，阻塞 HTTP 不会触发 "drop runtime" panic。
    fn wait_ready(&self, _expect: usize, timeout: Duration) -> Result<(), String> {
        let start = std::time::Instant::now();
        let client = reqwest::blocking::Client::new();
        let mut last: usize = 0;
        let mut stable: u32 = 0;
        loop {
            if start.elapsed() > timeout {
                return Ok(()); // 超时也返回，使用当前已加载的节点
            }
            if std::net::TcpStream::connect(format!("127.0.0.1:{}", self.ctrl_port)).is_ok() {
                if let Ok(r) = client
                    .get(format!("{}/proxies/ALL", self.base_url()))
                    .header("Authorization", self.auth())
                    .timeout(Duration::from_secs(3))
                    .send()
                {
                    if let Ok(v) = r.json::<Value>() {
                        if let Some(all) = v.get("all").and_then(|a| a.as_array()) {
                            let n = all.len();
                            if n > 0 && n == last {
                                stable += 1;
                            } else {
                                stable = 0;
                                last = n;
                            }
                            // 节点数连续 2 次不变 = 加载完成；或 10s 后已有节点也接受
                            if stable >= 2 || (start.elapsed() > Duration::from_secs(10) && n > 0) {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// 并发测所有节点延迟。返回 (index, Option<delay_ms>)，None=不可用。
    pub async fn test_all(
        &self,
        test_url: &str,
        timeout_ms: u64,
        concurrency: usize,
    ) -> Vec<(usize, Option<u64>)> {
        let client = reqwest::Client::new();
        let sem = std::sync::Arc::new(Semaphore::new(concurrency.max(1)));
        let mut set: JoinSet<(usize, Option<u64>)> = JoinSet::new();

        for (i, name) in self.names.iter().enumerate() {
            let sem = sem.clone();
            let client = client.clone();
            let base = self.base_url();
            let auth = self.auth();
            let tu = test_url.to_string();
            let nm = name.clone();
            set.spawn(async move {
                let _permit = sem.acquire().await;
                let url = format!("{base}/proxies/{nm}/delay?url={tu}&timeout={timeout_ms}");
                let res = client
                    .get(&url)
                    .header("Authorization", auth)
                    .timeout(Duration::from_millis(timeout_ms + 4000))
                    .send()
                    .await;
                match res {
                    Ok(r) => {
                        let v: Value = r.json().await.unwrap_or(Value::Null);
                        // 成功: {"delay":123}; 失败: {"message":"..."} 无 delay
                        let delay = v.get("delay").and_then(|d| d.as_u64());
                        (i, delay.filter(|d| *d > 0))
                    }
                    Err(_) => (i, None),
                }
            });
        }

        let mut out = Vec::with_capacity(self.names.len());
        while let Some(r) = set.join_next().await {
            if let Ok(pair) = r {
                out.push(pair);
            }
        }
        out.sort_by_key(|(i, _)| *i);
        out
    }
}

impl Drop for Mihomo {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // 子进程退出与临时目录清理放到独立 OS 线程，避免在 tokio 异步上下文里
            // 调用阻塞 API（child.wait() / remove_dir_all 占用中）触发 runtime panic。
            let dir = self.dir.clone();
            let _ = std::thread::spawn(move || {
                let _ = child.wait();
                let _ = fs::remove_dir_all(&dir);
            });
        }
    }
}

/// 把 Path 转成 Windows 正斜杠路径串（mihomo 是 Windows 程序，认正斜杠）
fn to_win_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    // 处理 /c/Users/... (Git Bash 挂载) → C:/Users/...
    if s.starts_with("/c/") {
        format!("C:{}", &s[2..])
    } else if s.starts_with("/d/") {
        format!("D:{}", &s[2..])
    } else {
        s
    }
}

/// 测试目标 URL（轻量 204，测速标准）
pub const DEFAULT_TEST_URL: &str = "http://www.gstatic.com/generate_204";

/// 备用测试 URL
pub const ALT_TEST_URL: &str = "https://www.google.com/generate_204";
