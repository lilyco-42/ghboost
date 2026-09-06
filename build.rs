//! build.rs — 链接 webview-capi 预编译库
//!
//! Windows: 链接 webview.dll + 系统库
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
            for lib in ["ole32", "oleaut32", "shlwapi", "version", "user32", "shell32"] {
                println!("cargo:rustc-link-lib={lib}");
            }
        }
        "macos" => {
            println!("cargo:rustc-link-lib=framework=WebKit");
            println!("cargo:rustc-link-lib=framework=Cocoa");
            println!("cargo:rustc-link-lib=framework=CoreGraphics");
        }
        "linux" => {
            println!("cargo:rustc-link-lib=webkit2gtk-4.1");
            println!("cargo:rustc-link-lib=gtk-3");
        }
        _ => {
            eprintln!("warning: webview-capi not configured for target OS: {target_os}");
        }
    }
}
