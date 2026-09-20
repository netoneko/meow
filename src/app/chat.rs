use alloc::string::String;
use alloc::format;
use core::sync::atomic::Ordering;

use crate::config::{Provider, DEFAULT_CONTEXT_WINDOW, COLOR_PEARL, COLOR_GREEN_LIGHT, COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_YELLOW, TOKEN_LIMIT_FOR_COMPACTION};
use crate::util::json_escape_to;
use crate::api::{self, StreamResponse, ToolCallData};
use crate::tools;
use crate::tui_app;
use super::history::{Message, Conversation, MAX_HISTORY_SIZE};

const MAX_TOOL_ITERATIONS: usize = 20;

/// Recovery path for a session nobody is babysitting (e.g. an unattended swarm
/// agent): once history crosses the message-count or token cap, drop it and
/// reseed with a placeholder rather than let the request grow without bound or
/// overflow the model's context window forever. Unlike the LLM-invoked
/// `CompactContext` tool below, this carries no semantic summary — it fires
/// whether or not anything asked for one, so it must stay cheap and unconditional.
fn auto_compact_if_needed(conversation: &mut Conversation, system_prompt: &str) {
    if conversation.len() < MAX_HISTORY_SIZE && conversation.tokens() < TOKEN_LIMIT_FOR_COMPACTION {
        return;
    }
    let count_before = conversation.len();
    let tokens_before = conversation.tokens();
    let compact_msg = format!(
        "[Auto-compaction] {} messages ({} tokens) were dropped after hitting the history/token limit. No summary was generated \u{2014} continue the task with what remains; re-read files or ask if you need the earlier detail.",
        count_before, tokens_before
    );
    conversation.reseed(&[
        Message::new("system", system_prompt),
        Message::new("user", &compact_msg),
        Message::new("assistant", "Understood, continuing."),
    ]);
    print_msg(
        COLOR_YELLOW,
        &format!(
            "\n[*] Auto-compacted: {} msgs/{} tokens -> {} msgs/{} tokens\n",
            count_before, tokens_before, conversation.len(), conversation.tokens()
        ),
    );
}

fn announce_tool_call(tc: &ToolCallData) {
    let args_trimmed = tc.arguments.trim();
    let call_line = if args_trimmed.is_empty() || args_trimmed == "{}" {
        format!("ToolCalled: {}", tc.name)
    } else {
        format!("ToolCalled: {} | Arguments {}", tc.name, args_trimmed)
    };
    print_notification(COLOR_YELLOW, &call_line, 0);
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        tui_app::render_status_now(&format!("[TOOL] Running: {}", tc.name));
    }
}

fn report_and_append_tool_result(conversation: &mut Conversation, tc: &ToolCallData, tool_result: tools::ToolResult, duration_us: u64) {
    let (color, status) = if tool_result.success { (COLOR_GREEN_LIGHT, "Success") } else { (COLOR_PEARL, "Failed") };
    let status_content = format!("Tool Status: {}", status);

    if tool_result.success {
        print_msg(COLOR_RESET, "\n");
        print_msg(COLOR_GRAY_BRIGHT, &tool_result.output);
        print_msg(COLOR_RESET, "\n\n");
        print_notification(color, &status_content, duration_us);
        print_msg(COLOR_RESET, "\n");
    } else {
        print_notification(color, &status_content, duration_us);
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
    conversation.append(&result_msg);
}

// A fork-based parallel tool-call dispatcher (run Shell/HttpFetch calls in
// forked children, reap via wait_any(), collect results from tempfiles) was
// prototyped here and reverted: forking from *inside* an already-forked
// child (Shell's own `spawn()` forks again internally) left the outer child
// dying before it could even create its result tempfile, in every run tried
// in this session's Docker/Alpine test environment. Not root-caused — could
// be the custom allocator, could be something else about nested fork() in
// that environment — and an unresolved bug in a default-enabled core-loop
// change is not something to ship. Sequential tool execution below is the
// verified-correct baseline; revisit parallel dispatch as its own
// investigation, not bundled into an unrelated debugging session.

pub fn chat_once(
    model: &str,
    provider: &Provider,
    user_message: &str,
    conversation: &mut Conversation,
    context_window: Option<usize>,
    system_prompt: &str,
) -> Result<bool, &'static str> {
    auto_compact_if_needed(conversation, system_prompt);
    conversation.append(&Message::new("user", user_message));

    // Did this turn actually do anything? A reasoning model can stream for
    // minutes, hit its token budget while still thinking, and return with
    // an empty `content` and no tool call — which is indistinguishable from
    // "nothing needed doing" unless we say so. Callers that must not lose a
    // turn (the live agent) use this to re-prompt.
    let mut produced = false;

    for iteration in 0..MAX_TOOL_ITERATIONS {
        let current_tokens = conversation.tokens();
        let mem_kb = libakuma::memory_usage() / 1024;
        let token_limit = context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);

        // The request body is streamed straight from the on-disk conversation
        // log into a temp file, so the conversation is never held in memory.
        let stream_result = api::send_with_retry(model, provider, conversation.path(), iteration > 0, current_tokens, token_limit, mem_kb);
        
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
                    conversation.append(&Message::new("assistant", &partial));
                    conversation.append(&Message::new("user", "[System: Your response was cut off mid-stream. Please continue exactly where you left off.]"));
                }
                continue;
            }

            StreamResponse::CompleteWithTools(content, tool_calls, stats) => {
                print_stats(&stats, &content);

                // Store assistant message with tool_calls for the history
                let tc_json = serialize_tool_calls(&tool_calls);
                let mut asst_msg = Message::new("assistant", &content);
                asst_msg.tool_calls_json = Some(tc_json);
                conversation.append(&asst_msg);

                for tc in &tool_calls {
                    if tc.name == "CompactContext" {
                        let summary = crate::json::string_at(&tc.arguments, &["summary"]).unwrap_or_default();
                        if summary.is_empty() {
                            let mut result_msg = Message::new("tool", "CompactContext requires a non-empty summary");
                            result_msg.tool_call_id = Some(tc.id.clone());
                            conversation.append(&result_msg);
                        } else {
                            let tokens_before = conversation.tokens();
                            let compact_msg = format!("[Previous Conversation Summary]\n{}\n[End Summary]\n\nThe conversation has been compacted. Continue from here.", summary);
                            conversation.reseed(&[
                                Message::new("system", system_prompt),
                                Message::new("user", &compact_msg),
                                Message::new("assistant", "Context loaded. Ready to continue."),
                            ]);
                            let tokens_after = conversation.tokens();
                            print_msg(COLOR_GREEN_LIGHT, &format!("\n[*] Context compacted: {} -> {} tokens\n", tokens_before, tokens_after));
                            // Compaction is work, even if nothing was said.
                            return Ok(true);
                        }
                        continue;
                    }

                    produced = true;
                    announce_tool_call(tc);
                    let tool_start = crate::util::now_us();
                    let tool_result = tools::execute_tool_by_name(&tc.name, &tc.arguments)
                        .unwrap_or_else(|| tools::ToolResult::err("Unknown or unsupported tool"));
                    let tool_duration_us = crate::util::now_us() - tool_start;
                    report_and_append_tool_result(conversation, tc, tool_result, tool_duration_us);
                }

                auto_compact_if_needed(conversation, system_prompt);
                continue;
            }

            StreamResponse::Complete(assistant_response, stats) => {
                print_stats(&stats, &assistant_response);
                if !assistant_response.is_empty() {
                    produced = true;
                    conversation.append(&Message::new("assistant", &assistant_response));
                }
                auto_compact_if_needed(conversation, system_prompt);
                if let Some(ctx_window) = context_window {
                    let current_tokens = conversation.tokens();
                    if current_tokens > TOKEN_LIMIT_FOR_COMPACTION && current_tokens < ctx_window {
                        print_msg(COLOR_RESET, "\n[!] Token count is high - consider asking to compact context\n");
                    }
                }
                return Ok(produced);
            } // end StreamResponse::Complete
        } // end match stream_result
    } // end for iteration
    print_msg(COLOR_RESET, "\n[!] Max tool iterations reached\n");
    Ok(produced)
}

/// `tc.id`/`tc.name` come from `extract_json_string`/`accumulate_tool_call_delta`
/// parsing the model provider's SSE response — not compile-time-controlled —
/// so they go through `json_escape_to` like `arguments` does. A provider that
/// ever returns a tool name/id containing `"` or `\` would otherwise write an
/// invalid JSON line to the conversation log, breaking every request after it.
fn serialize_tool_calls(tool_calls: &[ToolCallData]) -> String {
    let mut s = String::from("[");
    for (i, tc) in tool_calls.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str("{\"id\":\"");
        json_escape_to(&tc.id, &mut s);
        s.push_str("\",\"type\":\"function\",\"function\":{\"name\":\"");
        json_escape_to(&tc.name, &mut s);
        s.push_str("\",\"arguments\":\"");
        json_escape_to(&tc.arguments, &mut s);
        s.push_str("\"}}");
    }
    s.push(']');
    s
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    use alloc::format;
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- chat tests ---\n");

    // extract_json_string: basic
    total += 1;
    {
        let got = crate::json::string_at("{\"summary\":\"hello world\"}", &["summary"]);
        if got.as_deref() == Some("hello world") { passed += 1; }
        else { libakuma::print(&format!("  [!] extract_json_string basic: {:?}\n", got)); }
    }

    // extract_json_string: with escape sequences
    total += 1;
    {
        let got = crate::json::string_at("{\"summary\":\"line1\\nline2\"}", &["summary"]);
        if got.as_deref() == Some("line1\nline2") { passed += 1; }
        else { libakuma::print(&format!("  [!] extract_json_string escape: {:?}\n", got)); }
    }

    // extract_json_string: missing key returns None
    total += 1;
    {
        let got = crate::json::string_at("{\"other\":\"value\"}", &["summary"]);
        if got.is_none() { passed += 1; }
        else { libakuma::print(&format!("  [!] extract_json_string missing: got {:?}\n", got)); }
    }

    // extract_json_string: tolerates a space after the colon (Python json.dumps style)
    total += 1;
    {
        let got = crate::json::string_at("{\"summary\": \"hello world\"}", &["summary"]);
        if got.as_deref() == Some("hello world") { passed += 1; }
        else { libakuma::print(&format!("  [!] extract_json_string spaced: {:?}\n", got)); }
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

    // serialize_tool_calls: escapes a provider-supplied id/name containing a quote
    total += 1;
    {
        let calls = alloc::vec![crate::api::ToolCallData {
            id: String::from("call\"1"),
            name: String::from("She\\ll"),
            arguments: String::from("{}"),
        }];
        let json = serialize_tool_calls(&calls);
        let want = "[{\"id\":\"call\\\"1\",\"type\":\"function\",\"function\":{\"name\":\"She\\\\ll\",\"arguments\":\"{}\"}}]";
        if json == want { passed += 1; }
        else { libakuma::print(&format!("  [!] serialize_tool_calls escaping: got {:?} want {:?}\n", json, want)); }
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
    let tokens = stats.total_bytes.div_ceil(4) as u64;
    // Fixed-point integer math (×100 for KB, ×10 for TPS) avoids dragging in
    // core's f64 formatting machinery — ~9KB of code + grisu tables for one line.
    let kb_x100 = (stats.total_bytes as u64 * 100) / 1024;
    let tps_x10 = if stats.stream_us > 0 { (tokens * 10_000_000) / stats.stream_us } else { 0 };
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        if full_response.ends_with('\n') { tui_app::tui_print_with_indent("\n", "", 0, None); }
        else { tui_app::tui_print_with_indent("\n\n", "", 0, None); }
    } else if full_response.ends_with('\n') { libakuma::print("\n"); }
    else { libakuma::print("\n\n"); }
    let stats_content = format!("First: {}ms | Stream: {}ms | Size: {}.{:02}KB | TPS: {}.{}", stats.ttft_us / 1000, stats.stream_us / 1000, kb_x100 / 100, kb_x100 % 100, tps_x10 / 10, tps_x10 % 10);
    print_notification(COLOR_YELLOW, &stats_content, stats.ttft_us + stats.stream_us);
}

