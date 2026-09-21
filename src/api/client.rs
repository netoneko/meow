use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::Ordering;

#[cfg(feature = "linux-net")]
use crate::linux_net::resolve;
#[cfg(not(feature = "linux-net"))]
use libakuma::net::resolve;
use libakuma::net::TcpStream;
use libakuma_tls::{HttpHeaders, HttpStreamTls, StreamResult, TLS_RECORD_SIZE, find_headers_end, parse_status_line};
use crate::util::{StackBuffer, json_escape_to, now_us};
use core::fmt::Write;
use crate::ui::tui::layout::Stdout;

use crate::config::{Provider, OPENAI_TOOLS_JSON};
use crate::tui_app;
use super::types::{StreamResponse, StreamStats, ToolCallData};

fn debug_print(msg: &str) {
    if tui_app::DEBUG_MODE.load(Ordering::SeqCst) {
        libakuma::print("[meow:debug] ");
        libakuma::print(msg);
        libakuma::print("\n");
    }
}

const MAX_RETRIES: u32 = 10;
const DEFAULT_MAX_TOKENS: usize = 16384;

/// Caller-settable cap on one response's token budget. 0 = use
/// [`DEFAULT_MAX_TOKENS`].
///
/// It existed for the **live agent**, to stop a reasoning model spending 17
/// minutes of wall clock in one request while the main thread — then the
/// hub's only serving thread — sat waiting. Single ownership removed that
/// coupling: the owner thread serves regardless of what the agent is doing,
/// so a long turn now costs only that agent's own responsiveness.
///
/// A small cap was actively harmful, because the budget is spent on
/// thinking *first*: at 2048 a reasoning model hit `finish_reason: length`
/// while still working and emitted no answer and no tool call at all. A
/// budget that cannot fit the thinking plus the answer is not a safety
/// margin, it is a guaranteed empty turn.
static MAX_TOKENS_OVERRIDE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Set the token budget for subsequent requests (0 restores the default).
pub fn set_max_tokens(n: usize) {
    MAX_TOKENS_OVERRIDE.store(n, core::sync::atomic::Ordering::Relaxed);
}

fn effective_max_tokens() -> usize {
    match MAX_TOKENS_OVERRIDE.load(core::sync::atomic::Ordering::Relaxed) {
        0 => DEFAULT_MAX_TOKENS,
        n => n,
    }
}

/// How long a streaming response may deliver NO data before it is abandoned.
/// A healthy backend emits SSE chunks continuously; silence past this point
/// is a stuck server, not a slow model. Two minutes: far above any honest
/// time-to-first-byte, far below the 17-minute wedge that motivated it.
const STREAM_STALL_TIMEOUT_US: u64 = 120 * 1_000_000;

/// Hard ceiling on one streamed response, in bytes.
///
/// The reader had **no** bound of its own on a chatty server. `read_attempts`
/// is reset to 0 by every successful read, so the `read_attempts > 6000`
/// timeout only ever catches a *silent* peer; a model that streams forever is
/// never cut off. The only real bound was the server honouring
/// [`DEFAULT_MAX_TOKENS`], and in `--no-tui`/`-c` mode there is no cancel path
/// either ([`tui_app::tui_handle_input`] returns immediately when the TUI is
/// not active, so `tui_is_cancelled` can never become true). A model that fell
/// into a repetition cycle therefore printed for minutes with no way to stop
/// it — the reported symptom.
const MAX_RESPONSE_BYTES: usize = 512 * 1024;

/// Window scanned for a repetition cycle, the needle taken from its tail, and
/// how many times that needle must occur inside it to count as degenerate.
const GUARD_WINDOW_BYTES: usize = 8192;
const GUARD_NEEDLE_BYTES: usize = 128;
const GUARD_MIN_HITS: usize = 4;
/// Bytes of growth between two repetition scans, so the scan is amortised
/// rather than run per chunk.
const GUARD_STEP_BYTES: usize = 4096;

/// Last `n` bytes of `s`, rounded forward to a `char` boundary.
fn tail(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut i = s.len() - n;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

/// True when the tail of `s` is a model stuck in a repetition cycle.
///
/// Deliberately looks for a repeated *tail*, not a repeated whole response: a
/// degenerate stream starts with real content and only later falls into a
/// 2-cycle, so anchoring on the end is what catches it while the unique prefix
/// is still intact.
fn looks_degenerate(s: &str) -> bool {
    if s.len() < GUARD_WINDOW_BYTES {
        return false;
    }
    let window = tail(s, GUARD_WINDOW_BYTES);
    let needle = tail(window, GUARD_NEEDLE_BYTES);
    if needle.len() < GUARD_NEEDLE_BYTES {
        return false;
    }
    window.matches(needle).count() >= GUARD_MIN_HITS
}

/// Amortised runaway detector for one stream. `check` returns the reason to
/// stop, or `None` to keep reading.
struct RunawayGuard {
    next_check: usize,
}

impl RunawayGuard {
    fn new() -> Self {
        RunawayGuard { next_check: GUARD_WINDOW_BYTES }
    }

    fn check(&mut self, s: &str) -> Option<&'static str> {
        if s.len() >= MAX_RESPONSE_BYTES {
            return Some("response exceeded 512KB");
        }
        if s.len() < self.next_check {
            return None;
        }
        self.next_check = s.len() + GUARD_STEP_BYTES;
        if looks_degenerate(s) {
            return Some("model stuck in a repetition loop");
        }
        None
    }
}

/// Announce that the reader cut a stream off itself, then hand back the text
/// gathered so far as a completed response — which is what stops `chat_once`
/// re-asking for it (`StreamResponse::Complete` is its only terminating arm).
fn cut_off(reason: &str, is_tui: bool) {
    if is_tui {
        tui_app::finish_streaming();
    }
    tui_app::clear_streaming_status();
    libakuma::print("\n");
    libakuma::print(crate::config::COLOR_YELLOW);
    libakuma::print("     --- Stream cut off: ");
    libakuma::print(reason);
    libakuma::print("\n");
    libakuma::print(crate::config::COLOR_RESET);
}

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
        // Scheme + host + path, NOT `base_url + path`: `path` already contains
        // the base URL's path component (`build_request_path` derives it from
        // `provider.base_path()`), so concatenating the two printed the path
        // twice — e.g.
        // `https://api.z.ai/api/coding/paas/v4/api/coding/paas/v4/chat/completions`
        // for a perfectly correct request. Display-only, but the one line whose
        // whole job is to tell you where the request went is the worst place to
        // be wrong: it reads as a URL-building bug that isn't there.
        let scheme = if provider.is_https() { "https://" } else { "http://" };
        match provider.host_port() {
            Some((h, _)) => {
                let _ = write!(buf, "POST {}{}{}", scheme, h, path);
            }
            // Keep the raw value visible rather than printing nothing: an
            // unparseable base_url is itself worth seeing here.
            None => {
                let _ = write!(buf, "POST <unparseable base_url: {}>", provider.base_url);
            }
        }
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

    // Send exactly `body_len` bytes and say so if we cannot.
    //
    // `if n <= 0 { break }` treated a read error the same as EOF, which
    // sends a short body under a correct Content-Length. The far end then
    // either parses incomplete JSON (500, `missing closing quote`) or waits
    // forever for bytes that are not coming — both observed live
    // 2026-09-21, and neither says anything about meow in meow's own logs.
    // A request we cannot finish is worth one clear error here.
    let mut buf = [0u8; 8192];
    let mut sent = 0usize;
    while sent < body_len {
        let n = libakuma::read_fd(body_fd, &mut buf);
        if n < 0 {
            return Err("Failed to read request body");
        }
        if n == 0 {
            break; // genuine EOF
        }
        let n = n as usize;
        stream.write_all(&buf[..n]).map_err(|_| "Failed to send request body")?;
        sent += n;
    }
    if sent != body_len {
        return Err("Request body ended early (truncated request)");
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
/// Where this process stages its outgoing request body.
///
/// **Per-process, by pid.** This was a single fixed path, which is fine for
/// one `meow` and catastrophic for several sharing a filesystem: the litter
/// runs four agents in one container, and each one opened the same file
/// `O_TRUNC`, wrote its body, then read it back to send. They clobbered each
/// other mid-request — agent A writes an 8 KB body, agent B truncates the
/// file and writes 5 KB, agent A then streams 5 KB under a
/// `Content-Length` of 8 KB.
///
/// Every request-level failure chased in this session was this one bug
/// (2026-09-21): `500 parse error ... missing closing quote` when the short
/// body happened to match its declared length, an indefinite hang when it
/// did not, and `Request body ended early` once the send paths were taught
/// to count bytes. It scales with agent count, which is why it looked
/// intermittent and why it worsened as the litter grew.
fn request_body_path() -> String {
    let sandbox = crate::tools::get_sandbox_root();
    let pid = libakuma::getpid();
    if sandbox == "/" {
        format!("/tmp/.meow_request.{}.json", pid)
    } else {
        format!("{}/tmp/.meow_request.{}.json", sandbox, pid)
    }
}

/// Write `s` to `fd`, accumulating the byte count. Returns false on short write.
/// Write the whole string, looping over partial writes.
///
/// `write(2)` is allowed to write fewer bytes than asked and return the
/// count; that is not an error, it is the normal contract. This used to
/// treat any short write as a hard failure and stop, which silently
/// **truncated the request body** — and because the caller derives
/// `Content-Length` from the bytes it managed to write, the result was a
/// perfectly framed HTTP request containing incomplete JSON. The server
/// answered 500 and meow retried the whole turn.
///
/// Observed live 2026-09-21 against llama-server:
/// `parse error at line 1, column 7541: invalid string: missing closing
/// quote; last read: '"ListPeer'` — `ListPeers` being the last entry in the
/// tools schema, i.e. the body stopped mid-way through the largest single
/// write in the request.
fn fd_write_str(fd: i32, s: &str, total: &mut usize) -> bool {
    let bytes = s.as_bytes();
    let mut written = 0usize;
    while written < bytes.len() {
        let n = libakuma::write_fd(fd, &bytes[written..]);
        if n < 0 {
            return false;
        }
        if n == 0 {
            // No progress and no error: nothing more can be written, and
            // looping would spin forever on a body we cannot finish.
            return false;
        }
        written += n as usize;
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
    let _ = write!(scratch, "],\"stream\":true,\"max_tokens\":{},\"tools\":", effective_max_tokens());
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
    // 0 means "no content token has arrived yet", and every read of it must
    // say so. `stream_us` is `now_us() - stream_start_us`, so computing it
    // unguarded against a zero start yields the raw CLOCK_MONOTONIC value —
    // inside a container, the VM's uptime. That is where "Duration: 68m 42s"
    // on a fifteen-minute-old agent came from (2026-09-21), and it made every
    // slow-looking turn in the litter unreadable: a response carrying only
    // `reasoning_content` never sets this, so the stats line reported the
    // host clock instead of the request. Three of the four read sites had the
    // guard; the early returns did not.
    let mut stream_start_us = 0;
    let mut pending_tool_calls: Vec<ToolCallData> = Vec::new();
    let mut guard = RunawayGuard::new();
    // Last time a chunk arrived. `WouldBlock` only means "no data yet", so a
    // wedged backend presents identically to a slow one until a deadline
    // exists — and this loop is the live agent's ONLY serving thread, so an
    // unbounded wait here also deafens the litter hub (measured 2026-09-20:
    // 17 minutes at 0 bytes, every tool call refused behind a full backlog).
    let mut last_progress_us = now_us();

    loop {
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb);
        if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
            crate::ui::tui::render::render_footer(current_tokens, token_limit, mem_kb);
        }
        if tui_app::tui_is_cancelled() { return Err("Request cancelled"); }
        match stream.read_chunk() {
            StreamResult::Data(data) => {
                last_progress_us = now_us();
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
                                if let Some(reason) = guard.check(&full_response) {
                                    cut_off(reason, is_tui);
                                    let stats = StreamStats { ttft_us, stream_us: if first_token_received { now_us() - stream_start_us } else { 0 }, total_bytes: full_response.len() };
                                    if !pending_tool_calls.is_empty() {
                                        return Ok(StreamResponse::CompleteWithTools(full_response, pending_tool_calls, stats));
                                    }
                                    return Ok(StreamResponse::Complete(full_response, stats));
                                }
                            }
                            if done {
                                if is_tui { tui_app::finish_streaming(); }
                                tui_app::clear_streaming_status();
                                let stats = StreamStats { ttft_us, stream_us: if first_token_received { now_us() - stream_start_us } else { 0 }, total_bytes: full_response.len() };
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
                if now_us().saturating_sub(last_progress_us) > STREAM_STALL_TIMEOUT_US {
                    return Err("stream stalled: no data from provider");
                }
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
                            if let Some(reason) = guard.check(&full_response) {
                                cut_off(reason, is_tui);
                                let stats = StreamStats { ttft_us, stream_us: if first_token_received { now_us() - stream_start_us } else { 0 }, total_bytes: full_response.len() };
                                if !pending_tool_calls.is_empty() {
                                    return Ok(StreamResponse::CompleteWithTools(full_response, pending_tool_calls, stats));
                                }
                                return Ok(StreamResponse::Complete(full_response, stats));
                            }
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
    // 0 means "no content token has arrived yet", and every read of it must
    // say so. `stream_us` is `now_us() - stream_start_us`, so computing it
    // unguarded against a zero start yields the raw CLOCK_MONOTONIC value —
    // inside a container, the VM's uptime. That is where "Duration: 68m 42s"
    // on a fifteen-minute-old agent came from (2026-09-21), and it made every
    // slow-looking turn in the litter unreadable: a response carrying only
    // `reasoning_content` never sets this, so the stats line reported the
    // host clock instead of the request. Three of the four read sites had the
    // guard; the early returns did not.
    let mut stream_start_us = 0;
    let mut pending_tool_calls: Vec<ToolCallData> = Vec::new();
    let mut guard = RunawayGuard::new();

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
                                if let Some(reason) = guard.check(&full_response) {
                                    cut_off(reason, is_tui);
                                    let stats = StreamStats { ttft_us, stream_us: if first_token_received { now_us() - stream_start_us } else { 0 }, total_bytes: full_response.len() };
                                    if !pending_tool_calls.is_empty() {
                                        return Ok(StreamResponse::CompleteWithTools(full_response, pending_tool_calls, stats));
                                    }
                                    return Ok(StreamResponse::Complete(full_response, stats));
                                }
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
                    if let Some(body_start) = find_headers_end(&pending_data) {
                        let header_str = core::str::from_utf8(&pending_data[..body_start]).unwrap_or("");
                        let status = parse_status_line(header_str).unwrap_or(0);
                        if status != 200 {
                            if tui_app::DEBUG_MODE.load(Ordering::SeqCst) {
                                let status_line = header_str.lines().next().unwrap_or("?");
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
                        pending_data.drain(..body_start);
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
                                if let Some(reason) = guard.check(&full_response) {
                                    cut_off(reason, is_tui);
                                    let stats = StreamStats { ttft_us, stream_us: if first_token_received { now_us() - stream_start_us } else { 0 }, total_bytes: full_response.len() };
                                    if !pending_tool_calls.is_empty() {
                                        return Ok(StreamResponse::CompleteWithTools(full_response, pending_tool_calls, stats));
                                    }
                                    return Ok(StreamResponse::Complete(full_response, stats));
                                }
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
                        let stats = StreamStats { ttft_us, stream_us: if first_token_received { now_us() - stream_start_us } else { 0 }, total_bytes: full_response.len() };
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

/// Parse one SSE line into `(content, stream_is_over)`.
///
/// **A non-null `finish_reason` ends the stream, not just `data: [DONE]`.**
/// Keying the end solely off the `[DONE]` sentinel was wrong twice over:
///
/// * Not every OpenAI-compatible server sends one. When none arrives the reader
///   falls out of its loop with `stream_completed` still false, and the tail of
///   [`read_streaming_response_with_progress`] classifies a perfectly complete
///   answer as [`StreamResponse::Partial`]. That is invisible on a tool-call
///   turn — the `!pending_tool_calls.is_empty()` arm returns first — so it bites
///   only on the **final, tool-free turn**, where `chat_once` appends
///   "[System: Your response was cut off mid-stream…]" and re-asks, reprinting
///   the finished answer up to `MAX_TOOL_ITERATIONS` times.
/// * `data: [DONE]\n\n` is **14 bytes**, which is exactly the terminating chunk
///   that `docs/archive/AKUMA_AMD64_STREAM_END_STALL.md` measured arriving 60 s
///   after the body it terminates. Ending on `finish_reason` means the answer is
///   already complete and returned by the time that chunk is late, so the stall
///   costs meow nothing even while the kernel-side defect is open.
///
/// `finish_reason` is `null` on every chunk but the last, and `string_at`
/// returns `None` for a JSON null, so this only fires on the real final chunk.
fn parse_streaming_line(line: &str) -> Option<(String, bool)> {
    let line = line.trim();
    if line == "data: [DONE]" { return Some((String::new(), true)); }
    if !line.starts_with("data:") { return Some((String::new(), false)); }
    let json = line.strip_prefix("data:")?.trim();
    if json.is_empty() || json == "[DONE]" { return Some((String::new(), json == "[DONE]")); }
    let content = extract_openai_delta_content(json).unwrap_or_default();
    let finished = crate::json::string_at(json, &["choices", "0", "finish_reason"]).is_some();
    if content.is_empty() && !finished {
        // Thinking: show it so the wait is legible, but never return it as
        // answer text — the caller accumulates what it is given.
        if let Some(think) = extract_openai_delta_reasoning(json) {
            if !think.is_empty() {
                let mut stdout = Stdout;
                let _ = write!(stdout, "{}", think);
            }
        }
    }
    Some((content, finished))
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

/// One streamed delta's *answer* text — `content` only.
///
/// Deliberately NOT `reasoning_content`. A reasoning model puts its working
/// there and leaves `content` empty until it has finished thinking, and if
/// the budget runs out first `content` is never populated at all. Both are
/// worth showing an operator (see [`extract_openai_delta_reasoning`]), but
/// only one of them is an answer, and conflating them makes a model that
/// thought for two thousand tokens and then stopped look like it replied.
///
/// That distinction is what the live agent's empty-turn retry keys on
/// (`docs/LITTER_WORKFLOW.md` § "A turn that produces nothing").
fn extract_openai_delta_content(json: &str) -> Option<String> {
    crate::json::string_at(json, &["choices", "0", "delta", "content"])
}

/// One streamed delta's *thinking*, if the server reports it separately.
///
/// Rendered so a long silence is legible as work rather than as a hang —
/// before this, a model thinking for minutes printed nothing at all and was
/// indistinguishable from a stalled connection. It is never accumulated
/// into the answer.
fn extract_openai_delta_reasoning(json: &str) -> Option<String> {
    crate::json::string_at(json, &["choices", "0", "delta", "reasoning_content"])
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
