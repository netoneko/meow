use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::Ordering;

use crate::config::{Provider, DEFAULT_CONTEXT_WINDOW, COLOR_MEOW, COLOR_GRAY_DIM, COLOR_PEARL, COLOR_GREEN_LIGHT, COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_YELLOW, TOKEN_LIMIT_FOR_COMPACTION};
use crate::api::{self, StreamResponse};
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
    let mut total_fakes_detected: usize = 0;
    let mut all_responses = String::new();

    for iteration in 0..MAX_TOOL_ITERATIONS {
        let current_tokens = calculate_history_tokens(history);
        let mem_kb = libakuma::memory_usage() / 1024;
        let token_limit = context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);

        print_msg(COLOR_GRAY_DIM, "");

        let mut messages_json = String::from("[");
        for (i, msg) in history.iter().enumerate() {
            if i > 0 { messages_json.push(','); }
            messages_json.push_str(&msg.to_json());
        }
        messages_json.push(']');

        let stream_result = api::send_with_retry(model, provider, &messages_json, iteration > 0, current_tokens, token_limit, mem_kb)?;
        
        print_msg(COLOR_MEOW, "");
        
        let (assistant_response, mut stats) = match stream_result {
            StreamResponse::Complete(response, stats) => (response, stats),
            StreamResponse::Partial(partial, stats) => {
                print_stats(&stats, &partial);
                if !partial.is_empty() {
                    history.push(Message::new("assistant", &partial));
                    history.push(Message::new("user", "[System: Your response was cut off mid-stream. Please continue exactly where you left off.]"));
                }
                continue;
            }
        };

        let is_fake = assistant_response.contains("[Tool Result]");
        if is_fake {
            stats.fakes = 1;
            total_fakes_detected += 1;
        }

        print_stats(&stats, &assistant_response);

        if is_fake {
            let intent_phrases = extract_intent_phrases(&assistant_response);
            let mut self_check_msg = String::from("[System Notice] You outputted a fake '[Tool Result]'. You must NOT hallucinate tool results. \nIf you want to perform an action, you MUST use the precise tool for it.\n");
            if !intent_phrases.is_empty() {
                self_check_msg.push_str("\nBased on your stated intent: ");
                for (i, intent) in intent_phrases.iter().enumerate() {
                    if i > 0 { self_check_msg.push_str(", "); }
                    self_check_msg.push_str(&format!("\"{}\"", intent));
                }
                self_check_msg.push_str("\nPlease call the appropriate tool.\n");
            }
            self_check_msg.push_str("\nAvailable tools:\n(Refer to the tool list provided in your system prompt)");
            history.push(Message::new("user", &self_check_msg));
            print_notification(COLOR_PEARL, "Fake Tool Result detected", 0);
            continue;
        }

        all_responses.push_str(&assistant_response);
        all_responses.push('\n');

        if let Some(compact_result) = try_execute_compact_context(&assistant_response, history, system_prompt) {
            if compact_result.success {
                print_msg(COLOR_GREEN_LIGHT, "\n[*] Context compacted successfully nya~!\n");
            } else {
                print_msg(COLOR_PEARL, "\n[*] Failed to compact context nya...\n");
            }
            print_msg(COLOR_GRAY_BRIGHT, &compact_result.output);
            print_msg(COLOR_RESET, "\n\n");
            return Ok(());
        }

        let (mut current_llm_response_text, tool_calls) = tools::find_tool_calls(&assistant_response);

        if !tool_calls.is_empty() {
            for tool_call in tool_calls {
                total_tools_called += 1;
                if !current_llm_response_text.is_empty() {
                    history.push(Message::new("assistant", &current_llm_response_text));
                    current_llm_response_text.clear();
                }

                let tool_start = libakuma::uptime();
                let tool_result = if let Some(result) = tools::execute_tool_command(&tool_call.json) {
                    result
                } else {
                    tools::ToolResult::err("Failed to parse or execute tool command")
                };
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
                let tool_result_msg = if tool_result.success {
                    format!("[Tool Result]\n{}\n[End Tool Result]\n[Current Directory: {}]\n\nPlease continue your response based on this result.", tool_result.output, current_cwd)
                } else {
                    format!("[Tool Result]\nTool failed: {}\n[End Tool Result]\n[Current Directory: {}]\n\nPlease analyze the failure and try again with a corrected command or different approach.", tool_result.output, current_cwd)
                };
                history.push(Message::new("user", &tool_result_msg));
                trim_history(history);
                compact_history(history);
            }
            continue;
        }

        if !current_llm_response_text.is_empty() {
            history.push(Message::new("assistant", &current_llm_response_text));
        }
        
        trim_history(history);
        compact_history(history);

        let intent_phrases = extract_intent_phrases(&all_responses);
        let mismatch = !intent_phrases.is_empty() && total_tools_called == 0;
        let has_fakes = total_fakes_detected > 0;
        let intent_content = format!("Intent phrases: {} | Tools called: {} | Fakes: {}", intent_phrases.len(), total_tools_called, total_fakes_detected);
        
        if mismatch || has_fakes { print_notification(COLOR_PEARL, &intent_content, 0); }
        else { print_notification(COLOR_GREEN_LIGHT, &intent_content, 0); }

        if mismatch {
            print_notification(COLOR_PEARL, "Self check", 0);
            print_msg(COLOR_RESET, "\n\n");
            let mut intents_list = String::new();
            for (i, intent) in intent_phrases.iter().enumerate() { intents_list.push_str(&format!("  {}. \"{}\"\n", i + 1, intent)); }
            let self_check_msg = format!("[System Notice] You stated the following intention(s) but made 0 tool calls:\n{}\nDid you forget to output the tool call JSON? Please complete the actions you stated.", intents_list);
            history.push(Message::new("user", &self_check_msg));
            continue;
        }

        if let Some(ctx_window) = context_window {
            let current_tokens = calculate_history_tokens(history);
            if current_tokens > TOKEN_LIMIT_FOR_COMPACTION && current_tokens < ctx_window {
                print_msg(COLOR_RESET, "\n[!] Token count is high - consider asking Meow-chan to compact context\n");
            }
        }
        return Ok(());
    }
    print_msg(COLOR_RESET, "\n[!] Max tool iterations reached\n");
    Ok(())
}

fn print_msg(color: &str, s: &str) {
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        if !color.is_empty() && color != COLOR_RESET { tui_app::tui_print(color); }
        if !s.is_empty() { tui_app::tui_print(s); }
        if !color.is_empty() && color != COLOR_RESET { tui_app::tui_print(COLOR_RESET); }
    } else {
        if !color.is_empty() && color != COLOR_RESET { libakuma::print(color); }
        if !s.is_empty() { libakuma::print(s); }
        if !color.is_empty() && color != COLOR_RESET { libakuma::print(COLOR_RESET); }
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
    let stats_content = format!("First: {}ms | Stream: {}ms | Size: {:.2}KB | TPS: {:.1} | Fakes: {}", stats.ttft_us / 1000, stats.stream_us / 1000, stats.total_bytes as f64 / 1024.0, tps, stats.fakes);
    print_notification(COLOR_YELLOW, &stats_content, stats.ttft_us + stats.stream_us);
}

fn extract_intent_phrases(text: &str) -> Vec<String> {
    let starters = ["Let me", "I'll ", "I will ", "First, ", "Now I'll", "Now let me", "First I'll", "First let me"];
    let exclusions = ["let me know", "let me explain", "let me summarize", "let me clarify", "i'll help", "i'll be happy", "i'll wait", "i will help", "i will be happy", "i will wait", "if you need", "if you want", "if you'd like"];
    let mut intents = Vec::new();
    let lower_text = text.to_lowercase();
    for starter in starters {
        let lower_starter = starter.to_lowercase();
        let mut search_start = 0;
        while let Some(pos) = lower_text[search_start..].find(&lower_starter) {
            let abs_pos = search_start + pos;
            let after_starter = &text[abs_pos..];
            let mut end_pos = after_starter.len();
            for (i, c) in after_starter.char_indices() { if c == '\n' || c == '.' || c == '!' || c == '?' { end_pos = i + 1; break; } }
            let intent = after_starter[..end_pos].trim();
            if !exclusions.iter().any(|excl| intent.to_lowercase().contains(excl)) && !intent.is_empty() && intent.len() > starter.len() {
                let intent_str = String::from(intent);
                if !intents.contains(&intent_str) { intents.push(intent_str); }
            }
            search_start = abs_pos + 1;
        }
    }
    intents
}

fn try_execute_compact_context(response: &str, history: &mut Vec<Message>, system_prompt: &str) -> Option<tools::ToolResult> {
    let json_block = if let Some(start) = response.find("```json") {
        let end = response[start..].find("```\n").or_else(|| response[start..].rfind("```"))?;
        let (js, je) = (start + 7, start + end);
        if js < je && je <= response.len() { response[js..je].trim() } else { return None; }
    } else if let Some(start) = response.find("{\"command\"") {
        let (mut depth, mut end) = (0, start);
        for (i, c) in response[start..].chars().enumerate() {
            match c { '{' => depth += 1, '}' => { depth -= 1; if depth == 0 { end = start + i + 1; break; } } _ => {} }
        }
        if end > start { &response[start..end] } else { return None; }
    } else { return None; };

    if !json_block.contains("\"CompactContext\"") { return None; }
    let summary = extract_json_string(json_block, "summary")?;
    if summary.is_empty() { return Some(tools::ToolResult::err("CompactContext requires a non-empty summary")); }
    let tokens_before = calculate_history_tokens(history);
    history.clear();
    history.push(Message::new("system", system_prompt));
    history.push(Message::new("user", &format!("[Previous Conversation Summary]\n{}\n[End Summary]\n\nThe conversation above has been compacted. Continue from here.", summary)));
    history.push(Message::new("assistant", "Understood nya~! I've loaded the conversation summary into my memory banks. Ready to continue where we left off! (=^・ω・^=)"));
    let tokens_after = calculate_history_tokens(history);
    Some(tools::ToolResult::ok(format!("Context compacted: {} tokens -> {} tokens (saved {} tokens)", tokens_before, tokens_after, tokens_before - tokens_after)))
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
                        'n' => result.push('\n'), 'r' => result.push('\r'), 't' => result.push('\t'), '"' => result.push('"'), '\\' => result.push('\\'), '/' => result.push('/'),
                        'u' => {
                            let mut hex = String::new();
                            for _ in 0..4 { if let Some(h) = chars.next() { hex.push(h); } }
                            if let Ok(code) = u32::from_str_radix(&hex, 16) { if let Some(ch) = char::from_u32(code) { result.push(ch); } }
                        }
                        _ => { result.push('\\'); result.push(next); }
                    }
                }
            }
            _ => result.push(c),
        }
    }
    Some(result)
}