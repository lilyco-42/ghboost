//! build.rs — 链接 webview-capi 预编译库 + 嵌入 Windows 图标
//!
//! Windows: 链接 webview.dll + 系统库 + 嵌入 .ico
//! macOS: 链接 WebKit.framework
//! Linux: 手动链接 webkit2gtk + gtk3

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    match target_os.as_str() {
        "windows" => {
            let webview_dir = std::env::var("WEBVIEW_LIB_DIR").unwrap_or_else(|_| {
                let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
                format!("{manifest_dir}\\lib\\webview")
            });

            println!("cargo:rustc-link-search=native={webview_dir}");
            println!("cargo:rustc-link-lib=dylib=webview");

            // WebView2 运行时依赖
            for lib in [
                "ole32", "oleaut32", "shlwapi", "version", "user32", "shell32",
            ] {
                println!("cargo:rustc-link-lib={lib}");
            }

            // 嵌入 Windows 图标（winres 仅在 Windows 宿主机可用）
            embed_windows_icon();
        }
        "macos" => {
            println!("cargo:rustc-link-lib=framework=WebKit");
            println!("cargo:rustc-link-lib=framework=Cocoa");
            println!("cargo:rustc-link-lib=framework=CoreGraphics");
        }
        "linux" => {
            // Only link webkit2gtk/gtk-3 when webview feature is enabled.
            // Cross-compilation targets (musl, aarch64) typically skip this
            // because the host's x86_64 .so files aren't usable.
            if std::env::var("CARGO_FEATURE_WEBVIEW").is_ok() {
                println!("cargo:rustc-link-lib=webkit2gtk-4.1");
                println!("cargo:rustc-link-lib=gtk-3");
            }
        }
        _ => {
            eprintln!("warning: webview-capi not configured for target OS: {target_os}");
        }
    }
}

/// 嵌入 .ico 图标 + 文件版本信息到 Windows exe
/// 仅在 Windows 宿主机上编译（winres 是 cfg(windows) build-dependency）
#[cfg(windows)]
fn embed_windows_icon() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let ico_path = format!("{manifest_dir}\\assets\\ghboost.ico");
    if std::path::Path::new(&ico_path).exists() {
        let mut res = winres::WindowsResource::new();
        res.set_icon(&ico_path);
        res.set("ProductName", "ghboost");
        res.set("FileDescription", "GitHub Access Accelerator");
        res.set("CompanyName", "lilyco");
        res.set(
            "FileVersion",
            &std::env::var("CARGO_PKG_VERSION").unwrap_or_default(),
        );
        res.set(
            "ProductVersion",
            &std::env::var("CARGO_PKG_VERSION").unwrap_or_default(),
        );
        if let Err(e) = res.compile() {
            eprintln!("warning: failed to compile resource: {e}");
        }
    }
}

#[cfg(not(windows))]
fn embed_windows_icon() {}
