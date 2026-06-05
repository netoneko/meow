use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use libakuma::{spawn, waitpid, read_fd, close, open, open_flags};

use crate::config::{TOOL_BUFFER_SIZE, USE_PRETEND_SHELL};
use super::mod_types::ToolResult;
use super::pretend_shell;

const EAGAIN_ERRNO: i64 = -11; // Value of EAGAIN from libc_errno

/// Shell binary used to interpret command lines when the pretend shell is
/// disabled. busybox is a static-pie aarch64 build (see bootstrap/bin/busybox);
/// `busybox sh -c "<line>"` runs the ash applet, giving full shell grammar.
const SHELL_BIN: &str = "/bin/busybox";

pub fn tool_shell(command: &str) -> ToolResult {
    // Default path: meow's own in-process "pretend shell". It parses the
    // operators we care about (&&, ||, >, >>) itself and emulates redirects by
    // capturing each child's stdout and re-writing it to a file/socket backend.
    // This removes the hard dependency on busybox being present on disk.
    if USE_PRETEND_SHELL {
        return pretend_shell::run(command);
    }

    // Legacy path: route the line through busybox if it is installed, otherwise
    // tokenize on whitespace and exec the first token directly (no operators).
    let shell_fd = open(SHELL_BIN, open_flags::O_RDONLY);
    if shell_fd >= 0 {
        close(shell_fd);
        return run_and_capture(SHELL_BIN, &["sh", "-c", command]);
    }

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
pub fn resolve_binary(binary: &str) -> String {
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

/// Spawn `binary_path` with `args`, drive the child to completion, and invoke
/// `on_chunk` for each stdout chunk as it arrives. Enforces a 30s wall-clock
/// timeout (the child is killed if exceeded). If `on_chunk` returns `Err`, the
/// child is killed and that error is propagated. Returns the child's exit code.
///
/// This is the shared workhorse. Buffering vs streaming is the caller's choice:
/// `spawn_and_collect` accumulates into a `Vec`, while the pretend shell pipes
/// each chunk straight into a file/socket fd (bounded memory, no full buffer).
pub fn drain_child<F>(binary_path: &str, args: &[&str], mut on_chunk: F) -> Result<i32, String>
where
    F: FnMut(&[u8]) -> Result<(), String>,
{
    let result = match spawn(binary_path, Some(args)) {
        Some(r) => r,
        None => return Err(format!("Failed to spawn '{}' (not found?)", binary_path)),
    };

    let mut buf = [0u8; TOOL_BUFFER_SIZE];
    let mut waited_ms = 0u32;
    let max_wait_ms = 30000; // 30 seconds timeout

    loop {
        let n = read_fd(result.stdout_fd as i32, &mut buf);
        if n > 0 {
            if let Err(e) = on_chunk(&buf[..n as usize]) {
                let _ = libakuma::kill(result.pid);
                close(result.stdout_fd as i32);
                return Err(e);
            }
            waited_ms = 0; // Reset timeout while making progress
        } else if n < 0 && n == EAGAIN_ERRNO as isize {
            // EAGAIN: no data available right now, but process not exited.
        }

        if let Some((_pid, exit_code)) = waitpid(result.pid) {
            // Final aggressive drain of anything still buffered in the pipe.
            loop {
                let n_final = read_fd(result.stdout_fd as i32, &mut buf);
                if n_final > 0 {
                    if let Err(e) = on_chunk(&buf[..n_final as usize]) {
                        close(result.stdout_fd as i32);
                        return Err(e);
                    }
                } else {
                    break;
                }
            }
            close(result.stdout_fd as i32);
            return Ok(exit_code);
        }

        libakuma::sleep_ms(50);
        waited_ms += 50;
        if waited_ms >= max_wait_ms {
            let _ = libakuma::kill(result.pid);
            close(result.stdout_fd as i32);
            return Err(String::from("Command timed out after 30 seconds"));
        }
    }
}

/// Spawn `binary_path` with `args` and collect the child's stdout into a byte
/// buffer, returning `(output, exit_code)`. Caps the buffer at 1 MB (the child
/// is killed if exceeded). Used for output that must be returned to the model.
pub fn spawn_and_collect(binary_path: &str, args: &[&str]) -> Result<(Vec<u8>, i32), String> {
    let mut output = Vec::new();
    let max_shell_output = 1024 * 1024; // 1MB absolute max to avoid OOM
    let exit_code = drain_child(binary_path, args, |chunk| {
        if output.len() + chunk.len() > max_shell_output {
            return Err(String::from("Command produced too much output (exceeded 1MB limit)"));
        }
        output.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok((output, exit_code))
}

/// Spawn `binary_path` with `args`, collect stdout, and format a ToolResult.
fn run_and_capture(binary_path: &str, args: &[&str]) -> ToolResult {
    let (output, exit_code) = match spawn_and_collect(binary_path, args) {
        Ok(v) => v,
        Err(e) => return ToolResult::err(&e),
    };

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
        ToolResult::ok(result_str)
    } else {
        ToolResult { success: false, output: result_str }
    }
}

/// Tokenize a command string into arguments (quote-aware, no operators).
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
