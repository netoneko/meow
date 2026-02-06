use alloc::string::String;
use alloc::format;
use super::mod_types::ToolResult;
use super::shell::tool_shell;

pub fn tool_git_clone(url: &str) -> ToolResult {
    tool_shell(&format!("scratch clone {}", url))
}

pub fn tool_git_pull() -> ToolResult {
    tool_shell("scratch pull")
}

pub fn tool_git_fetch() -> ToolResult {
    tool_shell("scratch fetch")
}

pub fn tool_git_push(force: bool) -> ToolResult {
    if force {
        return ToolResult::err("DENIED: Force push is permanently disabled.");
    }
    tool_shell("scratch push")
}

pub fn tool_git_status() -> ToolResult {
    tool_shell("scratch status")
}

pub fn tool_git_branch(name: Option<&str>, delete: bool) -> ToolResult {
    match (name, delete) {
        (None, _) => tool_shell("scratch branch"),
        (Some(n), true) => tool_shell(&format!("scratch branch -d {}", n)),
        (Some(n), false) => tool_shell(&format!("scratch branch {}", n)),
    }
}

pub fn tool_git_add(path: &str) -> ToolResult {
    let add_result = tool_shell(&format!("scratch add {}", path));
    if !add_result.success {
        return add_result;
    }
    
    let status_result = tool_shell("scratch status");
    
    ToolResult::ok(format!(
        "{}\n\n--- Repository Status ---\n{}",
        add_result.output, status_result.output
    ))
}

pub fn tool_git_commit(message: &str, amend: bool) -> ToolResult {
    let escaped_message = message.replace('"', "\\\"");
    if amend {
        tool_shell(&format!("scratch commit --amend -m \"{}\"", escaped_message))
    } else {
        tool_shell(&format!("scratch commit -m \"{}\"", escaped_message))
    }
}

pub fn tool_git_checkout(branch: &str) -> ToolResult {
    tool_shell(&format!("scratch checkout {}", branch))
}

pub fn tool_git_config(key: &str, value: Option<&str>) -> ToolResult {
    match value {
        Some(v) => {
            let escaped_value = v.replace('"', "\\\"");
            tool_shell(&format!("scratch config {} \"{}\"", key, escaped_value))
        }
        None => tool_shell(&format!("scratch config {}", key)),
    }
}

pub fn tool_git_log(count: Option<usize>, oneline: bool) -> ToolResult {
    let mut cmd = String::from("scratch log");
    if let Some(n) = count {
        cmd.push_str(&format!(" -n {}", n));
    }
    if oneline {
        cmd.push_str(" --oneline");
    }
    tool_shell(&cmd)
}

pub fn tool_git_tag(name: Option<&str>, delete: bool) -> ToolResult {
    match (name, delete) {
        (None, _) => tool_shell("scratch tag"),
        (Some(n), true) => tool_shell(&format!("scratch tag -d {}", n)),
        (Some(n), false) => tool_shell(&format!("scratch tag {}", n)),
    }
}

pub fn tool_git_reset() -> ToolResult {
    tool_shell("scratch reset")
}