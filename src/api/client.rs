use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::Ordering;

use libakuma::net::{resolve, TcpStream};
use libakuma_tls::{HttpHeaders, HttpStreamTls, StreamResult, TLS_RECORD_SIZE};

use crate::config::{Provider, ApiType};
use crate::tui_app;
use super::types::{StreamResponse, StreamStats};

const MAX_RETRIES: u32 = 10;
const DEFAULT_MAX_TOKENS: usize = 16384;

/// Attempt to send request with retries and exponential backoff
pub fn send_with_retry(
    model: &str,
    provider: &Provider,
    history_json: &str,
    is_continuation: bool,
    current_tokens: usize,
    token_limit: usize,
    mem_kb: usize,
) -> Result<StreamResponse, &'static str> {
    let mut backoff_ms: u64 = 500;
    let is_tui = tui_app::TUI_ACTIVE.load(Ordering::SeqCst);

    let status_prefix = if is_continuation {
        "[MEOW] continuing"
    } else {
        "[MEOW] jacking in"
    };
    
    tui_app::update_streaming_status(status_prefix, 0, None);
    
    if !is_tui {
        if is_continuation {
            libakuma::print("[continuing");
        } else {
            libakuma::print("[jacking in");
        }
    }

    let start_time = libakuma::uptime();

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            if !is_tui {
                libakuma::print(&format!(" retry {}", attempt));
            }
            tui_app::update_streaming_status(&format!("{} retry {}", status_prefix, attempt), 0, None);
            poll_sleep(backoff_ms, current_tokens, token_limit, mem_kb);
            backoff_ms *= 2;
        }

        if tui_app::tui_is_cancelled() {
            if !is_tui {
                libakuma::print("\n[cancelled]");
            }
            tui_app::clear_streaming_status();
            return Err("Request cancelled");
        }

        if !is_tui {
            libakuma::print(".");
        }

        let stream = match connect_to_provider(provider) {
            Ok(s) => s,
            Err(e) => {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print(&format!("] {}", e)); }
                    return Err("Connection failed");
                }
                continue;
            }
        };

        tui_app::update_streaming_status("[MEOW] waiting", 0, None);
        if !is_tui { libakuma::print("."); }

        let (path, request_body) = build_chat_request(model, provider, history_json);

        if provider.is_https() {
            let (host, _) = provider.host_port().ok_or("Invalid URL")?;
            
            let mut read_buf = alloc::vec![0u8; TLS_RECORD_SIZE];
            let mut write_buf = alloc::vec![0u8; TLS_RECORD_SIZE];
            
            let mut http_stream = match HttpStreamTls::connect(stream, &host, &mut read_buf, &mut write_buf) {
                Ok(s) => s,
                Err(e) => {
                    if attempt == MAX_RETRIES - 1 {
                        if !is_tui { libakuma::print(&format!("] TLS error: {:?}", e)); }
                        return Err("TLS handshake failed");
                    }
                    continue;
                }
            };
            
            let mut headers = HttpHeaders::new();
            headers.content_type("application/json");
            if let Some(key) = &provider.api_key {
                headers.bearer_auth(key);
            }
            
            if let Err(_) = http_stream.post(&host, &path, &request_body, &headers) {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print("] "); }
                    return Err("Failed to send request");
                }
                continue;
            }
            
            if !is_tui { 
                libakuma::print("] waiting");
            }
            
            match read_streaming_with_http_stream_tls(&mut http_stream, start_time, provider, current_tokens, token_limit, mem_kb, is_tui) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e == "Request cancelled" { return Err(e); }
                    if attempt == MAX_RETRIES - 1 { return Err(e); }
                    if !is_tui { libakuma::print(&format!(" ({})", e)); }
                    continue;
                }
            }
        } else {
            if let Err(e) = send_post_request(&stream, &path, &request_body, provider) {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print("] "); }
                    return Err(e);
                }
                continue;
            }

            if !is_tui {
                libakuma::print("] waiting");
            }

            match read_streaming_response_with_progress(&stream, start_time, provider, current_tokens, token_limit, mem_kb, is_tui) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e == "Request cancelled" { return Err(e); }
                    if attempt == MAX_RETRIES - 1 { return Err(e); }
                    if !is_tui { libakuma::print(&format!(" ({})", e)); }
                    continue;
                }
            }
        }
    }

    Err("Max retries exceeded")
}

fn connect_to_provider(provider: &Provider) -> Result<TcpStream, String> {
    let (host, port) = provider.host_port().ok_or_else(|| String::from("Invalid provider URL"))?;
    let ip = resolve(&host).map_err(|_| format!("DNS resolution failed for: {}", host))?;
    let addr_str = format!("{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], port);
    TcpStream::connect(&addr_str).map_err(|_| format!("Connection failed to: {}", addr_str))
}

fn send_post_request(stream: &TcpStream, path: &str, body: &str, provider: &Provider) -> Result<(), &'static str> {
    let (host, port) = provider.host_port().ok_or("Invalid URL")?;
    let auth_header = match &provider.api_key {
        Some(key) => format!("Authorization: Bearer {}\r\n", key),
        None => String::new(),
    };
    let request = format!(
        "POST {} HTTP/1.0\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         {}Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        path, host, port, auth_header, body.len(), body
    );
    stream.write_all(request.as_bytes()).map_err(|_| "Failed to send request")
}

fn build_chat_request(model: &str, provider: &Provider, history_json: &str) -> (String, String) {
    match provider.api_type {
        ApiType::Ollama => {
            let body = format!(
                "{{\"model\":\"{}\",\"messages\":{},\"stream\":true,\"options\":{{\"num_predict\":{}}}}}",
                model, history_json, DEFAULT_MAX_TOKENS
            );
            (String::from("/api/chat"), body)
        }
        ApiType::OpenAI => {
            let body = format!(
                "{{\"model\":\"{}\",\"messages\":{},\"stream\":true,\"max_tokens\":{}}}",
                model, history_json, DEFAULT_MAX_TOKENS
            );
            let base = provider.base_path();
            let path = if base.is_empty() || base == "/" {
                String::from("/v1/chat/completions")
            } else if base.ends_with("/v1") {
                format!("{}/chat/completions", base)
            } else {
                format!("{}/chat/completions", base.trim_end_matches('/'))
            };
            (path, body)
        }
    }
}

fn read_streaming_with_http_stream_tls(
    stream: &mut HttpStreamTls<'_>,
    start_time: u64,
    provider: &Provider,
    current_tokens: usize,
    token_limit: usize,
    mem_kb: usize,
    is_tui: bool,
) -> Result<StreamResponse, &'static str> {
    let mut full_response = String::new();
    let mut pending_lines = String::new();
    let mut first_token_received = false;
    let mut stream_completed = false;
    let mut ttft_us = 0;
    let mut stream_start_us = 0;

    loop {
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb);
        if tui_app::tui_is_cancelled() { return Err("Request cancelled"); }
        match stream.read_chunk() {
            StreamResult::Data(data) => {
                if let Ok(s) = core::str::from_utf8(&data) { pending_lines.push_str(s); }
                while let Some(newline_pos) = pending_lines.find('\n') {
                    let line = &pending_lines[..newline_pos];
                    if !line.is_empty() {
                        if let Some((content, done)) = parse_streaming_line(line, provider) {
                            if !content.is_empty() {
                                if !first_token_received {
                                    first_token_received = true;
                                    let now = libakuma::uptime();
                                    ttft_us = now - start_time;
                                    stream_start_us = now;
                                    tui_app::update_streaming_status("[MEOW] streaming", 0, None);
                                    if !is_tui {
                                        libakuma::print(" ");
                                        print_elapsed(ttft_us / 1000);
                                        libakuma::print("\n");
                                    }
                                }
                                tui_app::tui_print(&content);
                                full_response.push_str(&content);
                            }
                            if done {
                                tui_app::clear_streaming_status();
                                return Ok(StreamResponse::Complete(full_response.clone(), StreamStats { ttft_us, stream_us: libakuma::uptime() - stream_start_us, total_bytes: 0, fakes: 0 }));
                            }
                        }
                    }
                    pending_lines.drain(..newline_pos + 1);
                }
            }
            StreamResult::WouldBlock => { libakuma::sleep_ms(10); }
            StreamResult::Done => {
                let remaining = pending_lines.trim();
                if !remaining.is_empty() {
                    if let Some((content, done)) = parse_streaming_line(remaining, provider) {
                        if !content.is_empty() {
                            if !first_token_received {
                                first_token_received = true;
                                let now = libakuma::uptime();
                                ttft_us = now - start_time;
                                stream_start_us = now;
                                tui_app::update_streaming_status("[MEOW] streaming", 0, Some(ttft_us / 1000));
                                if !is_tui {
                                    libakuma::print(" ");
                                    print_elapsed(ttft_us / 1000);
                                    libakuma::print("\n");
                                }
                            }
                            tui_app::tui_print(&content);
                            full_response.push_str(&content);
                        }
                        if done {
                            stream_completed = true;
                            tui_app::clear_streaming_status();
                        }
                    }
                }
                break;
            }
            StreamResult::Error(_) => { return Err("Server returned error"); }
        }
    }
    let stats = StreamStats { ttft_us, stream_us: if first_token_received { libakuma::uptime() - stream_start_us } else { 0 }, total_bytes: full_response.len(), fakes: 0 };
    if !stream_completed && !full_response.is_empty() {
        full_response.shrink_to_fit();
        return Ok(StreamResponse::Partial(full_response, stats));
    }
    full_response.shrink_to_fit();
    Ok(StreamResponse::Complete(full_response, stats))
}

fn read_streaming_response_with_progress(
    stream: &TcpStream,
    start_time: u64,
    provider: &Provider,
    current_tokens: usize,
    token_limit: usize,
    mem_kb: usize,
    is_tui: bool,
) -> Result<StreamResponse, &'static str> {
    let mut buf = [0u8; 1024];
    let mut pending_data = Vec::new();
    let mut headers_parsed = false;
    let mut full_response = String::new();
    let mut read_attempts = 0u32;
    let mut dots_printed = 0u32;
    let mut first_token_received = false;
    let mut any_data_received = false;
    let mut stream_completed = false;
    let mut ttft_us = 0;
    let mut stream_start_us = 0;

    loop {
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb);
        if tui_app::tui_is_cancelled() { return Err("Request cancelled"); }
        match stream.read(&mut buf) {
            Ok(0) => {
                if !any_data_received { return Err("Connection closed by server"); }
                if let Ok(remaining_str) = core::str::from_utf8(&pending_data) {
                    for line in remaining_str.trim().lines() {
                        if let Some((content, done)) = parse_streaming_line(line, provider) {
                            if !content.is_empty() {
                                if !first_token_received {
                                    first_token_received = true;
                                    let now = libakuma::uptime();
                                    ttft_us = now - start_time;
                                    stream_start_us = now;
                                    tui_app::update_streaming_status("[MEOW] streaming", 0, None);
                                    if !is_tui {
                                        for _ in 0..(7 + dots_printed) { libakuma::print("\x08 \x08"); }
                                        print_elapsed(ttft_us / 1000);
                                        libakuma::print("\n");
                                    }
                                }
                                tui_app::tui_print(&content);
                                full_response.push_str(&content);
                            }
                            if done { stream_completed = true; tui_app::clear_streaming_status(); }
                        }
                    }
                }
                break;
            }
            Ok(n) => {
                any_data_received = true;
                read_attempts = 0;
                pending_data.extend_from_slice(&buf[..n]);
                if !headers_parsed {
                    if let Some(pos) = find_header_end(&pending_data) {
                        let header_str = core::str::from_utf8(&pending_data[..pos]).unwrap_or("");
                        if !header_str.contains(" 200 ") { return Err("Server returned error"); }
                        headers_parsed = true;
                        pending_data.drain(..pos + 4);
                    }
                    continue;
                }
                if let Ok(body_str) = core::str::from_utf8(&pending_data) {
                    let last_newline = body_str.rfind('\n');
                    let complete_part = match last_newline { Some(pos) => &body_str[..pos + 1], None => continue };
                    let mut is_done = false;
                    for line in complete_part.lines() {
                        if line.is_empty() { continue; }
                        if let Some((content, done)) = parse_streaming_line(line, provider) {
                            if !content.is_empty() {
                                if !first_token_received {
                                    first_token_received = true;
                                    let now = libakuma::uptime();
                                    ttft_us = now - start_time;
                                    stream_start_us = now;
                                    tui_app::update_streaming_status("[MEOW] streaming", 0, None);
                                    if !is_tui {
                                        for _ in 0..(7 + dots_printed) { libakuma::print("\x08 \x08"); }
                                        print_elapsed(ttft_us / 1000);
                                        libakuma::print("\n");
                                    }
                                }
                                tui_app::tui_print(&content);
                                full_response.push_str(&content);
                            }
                            if done { is_done = true; tui_app::clear_streaming_status(); break; }
                        }
                    }
                    if let Some(pos) = last_newline { pending_data.drain(..pos + 1); }
                    if is_done {
                        stream_completed = true;
                        return Ok(StreamResponse::Complete(full_response, StreamStats { ttft_us, stream_us: libakuma::uptime() - stream_start_us, total_bytes: 0, fakes: 0 }));
                    }
                }
            }
            Err(e) => {
                if e.kind == libakuma::net::ErrorKind::WouldBlock || e.kind == libakuma::net::ErrorKind::TimedOut {
                    read_attempts += 1;
                    if read_attempts % 50 == 0 && !first_token_received && !is_tui { libakuma::print("."); dots_printed += 1; }
                    if read_attempts > 6000 { return Err("Timeout waiting for response"); }
                    libakuma::sleep_ms(10);
                    continue;
                }
                return Err("Network error");
            }
        }
    }
    let stats = StreamStats { ttft_us, stream_us: if first_token_received { libakuma::uptime() - stream_start_us } else { 0 }, total_bytes: full_response.len(), fakes: 0 };
    if !stream_completed && !full_response.is_empty() {
        full_response.shrink_to_fit();
        return Ok(StreamResponse::Partial(full_response, stats));
    }
    full_response.shrink_to_fit();
    Ok(StreamResponse::Complete(full_response, stats))
}

fn parse_streaming_line(line: &str, provider: &Provider) -> Option<(String, bool)> {
    match provider.api_type {
        ApiType::Ollama => {
            let done = line.contains("\"done\":true") || line.contains("\"done\": true");
            let content = extract_json_string(line, "content").unwrap_or_default();
            Some((content, done))
        }
        ApiType::OpenAI => {
            let line = line.trim();
            if line == "data: [DONE]" { return Some((String::new(), true)); }
            if !line.starts_with("data:") { return Some((String::new(), false)); }
            let json = line.strip_prefix("data:")?.trim();
            if json.is_empty() || json == "[DONE]" { return Some((String::new(), json == "[DONE]")); }
            Some((extract_openai_delta_content(json).unwrap_or_default(), false))
        }
    }
}

fn extract_openai_delta_content(json: &str) -> Option<String> {
    let delta_pos = json.find("\"delta\"")?;
    let after_delta = &json[delta_pos..];
    let content_pos = after_delta.find("\"content\"")?;
    let after_content = &after_delta[content_pos..];
    let colon_pos = after_content.find(':')?;
    let rest = &after_content[colon_pos + 1..];
    let trimmed = rest.trim_start();
    if !trimmed.starts_with('"') { return None; }
    let value_rest = &trimmed[1..];
    let mut result = String::new();
    let mut chars = value_rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    match next {
                        'n' => result.push('\n'),
                        'r' => result.push('\r'),
                        't' => result.push('\t'),
                        '"' => result.push('"'),
                        '\\' => result.push('\\'),
                        _ => { result.push('\\'); result.push(next); }
                    }
                }
            }
            _ => result.push(c),
        }
    }
    Some(result)
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
                        'n' => result.push('\n'),
                        'r' => result.push('\r'),
                        't' => result.push('\t'),
                        '"' => result.push('"'),
                        '\\' => result.push('\\'),
                        '/' => result.push('/'),
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

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) { if &data[i..i + 4] == b"\r\n\r\n" { return Some(i); } }
    None
}

fn print_elapsed(ms: u64) {
    if ms < 1000 { libakuma::print(&format!("~(=^‥^)ノ [{}ms]", ms)); }
    else { libakuma::print(&format!("~(=^‥^)ノ [{}.{}s]", ms / 1000, (ms % 1000) / 100)); }
}

fn poll_sleep(ms: u64, current_tokens: usize, token_limit: usize, mem_kb: usize) {
    let end = libakuma::uptime() + ms * 1000;
    while libakuma::uptime() < end { tui_app::tui_handle_input(current_tokens, token_limit, mem_kb); libakuma::sleep_ms(10); }
}
