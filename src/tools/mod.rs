pub mod context;
pub mod fs;
pub mod net;
pub mod shell;
pub mod helpers;
pub mod mod_types;

use alloc::string::String;
use alloc::format;

pub use mod_types::ToolResult;
pub use context::{get_working_dir, get_sandbox_root};
use helpers::{extract_string_field, extract_number_field};

/// Execute a tool by name with a JSON arguments object.
/// Used by the structured tool calling path (OpenAI format).
pub fn execute_tool_by_name(name: &str, args_json: &str) -> Option<ToolResult> {
    match name {
        "FileRead" => {
            let filename = extract_string_field(args_json, "filename")?;
            Some(fs::tool_file_read(&filename))
        }
        "FileWrite" => {
            let filename = extract_string_field(args_json, "filename")?;
            let content = extract_string_field(args_json, "content").unwrap_or_default();
            Some(fs::tool_file_write(&filename, &content))
        }
        "FileAppend" => {
            let filename = extract_string_field(args_json, "filename")?;
            let content = extract_string_field(args_json, "content")?;
            Some(fs::tool_file_append(&filename, &content))
        }
        "FileExists" => {
            let filename = extract_string_field(args_json, "filename")?;
            Some(fs::tool_file_exists(&filename))
        }
        "FileList" => {
            let path = extract_string_field(args_json, "path").unwrap_or_else(|| String::from("/"));
            Some(fs::tool_file_list(&path))
        }
        "FileDelete" => {
            let filename = extract_string_field(args_json, "filename")?;
            Some(fs::tool_file_delete(&filename))
        }
        "FolderCreate" => {
            let path = extract_string_field(args_json, "path")?;
            Some(fs::tool_folder_create(&path))
        }
        "FileCopy" => {
            let source = extract_string_field(args_json, "source")?;
            let dest = extract_string_field(args_json, "destination")?;
            Some(fs::tool_file_copy(&source, &dest))
        }
        "FileMove" => {
            let source = extract_string_field(args_json, "source")?;
            let dest = extract_string_field(args_json, "destination")?;
            Some(fs::tool_file_move(&source, &dest))
        }
        "HttpFetch" => {
            let url = extract_string_field(args_json, "url")?;
            Some(net::tool_http_fetch(&url))
        }
        "FileReadLines" => {
            let filename = extract_string_field(args_json, "filename")?;
            let start = extract_number_field(args_json, "start").unwrap_or(1);
            let end = extract_number_field(args_json, "end").unwrap_or(start + 50);
            Some(fs::tool_file_read_lines(&filename, start, end))
        }
        "CodeSearch" => {
            let pattern = extract_string_field(args_json, "pattern")?;
            let path = extract_string_field(args_json, "path").unwrap_or_else(|| String::from("."));
            let context = extract_number_field(args_json, "context").unwrap_or(2);
            Some(tool_code_search(&pattern, &path, context))
        }
        "FileEdit" => {
            let filename = extract_string_field(args_json, "filename")?;
            let old_text = extract_string_field(args_json, "old_text")?;
            let new_text = extract_string_field(args_json, "new_text")?;
            Some(fs::tool_file_edit(&filename, &old_text, &new_text))
        }
        "Shell" => {
            let cmd = extract_string_field(args_json, "cmd")?;
            Some(shell::tool_shell(&cmd))
        }
        "Cd" => {
            let path = extract_string_field(args_json, "path")?;
            Some(fs::tool_cd(&path))
        }
        "Pwd" => {
            Some(fs::tool_pwd())
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
