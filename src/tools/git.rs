use alloc::format;
use alloc::vec::Vec;
use super::mod_types::ToolResult;
use super::shell::tool_shell;

/// Run an arbitrary `git` subcommand via the shell, e.g. `args = "status"` or
/// `args = "commit -m \"msg\""`. This single tool replaces the former 13
/// per-subcommand git tools, all of which were thin `tool_shell("git ...")`
/// wrappers anyway.
///
/// Force-push stays blocked (as the old `GitPush` did). Note this guard is
/// best-effort: the raw `Shell` tool can still run `git push --force`, exactly
/// as it always could — the dedicated tool just refuses to spell it for you.
pub fn tool_git(args: &str) -> ToolResult {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.iter().any(|t| *t == "push") {
        let forced = tokens.iter().any(|t| {
            *t == "-f" || *t == "--force" || t.starts_with("--force-with-lease") || t.starts_with('+')
        });
        if forced {
            return ToolResult::err("DENIED: Force push is permanently disabled.");
        }
    }
    tool_shell(&format!("git {}", args))
}
