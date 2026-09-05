//! ghboost 核心库（纯逻辑，不依赖 lilyco）
//!
//! 提供两大子系统的核心能力，供两种宿主复用：
//! 1. **bin**（`src/main.rs`，lilyco 框架）—— 命令包装层
//! 2. **cdylib**（C ABI）—— 跨语言 / 跨设备 / 跨平台集成
//!
//! 设计要点：
//! - 核心函数全部是 `async`，由宿主用 `run_blocking` 在 tokio 运行时内驱动；
//! - 进度 / 日志通过 `sink: &dyn Fn(&Event)` 回调回传，宿主可忽略（FFI 用 `NO_SINK`）；
//! - 入参为纯数据结构（`BoostParams` / `ScanParams` / ...），FFI 层用 JSON 反序列化得到。

pub mod hosts;
pub mod nodes;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// 日志级别（与 lilyco 的 LogLevel 一一对应，但本库不依赖 lilyco）
#[derive(Debug, Clone, Copy)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// 核心逻辑向宿主回传的事件
#[derive(Debug, Clone)]
pub enum Event {
    /// 任务开始（带总量与说明）
    Started {
        total: Option<u64>,
        message: Option<String>,
    },
    /// 进度 tick
    Tick {
        current: u64,
        total: Option<u64>,
        message: String,
    },
    /// 日志
    Log { level: Level, message: String },
    /// 完成（带最终 JSON 输出与耗时）
    Done {
        output: serde_json::Value,
        elapsed_ms: u64,
    },
}

/// FFI 调用时使用的空 sink（丢弃所有事件）。每次调用构造一个局部闭包即可。
#[inline]
fn noop_sink() -> impl Fn(&Event) {
    |_: &Event| {}
}

/// 在 tokio 运行时内执行 future：优先复用当前运行时（bin 在 lilyco 运行时内时），
/// 否则新建一个多线程运行时。等价于原 `run_on_rt` / `boost` 里的 `Handle::try_current` 逻辑。
pub fn run_blocking<F, T>(fut: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(h) => h.block_on(fut),
        Err(_) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio 启动失败: {e}"))?
            .block_on(fut),
    }
}

// ─────────────────────────────────────────────────────────────
// C ABI (FFI)
// 约定：所有函数入参为 JSON 字符串（`*const c_char`），出参为 JSON 字符串
// （`*mut c_char`，由调用方用 `ghboost_free` 释放）。出错时返回 `{"error": "..."}`。
// 空指针 / 空串 / `{}` 视为使用全部默认参数。
// ─────────────────────────────────────────────────────────────

fn to_cstring(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        // 输出里出现 NUL 字节极罕见，做兜底
        Err(_) => CString::new("{\"error\":\"output contains NUL byte\"}")
            .unwrap()
            .into_raw(),
    }
}

fn ok_json(v: serde_json::Value) -> *mut c_char {
    to_cstring(serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string()))
}

fn err_json(msg: String) -> *mut c_char {
    ok_json(serde_json::json!({ "error": msg }))
}

/// 解析 C 字符串参数为 `serde_json::Value`。空 / null → 空对象（使用默认参数）。
unsafe fn parse_json_params(params: *const c_char) -> Result<serde_json::Value, String> {
    if params.is_null() {
        return Ok(serde_json::json!({}));
    }
    let cstr = CStr::from_ptr(params);
    let s = cstr
        .to_str()
        .map_err(|_| "参数不是合法 UTF-8".to_string())?;
    if s.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(s).map_err(|e| format!("参数 JSON 解析失败: {e}"))
}

/// 释放本库返回的字符串（C 侧 `ghboost_*` 得到的 `*mut c_char` 都必须用它释放）。
///
/// # Safety
/// `ptr` 必须是由本库 `ghboost_*` 函数返回、且尚未释放的指针，或 NULL。
#[no_mangle]
pub unsafe extern "C" fn ghboost_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // 重建 CString 并 drop，释放底层内存
    drop(CString::from_raw(ptr));
}

/// GitHub hosts 优选（等价于 CLI 的 `ghboost` / `ghboost --apply` / `ghboost --clean`）。
///
/// # Safety
/// `params` 可为 NULL 或指向 UTF-8 JSON 字符串；调用方负责释放返回指针。
#[no_mangle]
pub unsafe extern "C" fn ghboost_boost(params: *const c_char) -> *mut c_char {
    let p = match parse_json_params(params) {
        Ok(v) => v,
        Err(e) => return err_json(e),
    };
    let bp = match serde_json::from_value::<hosts::BoostParams>(p) {
        Ok(b) => b,
        Err(e) => return err_json(format!("参数校验失败: {e}")),
    };
    let sink = noop_sink();
    match run_blocking(hosts::boost_core(bp, &sink)) {
        Ok(v) => ok_json(v),
        Err(e) => err_json(e),
    }
}

/// 节点扫描（等价于 `ghboost scan`）。
///
/// # Safety
/// `params` 可为 NULL 或指向 UTF-8 JSON 字符串；调用方负责释放返回指针。
#[no_mangle]
pub unsafe extern "C" fn ghboost_scan(params: *const c_char) -> *mut c_char {
    let p = match parse_json_params(params) {
        Ok(v) => v,
        Err(e) => return err_json(e),
    };
    let sp = match serde_json::from_value::<nodes::ScanParams>(p) {
        Ok(s) => s,
        Err(e) => return err_json(format!("参数校验失败: {e}")),
    };
    let sink = noop_sink();
    match run_blocking(nodes::scan_core(sp, &sink)) {
        Ok(v) => ok_json(v),
        Err(e) => err_json(e),
    }
}

/// 节点测速（等价于 `ghboost test`，需本机 Mihomo 内核）。
///
/// # Safety
/// `params` 可为 NULL 或指向 UTF-8 JSON 字符串；调用方负责释放返回指针。
#[no_mangle]
pub unsafe extern "C" fn ghboost_test(params: *const c_char) -> *mut c_char {
    let p = match parse_json_params(params) {
        Ok(v) => v,
        Err(e) => return err_json(e),
    };
    let tp = match serde_json::from_value::<nodes::TestParams>(p) {
        Ok(t) => t,
        Err(e) => return err_json(format!("参数校验失败: {e}")),
    };
    let sink = noop_sink();
    match run_blocking(nodes::test_core(tp, &sink)) {
        Ok(v) => ok_json(v),
        Err(e) => err_json(e),
    }
}

/// 节点导出 / 注入（等价于 `ghboost add`）。
///
/// # Safety
/// `params` 可为 NULL 或指向 UTF-8 JSON 字符串；调用方负责释放返回指针。
#[no_mangle]
pub unsafe extern "C" fn ghboost_add(params: *const c_char) -> *mut c_char {
    let p = match parse_json_params(params) {
        Ok(v) => v,
        Err(e) => return err_json(e),
    };
    let ap = match serde_json::from_value::<nodes::AddParams>(p) {
        Ok(a) => a,
        Err(e) => return err_json(format!("参数校验失败: {e}")),
    };
    let sink = noop_sink();
    match run_blocking(nodes::add_core(ap, &sink)) {
        Ok(v) => ok_json(v),
        Err(e) => err_json(e),
    }
}

/// 版本信息（返回 JSON，需 `ghboost_free` 释放）。
///
/// # Safety
/// 无参数；返回的指针需由调用方用 `ghboost_free` 释放。
#[no_mangle]
pub unsafe extern "C" fn ghboost_version() -> *mut c_char {
    ok_json(serde_json::json!({
        "name": "ghboost",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
