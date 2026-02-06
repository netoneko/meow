pub mod context;
pub mod fs;
pub mod git;
pub mod chainlink;
pub mod net;
pub mod shell;
pub mod helpers;
pub mod mod_types;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;

pub use mod_types::{ToolResult, ToolCall};
pub use context::{get_working_dir, get_sandbox_root};
pub use chainlink::chainlink_available;
use helpers::{extract_string_field, extract_number_field};

/// Parse and execute a tool command from JSON
pub fn execute_tool_command(json: &str) -> Option<ToolResult> {
    let tool_name = extract_string_field(json, "tool")?;
    
    match tool_name.as_str() {
        "FileRead" => {
            let filename = extract_string_field(json, "filename")?;
            Some(fs::tool_file_read(&filename))
        }
        "FileWrite" => {
            let filename = extract_string_field(json, "filename")?;
            let content = extract_string_field(json, "content").unwrap_or_default();
            Some(fs::tool_file_write(&filename, &content))
        }
        "FileAppend" => {
            let filename = extract_string_field(json, "filename")?;
            let content = extract_string_field(json, "content")?;
            Some(fs::tool_file_append(&filename, &content))
        }
        "FileExists" => {
            let filename = extract_string_field(json, "filename")?;
            Some(fs::tool_file_exists(&filename))
        }
        "FileList" => {
            let path = extract_string_field(json, "path").unwrap_or_else(|| String::from("/"));
            Some(fs::tool_file_list(&path))
        }
        "FileDelete" => {
            let filename = extract_string_field(json, "filename")?;
            Some(fs::tool_file_delete(&filename))
        }
        "FolderCreate" => {
            let path = extract_string_field(json, "path")?;
            Some(fs::tool_folder_create(&path))
        }
        "FileRename" => {
            let source = extract_string_field(json, "source_filename")?;
            let dest = extract_string_field(json, "destination_filename")?;
            Some(fs::tool_file_rename(&source, &dest))
        }
        "FileCopy" => {
            let source = extract_string_field(json, "source")?;
            let dest = extract_string_field(json, "destination")?;
            Some(fs::tool_file_copy(&source, &dest))
        }
        "FileMove" => {
            let source = extract_string_field(json, "source")?;
            let dest = extract_string_field(json, "destination")?;
            Some(fs::tool_file_move(&source, &dest))
        }
        "HttpFetch" => {
            let url = extract_string_field(json, "url")?;
            Some(net::tool_http_fetch(&url))
        }
        "GitClone" => {
            let url = extract_string_field(json, "url")?;
            Some(git::tool_git_clone(&url))
        }
        "GitPull" => {
            Some(git::tool_git_pull())
        }
        "GitPush" => {
            let force = extract_string_field(json, "force")
                .map(|s| s == "true")
                .unwrap_or(false);
            Some(git::tool_git_push(force))
        }
        "GitStatus" => {
            Some(git::tool_git_status())
        }
        "GitBranch" => {
            let name = extract_string_field(json, "name");
            let delete = extract_string_field(json, "delete")
                .map(|s| s == "true")
                .unwrap_or(false);
            Some(git::tool_git_branch(name.as_deref(), delete))
        }
        "GitFetch" => {
            Some(git::tool_git_fetch())
        }
        "GitAdd" => {
            let path = extract_string_field(json, "path").unwrap_or_else(|| String::from("."));
            Some(git::tool_git_add(&path))
        }
        "GitCommit" => {
            let message = extract_string_field(json, "message")?;
            let amend = extract_string_field(json, "amend")
                .map(|s| s == "true")
                .unwrap_or(false);
            Some(git::tool_git_commit(&message, amend))
        }
        "GitCheckout" => {
            let branch = extract_string_field(json, "branch")?;
            Some(git::tool_git_checkout(&branch))
        }
        "GitConfig" => {
            let key = extract_string_field(json, "key")?;
            let value = extract_string_field(json, "value");
            Some(git::tool_git_config(&key, value.as_deref()))
        }
        "GitLog" => {
            let count = extract_number_field(json, "count");
            let oneline = extract_string_field(json, "oneline")
                .map(|s| s == "true")
                .unwrap_or(false);
            Some(git::tool_git_log(count, oneline))
        }
        "GitTag" => {
            let name = extract_string_field(json, "name");
            let delete = extract_string_field(json, "delete")
                .map(|s| s == "true")
                .unwrap_or(false);
            Some(git::tool_git_tag(name.as_deref(), delete))
        }
        "GitReset" => {
            Some(git::tool_git_reset())
        }
        "FileReadLines" => {
            let filename = extract_string_field(json, "filename")?;
            let start = extract_number_field(json, "start").unwrap_or(1);
            let end = extract_number_field(json, "end").unwrap_or(start + 50);
            Some(fs::tool_file_read_lines(&filename, start, end))
        }
        "CodeSearch" => {
            let pattern = extract_string_field(json, "pattern")?;
            let path = extract_string_field(json, "path").unwrap_or_else(|| String::from("."));
            let context = extract_number_field(json, "context").unwrap_or(2);
            Some(tool_code_search(&pattern, &path, context))
        }
        "FileEdit" => {
            let filename = extract_string_field(json, "filename")?;
            let old_text = extract_string_field(json, "old_text")?;
            let new_text = extract_string_field(json, "new_text")?;
            Some(fs::tool_file_edit(&filename, &old_text, &new_text))
        }
        "Shell" => {
            let cmd = extract_string_field(json, "cmd")?;
            Some(shell::tool_shell(&cmd))
        }
        "Cd" => {
            let path = extract_string_field(json, "path")?;
            Some(fs::tool_cd(&path))
        }
        "Pwd" => {
            Some(fs::tool_pwd())
        }
        "ChainlinkInit" => {
            if !chainlink_available() {
                return Some(ToolResult::err("chainlink not found in /bin"));
            }
            Some(chainlink::tool_chainlink_init())
        }
        "ChainlinkCreate" => {
            if !chainlink_available() {
                return Some(ToolResult::err("chainlink not found in /bin"));
            }
            let title = extract_string_field(json, "title")?;
            let description = extract_string_field(json, "description");
            let priority = extract_string_field(json, "priority");
            Some(chainlink::tool_chainlink_create(&title, description.as_deref(), priority.as_deref()))
        }
        "ChainlinkList" => {
            if !chainlink_available() {
                return Some(ToolResult::err("chainlink not found in /bin"));
            }
            let status = extract_string_field(json, "status");
            Some(chainlink::tool_chainlink_list(status.as_deref()))
        }
        "ChainlinkShow" => {
            if !chainlink_available() {
                return Some(ToolResult::err("chainlink not found in /bin"));
            }
            let id = extract_number_field(json, "id")?;
            Some(chainlink::tool_chainlink_show(id))
        }
        "ChainlinkClose" => {
            if !chainlink_available() {
                return Some(ToolResult::err("chainlink not found in /bin"));
            }
            let id = extract_number_field(json, "id")?;
            Some(chainlink::tool_chainlink_close(id))
        }
        "ChainlinkReopen" => {
            if !chainlink_available() {
                return Some(ToolResult::err("chainlink not found in /bin"));
            }
            let id = extract_number_field(json, "id")?;
            Some(chainlink::tool_chainlink_reopen(id))
        }
        "ChainlinkComment" => {
            if !chainlink_available() {
                return Some(ToolResult::err("chainlink not found in /bin"));
            }
            let id = extract_number_field(json, "id")?;
            let text = extract_string_field(json, "text")?;
            Some(chainlink::tool_chainlink_comment(id, &text))
        }
        "ChainlinkLabel" => {
            if !chainlink_available() {
                return Some(ToolResult::err("chainlink not found in /bin"));
            }
            let id = extract_number_field(json, "id")?;
            let label = extract_string_field(json, "label")?;
            Some(chainlink::tool_chainlink_label(id, &label))
        }
        _ => None,
    }
}

fn tool_code_search(pattern: &str, path: &str, context: usize) -> ToolResult {
    let resolved = match context::resolve_path(path) {
        Some(p) => p,
        None => return ToolResult::err(&format!(
            "Access denied: '{}' is outside the working directory '{}'",
            path, context::get_working_dir()
        )),
    };
    
    match crate::code_search::search_to_string(pattern, &resolved, context) {
        Ok(results) => ToolResult::ok(results),
        Err(e) => ToolResult::err(&format!("Search failed: {}", e)),
    }
}

/// Try to find all tool command JSON blocks in the LLM's response.
pub fn find_tool_calls(response: &str) -> (String, Vec<ToolCall>) {
    let mut tool_calls = Vec::new();
    let mut current_response = String::from(response);

    loop {
        let mut found_match = false;
        
        if let Some((json_block, start_offset, end_offset)) = find_code_block(&current_response) {
            if json_block.contains("\"command\"") && json_block.contains("\"tool\"") {
                tool_calls.push(ToolCall { json: json_block.to_string() });
                current_response.replace_range(start_offset..end_offset, "");
                found_match = true;
            }
        }
        
        if !found_match {
            if let Some((json_block, start_offset, end_offset)) = find_inline_json(&current_response) {
                if json_block.contains("\"command\"") && json_block.contains("\"tool\"") {
                    tool_calls.push(ToolCall { json: json_block.to_string() });
                    current_response.replace_range(start_offset..end_offset, "");
                    found_match = true;
                }
            }
        }
        
        if !found_match {
            break;
        }
    }
    
    (current_response.trim().to_string(), tool_calls)
}

fn find_code_block(text: &str) -> Option<(&str, usize, usize)> {
    let start = text.find("```json")?;
    let after_start = &text[start + 7..];
    let end_offset_in_after_start = after_start.find("\n```")
        .map(|p| p + 1)
        .or_else(|| after_start.find("```"))?;
    
    let json_block = after_start[..end_offset_in_after_start].trim();
    Some((json_block, start, start + 7 + end_offset_in_after_start + 3))
}

fn find_inline_json(text: &str) -> Option<(&str, usize, usize)> {
    let mut search_start = 0;
    
    while search_start < text.len() {
        let brace_pos = text[search_start..].find('{')?;
        let abs_brace_pos = search_start + brace_pos;
        
        let after_brace = &text[abs_brace_pos + 1..];
        let trimmed = after_brace.trim_start();
        
        if trimmed.starts_with("\"command\"") {
            if let Some(json_end) = find_matching_brace(&text[abs_brace_pos..]) {
                let json_block = &text[abs_brace_pos..abs_brace_pos + json_end + 1];
                return Some((json_block, abs_brace_pos, abs_brace_pos + json_end + 1));
            }
        }
        search_start = abs_brace_pos + 1;
    }
    None
}

fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;
    
    for (i, c) in s.chars().enumerate() {
        if escape_next {
            escape_next = false;
            continue;
        }
        
        match c {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    
    None
}
