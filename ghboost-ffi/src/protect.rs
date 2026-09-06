//! Android socket protector - prevents routing loops by calling VpnService.protect(fd)
//!
//! Based on meow-android's protect.rs implementation.

use jni::objects::{GlobalRef, JObject};
use jni::JNIEnv;
use std::os::raw::c_int;
use std::sync::{Arc, Mutex};

/// Socket protector trait - allows Rust code to protect sockets from VPN routing
pub trait SocketProtector: Send + Sync {
    /// Protect a socket file descriptor so it bypasses the VPN tunnel
    fn protect(&self, fd: c_int) -> Result<(), Box<dyn std::error::Error>>;
}

/// Global socket protector instance
static PROTECTOR: Mutex<Option<Arc<dyn SocketProtector>>> = Mutex::new(None);

/// Set the global socket protector (called from JNI during VPN setup)
pub fn set_protector(protector: Arc<dyn SocketProtector>) {
    if let Ok(mut guard) = PROTECTOR.lock() {
        *guard = Some(protector);
    }
}

/// Protect a socket file descriptor (called by tun2socks when creating outbound connections)
pub fn protect(fd: c_int) -> Result<(), Box<dyn std::error::Error>> {
    let guard = PROTECTOR.lock()?;
    match guard.as_ref() {
        Some(protector) => protector.protect(fd),
        None => Err("No socket protector installed".into()),
    }
}

/// Android-specific socket protector using JNI
struct AndroidSocketProtector {
    jvm: jni::JavaVM,
    service: GlobalRef,
}

impl SocketProtector for AndroidSocketProtector {
    fn protect(&self, fd: c_int) -> Result<(), Box<dyn std::error::Error>> {
        let mut env = self.jvm.attach_current_thread()?;
        
        let result = env.call_method(
            &self.service,
            "protect",
            "(I)Z",
            &[jni::objects::JValue::Int(fd)],
        )?;
        
        let protected = result.z()?;
        if protected {
            Ok(())
        } else {
            Err(format!("Failed to protect fd={}", fd).into())
        }
    }
}

/// Install the Android socket protector from JNI context
pub fn install(env: &mut JNIEnv, service: &JObject) {
    let jvm = env.get_java_vm().unwrap();
    let service_ref = env.new_global_ref(service).unwrap();
    
    let protector = Arc::new(AndroidSocketProtector {
        jvm,
        service: service_ref,
    });
    
    set_protector(protector);
}

/// Remove the socket protector
pub fn uninstall() {
    if let Ok(mut guard) = PROTECTOR.lock() {
        *guard = None;
    }
}
