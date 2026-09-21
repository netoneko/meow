use alloc::string::String;
use alloc::format;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static INIT: AtomicBool = AtomicBool::new(false);
// Safety: single-threaded userspace; atomic flag guards initialization
static mut SANDBOX: Option<String> = None;
static mut CURRENT: Option<String> = None;

// The current conversation's tool-output directory and a per-session
// sequence number, set by `Conversation` (new/resumed/reseeded) and read by
// `tools::create_tool_tempfile`. Kept here beside `SANDBOX`/`CURRENT`
// because it is the same shape of thing: ambient session context every tool
// call reads, set from one place. See `mod_types.rs::create_tool_tempfile`
// for why this exists — oversized tool output used to spill to a bare
// `/tmp/meow_tool_<timestamp>.txt` with no owner and no cleanup, ever.
static mut TOOL_OUTPUT_DIR: Option<String> = None;
static TOOL_OUTPUT_SEQ: AtomicU64 = AtomicU64::new(0);

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

/// Point tool-output spills at `dir` (a session directory, e.g. `conversation
/// .jsonl`'s own folder) and set the sequence counter to `starting_seq`.
/// Called whenever a `Conversation` starts, resumes, or reseeds.
///
/// `starting_seq` is `0` for a fresh or just-reseeded session (the old
/// numbering was exactly the history just dropped — see `Conversation::reseed`,
/// which unlinks those files by their deterministic names before calling this)
/// and the **probed** existing count for a resumed one (`mod_types::
/// probe_existing_tool_output_count`) — resuming with a blind `0` would let the
/// next spill overwrite `tool_1.txt` while the resumed conversation history
/// still points a prior turn at it.
pub fn set_tool_output_dir(dir: &str, starting_seq: u64) {
    ensure_init();
    unsafe { *core::ptr::addr_of_mut!(TOOL_OUTPUT_DIR) = Some(String::from(dir)); }
    TOOL_OUTPUT_SEQ.store(starting_seq, Ordering::Release);
}

pub fn tool_output_dir() -> Option<String> {
    ensure_init();
    unsafe { (*core::ptr::addr_of!(TOOL_OUTPUT_DIR)).clone() }
}

/// Next 1-based sequence number for a tool-output file in the current
/// session directory, and bump the counter. `reseed` reads
/// `tool_output_seq_count()` first to know the exact range to unlink.
pub fn next_tool_output_seq() -> u64 {
    TOOL_OUTPUT_SEQ.fetch_add(1, Ordering::AcqRel) + 1
}

/// How many tool-output files exist in the current session directory right
/// now — the high-water mark `reseed` unlinks up to before zeroing it.
pub fn tool_output_seq_count() -> u64 {
    TOOL_OUTPUT_SEQ.load(Ordering::Acquire)
}

/// The id most recently handed out by `next_tool_output_seq()` — i.e. this
/// tool call's own id, for `create_tool_tempfile` to name its spill file
/// after (`tool_<id>.txt`) without incrementing past it. The caller
/// (`chat.rs`) assigns the id once per call, before running it; anything the
/// call does that needs "which call is this" reads it back here rather than
/// drawing a second, independent number.
pub fn current_tool_output_seq() -> u64 {
    TOOL_OUTPUT_SEQ.load(Ordering::Acquire)
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
