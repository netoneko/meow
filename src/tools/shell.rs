use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use libakuma::{spawn, waitpid, read_fd, close, open, open_flags};

use crate::config::TOOL_BUFFER_SIZE;
use super::mod_types::ToolResult;

const EAGAIN_ERRNO: i64 = -11; // Value of EAGAIN from libc_errno

/// Shell binary used to interpret command lines. busybox is a static-pie
/// aarch64 build (see bootstrap/bin/busybox); `busybox sh -c "<line>"` runs the
/// ash applet, giving full shell grammar — &&, ||, |, ;, >, <, globbing, $VARS.
const SHELL_BIN: &str = "/bin/busybox";

pub fn tool_shell(command: &str) -> ToolResult {
    // Route the command line through a real shell so operators work as the
    // model expects. meow itself does NOT parse shell grammar — without this,
    // `tcc a.c && ./a` would pass `&&` and `./a` as extra argv to tcc.
    //
    // If no shell is installed (minimal image), fall back to the legacy path:
    // tokenize on whitespace and exec the first token directly (no operators).
    let shell_fd = open(SHELL_BIN, open_flags::O_RDONLY);
    if shell_fd >= 0 {
        close(shell_fd);
        return run_and_capture(SHELL_BIN, &["sh", "-c", command]);
    }

    // Fallback: no shell on disk — exec the first token directly.
    let tokens = tokenize_command(command);
    if tokens.is_empty() {
        return ToolResult::err("Empty command");
    }
    let binary_path = resolve_binary(&tokens[0]);
    // spawn() sets argv[0] to the binary path itself, so pass only tokens[1..].
    let args: Vec<&str> = tokens[1..].iter().map(|s| s.as_str()).collect();
    run_and_capture(&binary_path, &args)
}

/// Resolve a bare binary name against /bin and /usr/bin. Absolute or relative
/// paths are returned unchanged; an unresolved name is returned as-is so spawn
/// can report a clean "not found".
fn resolve_binary(binary: &str) -> String {
    if binary.starts_with('/') || binary.starts_with('.') {
        return String::from(binary);
    }
    for path in ["/bin/", "/usr/bin/"] {
        let full_path = format!("{}{}", path, binary);
        let fd = open(&full_path, open_flags::O_RDONLY);
        if fd >= 0 {
            close(fd);
            return full_path;
        }
    }
    String::from(binary)
}

/// Spawn `binary_path` with `args`, drain its stdout (with a 30s timeout and a
/// 1 MB output cap), and return the captured output plus exit code.
fn run_and_capture(binary_path: &str, args: &[&str]) -> ToolResult {
    // Spawn the process
    let result = match spawn(binary_path, Some(args)) {
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
        } else if n < 0 && n == EAGAIN_ERRNO as isize {
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