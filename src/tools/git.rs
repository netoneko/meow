use alloc::string::String;
use alloc::format;
use super::mod_types::ToolResult;
use super::shell::tool_shell;

pub fn tool_git_clone(url: &str) -> ToolResult {
    tool_shell(&format!("git clone {}", url))
}

pub fn tool_git_pull() -> ToolResult {
    tool_shell("git pull")
}

pub fn tool_git_fetch() -> ToolResult {
    tool_shell("git fetch")
}

pub fn tool_git_push(force: bool) -> ToolResult {
    if force {
        return ToolResult::err("DENIED: Force push is permanently disabled.");
    }
    tool_shell("git push")
}

pub fn tool_git_status() -> ToolResult {
    tool_shell("git status")
}

pub fn tool_git_branch(name: Option<&str>, delete: bool) -> ToolResult {
    match (name, delete) {
        (None, _) => tool_shell("git branch"),
        (Some(n), true) => tool_shell(&format!("git branch -d {}", n)),
        (Some(n), false) => tool_shell(&format!("git branch {}", n)),
    }
}

pub fn tool_git_add(path: &str) -> ToolResult {
    let add_result = tool_shell(&format!("git add {}", path));
    if !add_result.success {
        return add_result;
    }
    
    let status_result = tool_shell("git status");
    
    ToolResult::ok(format!(
        "{}\n\n--- Repository Status ---\n{}",
        add_result.output, status_result.output
    ))
}

pub fn tool_git_commit(message: &str, amend: bool) -> ToolResult {
    let escaped_message = message.replace('"', "\\\"");
    if amend {
        tool_shell(&format!("git commit --amend -m \"{}\"", escaped_message))
    } else {
        tool_shell(&format!("git commit -m \"{}\"", escaped_message))
    }
}

pub fn tool_git_checkout(branch: &str) -> ToolResult {
    tool_shell(&format!("git checkout {}", branch))
}

pub fn tool_git_config(key: &str, value: Option<&str>) -> ToolResult {
    match value {
        Some(v) => {
            let escaped_value = v.replace('"', "\\\"");
            tool_shell(&format!("git config {} \"{}\"", key, escaped_value))
        }
        None => tool_shell(&format!("git config {}", key)),
    }
}

pub fn tool_git_log(count: Option<usize>, oneline: bool) -> ToolResult {
    let mut cmd = String::from("git log");
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
        (None, _) => tool_shell("git tag"),
        (Some(n), true) => tool_shell(&format!("git tag -d {}", n)),
        (Some(n), false) => tool_shell(&format!("git tag {}", n)),
    }
}

pub fn tool_git_reset() -> ToolResult {
    tool_shell("git reset")
}