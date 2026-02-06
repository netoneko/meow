use alloc::string::String;
use alloc::format;
use crate::config::MAX_TOOL_OUTPUT_SIZE;
const MAX_FILE_SIZE: usize = 512 * 1024;
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
    
    pub fn err(message: &str) -> Self {
        Self { success: false, output: String::from(message) }
    }
}

/// Handle tool output that exceeds memory limits by writing it to a temp file.
fn handle_output_overflow(full_output: String) -> ToolResult {
    let sandbox = get_sandbox_root();
    let tmp_dir = if sandbox == "/" {
        String::from("/tmp")
    } else {
        format!("{}/tmp", sandbox)
    };
    
    let _ = mkdir(&tmp_dir);
    
    let timestamp = libakuma::uptime();
    let filename = format!("{}/meow_tool_{}.txt", tmp_dir, timestamp);
    
    let fd = open(&filename, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
    if fd >= 0 {
        let _ = write_fd(fd, full_output.as_bytes());
        close(fd);
        
        let mut truncated = String::from("[!] Output truncated due to memory limits nya~!
");
        truncated.push_str(&format!("Full output saved to: {}

", filename));
        truncated.push_str("Preview:
---
");
        
        let preview_len = core::cmp::min(full_output.len(), 4096);
        truncated.push_str(&full_output[..preview_len]);
        if full_output.len() > preview_len {
            truncated.push_str("
...");
        }
        truncated.push_str("
---

Note: You can use `FileReadLines` to read specific parts of the saved output or `CodeSearch` for targeted investigation nya~!");
        
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

pub struct ToolCall {
    pub json: String,
}
