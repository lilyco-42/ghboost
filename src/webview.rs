//! webview — webview-capi 原生 WebView FFI 绑定 + 安全封装
//!
//! 链接 `webview-capi` 预编译库（Windows: WebView2, macOS: WKWebView, Linux: WebKitGTK）
//! 提供 `WebView` 安全封装和 JS→Rust 回调桥接。

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::{Arc, Mutex};

// ── Raw FFI ──────────────────────────────────────────────

type WebviewT = *mut c_void;

type BindCallback = dyn Fn(String, String) -> String + Send + Sync;

extern "C" {
    fn webview_create(debug: c_int, window: *mut c_void) -> WebviewT;
    fn webview_destroy(w: WebviewT) -> c_int;
    fn webview_run(w: WebviewT) -> c_int;
    fn webview_terminate(w: WebviewT) -> c_int;
    fn webview_set_title(w: WebviewT, title: *const c_char) -> c_int;
    fn webview_set_size(w: WebviewT, width: c_int, height: c_int, hints: c_int) -> c_int;
    fn webview_navigate(w: WebviewT, url: *const c_char) -> c_int;
    fn webview_set_html(w: WebviewT, html: *const c_char) -> c_int;
    fn webview_init(w: WebviewT, js: *const c_char) -> c_int;
    fn webview_eval(w: WebviewT, js: *const c_char) -> c_int;
    fn webview_bind(
        w: WebviewT,
        name: *const c_char,
        fn_pointer: Option<extern "C" fn(*const c_char, *const c_char, *mut c_void)>,
        arg: *mut c_void,
    ) -> c_int;
    fn webview_return(
        w: WebviewT,
        id: *const c_char,
        status: c_int,
        result: *const c_char,
    ) -> c_int;
}

// ── Size hints ───────────────────────────────────────────

pub const HINT_NONE: c_int = 0;
pub const HINT_MIN: c_int = 1;
pub const HINT_MAX: c_int = 2;
pub const HINT_FIXED: c_int = 3;

// ── Callback storage ─────────────────────────────────────

/// 全局回调注册表：`webview_bind` 通过 raw pointer 传给 C 回调，
/// C 回调再通过此表查找 Rust 闭包。
static BINDINGS: Mutex<Option<Arc<Mutex<HashMap<String, Arc<BindCallback>>>>>> = Mutex::new(None);

/// 当前绑定的 webview 指针（供 trampoline 调用 webview_return）
static mut CURRENT_WEBVIEW: WebviewT = std::ptr::null_mut();

/// C 回调 trampoline — 由 `webview_bind` 注册到 C 库
extern "C" fn bind_trampoline(id: *const c_char, req: *const c_char, arg: *mut c_void) {
    let id_str = unsafe {
        if id.is_null() {
            return;
        }
        CStr::from_ptr(id).to_string_lossy().into_owned()
    };
    let req_str = unsafe {
        if req.is_null() {
            String::new()
        } else {
            CStr::from_ptr(req).to_string_lossy().into_owned()
        }
    };

    // _arg 是 binding name（CStr 指针）
    let name = unsafe {
        if arg.is_null() {
            return;
        }
        CStr::from_ptr(arg as *const c_char)
            .to_string_lossy()
            .into_owned()
    };

    let guard = BINDINGS.lock().unwrap();
    let map = guard.as_ref().expect("webview not initialized");
    let bindings = map.lock().unwrap();

    if let Some(callback) = bindings.get(&name) {
        let result = callback(id_str.clone(), req_str);
        // 把结果 return 给 JS 端
        unsafe {
            if !CURRENT_WEBVIEW.is_null() {
                if let Ok(c_id) = CString::new(id_str) {
                    if let Ok(c_result) = CString::new(result) {
                        webview_return(CURRENT_WEBVIEW, c_id.as_ptr(), 0, c_result.as_ptr());
                    }
                }
            }
        }
    } else {
        eprintln!("webview: unknown binding `{name}`");
    }
}

// ── WebView 安全封装 ─────────────────────────────────────

/// 原生 WebView 窗口封装
pub struct WebView {
    ptr: WebviewT,
    /// 持有 binding arg 的 CString，防止被 drop（trampoline 依赖指针存活）
    _binding_args: Vec<CString>,
}

unsafe impl Send for WebView {}
unsafe impl Sync for WebView {}

impl WebView {
    /// 创建新窗口。`debug=true` 开启 DevTools。
    pub fn new(debug: bool) -> Result<Self, String> {
        // 初始化全局回调注册表
        {
            let mut guard = BINDINGS.lock().unwrap();
            if guard.is_none() {
                *guard = Some(Arc::new(Mutex::new(HashMap::new())));
            }
        }

        let ptr = unsafe { webview_create(debug as c_int, std::ptr::null_mut()) };
        if ptr.is_null() {
            return Err("webview_create failed — WebView2 runtime missing?".into());
        }
        // 设置当前 webview 指针供 trampoline 使用
        unsafe {
            CURRENT_WEBVIEW = ptr;
        }
        Ok(Self {
            ptr,
            _binding_args: Vec::new(),
        })
    }

    /// 设置窗口标题
    pub fn set_title(&self, title: &str) -> Result<(), String> {
        let c = CString::new(title).map_err(|e| e.to_string())?;
        check(unsafe { webview_set_title(self.ptr, c.as_ptr()) })
    }

    /// 设置窗口尺寸。hint: HINT_NONE / HINT_MIN / HINT_MAX / HINT_FIXED
    pub fn set_size(&self, width: i32, height: i32, hints: i32) -> Result<(), String> {
        check(unsafe { webview_set_size(self.ptr, width, height, hints) })
    }

    /// 导航到 URL
    pub fn navigate(&self, url: &str) -> Result<(), String> {
        let c = CString::new(url).map_err(|e| e.to_string())?;
        check(unsafe { webview_navigate(self.ptr, c.as_ptr()) })
    }

    /// 设置内嵌 HTML（完全离线，零网络依赖）
    pub fn set_html(&self, html: &str) -> Result<(), String> {
        let c = CString::new(html).map_err(|e| e.to_string())?;
        check(unsafe { webview_set_html(self.ptr, c.as_ptr()) })
    }

    /// 注入 JS（页面加载前执行）
    pub fn init(&self, js: &str) -> Result<(), String> {
        let c = CString::new(js).map_err(|e| e.to_string())?;
        check(unsafe { webview_init(self.ptr, c.as_ptr()) })
    }

    /// 执行 JS（异步，结果通过 webview_bind 回调返回）
    pub fn eval(&self, js: &str) -> Result<(), String> {
        let c = CString::new(js).map_err(|e| e.to_string())?;
        check(unsafe { webview_eval(self.ptr, c.as_ptr()) })
    }

    /// 绑定 JS 函数到 Rust 闭包。
    ///
    /// JS 端调用 `window.${name}(...args)` 时，Rust 闭包会被调用。
    /// 闭包签名: `Fn(id: String, req: String) -> String`
    /// - `req`: JSON 数组字符串，包含 JS 传入的参数
    /// - 返回值: JSON 字符串，会传回 JS 端
    pub fn bind<F>(&mut self, name: &str, callback: F) -> Result<(), String>
    where
        F: Fn(String, String) -> String + Send + Sync + 'static,
    {
        let c_name = CString::new(name).map_err(|e| e.to_string())?;
        // arg 用作 binding name 传递给 trampoline（trampoline 通过此指针查找回调）
        let c_arg = CString::new(name).map_err(|e| e.to_string())?;

        // 注册到全局回调表
        {
            let guard = BINDINGS.lock().unwrap();
            let map = guard.as_ref().expect("webview not initialized");
            map.lock()
                .unwrap()
                .insert(name.to_string(), Arc::new(callback));
        }

        // 保存 CString 使其指针在 bind 后仍然有效
        let arg_ptr = c_arg.as_ptr();
        self._binding_args.push(c_arg);

        let rc = unsafe {
            webview_bind(
                self.ptr,
                c_name.as_ptr(),
                Some(bind_trampoline),
                arg_ptr as *mut c_void,
            )
        };

        check(rc)
    }

    /// 运行消息循环（阻塞直到窗口关闭或 terminate 被调用）
    pub fn run(&self) -> Result<(), String> {
        check(unsafe { webview_run(self.ptr) })
    }

    /// 终止消息循环（可从其他线程调用）
    pub fn terminate(&self) -> Result<(), String> {
        check(unsafe { webview_terminate(self.ptr) })
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                webview_destroy(self.ptr);
                CURRENT_WEBVIEW = std::ptr::null_mut();
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────

/// 在当前 WebView 上执行 JS（线程安全，可从任意线程调用）。
/// 用于后台线程向 WebView 推送日志/进度。
pub fn eval_global(js: &str) -> Result<(), String> {
    let w = unsafe { CURRENT_WEBVIEW };
    if w.is_null() {
        return Err("no active webview".into());
    }
    let c = CString::new(js).map_err(|e| e.to_string())?;
    check(unsafe { webview_eval(w, c.as_ptr()) })
}

fn check(rc: c_int) -> Result<(), String> {
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("webview error: code {rc}"))
    }
}
