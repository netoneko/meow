use alloc::string::String;
use alloc::format;
use alloc::vec::Vec;

use libakuma::{
    open, close, read_fd, write_fd, fstat, mkdir, read_dir,
    open_flags,
};

use super::context::{resolve_path, get_working_dir, get_sandbox_root, set_working_dir, normalize_path, is_within_sandbox};
use super::mod_types::ToolResult;
// MAX_FILE_SIZE is 512KB
const MAX_FILE_SIZE: usize = 512 * 1024;

/// Resolve path or return error
fn resolve_path_or_err(path: &str) -> Result<String, ToolResult> {
    match resolve_path(path) {
        Some(p) => Ok(p),
        None => Err(ToolResult::err(&format!(
            "Access denied: '{}' is outside the working directory '{}'",
            path, get_working_dir()
        ))),
    }
}

pub fn tool_file_read(filename: &str) -> ToolResult {
    let resolved = match resolve_path_or_err(filename) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    let fd = open(&resolved, open_flags::O_RDONLY);
    if fd < 0 {
        return ToolResult::err(&format!("Failed to open file: {}", filename));
    }
    
    // Get file size
    let stat = match fstat(fd) {
        Ok(s) => s,
        Err(_) => {
            close(fd);
            return ToolResult::err("Failed to get file info");
        }
    };
    
    let size = stat.st_size as usize;
    if size > MAX_FILE_SIZE {
        close(fd);
        return ToolResult::err("File too large (max 512KB)");
    }
    
    let mut buf = alloc::vec![0u8; size];
    let bytes_read = read_fd(fd, &mut buf);
    close(fd);
    
    if bytes_read < 0 {
        return ToolResult::err("Failed to read file");
    }
    
    match core::str::from_utf8(&buf[..bytes_read as usize]) {
        Ok(content) => ToolResult::ok(format!("Contents of '{}':\n```\n{}\n```", filename, content)),
        Err(_) => ToolResult::err("File contains non-UTF8 data"),
    }
}

pub fn tool_file_write(filename: &str, content: &str) -> ToolResult {
    let resolved = match resolve_path_or_err(filename) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    let fd = open(&resolved, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
    if fd < 0 {
        return ToolResult::err(&format!("Failed to create file: {}", filename));
    }
    
    let bytes_written = write_fd(fd, content.as_bytes());
    close(fd);
    
    if bytes_written < 0 {
        return ToolResult::err("Failed to write to file");
    }
    
    ToolResult::ok(format!("Successfully wrote {} bytes to '{}'", bytes_written, filename))
}

pub fn tool_file_append(filename: &str, content: &str) -> ToolResult {
    let resolved = match resolve_path_or_err(filename) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    let fd = open(&resolved, open_flags::O_WRONLY | open_flags::O_APPEND);
    if fd < 0 {
        return ToolResult::err(&format!("Failed to open file for append: {}", filename));
    }
    
    let bytes_written = write_fd(fd, content.as_bytes());
    close(fd);
    
    if bytes_written < 0 {
        return ToolResult::err("Failed to append to file");
    }
    
    ToolResult::ok(format!("Successfully appended {} bytes to '{}'", bytes_written, filename))
}

pub fn tool_file_exists(filename: &str) -> ToolResult {
    let resolved = match resolve_path_or_err(filename) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    let fd = open(&resolved, open_flags::O_RDONLY);
    if fd >= 0 {
        close(fd);
        ToolResult::ok(format!("'{}' exists", filename))
    } else {
        ToolResult::ok(format!("'{}' does not exist", filename))
    }
}

pub fn tool_file_list(path: &str) -> ToolResult {
    let resolved = match resolve_path_or_err(path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    match read_dir(&resolved) {
        Some(entries) => {
            let mut output = format!("Contents of '{}':\n", path);
            let mut count = 0;
            for entry in entries {
                let type_indicator = if entry.is_dir { "/" } else { "" };
                output.push_str(&format!("  {}{}\n", entry.name, type_indicator));
                count += 1;
            }
            if count == 0 {
                output.push_str("  (empty directory)\n");
            }
            ToolResult::ok(output)
        }
        None => ToolResult::err(&format!("Failed to list directory: {}", path)),
    }
}

pub fn tool_file_delete(filename: &str) -> ToolResult {
    let _resolved = match resolve_path_or_err(filename) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // Note: libakuma doesn't have unlink syscall yet
    ToolResult::err(&format!("Delete not yet implemented for: {}", filename))
}

pub fn tool_folder_create(path: &str) -> ToolResult {
    let resolved = match resolve_path_or_err(path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    let result = mkdir(&resolved);
    if result >= 0 {
        ToolResult::ok(format!("Successfully created directory: '{}'", path))
    } else {
        ToolResult::err(&format!("Failed to create directory: {}", path))
    }
}

pub fn tool_cd(path: &str) -> ToolResult {
    let cwd = get_working_dir();
    let sandbox = get_sandbox_root();
    
    // Compute the new absolute path
    let absolute = if path.starts_with('/') {
        String::from(path)
    } else if cwd == "/" {
        format!("/{}", path)
    } else {
        format!("{}/{}", cwd, path)
    };
    
    // Normalize the path (resolve . and ..)
    let new_path = normalize_path(&absolute);
    
    // Check if new path is within sandbox
    if !is_within_sandbox(&new_path, &sandbox) {
        return ToolResult::err(&format!(
            "Access denied: '{}' is outside the sandbox '{}'",
            new_path, sandbox
        ));
    }
    
    // Use chdir syscall to update the process's cwd
    let result = libakuma::chdir(&new_path);
    if result == 0 {
        // Update our working directory state
        set_working_dir(&new_path);
        ToolResult::ok(format!("Changed directory to: {}", new_path))
    } else if result == -2 {
        // ENOENT
        ToolResult::err(&format!("Directory not found: {}", new_path))
    } else {
        ToolResult::err(&format!("Failed to change directory: error {}", result))
    }
}

pub fn tool_pwd() -> ToolResult {
    let cwd = get_working_dir();
    let sandbox = get_sandbox_root();
    
    if sandbox == "/" {
        ToolResult::ok(format!("{} (no sandbox)", cwd))
    } else if cwd == sandbox {
        ToolResult::ok(format!("{} (sandbox root)", cwd))
    } else {
        ToolResult::ok(format!("{} (sandbox: {})", cwd, sandbox))
    }
}

pub fn tool_file_rename(source: &str, dest: &str) -> ToolResult {
    let src_resolved = match resolve_path_or_err(source) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let dst_resolved = match resolve_path_or_err(dest) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    match tool_file_copy_internal(&src_resolved, &dst_resolved) {
        Ok(_) => ToolResult::ok(format!("Renamed '{}' to '{}' (note: original not deleted yet)", source, dest)),
        Err(e) => ToolResult::err(&e),
    }
}

pub fn tool_file_copy(source: &str, dest: &str) -> ToolResult {
    let src_resolved = match resolve_path_or_err(source) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let dst_resolved = match resolve_path_or_err(dest) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    match tool_file_copy_internal(&src_resolved, &dst_resolved) {
        Ok(msg) => ToolResult::ok(msg),
        Err(e) => ToolResult::err(&e),
    }
}

fn tool_file_copy_internal(source: &str, dest: &str) -> Result<String, String> {
    let src_fd = open(source, open_flags::O_RDONLY);
    if src_fd < 0 {
        return Err(format!("Failed to open source: {}", source));
    }
    
    let stat = match fstat(src_fd) {
        Ok(s) => s,
        Err(_) => {
            close(src_fd);
            return Err(String::from("Failed to get file info"));
        }
    };
    
    let size = stat.st_size as usize;
    if size > MAX_FILE_SIZE {
        close(src_fd);
        return Err(String::from("File too large"));
    }
    
    let mut buf = alloc::vec![0u8; size];
    let bytes_read = read_fd(src_fd, &mut buf);
    close(src_fd);
    
    if bytes_read < 0 {
        return Err(String::from("Failed to read source file"));
    }
    
    let dst_fd = open(dest, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
    if dst_fd < 0 {
        return Err(format!("Failed to create destination: {}", dest));
    }
    
    let bytes_written = write_fd(dst_fd, &buf[..bytes_read as usize]);
    close(dst_fd);
    
    if bytes_written < 0 {
        return Err(String::from("Failed to write destination file"));
    }
    
    Ok(format!("Copied '{}' to '{}' ({} bytes)", source, dest, bytes_written))
}

pub fn tool_file_move(source: &str, dest: &str) -> ToolResult {
    let src_resolved = match resolve_path_or_err(source) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let dst_resolved = match resolve_path_or_err(dest) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    match tool_file_copy_internal(&src_resolved, &dst_resolved) {
        Ok(_) => ToolResult::ok(format!("Moved '{}' to '{}' (note: source not deleted yet)", source, dest)),
        Err(e) => ToolResult::err(&e),
    }
}

pub fn tool_file_read_lines(filename: &str, start: usize, end: usize) -> ToolResult {
    let resolved = match resolve_path_or_err(filename) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    let fd = open(&resolved, open_flags::O_RDONLY);
    if fd < 0 {
        return ToolResult::err(&format!("Failed to open file: {}", filename));
    }

    let stat = match fstat(fd) {
        Ok(s) => s,
        Err(_) => {
            close(fd);
            return ToolResult::err("Failed to get file info");
        }
    };

    let size = stat.st_size as usize;
    if size > MAX_FILE_SIZE * 4 {
        close(fd);
        return ToolResult::err("File too large");
    }

    let mut buf = alloc::vec![0u8; size];
    let bytes_read = read_fd(fd, &mut buf);
    close(fd);

    if bytes_read <= 0 {
        return ToolResult::err("Failed to read file");
    }

    let content = match core::str::from_utf8(&buf[..bytes_read as usize]) {
        Ok(s) => s,
        Err(_) => return ToolResult::err("File contains non-UTF8 data"),
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let start_idx = start.saturating_sub(1); // Convert to 0-indexed
    let end_idx = end.min(total_lines);

    if start_idx >= total_lines {
        return ToolResult::err(&format!(
            "Start line {} is beyond file length ({} lines)",
            start, total_lines
        ));
    }

    let mut output = format!(
        "Lines {}-{} of '{}' ({} total lines):\n```\n",
        start, end_idx, filename, total_lines
    );

    for (idx, line) in lines[start_idx..end_idx].iter().enumerate() {
        let line_num = start_idx + idx + 1;
        output.push_str(&format!("{:>4}: {}\n", line_num, line));
    }
    output.push_str("```");

    ToolResult::ok(output)
}

pub fn tool_file_edit(filename: &str, old_text: &str, new_text: &str) -> ToolResult {
    let resolved = match resolve_path_or_err(filename) {
        Ok(p) => p,
        Err(e) => return e,
    };
    
    let fd = open(&resolved, open_flags::O_RDONLY);
    if fd < 0 {
        return ToolResult::err(&format!("Failed to open file: {}", filename));
    }

    let stat = match fstat(fd) {
        Ok(s) => s,
        Err(_) => {
            close(fd);
            return ToolResult::err("Failed to get file info");
        }
    };

    let size = stat.st_size as usize;
    if size > MAX_FILE_SIZE * 4 {
        close(fd);
        return ToolResult::err("File too large");
    }

    let mut buf = alloc::vec![0u8; size];
    let bytes_read = read_fd(fd, &mut buf);
    close(fd);

    if bytes_read <= 0 {
        return ToolResult::err("Failed to read file");
    }

    let content = match core::str::from_utf8(&buf[..bytes_read as usize]) {
        Ok(s) => String::from(s),
        Err(_) => return ToolResult::err("File contains non-UTF8 data"),
    };

    let occurrences: Vec<_> = content.match_indices(old_text).collect();

    if occurrences.is_empty() {
        return ToolResult::err(&format!(
            "Text not found in '{}'. Make sure the text matches exactly (including whitespace).",
            filename
        ));
    }

    if occurrences.len() > 1 {
        let mut line_nums = Vec::new();
        for (pos, _) in &occurrences {
            let line_num = content[..*pos].matches('\n').count() + 1;
            line_nums.push(line_num);
        }
        return ToolResult::err(&format!(
            "Found {} occurrences at lines {:?}. Please provide more context to make the match unique.",
            occurrences.len(),
            line_nums
        ));
    }

    let (match_pos, _) = occurrences[0];
    let new_content = content.replace(old_text, new_text);

    let fd = open(
        &resolved,
        open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC,
    );
    if fd < 0 {
        return ToolResult::err(&format!("Failed to open file for writing: {}", filename));
    }

    let bytes_written = write_fd(fd, new_content.as_bytes());
    close(fd);

    if bytes_written < 0 {
        return ToolResult::err("Failed to write file");
    }

    let line_num = content[..match_pos].matches('\n').count() + 1;

    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    let mut diff = format!("Modified '{}' at line {}:\n```diff\n", filename, line_num);
    for line in &old_lines {
        diff.push_str(&format!("- {}\n", line));
    }
    for line in &new_lines {
        diff.push_str(&format!("+ {}\n", line));
    }
    diff.push_str("```");

    ToolResult::ok(diff)
}
