//! Meow-chan - Cyberpunk Neko AI Assistant
//!
//! A cute cybernetically-enhanced catgirl AI that connects to Ollama LLMs.
//! Default model: gemma3:27b with a custom cyber-neko persona.
//!
//! Usage:
//!   meow                    # Interactive mode with Meow-chan
//!   meow init               # Configure providers and models
//!   meow -m llama3.2        # Use different neural link
//!   meow --provider NAME    # Use specific provider
//!   meow "quick question"   # One-shot query
//!
//! Commands:
//!   /clear    - Wipe memory banks nya~
//!   /model    - Check/switch/list neural links
//!   /provider - Check/switch providers
//!   /tokens   - Show token usage
//!   /quit     - Jack out of the matrix

#![no_std]
#![no_main]

extern crate alloc;

mod code_search;
mod config;
mod providers;
mod tools;
mod tui_app;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use config::{ApiType, Config, Provider, TOKEN_LIMIT_FOR_COMPACTION, DEFAULT_CONTEXT_WINDOW, SYSTEM_PROMPT_BASE, COLOR_PEARL, COLOR_GREEN_LIGHT, COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_GRAY_DIM, COLOR_MEOW, COLOR_YELLOW};
use libakuma::net::{resolve, TcpStream};
use libakuma::{arg, argc, exit};
use libakuma_tls::{HttpHeaders, HttpStreamTls, StreamResult, TLS_RECORD_SIZE};
use core::sync::atomic::Ordering;

/// Print a string to stdout, with TUI-aware wrapping if active
fn print(s: &str) {
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        tui_app::tui_print(s);
    } else {
        libakuma::print(s);
    }
}

/// Print metadata with 9 spaces of indent (matching LLM response start)
fn print_metadata(content: &str, color: &str) {
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        tui_app::tui_print_with_indent(content, "     --- ", 9, Some(color));
    } else {
        libakuma::print(color);
        libakuma::print("     --- ");
        libakuma::print(content);
        libakuma::print(COLOR_RESET);
    }
}

// Chainlink issue tracker tools section (appended when chainlink is available)
const CHAINLINK_TOOLS_SECTION: &str = r#"
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

/// Build system prompt, including chainlink tools if available
fn build_system_prompt() -> String {
    let mut prompt = String::from(SYSTEM_PROMPT_BASE);
    
    if tools::chainlink_available() {
        prompt.push_str(CHAINLINK_TOOLS_SECTION);
    }
    
    prompt
}

/// Estimate token count for a string (rough approximation: ~4 chars per token)
fn estimate_tokens(text: &str) -> usize {
    // Rough approximation: average of 4 characters per token for English text
    (text.len() + 3) / 4
}

/// Calculate total tokens in message history
fn calculate_history_tokens(history: &[Message]) -> usize {
    history
        .iter()
        .map(|msg| estimate_tokens(&msg.content) + estimate_tokens(&msg.role) + 4) // +4 for JSON overhead
        .sum()
}

// ============================================================================
// Entry Point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let code = main();
    exit(code);
}

fn main() -> i32 {
    // Load config from /etc/meow/config
    let mut app_config = Config::load();

    let mut model_override: Option<String> = None;
    let mut provider_override: Option<String> = None;
    let mut one_shot_message: Option<String> = None;
    let mut use_tui = true; // Default to TUI mode

    // Parse command line arguments
    let mut i = 1;

    // Check for init subcommand first
    if argc() > 1 {
        if let Some(first_arg) = arg(1) {
            if first_arg == "init" {
                return run_init(&mut app_config);
            }
        }
    }

    while i < argc() {
        if let Some(arg_str) = arg(i) {
            if arg_str == "-m" || arg_str == "--model" {
                i += 1;
                if let Some(m) = arg(i) {
                    model_override = Some(String::from(m));
                } else {
                    print("meow: -m requires a model name\n");
                    return 1;
                }
            } else if arg_str == "-p" || arg_str == "--provider" {
                i += 1;
                if let Some(p) = arg(i) {
                    provider_override = Some(String::from(p));
                } else {
                    print("meow: --provider requires a provider name\n");
                    return 1;
                }
            } else if arg_str == "--tui" {
                use_tui = true;
            } else if arg_str == "-h" || arg_str == "--help" {
                print_usage();
                return 0;
            } else if !arg_str.starts_with('-') {
                one_shot_message = Some(String::from(arg_str));
                use_tui = false; // Disable TUI for one-shot questions
            }
        }
        i += 1;
    }

    // Apply provider override
    if let Some(ref prov_name) = provider_override {
        if app_config.get_provider(prov_name).is_some() {
            app_config.current_provider = prov_name.clone();
        } else {
            print("meow: unknown provider '");
            print(prov_name);
            print("'. Run 'meow init' to configure.\n");
            return 1;
        }
    }

    // Apply model override
    if let Some(ref m) = model_override {
        app_config.current_model = m.clone();
    }

    // Get current provider config (fallback to defaults if none configured)
    let current_provider = app_config
        .get_current_provider()
        .cloned()
        .unwrap_or_else(Provider::ollama_default);

    let model = app_config.current_model.clone();

    // Build system prompt once (includes chainlink if available)
    let system_prompt = build_system_prompt();

    if use_tui || one_shot_message.is_none() {
        // Initialize chat history with system prompt
        let mut history: Vec<Message> = Vec::new();
        history.push(Message::new("system", &system_prompt));
        
        // Add initial context with current working directory
        let initial_cwd = tools::get_working_dir();
        let sandbox_root = tools::get_sandbox_root();
        let cwd_context = if sandbox_root == "/" {
            format!(
                "[System Context] Your current working directory is: {}\nNo sandbox restrictions - you can access any path.",
                initial_cwd
            )
        } else {
            format!(
                "[System Context] Your current working directory is: {}\nSandbox root: {} (you cannot access paths outside this directory)\nUse relative paths like 'docs/' instead of absolute paths like '/docs/'.",
                initial_cwd, sandbox_root
            )
        };
        history.push(Message::new("user", &cwd_context));
        history.push(Message::new("assistant", 
            "Understood nya~! I'll use relative paths for file operations within the current directory. Ready to help! (=^・ω・^=)"
        ));

        // Query model info for context window size
        let context_window = match providers::query_model_info(&model, &current_provider) {
            Some(ctx) => ctx,
            None => DEFAULT_CONTEXT_WINDOW,
        };

        let mut current_model = model;
        let mut current_provider = current_provider;

        if let Err(e) = tui_app::run_tui(&mut current_model, &mut current_provider, &mut app_config, &mut history, context_window, &system_prompt) {
            print("TUI Error: ");
            print(e);
            print("\n");
            return 1;
        }
        return 0;
    }

    // One-shot mode
    if let Some(msg) = one_shot_message {
        let mut history = Vec::new();
        history.push(Message::new("system", &system_prompt));
        
        // Add cwd context for one-shot mode too
        let initial_cwd = tools::get_working_dir();
        let sandbox_root = tools::get_sandbox_root();
        let cwd_context = if sandbox_root == "/" {
            format!(
                "[System Context] Current working directory: {}\nNo sandbox restrictions.",
                initial_cwd
            )
        } else {
            format!(
                "[System Context] Current working directory: {}\nSandbox root: {} - use relative paths.",
                initial_cwd, sandbox_root
            )
        };
        history.push(Message::new("user", &cwd_context));
        history.push(Message::new("assistant", "Understood nya~!"));
        
        return match chat_once(&model, &current_provider, &msg, &mut history, None, &system_prompt) {
            Ok(_) => {
                print("\n");
                0
            }
            Err(e) => {
                print("～ Nyaa~! ");
                print(e);
                print(" (=ＴェＴ=) ～\n");
                1
            }
        };
    }

    0
}

fn print_usage() {
    print("  /\\_/\\\n");
    print(" ( o.o )  ～ MEOW-CHAN PROTOCOL ～\n");
    print("  > ^ <   Cyberpunk Neko AI Assistant\n\n");
    print("Usage: meow [OPTIONS] [MESSAGE]\n");
    print("       meow init              # Configure providers\n\n");
    print("Options:\n");
    print("  -m, --model <NAME>      Neural link override\n");
    print("  -p, --provider <NAME>   Use specific provider\n");
    print("  --tui                   Interactive TUI (default)\n");
    print("  -h, --help              Display this transmission\n\n");
    print("Interactive Commands:\n");
    print("  /clear              Wipe memory banks nya~\n");
    print("  /model [NAME]       Check/switch/list neural links\n");
    print("  /provider [NAME]    Check/switch providers\n");
    print("  /tokens             Show current token usage\n");
    print("  /help               Command protocol\n");
    print("  /quit               Jack out\n\n");
    print("Examples:\n");
    print("  meow                          # Interactive mode\n");
    print("  meow init                     # Configure providers\n");
    print("  meow \"explain rust\"           # Quick question\n");
    print("  meow -p openai -m gpt-4o      # Use OpenAI\n");
}

// ============================================================================
// Init Command
// ============================================================================

fn run_init(config: &mut Config) -> i32 {
    print("\n");
    print("  /\\_/\\  ╔══════════════════════════════════════╗\n");
    print(" ( o.o ) ║  M E O W - C H A N   I N I T         ║\n");
    print("  > ^ <  ║  ～ Provider Configuration ～        ║\n");
    print(" /|   |\\ ╚══════════════════════════════════════╝\n");
    print("\n");

    // List current providers
    print("～ Current providers: ～\n");
    if config.providers.is_empty() {
        print("  (none configured)\n");
    } else {
        for p in &config.providers {
            let current = if p.name == config.current_provider {
                " (current)"
            } else {
                ""
            };
            let api_type = match p.api_type {
                ApiType::Ollama => "Ollama",
                ApiType::OpenAI => "OpenAI",
            };
            print(&format!(
                "  - {} [{}]: {}{}\n",
                p.name, api_type, p.base_url, current
            ));
        }
    }
    print("\n  Current model: ");
    print(&config.current_model);
    print("\n  Config file: /etc/meow/config\n\n");

    print("～ To add a provider, edit /etc/meow/config manually ～\n");
    print("   Format:\n");
    print("   [provider:name]\n");
    print("   base_url=http://host:port\n");
    print("   api_type=ollama|openai\n");
    print("   api_key=your-key-here (optional)\n\n");

    0
}

// ============================================================================
// Command Handling
// ============================================================================

pub enum CommandResult {
    Continue,
    Quit,
}

pub fn handle_command(
    cmd: &str,
    model: &mut String,
    provider: &mut Provider,
    config: &mut Config,
    history: &mut Vec<Message>,
    system_prompt: &str,
) -> (CommandResult, Option<String>) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let arg = parts.get(1).map(|s| s.trim());

    match command {
        "/quit" | "/exit" | "/q" => {
            (CommandResult::Quit, Some(String::from("～ Meow-chan is jacking out... Stay preem, choom! ฅ^•ﻌ•^ฅ ～")))
        }
        "/clear" | "/reset" => {
            history.clear();
            history.push(Message::new("system", system_prompt));
            (CommandResult::Continue, Some(String::from("～ *swishes tail* Memory wiped nya~! Fresh start! (=^・ω・^=)")))
        }
        "/model" => {
            match arg {
                Some("?") | Some("list") => {
                    let mut output = String::from("～ Available neural links: ～\n");
                    match providers::list_models(provider) {
                        Ok(models) => {
                            if models.is_empty() {
                                (CommandResult::Continue, Some(String::from("～ No models found nya...")))
                            } else {
                                for (i, m) in models.iter().enumerate() {
                                    let current_marker = if m.name == *model { " (current)" } else { "" };
                                    let size_info = m
                                        .parameter_size
                                        .as_ref()
                                        .map(|s| format!(" [{}]", s))
                                        .unwrap_or_default();
                                    output.push_str(&format!(
                                        "  {}. {}{}{}\n",
                                        i + 1,
                                        m.name,
                                        size_info,
                                        current_marker
                                    ));
                                }
                                (CommandResult::Continue, Some(output))
                            }
                        }
                        Err(e) => {
                            (CommandResult::Continue, Some(format!("～ Failed to fetch models: {:?}", e)))
                        }
                    }
                }
                Some(new_model) => {
                    *model = String::from(new_model);
                    config.current_model = String::from(new_model);
                    let _ = config.save();
                    tui_app::set_model_and_provider(&model, &provider.name);
                    (CommandResult::Continue, Some(format!("～ *ears twitch* Neural link reconfigured to: {} nya~!", new_model)))
                }
                None => {
                    (CommandResult::Continue, Some(format!("～ Current neural link: {}\n  Tip: Use '/model list' to see available models nya~!", model)))
                }
            }
        }
        "/provider" => {
            match arg {
                Some("?") | Some("list") => {
                    let mut output = String::from("～ Configured providers: ～\n");
                    for (i, p) in config.providers.iter().enumerate() {
                        let current_marker = if p.name == provider.name { " (current)" } else { "" };
                        let api_type = match p.api_type {
                            ApiType::Ollama => "Ollama",
                            ApiType::OpenAI => "OpenAI",
                        };
                        output.push_str(&format!(
                            "  {}. {} ({}) [{}]{}\n",
                            i + 1,
                            p.name,
                            p.base_url,
                            api_type,
                            current_marker
                        ));
                    }
                    (CommandResult::Continue, Some(output))
                }
                Some(prov_name) => {
                    if let Some(p) = config.get_provider(prov_name) {
                        *provider = p.clone();
                        config.current_provider = String::from(prov_name);
                        let _ = config.save();
                        tui_app::set_model_and_provider(&model, &provider.name);
                        (CommandResult::Continue, Some(format!("～ *ears twitch* Switched to provider: {} nya~!", prov_name)))
                    } else {
                        (CommandResult::Continue, Some(format!("～ Unknown provider: {} ...Run 'meow init' to add it nya~", prov_name)))
                    }
                }
                None => {
                    (CommandResult::Continue, Some(format!("～ Current provider: {} ({})\n  Tip: Use '/provider list' to see configured providers nya~!", provider.name, provider.base_url)))
                }
            }
        }
        "/tokens" => {
            let current = calculate_history_tokens(history);
            (CommandResult::Continue, Some(format!(
                "～ Current token usage: {} / {} \n  Tip: Ask Meow to 'compact the context' when tokens are high nya~!",
                current, TOKEN_LIMIT_FOR_COMPACTION
            )))
        }
        "/hotkeys" | "/shortcuts" => {
            let output = String::from("┌────────────────────────────────────────────────┐\n\
                                       │             Meow's Input Shortcuts             │\n\
                                       ├────────────────────────────────────────────────┤\n\
                                       │ Shift+Enter      - Insert newline (multiline)  │\n\
                                       │ Ctrl+J           - Insert newline (fallback)   │\n\
                                       │ Ctrl+A / Home    - Move to start of line       │\n\
                                       │ Ctrl+E / End     - Move to end of line         │\n\
                                       │ Ctrl+W           - Delete previous word        │\n\
                                       │ Ctrl+U           - Clear entire input line     │\n\
                                       │ Alt+B / Opt+Left - Move back one word          │\n\
                                       │ Alt+F / Opt+Right- Move forward one word       │\n\
                                       │ Arrows           - Navigate history and line   │\n\
                                       │ ESC / Ctrl+C     - Cancel current AI request   │\n\
                                       ├────────────────────────────────────────────────┤\n\
                                       │ Note: Some terminals intercept Ctrl+W/U/C.     │\n\
                                       │ Try: iTerm2 Prefs > Keys > Left Option = Esc+  │\n\
                                       │ Or disable system shortcuts for these keys.    │\n\
                                       └────────────────────────────────────────────────┘\n");
            (CommandResult::Continue, Some(output))
        }
        "/help" | "/?" => {
            let output = String::from("┌────────────────────────────────────────────────┐\n\
                                       │             Meow's Command Protocol            │\n\
                                       ├────────────────────────────────────────────────┤\n\
                                       │ /clear        - Wipe memory banks nya~         │\n\
                                       │ /model [NAME] - Check/switch neural link       │\n\
                                       │ /model list   - List available models          │\n\
                                       │ /provider     - Check/switch provider          │\n\
                                       │ /provider list- List configured providers      │\n\
                                       │ /tokens       - Show current token usage       │\n\
                                       │ /hotkeys      - Show input shortcuts           │\n\
                                       │ /quit         - Jack out of the matrix         │\n\
                                       │ /help         - This help screen               │\n\
                                       ├────────────────────────────────────────────────┤\n\
                                       │ Context compaction: When token count is high,  │\n\
                                       │ ask Meow to compact the context to free up     │\n\
                                       │ memory nya~!                                   │\n\
                                       └────────────────────────────────────────────────┘\n");
            (CommandResult::Continue, Some(output))
        }
        "/rawtest" | "/keytest" => {
            // Test mode to show raw key bytes for 10 seconds
            let output = String::from("Raw key test mode for 10 seconds. Press keys to see their byte codes:\n");
            libakuma::print(&output);
            
            let start = libakuma::uptime();
            let duration_us = 10_000_000u64; // 10 seconds
            let mut buf = [0u8; 16];
            
            while libakuma::uptime() - start < duration_us {
                let n = libakuma::poll_input_event(100, &mut buf);
                if n > 0 {
                    let mut hex = String::from("  Bytes: ");
                    for i in 0..(n as usize) {
                        hex.push_str(&format!("{:02X} ", buf[i]));
                    }
                    hex.push('\n');
                    libakuma::print(&hex);
                }
            }
            
            (CommandResult::Continue, Some(String::from("Raw key test complete.")))
        }
        _ => {
            (CommandResult::Continue, Some(format!("～ Nyaa? Unknown command: {} ...Meow-chan is confused (=｀ω´=)", command)))
        }
    }
}

// ============================================================================
// Chat Message Types
// ============================================================================

#[derive(Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: String::from(role),
            content: String::from(content),
        }
    }

    pub fn to_json(&self) -> String {
        let escaped_content = json_escape(&self.content);
        format!(
            "{{\"role\":\"{}\",\"content\":\"{}\"}}",
            self.role, escaped_content
        )
    }
}

// ============================================================================
// Ollama API Communication
// ============================================================================

// Maximum number of messages to keep in history (including system prompt)
// Keep it small to avoid memory issues - system prompt + ~4 exchanges
pub const MAX_HISTORY_SIZE: usize = 10;

/// Trim history to prevent memory overflow
/// Keeps the system prompt (first message) and recent messages
pub fn trim_history(history: &mut Vec<Message>) {
    if history.len() > MAX_HISTORY_SIZE {
        // Keep first message (system prompt) and last (MAX_HISTORY_SIZE - 1) messages
        let to_remove = history.len() - MAX_HISTORY_SIZE;
        history.drain(1..1 + to_remove);
    }
}

/// Compact all strings in history to release excess memory
pub fn compact_history(history: &mut Vec<Message>) {
    for msg in history.iter_mut() {
        msg.role.shrink_to_fit();
        msg.content.shrink_to_fit();
    }
    history.shrink_to_fit();
}

const MAX_RETRIES: u32 = 10;

pub struct StreamStats {
    pub ttft_us: u64,
    pub stream_us: u64,
    pub total_bytes: usize,
}

pub enum StreamResponse {
    /// Response completed normally (server sent done signal)
    Complete(String, StreamStats),
    /// Response was interrupted mid-stream (connection closed before done signal)
    Partial(String, StreamStats),
}

/// Format microseconds into human-readable duration (min, sec, ms)
fn format_duration(us: u64) -> String {
    let ms = us / 1000;
    let total_secs = ms / 1000;
    let remaining_ms = ms % 1000;
    
    if total_secs >= 60 {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{}m {}s {}ms", mins, secs, remaining_ms)
    } else if total_secs > 0 {
        format!("{}s {}ms", total_secs, remaining_ms)
    } else {
        format!("{}ms", remaining_ms)
    }
}

/// Format current timestamp as ISO 8601 UTC
fn format_iso8601_utc() -> String {
    let ts_us = libakuma::time();
    let total_secs = ts_us / 1_000_000;
    
    // Simple math for UTC date/time (ignoring leap seconds)
    let s = (total_secs % 60) as u32;
    let m = ((total_secs / 60) % 60) as u32;
    let h = ((total_secs / 3600) % 24) as u32;
    
    let days_since_epoch = (total_secs / 86400) as i32;
    
    // Basic year/month/day calculation
    let mut year = 1970;
    let mut days_remaining = days_since_epoch;
    
    loop {
        let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if is_leap { 366 } else { 365 };
        if days_remaining < days_in_year {
            break;
        }
        days_remaining -= days_in_year;
        year += 1;
    }
    
    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let month_days = if is_leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    
    let mut month = 1;
    let mut day = 1;
    for (i, &days) in month_days.iter().enumerate() {
        if days_remaining < days {
            month = i + 1;
            day = days_remaining + 1;
            break;
        }
        days_remaining -= days;
    }
    
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, month, day, h, m, s
    )
}

/// Print streaming statistics in orange (yellow metric color)
fn print_stats(stats: &StreamStats, full_response: &str) {
    let tps = if stats.stream_us > 0 {
        let tokens = estimate_tokens_from_bytes(stats.total_bytes);
        (tokens as f64) / (stats.stream_us as f64 / 1_000_000.0)
    } else {
        0.0
    };

    let ttft_ms = stats.ttft_us / 1000;
    let stream_ms = stats.stream_us / 1000;
    let kb = stats.total_bytes as f64 / 1024.0;

    // Ensure model output is exactly one line apart from stats.
    // full_response might end with \n (printed by streaming loop).
    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
        if full_response.ends_with('\n') {
            tui_app::tui_print_with_indent("\n", "", 0, None);
        } else {
            tui_app::tui_print_with_indent("\n\n", "", 0, None);
        }
    } else {
        if full_response.ends_with('\n') {
            libakuma::print("\n");
        } else {
            libakuma::print("\n\n");
        }
    }

    let stats_content = format!(
        "{} | First: {}ms | Stream: {}ms | Size: {:.2}KB | TPS: {:.1}\n",
        format_iso8601_utc(),
        ttft_ms,
        stream_ms,
        kb,
        tps
    );
    
    print_metadata(&stats_content, COLOR_YELLOW);
}

fn estimate_tokens_from_bytes(bytes: usize) -> usize {
    (bytes + 3) / 4
}

/// Attempt to send request with retries and exponential backoff
pub fn send_with_retry(
    model: &str,
    provider: &Provider,
    history: &[Message],
    is_continuation: bool,
    current_tokens: usize,
    token_limit: usize,
    mem_kb: usize,
) -> Result<StreamResponse, &'static str> {
    let mut backoff_ms: u64 = 500;
    let is_tui = tui_app::TUI_ACTIVE.load(core::sync::atomic::Ordering::SeqCst);

    let status_prefix = if is_continuation {
        "[MEOW] continuing"
    } else {
        "[MEOW] jacking in"
    };
    
    // Update status pane (TUI mode) - dots are managed by render loop
    tui_app::update_streaming_status(status_prefix, 0, None);
    
    // Print inline only for non-TUI mode
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

        // Connect (TCP for both HTTP and HTTPS)
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

        // Update status to "waiting" - dots managed by render loop
        tui_app::update_streaming_status("[MEOW] waiting", 0, None);
        if !is_tui { libakuma::print("."); }

        let (path, request_body) = build_chat_request(model, provider, history);

        // Handle HTTPS vs HTTP
        if provider.is_https() {
            // HTTPS path with TLS using HttpStreamTls
            let (host, _) = provider.host_port().ok_or("Invalid URL")?;
            
            // Allocate TLS buffers
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
            
            // Build headers
            let mut headers = HttpHeaders::new();
            headers.content_type("application/json");
            if let Some(key) = &provider.api_key {
                headers.bearer_auth(key);
            }
            
            // Send request over TLS
            if let Err(_) = http_stream.post(&host, &path, &request_body, &headers) {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print("] "); }
                    return Err("Failed to send request");
                }
                continue;
            }
            
            if !is_tui { 
                libakuma::print("] waiting");
                libakuma::print(COLOR_RESET);
            }
            
            match read_streaming_with_http_stream_tls(&mut http_stream, start_time, provider, current_tokens, token_limit, mem_kb, is_tui) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e == "Request cancelled" {
                        return Err(e);
                    }
                    if attempt == MAX_RETRIES - 1 {
                        return Err(e);
                    }
                    if !is_tui { libakuma::print(&format!(" ({})", e)); }
                    continue;
                }
            }
        } else {
            // HTTP path (existing code)
            if let Err(e) = send_post_request(&stream, &path, &request_body, provider) {
                if attempt == MAX_RETRIES - 1 {
                    if !is_tui { libakuma::print("] "); }
                    return Err(e);
                }
                continue;
            }

            if !is_tui {
                libakuma::print("] waiting");
                libakuma::print(COLOR_RESET);
            }

            match read_streaming_response_with_progress(&stream, start_time, provider, current_tokens, token_limit, mem_kb, is_tui) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e == "Request cancelled" {
                        return Err(e);
                    }
                    if attempt == MAX_RETRIES - 1 {
                        return Err(e);
                    }
                    if !is_tui { libakuma::print(&format!(" ({})", e)); }
                    continue;
                }
            }
        }
    }

    Err("Max retries exceeded")
}

const MAX_TOOL_ITERATIONS: usize = 20;

/// Extract intent phrases like "Let me..." from text, capturing until newline or sentence end
fn extract_intent_phrases(text: &str) -> Vec<String> {
    let starters = ["Let me", "I'll ", "I will ", "First, ", "Now I'll", "Now let me", "First I'll", "First let me"];
    
    // Phrases that look like intents but are actually offers/questions, not action statements
    let exclusions = [
        "let me know",
        "let me explain",
        "let me summarize", 
        "let me clarify",
        "i'll help",
        "i'll be happy",
        "i'll wait",
        "i will help",
        "i will be happy",
        "i will wait",
        "if you need",
        "if you want",
        "if you'd like",
    ];
    
    let mut intents = Vec::new();
    
    for starter in starters {
        let lower_text = text.to_lowercase();
        let lower_starter = starter.to_lowercase();
        
        let mut search_start = 0;
        while let Some(pos) = lower_text[search_start..].find(&lower_starter) {
            let abs_pos = search_start + pos;
            let after_starter = &text[abs_pos..];
            
            // Find end of this intent: newline or sentence end (. ! ?)
            let mut end_pos = after_starter.len();
            for (i, c) in after_starter.char_indices() {
                if c == '\n' || c == '.' || c == '!' || c == '?' {
                    end_pos = i + 1; // Include the punctuation
                    break;
                }
            }
            
            let intent = after_starter[..end_pos].trim();
            let intent_lower = intent.to_lowercase();
            
            // Check if this matches an exclusion pattern
            let is_excluded = exclusions.iter().any(|excl| intent_lower.contains(excl));
            
            if !is_excluded && !intent.is_empty() && intent.len() > starter.len() {
                // Avoid duplicates
                let intent_str = String::from(intent);
                if !intents.contains(&intent_str) {
                    intents.push(intent_str);
                }
            }
            
            search_start = abs_pos + 1;
        }
    }
    
    intents
}

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

    // Track tool calls and stated intentions across all iterations
    let mut total_tools_called: usize = 0;
    let mut all_responses = String::new();

    for iteration in 0..MAX_TOOL_ITERATIONS {
        // Calculate metrics for background input handling
        let current_tokens = calculate_history_tokens(history);
        let mem_kb = libakuma::memory_usage() / 1024;
        let token_limit = context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);

        // Set status message color
        print(COLOR_GRAY_DIM);
        let stream_result = send_with_retry(model, provider, history, iteration > 0, current_tokens, token_limit, mem_kb)?;
        
        // Re-apply assistant color for the response content
        print(COLOR_MEOW);
        
        // Handle response and stats
        let (assistant_response, stats) = match stream_result {
            StreamResponse::Complete(response, stats) => (response, stats),
            StreamResponse::Partial(partial, stats) => {
                // Print stats for partial response
                print_stats(&stats, &partial);
                
                // Add partial response as assistant message
                if !partial.is_empty() {
                    history.push(Message::new("assistant", &partial));
                    // Add continuation prompt
                    history.push(Message::new("user", 
                        "[System: Your response was cut off mid-stream. Please continue exactly where you left off.]"));
                }
                // Continue to next iteration to get the rest
                continue;
            }
        };

        // Accumulate all responses for intent counting
        all_responses.push_str(&assistant_response);
        all_responses.push('\n');

        // Print stats for the assistant response
        print_stats(&stats, &assistant_response);

        // First check for CompactContext tool (handled specially)
        if let Some(compact_result) = try_execute_compact_context(&assistant_response, history, system_prompt) {
            if compact_result.success {
                print(COLOR_GREEN_LIGHT);
                print("\n[*] Context compacted successfully nya~!\n");
            } else {
                print(COLOR_PEARL);
                print("\n[*] Failed to compact context nya...\n");
            }
            print(COLOR_GRAY_BRIGHT);
            print(&compact_result.output);
            print(COLOR_RESET);
            print("\n\n");
            total_tools_called += 1;
            return Ok(());
        }

        let (mut current_llm_response_text, tool_calls) = tools::find_tool_calls(&assistant_response);

        if !tool_calls.is_empty() {
            for tool_call in tool_calls {
                total_tools_called += 1;

                if !current_llm_response_text.is_empty() {
                    // Use ORIGINAL segment for history
                    history.push(Message::new("assistant", &current_llm_response_text));
                    current_llm_response_text.clear(); // Clear after adding to history
                }

                // Execute the tool
                let tool_start = libakuma::uptime();
                let tool_result = if let Some(result) = tools::execute_tool_command(&tool_call.json) {
                    result
                } else {
                    tools::ToolResult::err("Failed to parse or execute tool command")
                };
                let tool_duration_us = libakuma::uptime() - tool_start;
                let tool_duration_str = format_duration(tool_duration_us);
                
                let (color, status) = if tool_result.success {
                    (COLOR_GREEN_LIGHT, "Success")
                } else {
                    (COLOR_PEARL, "Failed")
                };

                let status_content = format!(
                    " --- {} | Tool Status: {} | Duration: {}\n",
                    format_iso8601_utc(),
                    status,
                    tool_duration_str
                );

                if tool_result.success {
                    print("\n");
                    print(COLOR_GRAY_BRIGHT);
                    print(&tool_result.output);
                    print(COLOR_RESET);
                    print("\n");
                    print_metadata(&status_content, color);
                    print("\n");
                } else {
                    print_metadata(&status_content, color);
                    print("\n");
                    print(COLOR_PEARL);
                    print("[*] Tool failed\n\n");
                    print(COLOR_GRAY_BRIGHT);
                    print(&tool_result.output);
                    print(COLOR_RESET);
                    print("\n\n");
                }

                // Include current cwd in tool results so LLM always knows where it is
                let current_cwd = tools::get_working_dir();
                let tool_result_msg = format!(
                    "[Tool Result]\n{}\n[End Tool Result]\n[Current Directory: {}]\n\nPlease continue your response based on this result.",
                    tool_result.output, current_cwd
                );
                history.push(Message::new("user", &tool_result_msg));
                
                // Compact after tool execution to release memory
                trim_history(history);
                compact_history(history);
            }
            // Continue loop to give LLM a chance to respond after tool execution
            continue;
        }

        // No tool found - this is the final response
        if !current_llm_response_text.is_empty() {
            history.push(Message::new("assistant", &current_llm_response_text));
        }
        
        // Final trim and compact for this turn
        trim_history(history);
        compact_history(history);

        // Extract intent phrases from all accumulated responses
        let intent_phrases = extract_intent_phrases(&all_responses);
        let mismatch = !intent_phrases.is_empty() && total_tools_called == 0;

        let intent_content = format!(" --- Intent phrases: {} | Tools called: {}\n", intent_phrases.len(), total_tools_called);
        if mismatch {
            print_metadata(&intent_content, COLOR_PEARL);
        } else {
            print_metadata(&intent_content, COLOR_GREEN_LIGHT);
        }

        // Check for mismatch: stated intentions but no tool calls
        if mismatch {
            // Model stated intent but never called any tools
            print(&format!(
                "\n[!] Detected {} intent phrase(s) but {} tool call(s) - prompting self-check\n",
                intent_phrases.len(), total_tools_called
            ));
            
            // Format the intents as a list
            let mut intents_list = String::new();
            for (i, intent) in intent_phrases.iter().enumerate() {
                intents_list.push_str(&format!("  {}. \"{}\"\n", i + 1, intent));
            }
            
            // Add a prompt for the model to self-check with the actual stated intents
            let self_check_msg = format!(
                "[System Notice] You stated the following intention(s) but made 0 tool calls:\n{}\n\
                Did you forget to output the tool call JSON? Please complete the actions you stated.",
                intents_list
            );
            history.push(Message::new("user", &self_check_msg));
            
            // Give the model another chance to complete the tool call
            continue;
        }

        // Check if we should hint about context compaction
        if let Some(ctx_window) = context_window {
            let current_tokens = calculate_history_tokens(history);
            if current_tokens > TOKEN_LIMIT_FOR_COMPACTION && current_tokens < ctx_window {
                print("\n[!] Token count is high - consider asking Meow-chan to compact context\n");
            }
        }

        return Ok(());
    }

    print("\n[!] Max tool iterations reached\n");
    Ok(())
}

/// Try to find and execute CompactContext tool in the response
fn try_execute_compact_context(
    response: &str,
    history: &mut Vec<Message>,
    system_prompt: &str,
) -> Option<tools::ToolResult> {
    // Look for CompactContext tool call
    let json_block = if let Some(start) = response.find("```json") {
        let end = response[start..]
            .find("```\n")
            .or_else(|| response[start..].rfind("```"))?;
        let json_start = start + 7;
        let json_end = start + end;
        if json_start < json_end && json_end <= response.len() {
            response[json_start..json_end].trim()
        } else {
            return None;
        }
    } else if let Some(start) = response.find("{\"command\"") {
        let mut depth = 0;
        let mut end = start;
        for (i, c) in response[start..].chars().enumerate() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end > start {
            &response[start..end]
        } else {
            return None;
        }
    } else {
        return None;
    };

    // Check if it's a CompactContext tool
    if !json_block.contains("\"CompactContext\"") {
        return None;
    }

    // Extract the summary
    let summary = extract_json_string(json_block, "summary")?;

    if summary.is_empty() {
        return Some(tools::ToolResult::err(
            "CompactContext requires a non-empty summary",
        ));
    }

    // Calculate tokens before compaction
    let tokens_before = calculate_history_tokens(history);

    // Replace history with system prompt + summary
    history.clear();
    history.push(Message::new("system", system_prompt));

    let summary_msg = format!(
        "[Previous Conversation Summary]\n{}\n[End Summary]\n\nThe conversation above has been compacted. Continue from here.",
        summary
    );
    history.push(Message::new("user", &summary_msg));
    history.push(Message::new(
        "assistant",
        "Understood nya~! I've loaded the conversation summary into my memory banks. Ready to continue where we left off! (=^・ω・^=)",
    ));

    let tokens_after = calculate_history_tokens(history);

    Some(tools::ToolResult::ok(format!(
        "Context compacted: {} tokens -> {} tokens (saved {} tokens)",
        tokens_before,
        tokens_after,
        tokens_before - tokens_after
    )))
}

fn connect_to_provider(provider: &Provider) -> Result<TcpStream, String> {
    let (host, port) = provider
        .host_port()
        .ok_or_else(|| String::from("Invalid provider URL"))?;

    let ip = resolve(&host).map_err(|_| format!("DNS resolution failed for: {}", host))?;

    let addr_str = format!("{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], port);

    TcpStream::connect(&addr_str)
        .map_err(|_| format!("Connection failed to: {}", addr_str))
}

/// Read streaming response using HttpStreamTls
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

    const RESPONSE_WARNING_THRESHOLD: usize = 64 * 1024;
    let mut warned_large_response = false;

    loop {
        // Process background input during streaming FIRST
        // so that ESC/Ctrl+C can be caught in the same iteration
        // (dots animation is handled by render loop every 10th repaint)
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb);

        if tui_app::tui_is_cancelled() {
            print("\n[cancelled]");
            return Err("Request cancelled");
        }

        match stream.read_chunk() {
            StreamResult::Data(data) => {
                // Append new data to pending lines
                if let Ok(s) = core::str::from_utf8(&data) {
                    pending_lines.push_str(s);
                }
                
                // Process complete lines
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
                                    let elapsed_ms = ttft_us / 1000;
                                    // Update status pane with timing
                                    tui_app::update_streaming_status("[MEOW] streaming", 0, Some(elapsed_ms));
                                    if !is_tui {
                                        libakuma::print(" ");
                                        print_elapsed(elapsed_ms);
                                        libakuma::print("\n");
                                    }
                                }
                                print(COLOR_MEOW);
                                print(&content);

                                // Always accumulate full response
                                full_response.push_str(&content);
                                
                                // Warn once if response is getting large
                                if !warned_large_response && full_response.len() > RESPONSE_WARNING_THRESHOLD {
                                    warned_large_response = true;
                                    print("\n[!] Response exceeds 64KB, memory pressure possible\n");
                                }
                            }
                            if done {
                                stream_completed = true;
                                tui_app::clear_streaming_status();
                                let total_bytes = full_response.len();
                                let stream_us = if first_token_received { libakuma::uptime() - stream_start_us } else { 0 };
                                return Ok(StreamResponse::Complete(full_response, StreamStats { ttft_us, stream_us, total_bytes }));
                            }
                        }
                    }
                    
                    // Remove processed line
                    pending_lines.drain(..newline_pos + 1);
                }
            }
            StreamResult::WouldBlock => {
                libakuma::sleep_ms(10);
            }
            StreamResult::Done => {
                // Process any remaining data in pending_lines that didn't end with newline
                let remaining = pending_lines.trim();
                if !remaining.is_empty() {
                    if let Some((content, done)) = parse_streaming_line(remaining, provider) {
                        if !content.is_empty() {
                            if !first_token_received {
                                first_token_received = true;
                                let now = libakuma::uptime();
                                ttft_us = now - start_time;
                                stream_start_us = now;
                                let elapsed_ms = ttft_us / 1000;
                                tui_app::update_streaming_status("[MEOW] streaming", 0, Some(elapsed_ms));
                                if !is_tui {
                                    libakuma::print(" ");
                                    print_elapsed(elapsed_ms);
                                    libakuma::print("\n");
                                }
                            }
                            print(COLOR_MEOW);
                            print(&content);
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
            StreamResult::Error(e) => {
                print(&format!("\n[Error: {:?}]", e));
                return Err("Server returned error");
            }
        }
    }

    let total_bytes = full_response.len();
    let stream_us = if first_token_received { libakuma::uptime() - stream_start_us } else { 0 };
    let stats = StreamStats { ttft_us, stream_us, total_bytes };

    // Check if stream completed properly
    if !stream_completed && !full_response.is_empty() {
        // Return partial response for continuation
        print("\n[!] Stream interrupted, will continue...\n");
        full_response.shrink_to_fit();
        return Ok(StreamResponse::Partial(full_response, stats));
    }

    // Compact the response to release excess capacity
    full_response.shrink_to_fit();
    Ok(StreamResponse::Complete(full_response, stats))
}

// Default max tokens for model output - high enough to not truncate tool calls
const DEFAULT_MAX_TOKENS: usize = 16384;

fn build_chat_request(model: &str, provider: &Provider, history: &[Message]) -> (String, String) {
    let mut messages_json = String::from("[");
    for (i, msg) in history.iter().enumerate() {
        if i > 0 {
            messages_json.push(',');
        }
        messages_json.push_str(&msg.to_json());
    }
    messages_json.push(']');

    match provider.api_type {
        ApiType::Ollama => {
            // Use num_predict option to set max output tokens
            let body = format!(
                "{{\"model\":\"{}\",\"messages\":{},\"stream\":true,\"options\":{{\"num_predict\":{}}}}}",
                model, messages_json, DEFAULT_MAX_TOKENS
            );
            (String::from("/api/chat"), body)
        }
        ApiType::OpenAI => {
            // Use max_tokens for OpenAI-compatible APIs
            let body = format!(
                "{{\"model\":\"{}\",\"messages\":{},\"stream\":true,\"max_tokens\":{}}}",
                model, messages_json, DEFAULT_MAX_TOKENS
            );
            // Use base_path from URL if provided
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

// ============================================================================
// HTTP Client
// ============================================================================

fn send_post_request(
    stream: &TcpStream,
    path: &str,
    body: &str,
    provider: &Provider,
) -> Result<(), &'static str> {
    let (host, port) = provider.host_port().ok_or("Invalid provider URL")?;

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

    stream
        .write_all(request.as_bytes())
        .map_err(|_| "Failed to send request")
}

/// Read streaming response with progress indicator
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

    const RESPONSE_WARNING_THRESHOLD: usize = 64 * 1024;
    let mut warned_large_response = false;

    loop {
        // Process background input during streaming FIRST
        // so that ESC/Ctrl+C can be caught in the same iteration
        // (dots animation is handled by render loop every 10th repaint)
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb);

        if tui_app::tui_is_cancelled() {
            print("\n[cancelled]");
            return Err("Request cancelled");
        }

        match stream.read(&mut buf) {
            Ok(0) => {
                if !any_data_received {
                    return Err("Connection closed by server");
                }
                // Process any remaining data in pending_data before returning
                if let Ok(remaining_str) = core::str::from_utf8(&pending_data) {
                    let remaining = remaining_str.trim();
                    if !remaining.is_empty() {
                        for line in remaining.lines() {
                            if let Some((content, done)) = parse_streaming_line(line, provider) {
                                if !content.is_empty() {
                                    if !first_token_received {
                                        first_token_received = true;
                                        let now = libakuma::uptime();
                                        ttft_us = now - start_time;
                                        stream_start_us = now;
                                        let elapsed_ms = ttft_us / 1000;
                                        tui_app::update_streaming_status("[MEOW] streaming", 0, Some(elapsed_ms));
                                        if !is_tui {
                                            for _ in 0..(7 + dots_printed) {
                                                libakuma::print("\x08 \x08");
                                            }
                                            print_elapsed(elapsed_ms);
                                            libakuma::print("\n");
                                        }
                                    }
                                    print(COLOR_MEOW);
                                    print(&content);
                                    full_response.push_str(&content);
                                }
                                if done {
                                    stream_completed = true;
                                    tui_app::clear_streaming_status();
                                }
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
                        if !header_str.starts_with("HTTP/1.") {
                            return Err("Invalid HTTP response");
                        }
                        if !header_str.contains(" 200 ") {
                            let status_line = header_str.lines().next().unwrap_or("Unknown status");
                            print(&format!("\n[HTTP Error: {}]", status_line));

                            let body_start = pos + 4;
                            if pending_data.len() > body_start {
                                let body_preview = core::str::from_utf8(&pending_data[body_start..])
                                    .unwrap_or("")
                                    .chars()
                                    .take(200)
                                    .collect::<String>();
                                if !body_preview.is_empty() {
                                    print(&format!("\n[Response: {}]", body_preview.trim()));
                                }
                            }

                            if header_str.contains(" 404 ") {
                                return Err("Model not found (404)");
                            }
                            return Err("Server returned error");
                        }
                        headers_parsed = true;
                        pending_data.drain(..pos + 4);
                    }
                    continue;
                }

                if let Ok(body_str) = core::str::from_utf8(&pending_data) {
                    let last_newline = body_str.rfind('\n');
                    let complete_part = match last_newline {
                        Some(pos) => &body_str[..pos + 1],
                        None => continue,
                    };

                    let mut is_done = false;
                    for line in complete_part.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        if let Some((content, done)) = parse_streaming_line(line, provider) {
                            if !content.is_empty() {
                                if !first_token_received {
                                    first_token_received = true;
                                    let now = libakuma::uptime();
                                    ttft_us = now - start_time;
                                    stream_start_us = now;
                                    let elapsed_ms = ttft_us / 1000;
                                    tui_app::update_streaming_status("[MEOW] streaming", 0, Some(elapsed_ms));
                                    if !is_tui {
                                        for _ in 0..(7 + dots_printed) {
                                            libakuma::print("\x08 \x08");
                                        }
                                        print_elapsed(elapsed_ms);
                                        libakuma::print("\n");
                                    }
                                }
                                print(COLOR_MEOW);
                                print(&content);

                                // Always accumulate full response
                                full_response.push_str(&content);
                                
                                // Warn once if response is getting large
                                if !warned_large_response && full_response.len() > RESPONSE_WARNING_THRESHOLD {
                                    warned_large_response = true;
                                    print("\n[!] Response exceeds 64KB, memory pressure possible\n");
                                }
                            }
                            if done {
                                is_done = true;
                                tui_app::clear_streaming_status();
                                break;
                            }
                        }
                    }

                    let drain_pos = last_newline;
                    if let Some(pos) = drain_pos {
                        pending_data.drain(..pos + 1);
                    }

                    if is_done {
                        stream_completed = true;
                        let total_bytes = full_response.len();
                        let stream_us = if first_token_received { libakuma::uptime() - stream_start_us } else { 0 };
                        return Ok(StreamResponse::Complete(full_response, StreamStats { ttft_us, stream_us, total_bytes }));
                    }
                }
            }
            Err(e) => {
                if e.kind == libakuma::net::ErrorKind::WouldBlock
                    || e.kind == libakuma::net::ErrorKind::TimedOut
                {
                    read_attempts += 1;

                    if read_attempts % 50 == 0 && !first_token_received && !is_tui {
                        libakuma::print(".");
                        dots_printed += 1;
                    }

                    if read_attempts > 6000 {
                        return Err("Timeout waiting for response");
                    }
                    libakuma::sleep_ms(10);
                    continue;
                }
                if e.kind == libakuma::net::ErrorKind::ConnectionRefused {
                    return Err("Connection refused - is provider running?");
                }
                if e.kind == libakuma::net::ErrorKind::ConnectionReset {
                    return Err("Connection reset by server");
                }
                return Err("Network error");
            }
        }
    }

    let total_bytes = full_response.len();
    let stream_us = if first_token_received { libakuma::uptime() - stream_start_us } else { 0 };
    let stats = StreamStats { ttft_us, stream_us, total_bytes };

    // Check if stream completed properly
    if !stream_completed && !full_response.is_empty() {
        // Return partial response for continuation
        print("\n[!] Stream interrupted, will continue...\n");
        full_response.shrink_to_fit();
        return Ok(StreamResponse::Partial(full_response, stats));
    }

    // Compact the response to release excess capacity
    full_response.shrink_to_fit();
    Ok(StreamResponse::Complete(full_response, stats))
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

// ============================================================================
// JSON Parsing (minimal, for NDJSON response)
// ============================================================================

/// Parse a streaming response line based on provider type
fn parse_streaming_line(line: &str, provider: &Provider) -> Option<(String, bool)> {
    match provider.api_type {
        ApiType::Ollama => {
            // Ollama uses NDJSON: {"message":{"content":"..."}, "done":true/false}
            let done = line.contains("\"done\":true") || line.contains("\"done\": true");
            let content = extract_json_string(line, "content").unwrap_or_default();
            Some((content, done))
        }
        ApiType::OpenAI => {
            // OpenAI uses SSE: data: {"choices":[{"delta":{"content":"..."}}]}
            // End signal: data: [DONE]
            let line = line.trim();

            if line == "data: [DONE]" {
                return Some((String::new(), true));
            }

            if !line.starts_with("data:") {
                return Some((String::new(), false));
            }

            let json = line.strip_prefix("data:")?.trim();
            if json.is_empty() || json == "[DONE]" {
                return Some((String::new(), json == "[DONE]"));
            }

            // Extract content from delta
            let content = extract_openai_delta_content(json).unwrap_or_default();
            Some((content, false))
        }
    }
}

/// Extract content from OpenAI streaming delta
fn extract_openai_delta_content(json: &str) -> Option<String> {
    // Look for "delta":{"content":"..."}
    let delta_pos = json.find("\"delta\"")?;
    let after_delta = &json[delta_pos..];
    let content_pos = after_delta.find("\"content\"")?;
    let after_content = &after_delta[content_pos..];

    // Find the value
    let colon_pos = after_content.find(':')?;
    let rest = &after_content[colon_pos + 1..];
    let trimmed = rest.trim_start();

    if !trimmed.starts_with('"') {
        return None;
    }

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
                        _ => {
                            result.push('\\');
                            result.push(next);
                        }
                    }
                }
            }
            _ => result.push(c),
        }
    }

    Some(result)
}

/// Extract a string value from JSON by key
/// Handles basic escape sequences
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    // Build search pattern: "key":"
    let pattern = format!("\"{}\":\"", key);
    let start = json.find(&pattern)?;
    let value_start = start + pattern.len();

    // Find the end quote (handling escapes)
    let rest = &json[value_start..];
    let mut result = String::new();
    let mut chars = rest.chars().peekable();
    
    while let Some(c) = chars.next() {
        match c {
            '"' => break, // End of string
            '\\' => {
                // Handle escape sequences
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
                            // Unicode escape: \uXXXX
                            let mut hex = String::new();
                            for _ in 0..4 {
                                if let Some(h) = chars.next() {
                                    hex.push(h);
                                }
                            }
                            if let Ok(code) = u32::from_str_radix(&hex, 16) {
                                if let Some(ch) = char::from_u32(code) {
                                    result.push(ch);
                                }
                            }
                        }
                        _ => {
                            result.push('\\');
                            result.push(next);
                        }
                    }
                }
            }
            _ => result.push(c),
        }
    }

    Some(result)
}

/// Escape a string for JSON
fn json_escape(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => {
                // Use \uXXXX for other control characters
                let code = c as u32;
                result.push_str(&format!("\\u{:04x}", code));
            }
            _ => result.push(c),
        }
    }
    result
}

/// Print elapsed time in a cute format (uses status cursor)
fn print_elapsed(ms: u64) {
    if ms < 1000 {
        print(&format!("~(=^‥^)ノ [{}ms]", ms));
    } else {
        let secs = ms / 1000;
        let remainder = (ms % 1000) / 100; // one decimal
        print(&format!("~(=^‥^)ノ [{}.{}s]", secs, remainder));
    }
}

/// Sleep while polling for TUI input to keep the interface responsive.
fn poll_sleep(ms: u64, current_tokens: usize, token_limit: usize, mem_kb: usize) {
    let start = libakuma::uptime();
    let end = start + ms * 1000;
    while libakuma::uptime() < end {
        tui_app::tui_handle_input(current_tokens, token_limit, mem_kb);
        libakuma::sleep_ms(10);
    }
}

// ============================================================================
// Chat Message Types
// ============================================================================
