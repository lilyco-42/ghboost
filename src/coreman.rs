//! coreman — xray / sing-box 内核进程管理（mihomo 继续走 `mihomo.rs`）。
//!
//! 分工：mihomo 有 REST API 可热重载；xray / sing-box 在本项目里一律
//! 「换配置 = 重启」，简单且没有热更新静默半生效的坑。
//!
//! 流程：定位二进制 → 配置落盘 → 内核自带 check 验配置 → spawn → 等端口 listen。
//! check 是排障闭环的关键：配置被拒时把内核 stderr 原文带出来，
//! 否则外面只看到「起不来」，无从下手。
//!
//! 配置内容由 `corecfg::emit` 生成、经 `start_with_config()` 传入，本模块不碰协议。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::corecfg::CoreKind;

/// 规范二进制档名主干（不含 `.exe`）。
pub fn binary_stem(kind: CoreKind) -> &'static str {
    match kind {
        CoreKind::Mihomo => "mihomo",
        CoreKind::Xray => "xray",
        CoreKind::SingBox => "sing-box",
    }
}

/// 规范档名（含平台后缀）。
pub fn binary_name(kind: CoreKind) -> String {
    if cfg!(windows) {
        format!("{}.exe", binary_stem(kind))
    } else {
        binary_stem(kind).to_string()
    }
}

/// 配置所在目录（mihomo 的 `-d` 要的是目录不是文件）。
fn parent_dir(config: &Path) -> String {
    match config.parent() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => String::new(),
    }
}

/// 该内核的默认配置文件路径（面板 / CLI `core check` 的缺省值）。
///
/// 与 `CoreManager::new` 的落盘位置一致：`%LOCALAPPDATA%\ghboost\<stem>\config.json`。
pub fn default_config_path(kind: CoreKind) -> PathBuf {
    crate::web::ghboost_dir()
        .join(binary_stem(kind))
        .join("config.json")
}

/// 正常运行的启动参数（纯函数，单测钉死）。
/// xray 新旧都认 `run -c`；sing-box 1.10+ 必须显式 `run`。
pub fn launch_args(kind: CoreKind, config: &Path) -> Vec<String> {
    let c = config.to_string_lossy().into_owned();
    match kind {
        // mihomo 不经本模块，参数矩阵仍保持完整（`-d <dir>` → 读该目录 config.yaml）。
        CoreKind::Mihomo => vec!["-d".into(), parent_dir(config)],
        CoreKind::Xray => vec!["run".into(), "-c".into(), c],
        CoreKind::SingBox => vec!["run".into(), "-c".into(), c],
    }
}

/// 启动前验配置的参数（纯函数）：xray `-test`、sing-box `check`、mihomo `-t`。
pub fn check_args(kind: CoreKind, config: &Path) -> Vec<String> {
    let c = config.to_string_lossy().into_owned();
    match kind {
        CoreKind::Mihomo => vec!["-t".into(), "-d".into(), parent_dir(config)],
        CoreKind::Xray => vec!["run".into(), "-test".into(), "-c".into(), c],
        CoreKind::SingBox => vec!["check".into(), "-c".into(), c],
    }
}

/// PATH 兜底查找（随包目录没命中时用；开发机 / 用户自装场景）。
fn path_lookup(stem: &str) -> Option<PathBuf> {
    let out = Command::new(if cfg!(windows) { "where" } else { "which" })
        .arg(stem)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next()?;
    let p = PathBuf::from(line.trim());
    p.exists().then_some(p)
}

/// 内核运行状态（面板展示用；没有 mihomo 那样的流量统计面）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CoreStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub mixed_port: u16,
}

/// xray / sing-box 进程句柄。
pub struct CoreManager {
    kind: CoreKind,
    binary: PathBuf,
    config_path: PathBuf,
    mixed_port: u16,
    process: Arc<Mutex<Option<Child>>>,
}

impl CoreManager {
    /// 定位内核：随包 bin 目录（与 mihomo 同址）→ 档名规范化自愈 → PATH。
    pub fn locate(kind: CoreKind) -> Result<PathBuf, String> {
        let name = binary_name(kind);
        let dir = crate::web::kernel_bin_dir();
        let want = dir.join(&name);
        if !want.exists() {
            // 上游发行包解出来是 `Xray-windows-64.exe` 这类带后缀的名字，正规化一次。
            crate::web::normalize_kernel_name(&dir, &name);
        }
        if want.exists() {
            return Ok(want);
        }
        if let Some(p) = path_lookup(binary_stem(kind)) {
            return Ok(p);
        }
        Err(format!(
            "找不到 {stem} 内核。随包位置应为 {}；若自行安装，把所在目录加入 PATH 后重试。",
            want.display(),
            stem = binary_stem(kind)
        ))
    }

    /// 创建管理器（定位内核、准备配置目录；不启动）。
    pub fn new(kind: CoreKind, mixed_port: u16) -> Result<Self, String> {
        if kind == CoreKind::Mihomo {
            return Err("mihomo 不经 coreman，走 MihomoManager".into());
        }
        let binary = Self::locate(kind)?;
        let dir = crate::web::ghboost_dir().join(binary_stem(kind));
        std::fs::create_dir_all(&dir).map_err(|e| format!("建配置目录失败: {e}"))?;
        let config_path = dir.join("config.json");
        Ok(Self {
            kind,
            binary,
            config_path,
            mixed_port,
            process: Arc::new(Mutex::new(None)),
        })
    }

    pub fn kind(&self) -> CoreKind {
        self.kind
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn mixed_port(&self) -> u16 {
        self.mixed_port
    }

    /// 用内核自己的 test/check 子命令验配置，失败带出 stderr（排障闭环的关键）。
    pub fn check_config(&self) -> Result<(), String> {
        let out = Command::new(&self.binary)
            .args(check_args(self.kind, &self.config_path))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{} check 失败: {e}", binary_stem(self.kind)))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let raw = if err.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout)
        } else {
            err
        };
        // 只留最后 8 行：配置错误的结论在尾部，前面多是 banner。
        let lines: Vec<&str> = raw.lines().collect();
        let skip = lines.len().saturating_sub(8);
        let tail = lines[skip..].join("\n");
        Err(format!(
            "{} 拒绝了配置（{}）：\n{tail}",
            binary_stem(self.kind),
            out.status
        ))
    }

    /// 落盘配置 → check 验证 → 重启 → 等端口 listen。
    pub fn start_with_config(&mut self, config_json: &str) -> Result<CoreStatus, String> {
        std::fs::write(&self.config_path, config_json).map_err(|e| format!("写配置失败: {e}"))?;
        self.check_config()?;
        self.stop()?;
        let mut child = Command::new(&self.binary)
            .args(launch_args(self.kind, &self.config_path))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 {} 失败: {e}", binary_stem(self.kind)))?;
        std::thread::sleep(Duration::from_millis(300));
        match child.try_wait() {
            Ok(Some(status)) => Err(format!(
                "{} 启动后立即退出（状态码 {status}）。配置已过 check，多半是端口被占：{}",
                binary_stem(self.kind),
                self.config_path.display()
            )),
            Ok(None) => {
                // 先放锁再探端口：Mutex 不可重入（mihomo.rs 里踩过，见那的注释）。
                {
                    let mut g = self.process.lock().map_err(|e| e.to_string())?;
                    *g = Some(child);
                }
                self.wait_port_ready(Duration::from_secs(5))?;
                Ok(self.status())
            }
            Err(e) => Err(format!("检查进程状态失败: {e}")),
        }
    }

    /// 等 mixed 端口真正 listen（xray/sing-box 没有可轮询的 REST API）。
    fn wait_port_ready(&self, budget: Duration) -> Result<(), String> {
        let deadline = Instant::now() + budget;
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.mixed_port));
        let mut last = "从未连上".to_string();
        while Instant::now() < deadline {
            match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)) {
                Ok(_) => return Ok(()),
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        Err(format!(
            "{} 的代理端口 {} 没监听到（{last}）",
            binary_stem(self.kind),
            self.mixed_port
        ))
    }

    /// 停止内核进程。
    pub fn stop(&self) -> Result<(), String> {
        {
            let mut g = self.process.lock().map_err(|e| e.to_string())?;
            if let Some(child) = g.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            *g = None;
        }
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.status().running
    }

    pub fn status(&self) -> CoreStatus {
        // try_wait 需要 &mut child；取锁失败（中毒）时拿毒内值继续读状态，
        // 状态读取不值得整个进程崩掉。
        let mut g = self.process.lock().unwrap_or_else(|e| e.into_inner());
        let (running, pid) = match g.as_mut() {
            Some(c) => match c.try_wait() {
                Ok(None) => (true, Some(c.id())),
                _ => (false, None),
            },
            None => (false, None),
        };
        CoreStatus {
            running,
            pid,
            mixed_port: self.mixed_port,
        }
    }
}

impl Drop for CoreManager {
    fn drop(&mut self) {
        // 只调 stop：它在自己块内拿/放锁，不会与 Drop 期间的借用死锁
        // （mihomo.rs 曾因 Drop→stop→get_status 嵌套拿锁把 cargo test 挂死 6 小时）。
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 档名映射() {
        assert_eq!(binary_stem(CoreKind::Mihomo), "mihomo");
        assert_eq!(binary_stem(CoreKind::Xray), "xray");
        assert_eq!(binary_stem(CoreKind::SingBox), "sing-box");
        assert!(binary_name(CoreKind::Xray).starts_with("xray"));
        assert!(binary_name(CoreKind::SingBox).starts_with("sing-box"));
    }

    #[test]
    fn 启动与check参数矩阵() {
        let cfg = PathBuf::from("/opt/g/config.json");

        let a = launch_args(CoreKind::Xray, &cfg);
        assert_eq!(a[0], "run");
        assert_eq!(a[1], "-c");
        assert_eq!(a[2], "/opt/g/config.json");

        let s = launch_args(CoreKind::SingBox, &cfg);
        assert_eq!(s[0], "run");
        assert_eq!(s[1], "-c");

        let m = launch_args(CoreKind::Mihomo, &cfg);
        assert_eq!(m[0], "-d");
        assert_eq!(m[1], "/opt/g");
    }

    #[test]
    fn check参数矩阵() {
        let cfg = PathBuf::from("/opt/g/config.json");
        assert_eq!(check_args(CoreKind::Xray, &cfg)[1], "-test");
        assert_eq!(check_args(CoreKind::SingBox, &cfg)[0], "check");
        assert_eq!(check_args(CoreKind::Mihomo, &cfg)[0], "-t");
    }
}
