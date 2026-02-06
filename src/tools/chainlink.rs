use alloc::string::String;
use alloc::format;
use libakuma::{open, close, open_flags};

use super::mod_types::ToolResult;
use super::shell::tool_shell;

pub fn chainlink_available() -> bool {
    let fd = open("/bin/chainlink", open_flags::O_RDONLY);
    if fd >= 0 {
        close(fd);
        true
    } else {
        false
    }
}

pub fn tool_chainlink_init() -> ToolResult {
    tool_shell("chainlink init")
}

pub fn tool_chainlink_create(title: &str, description: Option<&str>, priority: Option<&str>) -> ToolResult {
    let mut cmd = format!("chainlink create \"{}\"", title.replace('"', "\\\""));
    if let Some(desc) = description {
        cmd.push_str(&format!(" -d \"{}\"", desc.replace('"', "\\\"")));
    }
    if let Some(prio) = priority {
        cmd.push_str(&format!(" -p {}", prio));
    }
    tool_shell(&cmd)
}

pub fn tool_chainlink_list(status: Option<&str>) -> ToolResult {
    match status {
        Some(s) => tool_shell(&format!("chainlink list -s {}", s)),
        None => tool_shell("chainlink list"),
    }
}

pub fn tool_chainlink_show(id: usize) -> ToolResult {
    tool_shell(&format!("chainlink show {}", id))
}

pub fn tool_chainlink_close(id: usize) -> ToolResult {
    tool_shell(&format!("chainlink close {}", id))
}

pub fn tool_chainlink_reopen(id: usize) -> ToolResult {
    tool_shell(&format!("chainlink reopen {}", id))
}

pub fn tool_chainlink_comment(id: usize, text: &str) -> ToolResult {
    let escaped = text.replace('"', "\\\"");
    tool_shell(&format!("chainlink comment {} \"{}\"", id, escaped))
}

pub fn tool_chainlink_label(id: usize, label: &str) -> ToolResult {
    tool_shell(&format!("chainlink label {} \"{}\"", id, label))
}

pub const CHAINLINK_TOOLS_SECTION: &str = r#"
### Issue Tracker Tools (Chainlink):

31. **ChainlinkInit** - Initialize the issue tracker database
    Args: `{}`
    Note: Creates .chainlink/issues.db in current directory.

32. **ChainlinkCreate** - Create a new issue
    Args: `{"title": "Issue title", "description": "optional desc", "priority": "low|medium|high"}`
    Note: Priority defaults to "medium" if not specified.

33. **ChainlinkList** - List issues
    Args: `{"status": "open|closed|all"}`
    Note: Defaults to "open" if status not specified.

34. **ChainlinkShow** - Show issue details with comments and labels
    Args: `{"id": 1}`

35. **ChainlinkClose** - Close an issue
    Args: `{"id": 1}`

36. **ChainlinkReopen** - Reopen a closed issue
    Args: `{"id": 1}`

37. **ChainlinkComment** - Add a comment to an issue
    Args: `{"id": 1, "text": "Comment text"}`

38. **ChainlinkLabel** - Add a label to an issue
    Args: `{"id": 1, "label": "bug"}`
"#;