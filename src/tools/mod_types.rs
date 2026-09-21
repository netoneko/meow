use alloc::string::String;
use alloc::format;
use core::fmt::Write;
use crate::config::MAX_TOOL_OUTPUT_SIZE;
use libakuma::{mkdir, open, open_flags, write_fd, close};

use super::context::get_sandbox_root;

pub struct ToolResult {
    pub success: bool,
    pub output: String,
}

impl ToolResult {
    pub fn ok(output: String) -> Self {
        if output.len() > MAX_TOOL_OUTPUT_SIZE {
            return handle_output_overflow(output);
        }
        Self { success: true, output }
    }
    
    pub fn err(message: impl Into<String>) -> Self {
        Self { success: false, output: message.into() }
    }
}

/// Create a fresh file for spilling oversized tool output. Returns the open
/// write fd and its path, or `None` if the file could not be created. Shared
/// by `handle_output_overflow` (whole-buffer spill) and the shell's streaming
/// capture sink (`pretend_shell::ReportSink`).
///
/// Lives **inside the current conversation's session directory**, named after
/// *this tool call's own id* (`tool_7.txt` for call `#7`) rather than a bare
/// `/tmp/meow_tool_<timestamp>.txt` with no owner. The id itself is assigned
/// once per call in `chat.rs`, before the call runs — read back here via
/// `current_tool_output_seq()`, not drawn fresh, so a spill file's name always
/// matches the `[tool #N]` reference the model was shown for that same call.
/// Two things that buys: these are found next to the conversation that
/// produced them for later inspection, and `Conversation::reseed` can delete
/// exactly the ones tied to history it just dropped, by name, with no
/// directory listing needed (this environment has no `readdir`). Falls back
/// to the old bare-timestamp path under `/tmp` only when no session has set an
/// output directory yet (a tool call before any `Conversation` exists — not
/// expected in practice, but cheaper to handle than to rule out).
pub fn create_tool_tempfile() -> Option<(i32, String)> {
    let filename = match super::context::tool_output_dir() {
        Some(dir) => {
            let _ = mkdir(&dir);
            let seq = super::context::current_tool_output_seq();
            format!("{}/tool_{}.txt", dir, seq)
        }
        None => {
            let sandbox = get_sandbox_root();
            let tmp_dir = if sandbox == "/" {
                String::from("/tmp")
            } else {
                format!("{}/tmp", sandbox)
            };
            let _ = mkdir(&tmp_dir);
            format!("{}/meow_tool_{}.txt", tmp_dir, crate::util::now_us())
        }
    };

    let fd = open(&filename, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
    if fd >= 0 {
        Some((fd, filename))
    } else {
        None
    }
}

/// Handle tool output that exceeds memory limits by writing it to a temp file.
fn handle_output_overflow(full_output: String) -> ToolResult {
    if let Some((fd, filename)) = create_tool_tempfile() {
        let _ = write_fd(fd, full_output.as_bytes());
        close(fd);

        let mut truncated = String::from("[!] Output truncated due to memory limits.\n");
        let _ = write!(truncated, "Full output saved to: {}\n\n", filename);
        truncated.push_str("Preview:\n---\n");

        let preview_len = core::cmp::min(full_output.len(), 4096);
        truncated.push_str(&full_output[..preview_len]);
        if full_output.len() > preview_len {
            truncated.push_str("\n...");
        }
        truncated.push_str("\n---\n\nNote: You can use `FileReadLines` to read specific parts of the saved output or `CodeSearch` for targeted investigation.");
        
        ToolResult {
            success: true,
            output: truncated,
        }
    } else {
        let mut truncated = String::from("[!] Output truncated (failed to write to temp file)

");
        let preview_len = core::cmp::min(full_output.len(), MAX_TOOL_OUTPUT_SIZE - 256);
        truncated.push_str(&full_output[..preview_len]);
        truncated.push_str("
...");
        
        ToolResult {
            success: true,
            output: truncated,
        }
    }
}

