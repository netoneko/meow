use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use libakuma::{spawn, waitpid, read_fd, close, open, open_flags};

use crate::config::TOOL_BUFFER_SIZE;
use super::mod_types::ToolResult;

const EAGAIN_ERRNO: i64 = -11; // Value of EAGAIN from libc_errno

pub fn tool_shell(command: &str) -> ToolResult {
    // Parse the command to get the binary and arguments
    // Simple tokenizer: split on whitespace, respecting quotes
    let tokens = tokenize_command(command);
    if tokens.is_empty() {
        return ToolResult::err("Empty command");
    }

    let binary = &tokens[0];
    // Skip argv[0] - the kernel adds the program name automatically
    let args: Vec<&str> = tokens[1..].iter().map(|s| s.as_str()).collect();

    // Check for the binary in common paths
    let binary_path = if binary.starts_with('/') || binary.starts_with('.') {
        binary.clone()
    } else {
        // Try to find the binary
        let paths = ["/bin/", "/usr/bin/"];
        let mut found = None;
        for path in paths {
            let full_path = format!("{}{}", path, binary);
            let fd = open(&full_path, open_flags::O_RDONLY);
            if fd >= 0 {
                close(fd);
                found = Some(full_path);
                break;
            }
        }
        match found {
            Some(p) => p,
            None => {
                // Try the binary name directly
                binary.clone()
            }
        }
    };

    // Spawn the process
    let result = match spawn(&binary_path, Some(&args[..])) {
        Some(r) => r,
        None => return ToolResult::err(&format!("Failed to spawn '{}' (not found?)", binary_path)),
    };

    // Read output from child process
    let mut output = Vec::new();
    let mut buf = [0u8; TOOL_BUFFER_SIZE]; 
    let mut waited_ms = 0u32;
    let max_wait_ms = 30000; // 30 seconds timeout
    let max_shell_output = 1024 * 1024; // 1MB absolute max for shell output to avoid OOM

    loop {
        // Try to read all available data without blocking indefinitely
        let n = read_fd(result.stdout_fd as i32, &mut buf);
        if n > 0 {
            if output.len() + n as usize > max_shell_output {
                let _ = libakuma::kill(result.pid); // Kill runaway process
                close(result.stdout_fd as i32);
                return ToolResult::err("Command produced too much output (exceeded 1MB limit)");
            }
            output.extend_from_slice(&buf[..n as usize]);
            waited_ms = 0; // Reset timeout if we're making progress
        } else if n < 0 && (n as i64) == EAGAIN_ERRNO as i64 {
            // EAGAIN: no data available right now, but process not exited.
        }

        // Check if process has exited
        if let Some((_pid, exit_code)) = waitpid(result.pid) {
            // Process has exited. Do one final aggressive drain to ensure all remaining output is captured.
            loop {
                let n_final = read_fd(result.stdout_fd as i32, &mut buf);
                if n_final > 0 {
                    output.extend_from_slice(&buf[..n_final as usize]);
                } else {
                    break;
                }
            }
            close(result.stdout_fd as i32);

            let output_str = core::str::from_utf8(&output).unwrap_or("<binary output>");

            let mut result_str = String::new();
            if !output_str.is_empty() {
                result_str.push_str("stdout:\n```\n");
                result_str.push_str(output_str);
                result_str.push_str("```\n");
                result_str.push_str(&format!("Exit code: {}", exit_code));
            } else {
                result_str.push_str(&format!("(No output)\nExit code: {}", exit_code));
            }

            if exit_code == 0 {
                return ToolResult::ok(result_str);
            } else {
                return ToolResult {
                    success: false,
                    output: result_str,
                };
            }
        }
        
        // If no data and process not exited, sleep briefly before next poll
        libakuma::sleep_ms(50);
        waited_ms += 50;

        if waited_ms >= max_wait_ms {
            let _ = libakuma::kill(result.pid);
            close(result.stdout_fd as i32);
            return ToolResult::err("Command timed out after 30 seconds");
        }
    }
}

/// Tokenize a command string into arguments
pub fn tokenize_command(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;

    for c in cmd.chars() {
        if escape_next {
            current.push(c);
            escape_next = false;
            continue;
        }

        match c {
            '\\' if !in_single_quote => {
                escape_next = true;
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => {
                current.push(c);
            }
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}