use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::Ordering;

#[cfg(feature = "linux-net")]
use crate::linux_net::resolve;
#[cfg(not(feature = "linux-net"))]
use libakuma::net::resolve;
use libakuma::net::TcpStream;
use libakuma_tls::{HttpHeaders, HttpStreamTls, StreamResult, TLS_RECORD_SIZE};
use crate::util::{StackBuffer, json_escape_to};
use core::fmt::Write;
use crate::ui::tui::layout::Stdout;

use crate::config::{Provider, OPENAI_TOOLS_JSON};
use crate::tui_app;
use super::types::{StreamResponse, StreamStats, ToolCallData};

#[cfg(feature = "linux-net")]
fn now_us() -> u64 { crate::linux_net::uptime_us() }
#[cfg(not(feature = "linux-net"))]
fn now_us() -> u64 { libakuma::uptime() }

fn debug_print(msg: &str) {
    if tui_app::DEBUG_MODE.load(Ordering::SeqCst) {
        libakuma::print("[meow:debug] ");
        libakuma::print(msg);
        libakuma::print("\n");
    }
}

const MAX_RETRIES: u32 = 10;
const DEFAULT_MAX_TOKENS: usize = 16384;

/// Serialize the chat request body to a temp file, stream it to the provider
/// with retries, then remove the file. Serializing to disk (rather than into a
/// single growing `String`) keeps the whole conversation from ever being
/// resident in memory at send time.
pub fn send_with_retry(
    model: &str,
    provider: &Provider,
    conversation_path: &str,
    is_continuation: bool,
    current_tokens: usize,
    token_limit: usize,
    mem_kb: usize,
) -> Result<StreamResponse, &'static str> {
    let body_path = request_body_path();
    let body_len = write_chat_body(&body_path, model, conversation_path)?;
    let result = send_with_retry_inner(
        provider, &body_path, body_len,
        is_continuation, current_tokens, token_limit, mem_kb,
    );
    libakuma::unlink(&body_path);
    result
}

/// Retry/backoff loop. The request body is read fresh from `body_path` on each
/// attempt and streamed to the socket in chunks.
#[allow(clippy::too_many_arguments)]
fn send_with_retry_inner(
    provider: &Provider,
    body_path: &str,
    body_len: usize,
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

    let start_time = now_us();
    let path = build_request_path(provider);
    {
        let mut buf_data = [0u8; 256];
        let mut buf = StackBuffer::new(&mut buf_data);
        let _ = write!(buf, "POST {}{}", provider.base_url, path);
        debug_print(buf.as_str());
    }

    // TLS record buffers are ~17KB each. Allocate them once and reuse across
    // retry attempts (only for HTTPS providers) instead of per-attempt.
    let needs_tls = provider.is_https();
    let mut tls_read_buf: Vec<u8> = if needs_tls { alloc::vec![0u8; TLS_RECORD_SIZE] } else { Vec::new() };
    let mut tls_write_buf: Vec<u8> = if needs_tls { alloc::vec![0u8; TLS_RECORD_SIZE] } else { Vec::new() };

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            if !is_tui {
                let mut stdout = Stdout;
                let _ = write!(stdout, " retry {}", attempt);
            }
            let mut status_buf_data = [0u8; 64];
            let mut status_buf = StackBuffer::new(&mut status_buf_data);
            let _ = write!(status_buf, "{} retry {}", status_prefix, attempt);
            tui_app::update_streaming_status(status_buf.as_str(), 0, None);
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
                if tui_app::DEBUG_MODE.load(Ordering::SeqCst) {
                    let mut stdout = Stdout;
                    let _ = write!(stdout, "\n[meow:debug] connect error (attempt {}): {}\n", attempt, e);
                }
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui {
                        let mut stdout = Stdout;
                        let _ = write!(stdout, "] {}", e);
                    }
                    return Err("Connection failed");
                }
                continue;
            }
        };

        tui_app::update_streaming_status("[MEOW] waiting", 0, None);
        if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
            crate::ui::tui::render::render_footer(current_tokens, token_limit, mem_kb);
        }
        if !is_tui { libakuma::print("."); }

        if provider.is_https() {
            let (host, _) = provider.host_port().ok_or("Invalid URL")?;

            let mut http_stream = match HttpStreamTls::connect(stream, &host, &mut tls_read_buf, &mut tls_write_buf) {
                Ok(s) => s,
                Err(e) => {
                                    if attempt == MAX_RETRIES - 1 {
                                        if !is_tui { 
                                            let mut stdout = Stdout;
                                            let _ = write!(stdout, "] TLS error: {:?}", e); 
                                        }                        return Err("TLS handshake failed");
                    }
                    continue;
                }
            };
            
            let mut headers = HttpHeaders::new();
            headers.content_type("application/json");
            if let Some(key) = &provider.api_key {
                headers.bearer_auth(key);
            }
            
            let body_fd = libakuma::open(body_path, libakuma::open_flags::O_RDONLY);
            if body_fd < 0 {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print("] "); }
                    return Err("Failed to open request buffer");
                }
                continue;
            }
            let post_result = http_stream.post_from_fd(&host, &path, body_len, body_fd, &headers);
            libakuma::close(body_fd);
            if post_result.is_err() {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print("] "); }
                    return Err("Failed to send request");
                }
                continue;
            }
            
            if !is_tui { 
                libakuma::print("] waiting");
            }
            
            match read_streaming_with_http_stream_tls(&mut http_stream, start_time, current_tokens, token_limit, mem_kb, is_tui) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e == "Request cancelled" { return Err(e); }
                    if attempt == MAX_RETRIES - 1 { return Err(e); }
                    if !is_tui { 
                        let mut stdout = Stdout;
                        let _ = write!(stdout, " ({})", e); 
                    }
                    continue;
                }
            }
        } else {
            let body_fd = libakuma::open(body_path, libakuma::open_flags::O_RDONLY);
            if body_fd < 0 {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print("] "); }
                    return Err("Failed to open request buffer");
                }
                continue;
            }
            let post_result = send_post_request_from_fd(&stream, &path, body_len, body_fd, provider);
            libakuma::close(body_fd);
            if let Err(e) = post_result {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print("] "); }
                    return Err(e);
                }
                continue;
            }

            if !is_tui {
                libakuma::print("] waiting");
            }

            match read_streaming_response_with_progress(&stream, start_time, current_tokens, token_limit, mem_kb, is_tui) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e == "Request cancelled" { return Err(e); }
                    if attempt == MAX_RETRIES - 1 { return Err(e); }
                    if !is_tui { 
                        let mut stdout = Stdout;
                        let _ = write!(stdout, " ({})", e); 
                    }
                    continue;
                }
            }
        }
    }

    Err("Max retries exceeded")
}

fn connect_to_provider(provider: &Provider) -> Result<TcpStream, String> {
    let (host, port) = provider.host_port().ok_or_else(|| String::from("Invalid provider URL"))?;
    {
        let mut buf_data = [0u8; 128];
        let mut buf = StackBuffer::new(&mut buf_data);
        let _ = write!(buf, "resolving {}:{}", host, port);
        debug_print(buf.as_str());
    }
    let ip = resolve(&host).map_err(|_| format!("DNS resolution failed for: {}", host))?;
    let addr_str = format!("{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], port);
    {
        let mut buf_data = [0u8; 128];
        let mut buf = StackBuffer::new(&mut buf_data);
        let _ = write!(buf, "connecting to {}", addr_str);
        debug_print(buf.as_str());
    }
    TcpStream::connect(&addr_str).map_err(|_| format!("Connection failed to: {}", addr_str))
}

/// Send a plain-HTTP POST whose body is streamed from `body_fd` in chunks.
/// `body_fd` must be positioned at the start; `body_len` is its byte length.
fn send_post_request_from_fd(
    stream: &TcpStream,
    path: &str,
    body_len: usize,
    body_fd: i32,
    provider: &Provider,
) -> Result<(), &'static str> {
    let (host, port) = provider.host_port().ok_or("Invalid URL")?;
    let auth_header = match &provider.api_key {
        Some(key) => format!("Authorization: Bearer {}\r\n", key),
        None => String::new(),
    };
    let header = format!(
        "POST {} HTTP/1.0\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         {}Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        path, host, port, auth_header, body_len
    );
    stream.write_all(header.as_bytes()).map_err(|_| "Failed to send request")?;

    let mut buf = [0u8; 8192];
    loop {
        let n = libakuma::read_fd(body_fd, &mut buf);
        if n <= 0 {
            break;
        }
        stream.write_all(&buf[..n as usize]).map_err(|_| "Failed to send request body")?;
    }
    Ok(())
}

/// Compute the chat-completions URL path for a provider.
fn build_request_path(provider: &Provider) -> String {
    let base = provider.base_path();
    if base.is_empty() || base == "/" {
        String::from("/v1/chat/completions")
    } else if base.ends_with("/v1") {
        format!("{}/chat/completions", base)
    } else {
        format!("{}/chat/completions", base.trim_end_matches('/'))
    }
}

/// Path of the temp file used to stage the request body (sandbox-aware).
fn request_body_path() -> String {
    let sandbox = crate::tools::get_sandbox_root();
    if sandbox == "/" {
        String::from("/tmp/.meow_request.json")
    } else {
        format!("{}/tmp/.meow_request.json", sandbox)
    }
}

/// Write `s` to `fd`, accumulating the byte count. Returns false on short write.
fn fd_write_str(fd: i32, s: &str, total: &mut usize) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return true;
    }
    let n = libakuma::write_fd(fd, bytes);
    if n < 0 || n as usize != bytes.len() {
        return false;
    }
    *total += bytes.len();
    true
}

/// Serialize the full OpenAI chat-completions request body to `path`, reading
/// the conversation messages from the on-disk JSONL log one line at a time so
/// peak memory stays bounded by the largest single message — the whole
/// conversation is never materialized in RAM (the static tools schema is
/// streamed directly from the const, never copied). Returns body bytes written.
fn write_chat_body(path: &str, model: &str, conversation_path: &str) -> Result<usize, &'static str> {
    let sandbox = crate::tools::get_sandbox_root();
    let tmp_dir = if sandbox == "/" { String::from("/tmp") } else { format!("{}/tmp", sandbox) };
    let _ = libakuma::mkdir(&tmp_dir);

    let fd = libakuma::open(
        path,
        libakuma::open_flags::O_WRONLY | libakuma::open_flags::O_CREAT | libakuma::open_flags::O_TRUNC,
    );
    if fd < 0 {
        return Err("Failed to create request buffer");
    }

    let mut total = 0usize;
    let ok = write_chat_body_inner(fd, model, conversation_path, &mut total);
    libakuma::close(fd);

    if ok { Ok(total) } else { Err("Failed to write request buffer") }
}

fn write_chat_body_inner(
    fd: i32,
    model: &str,
    conversation_path: &str,
    total: &mut usize,
) -> bool {
    let mut scratch = String::new();
    scratch.push_str("{\"model\":\"");
    json_escape_to(model, &mut scratch);
    scratch.push_str("\",\"messages\":[");
    if !fd_write_str(fd, &scratch, total) {
        return false;
    }

    if !stream_conversation_messages(fd, conversation_path, total) {
        return false;
    }

    scratch.clear();
    let _ = write!(scratch, "],\"stream\":true,\"max_tokens\":{},\"tools\":", DEFAULT_MAX_TOKENS);
    if !fd_write_str(fd, &scratch, total) {
        return false;
    }
    // Stream the large, static tools schema straight from the const.
    if !fd_write_str(fd, OPENAI_TOOLS_JSON, total) {
        return false;
    }
    fd_write_str(fd, ",\"tool_choice\":\"auto\"}", total)
}

/// Stream the JSONL conversation log into the request body as the contents of
/// the `messages` array: each complete line is one message object, emitted
/// comma-separated. Reads in fixed chunks, splitting on '\n' so RAM is bounded
/// by the longest single message. A trailing partial line (no newline — e.g. a
/// torn append) is dropped. A missing/empty log yields an empty array.
fn stream_conversation_messages(fd: i32, conversation_path: &str, total: &mut usize) -> bool {
    let cfd = libakuma::open(conversation_path, libakuma::open_flags::O_RDONLY);
    if cfd < 0 {
        return true; // no conversation yet -> empty messages array
    }
    let mut buf = [0u8; 4096];
    let mut carry: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let n = libakuma::read_fd(cfd, &mut buf);
        if n <= 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n as usize]);
        while let Some(nl) = carry.iter().position(|&b| b == b'\n') {
            {
                let line = core::str::from_utf8(&carry[..nl]).unwrap_or("").trim();
                if !line.is_empty() {
                    if !first && !fd_write_str(fd, ",", total) {
                        libakuma::close(cfd);
                        return false;
                    }
                    first = false;
                    if !fd_write_str(fd, line, total) {
                        libakuma::close(cfd);
                        return false;
                    }
                }
            }
            carry.drain(..nl + 1);
        }
    }
    libakuma::close(cfd);
    true
}

fn read_streaming_with_http_stream_tls(
    stream: &mut HttpStreamTls<'_>,
    start_time: u64,
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
    let mut pending_tool_calls: Vec<ToolCallData> = Vec::new();

    loop {
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb);
        if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
            crate::ui::tui::render::render_footer(current_tokens, token_limit, mem_kb);
        }
        if tui_app::tui_is_cancelled() { return Err("Request cancelled"); }
        match stream.read_chunk() {
            StreamResult::Data(data) => {
                if let Ok(s) = core::str::from_utf8(&data) { pending_lines.push_str(s); }
                while let Some(newline_pos) = pending_lines.find('\n') {
                    let line = &pending_lines[..newline_pos];
                    if !line.is_empty() {
                        accumulate_tool_call_delta(line, &mut pending_tool_calls);
                        if let Some((content, done)) = parse_streaming_line(line) {
                            if !content.is_empty() {
                                if !first_token_received {
                                    first_token_received = true;
                                    let now = now_us();
                                    ttft_us = now - start_time;
                                    stream_start_us = now;
                                    tui_app::update_streaming_status("[MEOW] streaming", 0, None);
                                    if !is_tui {
                                        libakuma::print(" ");
                                        print_elapsed(ttft_us / 1000);
                                        libakuma::print("\n");
                                    } else {
                                        tui_app::start_streaming(9);
                                    }
                                }
                                if is_tui {
                                    tui_app::process_streaming_chunk(&content);
                                } else {
                                    tui_app::tui_print_assistant(&content);
                                }
                                full_response.push_str(&content);
                            }
                            if done {
                                if is_tui { tui_app::finish_streaming(); }
                                tui_app::clear_streaming_status();
                                let stats = StreamStats { ttft_us, stream_us: now_us() - stream_start_us, total_bytes: full_response.len() };
                                if !pending_tool_calls.is_empty() {
                                    return Ok(StreamResponse::CompleteWithTools(full_response, pending_tool_calls, stats));
                                }
                                return Ok(StreamResponse::Complete(full_response, stats));
                            }
                        }
                    }
                    pending_lines.drain(..newline_pos + 1);
                }
            }
            StreamResult::WouldBlock => {
                if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
                    crate::ui::tui::render::render_footer(current_tokens, token_limit, mem_kb);
                }
                libakuma::sleep_ms(1);
            }
            StreamResult::Done => {
                let remaining = String::from(pending_lines.trim());
                if !remaining.is_empty() {
                    accumulate_tool_call_delta(&remaining, &mut pending_tool_calls);
                    if let Some((content, done)) = parse_streaming_line(&remaining) {
                        if !content.is_empty() {
                            if !first_token_received {
                                first_token_received = true;
                                let now = now_us();
                                ttft_us = now - start_time;
                                stream_start_us = now;
                                tui_app::update_streaming_status("[MEOW] streaming", 0, None);
                                if !is_tui {
                                    libakuma::print(" ");
                                    print_elapsed(ttft_us / 1000);
                                    libakuma::print("\n");
                                } else {
                                    tui_app::start_streaming(9);
                                }
                            }
                            if is_tui {
                                tui_app::process_streaming_chunk(&content);
                            } else {
                                tui_app::tui_print(&content);
                            }
                            full_response.push_str(&content);
                        }
                        if done {
                            if is_tui { tui_app::finish_streaming(); }
                            stream_completed = true;
                            tui_app::clear_streaming_status();
                        }
                    }
                }
                break;
            }
            StreamResult::Error(e) => {
                if is_tui { tui_app::finish_streaming(); }
                if tui_app::DEBUG_MODE.load(Ordering::SeqCst) {
                    let mut stdout = Stdout;
                    let _ = write!(stdout, "\n[meow:debug] stream error: {:?}\n", e);
                }
                return Err("Server returned error");
            }
        }
    }
    let stats = StreamStats { ttft_us, stream_us: if first_token_received { now_us() - stream_start_us } else { 0 }, total_bytes: full_response.len() };
    if !pending_tool_calls.is_empty() {
        if is_tui { tui_app::finish_streaming(); }
        tui_app::clear_streaming_status();
        return Ok(StreamResponse::CompleteWithTools(full_response, pending_tool_calls, stats));
    }
    if !stream_completed && !full_response.is_empty() {
        return Ok(StreamResponse::Partial(full_response, stats));
    }
    Ok(StreamResponse::Complete(full_response, stats))
}

fn read_streaming_response_with_progress(
    stream: &TcpStream,
    start_time: u64,
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
    let mut pending_tool_calls: Vec<ToolCallData> = Vec::new();

    loop {
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb);
        if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
            crate::ui::tui::render::render_footer(current_tokens, token_limit, mem_kb);
        }
        if tui_app::tui_is_cancelled() { return Err("Request cancelled"); }
        match stream.read(&mut buf) {
            Ok(0) => {
                if !any_data_received { return Err("Connection closed by server"); }
                if let Ok(remaining_str) = core::str::from_utf8(&pending_data) {
                    for line in remaining_str.trim().lines() {
                        accumulate_tool_call_delta(line, &mut pending_tool_calls);
                        if let Some((content, done)) = parse_streaming_line(line) {
                            if !content.is_empty() {
                                if !first_token_received {
                                    first_token_received = true;
                                    let now = now_us();
                                    ttft_us = now - start_time;
                                    stream_start_us = now;
                                    tui_app::update_streaming_status("[MEOW] streaming", 0, None);
                                    if !is_tui {
                                        for _ in 0..(7 + dots_printed) { libakuma::print("\x08 \x08"); }
                                        print_elapsed(ttft_us / 1000);
                                        libakuma::print("\n");
                                    } else {
                                        tui_app::start_streaming(9);
                                    }
                                }
                                if is_tui {
                                    tui_app::process_streaming_chunk(&content);
                                } else {
                                    tui_app::tui_print_assistant(&content);
                                }
                                full_response.push_str(&content);
                            }
                            if done {
                                if is_tui { tui_app::finish_streaming(); }
                                stream_completed = true;
                                tui_app::clear_streaming_status();
                            }
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
                        if !header_str.contains(" 200 ") {
                            if tui_app::DEBUG_MODE.load(Ordering::SeqCst) {
                                let status_line = header_str.lines().next().unwrap_or("?");
                                let body_start = pos + 4;
                                let body_preview_end = (body_start + 512).min(pending_data.len());
                                let body_snippet = core::str::from_utf8(&pending_data[body_start..body_preview_end]).unwrap_or("(non-utf8)");
                                let mut stdout = Stdout;
                                let _ = write!(stdout, "\n[meow:debug] server error: {}\n[meow:debug] body: {}\n", status_line, body_snippet);
                            }
                            return Err("Server returned error");
                        }
                        if tui_app::DEBUG_MODE.load(Ordering::SeqCst) {
                            let status_line = header_str.lines().next().unwrap_or("?");
                            let mut stdout = Stdout;
                            let _ = write!(stdout, "\n[meow:debug] response: {}\n", status_line);
                        }
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
                        accumulate_tool_call_delta(line, &mut pending_tool_calls);
                        if let Some((content, done)) = parse_streaming_line(line) {
                            if !content.is_empty() {
                                if !first_token_received {
                                    first_token_received = true;
                                    let now = now_us();
                                    ttft_us = now - start_time;
                                    stream_start_us = now;
                                    tui_app::update_streaming_status("[MEOW] streaming", 0, None);
                                    if !is_tui {
                                        for _ in 0..(7 + dots_printed) { libakuma::print("\x08 \x08"); }
                                        print_elapsed(ttft_us / 1000);
                                        libakuma::print("\n");
                                    } else {
                                        tui_app::start_streaming(9);
                                    }
                                }
                                if is_tui {
                                    tui_app::process_streaming_chunk(&content);
                                } else {
                                    tui_app::tui_print_assistant(&content);
                                }
                                full_response.push_str(&content);
                            }
                            if done {
                                if is_tui { tui_app::finish_streaming(); }
                                is_done = true;
                                tui_app::clear_streaming_status();
                                break;
                            }
                        }
                    }
                    if let Some(pos) = last_newline { pending_data.drain(..pos + 1); }
                    if is_done {
                        let stats = StreamStats { ttft_us, stream_us: now_us() - stream_start_us, total_bytes: full_response.len() };
                        if !pending_tool_calls.is_empty() {
                            return Ok(StreamResponse::CompleteWithTools(full_response, pending_tool_calls, stats));
                        }
                        return Ok(StreamResponse::Complete(full_response, stats));
                    }
                }
            }
            Err(e) => {
                if e.kind == libakuma::net::ErrorKind::WouldBlock || e.kind == libakuma::net::ErrorKind::TimedOut {
                    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
                        crate::ui::tui::render::render_footer(current_tokens, token_limit, mem_kb);
                    }
                    read_attempts += 1;
                    if read_attempts.is_multiple_of(50) && !first_token_received && !is_tui { libakuma::print("."); dots_printed += 1; }
                    if read_attempts > 6000 { return Err("Timeout waiting for response"); }
                    libakuma::sleep_ms(1);
                    continue;
                }
                return Err("Network error");
            }
        }
    }
    let stats = StreamStats { ttft_us, stream_us: if first_token_received { now_us() - stream_start_us } else { 0 }, total_bytes: full_response.len() };
    if !pending_tool_calls.is_empty() {
        return Ok(StreamResponse::CompleteWithTools(full_response, pending_tool_calls, stats));
    }
    if !stream_completed && !full_response.is_empty() {
        return Ok(StreamResponse::Partial(full_response, stats));
    }
    Ok(StreamResponse::Complete(full_response, stats))
}

fn parse_streaming_line(line: &str) -> Option<(String, bool)> {
    let line = line.trim();
    if line == "data: [DONE]" { return Some((String::new(), true)); }
    if !line.starts_with("data:") { return Some((String::new(), false)); }
    let json = line.strip_prefix("data:")?.trim();
    if json.is_empty() || json == "[DONE]" { return Some((String::new(), json == "[DONE]")); }
    Some((extract_openai_delta_content(json).unwrap_or_default(), false))
}

/// Accumulate a tool_call delta from an OpenAI SSE line into the pending list.
/// Returns true if the stream signals finish_reason "tool_calls".
///
/// Each `data:` line is one complete JSON chunk, so the whole line is walked
/// as a document; a string is only treated as an `id`/`name`/`arguments`
/// fragment if `tool_calls` appears somewhere above it in the path — this is
/// what excludes the chunk's own top-level `"id"` (every OpenAI-compatible
/// chunk has one) from being mistaken for a tool call id. Whether a provider
/// nests `name`/`arguments` under `tool_calls[].function` (OpenAI) or not is
/// deliberately not encoded in the path match, for the same compatibility
/// reason `json_value_start` used to tolerate spacing differences.
///
/// The three fields are collected independently and only applied to `pending`
/// *after* the walk finishes, rather than in visit order: at least one
/// OpenAI-compatible server (mlx-server) emits `function` (name + arguments)
/// *before* `id` in the same object, and a single left-to-right pass that
/// pushes a new `ToolCallData` on `id` would silently drop `name`/`arguments`
/// seen before that push had happened.
fn accumulate_tool_call_delta(line: &str, pending: &mut Vec<ToolCallData>) -> bool {
    let line = line.trim();
    if !line.starts_with("data:") { return false; }
    let json = match line.strip_prefix("data:") { Some(j) => j.trim(), None => return false };
    if json.is_empty() || json == "[DONE]" { return false; }

    let is_finish = crate::json::string_at(json, &["choices", "0", "finish_reason"]).as_deref() == Some("tool_calls");

    let mut id = None;
    let mut name = None;
    let mut arguments = None;
    let _ = crate::json::walk(json, |path, value| {
        let crate::json::Value::Str(s) = value else { return };
        let segs = path.segments();
        if !segs.iter().any(|seg| matches!(seg, crate::json::Seg::Key(k) if k == "tool_calls")) {
            return;
        }
        match segs.last() {
            Some(crate::json::Seg::Key(k)) if k == "id" && id.is_none() => id = Some(String::from(s)),
            Some(crate::json::Seg::Key(k)) if k == "name" && name.is_none() => name = Some(String::from(s)),
            Some(crate::json::Seg::Key(k)) if k == "arguments" && arguments.is_none() => arguments = Some(String::from(s)),
            _ => {}
        }
    });

    if let Some(id) = id {
        if !id.is_empty() {
            pending.push(ToolCallData { id, name: String::new(), arguments: String::new() });
        }
    }
    if let Some(name) = name {
        if !name.is_empty() {
            if let Some(last) = pending.last_mut() { last.name = name; }
        }
    }
    if let Some(arguments) = arguments {
        if let Some(last) = pending.last_mut() { last.arguments.push_str(&arguments); }
    }

    is_finish
}

fn extract_openai_delta_content(json: &str) -> Option<String> {
    crate::json::string_at(json, &["choices", "0", "delta", "content"])
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) { if &data[i..i + 4] == b"\r\n\r\n" { return Some(i); } }
    None
}

fn print_elapsed(ms: u64) {
    let mut buf_data = [0u8; 32];
    let mut buf = StackBuffer::new(&mut buf_data);
    let mut stdout = Stdout;
    if ms < 1000 {
        let _ = write!(buf, "[{}ms]", ms);
        let _ = write!(stdout, "{}", buf.as_str());
    } else {
        let _ = write!(buf, "[{}.{}s]", ms / 1000, (ms % 1000) / 100);
        let _ = write!(stdout, "{}", buf.as_str());
    }
}

fn poll_sleep(ms: u64, current_tokens: usize, token_limit: usize, mem_kb: usize) {
    let end = now_us() + ms * 1000;
    while now_us() < end { 
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb); 
        if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
            crate::ui::tui::render::render_footer(current_tokens, token_limit, mem_kb);
        }
        libakuma::sleep_ms(10); 
    }
}
