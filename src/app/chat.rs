use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::Ordering;

use crate::config::{Provider, DEFAULT_CONTEXT_WINDOW, COLOR_PEARL, COLOR_GREEN_LIGHT, COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_YELLOW, TOKEN_LIMIT_FOR_COMPACTION};
use crate::api::{self, StreamResponse, ToolCallData};
use crate::tools;
use crate::tui_app;
use super::history::{Message, trim_history, compact_history, calculate_history_tokens};

const MAX_TOOL_ITERATIONS: usize = 20;

pub fn chat_once(
    model: &str,
    provider: &Provider,
    user_message: &str,
    history: &mut Vec<Message>,
    context_window: Option<usize>,
    system_prompt: &str,
) -> Result<(), &'static str> {
    trim_history(history);
    history.push(Message::new("user", user_message));

    let mut total_tools_called: usize = 0;

    for iteration in 0..MAX_TOOL_ITERATIONS {
        let current_tokens = calculate_history_tokens(history);
        let mem_kb = libakuma::memory_usage() / 1024;
        let token_limit = context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);

        let mut messages_json = String::with_capacity(current_tokens * 4);
        messages_json.push('[');
        for (i, msg) in history.iter().enumerate() {
            if i > 0 { messages_json.push(','); }
            msg.write_json(&mut messages_json);
        }
        messages_json.push(']');

        let stream_result = api::send_with_retry(model, provider, &messages_json, iteration > 0, current_tokens, token_limit, mem_kb);
        
        let stream_result = match stream_result {
            Ok(res) => res,
            Err(e) => {
                print_msg(COLOR_RESET, "\n");
                print_notification(COLOR_PEARL, &format!("Request error: {}", e), 0);
                return Err(e);
            }
        };
        
        match stream_result {
            StreamResponse::Partial(partial, stats) => {
                print_stats(&stats, &partial);
                if !partial.is_empty() {
                    history.push(Message::new("assistant", &partial));
                    history.push(Message::new("user", "[System: Your response was cut off mid-stream. Please continue exactly where you left off.]"));
                }
                continue;
            }

            StreamResponse::CompleteWithTools(content, tool_calls, stats) => {
                print_stats(&stats, &content);

                // Store assistant message with tool_calls for the history
                let tc_json = serialize_tool_calls(&tool_calls);
                let mut asst_msg = Message::new("assistant", &content);
                asst_msg.tool_calls_json = Some(tc_json);
                history.push(asst_msg);

                for tc in &tool_calls {
                    total_tools_called += 1;

                    if tc.name == "CompactContext" {
                        let summary = extract_json_string(&tc.arguments, "summary").unwrap_or_default();
                        if summary.is_empty() {
                            let mut result_msg = Message::new("tool", "CompactContext requires a non-empty summary");
                            result_msg.tool_call_id = Some(tc.id.clone());
                            history.push(result_msg);
                        } else {
                            let tokens_before = calculate_history_tokens(history);
                            history.clear();
                            history.push(Message::new("system", system_prompt));
                            let compact_msg = format!("[Previous Conversation Summary]\n{}\n[End Summary]\n\nThe conversation has been compacted. Continue from here.", summary);
                            history.push(Message::new("user", &compact_msg));
                            history.push(Message::new("assistant", "Context loaded. Ready to continue."));
                            let tokens_after = calculate_history_tokens(history);
                            print_msg(COLOR_GREEN_LIGHT, &format!("\n[*] Context compacted: {} -> {} tokens\n", tokens_before, tokens_after));
                            return Ok(());
                        }
                        continue;
                    }

                    let tool_start = libakuma::uptime();
                    let tool_result = tools::execute_tool_by_name(&tc.name, &tc.arguments)
                        .unwrap_or_else(|| tools::ToolResult::err("Unknown or unsupported tool"));
                    let tool_duration_us = libakuma::uptime() - tool_start;

                    let (color, status) = if tool_result.success { (COLOR_GREEN_LIGHT, "Success") } else { (COLOR_PEARL, "Failed") };
                    let status_content = format!("Tool Status: {}", status);

                    if tool_result.success {
                        print_msg(COLOR_RESET, "\n");
                        print_msg(COLOR_GRAY_BRIGHT, &tool_result.output);
                        print_msg(COLOR_RESET, "\n\n");
                        print_notification(color, &status_content, tool_duration_us);
                        print_msg(COLOR_RESET, "\n");
                    } else {
                        print_notification(color, &status_content, tool_duration_us);
                        print_msg(COLOR_RESET, "\n");
                        print_msg(COLOR_GRAY_BRIGHT, &tool_result.output);
                        print_msg(COLOR_RESET, "\n\n");
                    }

                    let current_cwd = tools::get_working_dir();
                    let result_content = if tool_result.success {
                        format!("{}\n[Current Directory: {}]", tool_result.output, current_cwd)
                    } else {
                        format!("Tool failed: {}\n[Current Directory: {}]\n\nPlease analyze the failure and try again.", tool_result.output, current_cwd)
                    };
                    let mut result_msg = Message::new("tool", &result_content);
                    result_msg.tool_call_id = Some(tc.id.clone());
                    history.push(result_msg);
                }

                trim_history(history);
                compact_history(history);
                continue;
            }

            StreamResponse::Complete(assistant_response, stats) => {
                print_stats(&stats, &assistant_response);
                if !assistant_response.is_empty() {
                    history.push(Message::new("assistant", &assistant_response));
                }
                trim_history(history);
                compact_history(history);
                if let Some(ctx_window) = context_window {
                    let current_tokens = calculate_history_tokens(history);
                    if current_tokens > TOKEN_LIMIT_FOR_COMPACTION && current_tokens < ctx_window {
                        print_msg(COLOR_RESET, "\n[!] Token count is high - consider asking to compact context\n");
                    }
                }
                return Ok(());
            } // end StreamResponse::Complete
        } // end match stream_result
    } // end for iteration
    print_msg(COLOR_RESET, "\n[!] Max tool iterations reached\n");
    Ok(())
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = json.find(&pattern)?;
    let value_start = start + pattern.len();
    let mut result = String::new();
    let mut chars = json[value_start..].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    match next {
                        'n' => result.push('\n'), 'r' => result.push('\r'), 't' => result.push('\t'),
                        '"' => result.push('"'), '\\' => result.push('\\'), '/' => result.push('/'),
                        _ => { result.push('\\'); result.push(next); }
                    }
                }
            }
            _ => result.push(c),
        }
    }
    Some(result)
}

fn serialize_tool_calls(tool_calls: &[ToolCallData]) -> String {
    let mut s = String::from("[");
    for (i, tc) in tool_calls.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str("{\"id\":\"");
        s.push_str(&tc.id);
        s.push_str("\",\"type\":\"function\",\"function\":{\"name\":\"");
        s.push_str(&tc.name);
        s.push_str("\",\"arguments\":\"");
        json_escape_to(&tc.arguments, &mut s);
        s.push_str("\"}}");
    }
    s.push(']');
    s
}

fn json_escape_to(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
}

pub fn run_tests() -> i32 {
    use alloc::format;
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- chat tests ---\n");

    // extract_json_string: basic
    total += 1;
    {
        let got = extract_json_string("{\"summary\":\"hello world\"}", "summary");
        if got.as_deref() == Some("hello world") { passed += 1; }
        else { libakuma::print(&format!("  [!] extract_json_string basic: {:?}\n", got)); }
    }

    // extract_json_string: with escape sequences
    total += 1;
    {
        let got = extract_json_string("{\"summary\":\"line1\\nline2\"}", "summary");
        if got.as_deref() == Some("line1\nline2") { passed += 1; }
        else { libakuma::print(&format!("  [!] extract_json_string escape: {:?}\n", got)); }
    }

    // extract_json_string: missing key returns None
    total += 1;
    {
        let got = extract_json_string("{\"other\":\"value\"}", "summary");
        if got.is_none() { passed += 1; }
        else { libakuma::print(&format!("  [!] extract_json_string missing: got {:?}\n", got)); }
    }

    // json_escape_to: basic special chars
    total += 1;
    {
        let mut out = String::new();
        json_escape_to("a\nb\tc\"d\\e", &mut out);
        let want = "a\\nb\\tc\\\"d\\\\e";
        if out == want { passed += 1; }
        else { libakuma::print(&format!("  [!] json_escape_to: got {:?} want {:?}\n", out, want)); }
    }

    // json_escape_to: no-op for clean ASCII
    total += 1;
    {
        let mut out = String::new();
        json_escape_to("hello world", &mut out);
        if out == "hello world" { passed += 1; }
        else { libakuma::print(&format!("  [!] json_escape_to clean: {:?}\n", out)); }
    }

    // serialize_tool_calls: single call
    total += 1;
    {
        let calls = alloc::vec![crate::api::ToolCallData {
            id: String::from("call1"),
            name: String::from("Shell"),
            arguments: String::from("{\"cmd\":\"ls\"}"),
        }];
        let json = serialize_tool_calls(&calls);
        if json.contains("\"id\":\"call1\"") && json.contains("\"name\":\"Shell\"") { passed += 1; }
        else { libakuma::print(&format!("  [!] serialize_tool_calls: {:?}\n", json)); }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}

fn print_msg(color: &str, s: &str) {
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        crate::tui_app::tui_print_with_indent(s, "", 9, Some(color));
    } else {
        if color != COLOR_RESET { libakuma::print(color); }
        libakuma::print(s);
        if color != COLOR_RESET { libakuma::print(COLOR_RESET); }
    }
}

fn print_notification(color: &str, message: &str, duration_us: u64) {
    let mut content = String::from(message);
    if duration_us > 0 {
        content.push_str(" | Duration: ");
        content.push_str(&format_duration(duration_us));
    }
    content.push('\n');
    
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        let col = tui_app::CUR_COL.load(Ordering::SeqCst);
        if col != 0 { tui_app::tui_print_with_indent("\n", "", 0, None); }
        tui_app::tui_print_with_indent(&content, "     --- ", 9, Some(color));
    } else {
        libakuma::print(color);
        libakuma::print("     --- ");
        libakuma::print(&content);
        libakuma::print(COLOR_RESET);
    }
}

fn format_duration(us: u64) -> String {
    let ms = us / 1000;
    if ms >= 60000 { format!("{}m {}s {}ms", ms / 60000, (ms % 60000) / 1000, ms % 1000) }
    else if ms >= 1000 { format!("{}s {}ms", ms / 1000, ms % 1000) }
    else { format!("{}ms", ms) }
}

fn print_stats(stats: &api::StreamStats, full_response: &str) {
    let tokens = (stats.total_bytes + 3) / 4;
    let tps = if stats.stream_us > 0 { (tokens as f64) / (stats.stream_us as f64 / 1_000_000.0) } else { 0.0 };
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        if full_response.ends_with('\n') { tui_app::tui_print_with_indent("\n", "", 0, None); }
        else { tui_app::tui_print_with_indent("\n\n", "", 0, None); }
    } else {
        if full_response.ends_with('\n') { libakuma::print("\n"); }
        else { libakuma::print("\n\n"); }
    }
    let stats_content = format!("First: {}ms | Stream: {}ms | Size: {:.2}KB | TPS: {:.1}", stats.ttft_us / 1000, stats.stream_us / 1000, stats.total_bytes as f64 / 1024.0, tps);
    print_notification(COLOR_YELLOW, &stats_content, stats.ttft_us + stats.stream_us);
}

