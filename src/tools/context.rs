use alloc::string::String;
use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};
use core::cell::UnsafeCell;

// ============================================================================
// Working Directory State (atomic, with sandbox support)
// ============================================================================

/// Working directory state with separate sandbox root and current directory
struct WorkingDirState {
    /// The sandbox root - set once at startup, paths cannot escape this
    sandbox_root: String,
    /// Current working directory (always within sandbox_root)
    current_dir: String,
}

/// Thread-safe working directory storage
/// 
/// Uses lazy initialization with atomic flag. Safe for single-threaded use
/// (which is the case for userspace programs on Akuma).
struct AtomicWorkingDir {
    initialized: AtomicBool,
    state: UnsafeCell<Option<WorkingDirState>>,
}

// Safety: Akuma userspace is single-threaded, and we use atomic for init check
unsafe impl Sync for AtomicWorkingDir {}

impl AtomicWorkingDir {
    const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            state: UnsafeCell::new(None),
        }
    }
    
    /// Initialize on first access using process cwd
    fn ensure_init(&self) {
        if !self.initialized.load(Ordering::Acquire) {
            // Get initial cwd from kernel
            let initial_cwd = String::from(libakuma::getcwd());
            
            // Set both sandbox root and current dir to initial cwd
            // Safety: single-threaded, checked by atomic flag
            unsafe {
                *self.state.get() = Some(WorkingDirState {
                    sandbox_root: initial_cwd.clone(),
                    current_dir: initial_cwd,
                });
            }
            self.initialized.store(true, Ordering::Release);
        }
    }
    
    /// Get the current working directory
    fn get_current(&self) -> String {
        self.ensure_init();
        // Safety: initialized above, single-threaded
        unsafe {
            (*self.state.get()).as_ref().unwrap().current_dir.clone()
        }
    }
    
    /// Get the sandbox root (initial cwd, immutable after init)
    fn get_sandbox_root(&self) -> String {
        self.ensure_init();
        // Safety: initialized above, single-threaded
        unsafe {
            (*self.state.get()).as_ref().unwrap().sandbox_root.clone()
        }
    }
    
    /// Set the current working directory (must be within sandbox)
    fn set_current(&self, path: String) {
        self.ensure_init();
        // Safety: initialized above, single-threaded
        unsafe {
            (*self.state.get()).as_mut().unwrap().current_dir = path;
        }
    }
}

static WORKING_DIR: AtomicWorkingDir = AtomicWorkingDir::new();

/// Get the current working directory
pub fn get_working_dir() -> String {
    WORKING_DIR.get_current()
}

/// Get the sandbox root directory
pub fn get_sandbox_root() -> String {
    WORKING_DIR.get_sandbox_root()
}

/// Set the current working directory (internal, after validation)
pub fn set_working_dir(path: &str) {
    // Normalize path - ensure it starts with /
    let normalized = if path.starts_with('/') {
        String::from(path)
    } else {
        format!("/{}", path)
    };
    
    // Remove trailing slash unless it's root
    let normalized = if normalized.len() > 1 && normalized.ends_with('/') {
        String::from(&normalized[..normalized.len()-1])
    } else {
        normalized
    };
    
    WORKING_DIR.set_current(normalized);
}

/// Normalize a path by resolving . and .. components
pub fn normalize_path(path: &str) -> String {
    let mut parts: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    
    for component in path.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                parts.pop(); // Go up one level (or stay at root if empty)
            }
            name => parts.push(name),
        }
    }
    
    if parts.is_empty() {
        String::from("/")
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Check if a path is within the sandbox root
pub fn is_within_sandbox(path: &str, sandbox: &str) -> bool {
    // Root sandbox allows everything
    if sandbox == "/" {
        return true;
    }
    
    // Path must be the sandbox or start with sandbox/
    path == sandbox || path.starts_with(&format!("{}/", sandbox))
}

/// Resolve a path relative to the current working directory
/// Returns None if the path escapes the sandbox root
pub fn resolve_path(path: &str) -> Option<String> {
    let cwd = get_working_dir();
    let sandbox = get_sandbox_root();
    
    // Compute the absolute path
    let absolute = if path.starts_with('/') {
        // Already absolute
        String::from(path)
    } else {
        // Relative to cwd
        if cwd == "/" {
            format!("/{}", path)
        } else {
            format!("{}/{}", cwd, path)
        }
    };
    
    // Normalize the path (resolve . and ..)
    let normalized = normalize_path(&absolute);
    
    // Check if normalized path is within sandbox
    if !is_within_sandbox(&normalized, &sandbox) {
        return None;
    }
    
    Some(normalized)
}
