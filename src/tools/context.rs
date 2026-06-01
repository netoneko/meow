use alloc::string::String;
use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};

static INIT: AtomicBool = AtomicBool::new(false);
// Safety: single-threaded userspace; atomic flag guards initialization
static mut SANDBOX: Option<String> = None;
static mut CURRENT: Option<String> = None;

fn ensure_init() {
    if !INIT.load(Ordering::Acquire) {
        let cwd = String::from(libakuma::getcwd());
        unsafe {
            *core::ptr::addr_of_mut!(SANDBOX) = Some(cwd.clone());
            *core::ptr::addr_of_mut!(CURRENT) = Some(cwd);
        }
        INIT.store(true, Ordering::Release);
    }
}

pub fn get_working_dir() -> String {
    ensure_init();
    unsafe { (*core::ptr::addr_of!(CURRENT)).as_ref().unwrap().clone() }
}

pub fn get_sandbox_root() -> String {
    ensure_init();
    unsafe { (*core::ptr::addr_of!(SANDBOX)).as_ref().unwrap().clone() }
}

pub fn set_working_dir(path: &str) {
    ensure_init();
    let mut s = if path.starts_with('/') { String::from(path) } else { format!("/{}", path) };
    if s.len() > 1 && s.ends_with('/') { s.pop(); }
    unsafe { *(*core::ptr::addr_of_mut!(CURRENT)).as_mut().unwrap() = s; }
}

pub fn normalize_path(path: &str) -> String {
    let mut parts: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => { parts.pop(); }
            name => parts.push(name),
        }
    }
    if parts.is_empty() { String::from("/") } else { format!("/{}", parts.join("/")) }
}

pub fn is_within_sandbox(path: &str, sandbox: &str) -> bool {
    sandbox == "/" || path == sandbox || path.starts_with(&format!("{}/", sandbox))
}

pub fn resolve_path(path: &str) -> Option<String> {
    let cwd = get_working_dir();
    let sandbox = get_sandbox_root();
    let absolute = if path.starts_with('/') {
        String::from(path)
    } else if cwd == "/" {
        format!("/{}", path)
    } else {
        format!("{}/{}", cwd, path)
    };
    let normalized = normalize_path(&absolute);
    is_within_sandbox(&normalized, &sandbox).then_some(normalized)
}
