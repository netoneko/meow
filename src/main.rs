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

use config::{ApiType, Config, Provider, TOKEN_LIMIT_FOR_COMPACTION, DEFAULT_CONTEXT_WINDOW, SYSTEM_PROMPT_BASE};
use libakuma::net::{resolve, TcpStream};
use libakuma::{arg, argc, exit, fd, read};
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
            } else if arg_str == "--classic" {
                use_tui = false;
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

    if use_tui {
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

    // Interactive mode
    print_banner();
    print("  [Provider] ");
    print(&current_provider.name);
    print(" (");
    print(&current_provider.base_url);
    print(")\n  [Neural Link] Model: ");
    print(&model);

    // Query model info for context window size
    print("\n  [Context] Querying model info...");
    let context_window = match providers::query_model_info(&model, &current_provider) {
        Some(ctx) => {
            print(&format!(" {}k tokens max", ctx / 1000));
            ctx
        }
        None => {
            print(&format!(
                " (using default: {}k)",
                DEFAULT_CONTEXT_WINDOW / 1000
            ));
            DEFAULT_CONTEXT_WINDOW
        }
    };

    print("\n  [Token Limit] Compact context suggested at ");
    print(&format!("{}k tokens", TOKEN_LIMIT_FOR_COMPACTION / 1000));
    if tools::chainlink_available() {
        print("\n  [Chainlink] Issue tracker tools enabled");
    }
    print("\n  [Protocol] Type /help for commands, /quit to jack out\n\n");

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

    // Mutable state for current session
    let mut current_model = model;
    let mut current_prov = current_provider;

    loop {
        // Calculate current token count
        let current_tokens = calculate_history_tokens(&history);
        let token_display = if current_tokens >= 1000 {
            format!("{}k", current_tokens / 1000)
        } else {
            format!("{}", current_tokens)
        };

        // Get memory usage
        let mem_kb = libakuma::memory_usage() / 1024;
        let mem_display = if mem_kb >= 1024 {
            format!("{}M", mem_kb / 1024)
        } else {
            format!("{}K", mem_kb)
        };
        
        // Warn if memory is getting high (>2MB)
        if mem_kb > 2048 {
            print("[!] Memory high - consider /clear to reset\n");
        }

        // Print prompt with token count and memory
        print(&format!(
            "[{}/{}k|{}] (=^･ω･^=) > ",
            token_display,
            TOKEN_LIMIT_FOR_COMPACTION / 1000,
            mem_display
        ));

        // Read user input
        let input = match read_line() {
            Some(line) => line,
            None => {
                print("\n～ Meow-chan is jacking out... Bye bye~! ฅ^•ﻌ•^ฅ ～\n");
                break;
            }
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Handle commands
        if trimmed.starts_with('/') {
            let (res, output) = handle_command(trimmed, &mut current_model, &mut current_prov, &mut app_config, &mut history, &system_prompt);
            if let Some(out) = output {
                print(&out);
                print("\n\n");
            }
            match res {
                CommandResult::Continue => continue,
                CommandResult::Quit => break,
            }
        }

        // Send message to provider
        print("\n");
        match chat_once(&current_model, &current_prov, trimmed, &mut history, Some(context_window), &system_prompt) {
            Ok(_) => {
                print("\n\n");
            }
            Err(e) => {
                print("\n[!] Nyaa~! Error in the matrix: ");
                print(e);
                print(" (=ＴェＴ=)\n\n");
            }
        }
        
        // Compact strings to release excess memory
        compact_history(&mut history);
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
    print("  --classic               Old school neural link interface\n");
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

fn print_banner() {
    print("\n");
    print("  /\\_/\\  ╔══════════════════════════════════════╗\n");
    print(" ( o.o ) ║  M E O W - C H A N   v1.0            ║\n");
    print("  > ^ <  ║  ～ Cyberpunk Neko AI Assistant ～   ║\n");
    print(" /|   |\\ ╚══════════════════════════════════════╝\n");
    print("(_|   |_)  ฅ^•ﻌ•^ฅ  Jacking into the Net...  \n");
    print("\n");
    print(" ┌─────────────────────────────────────────────┐\n");
    print(" │ Welcome~! Meow-chan is online nya~! ♪(=^･ω･^)ﾉ │\n");
    print(" │ Press ESC to cancel requests~               │\n");
    print(" └─────────────────────────────────────────────┘\n\n");
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
                "～ Current token usage: {} / {} \n  Tip: Ask Meow-chan to 'compact the context' when tokens are high nya~!",
                current, TOKEN_LIMIT_FOR_COMPACTION
            )))
        }
        "/help" | "/?" => {
            let output = String::from("┌──────────────────────────────────────────────┐\n\
                                       │  ～ Meow-chan's Command Protocol ～          │\n\
                                       ├──────────────────────────────────────────────┤\n\
                                       │  /clear        - Wipe memory banks nya~      │\n\
                                       │  /model [NAME] - Check/switch neural link    │\n\
                                       │  /model list   - List available models       │\n\
                                       │  /provider     - Check/switch provider       │\n\
                                       │  /provider list- List configured providers   │\n\
                                       │  /tokens       - Show current token usage    │\n\
                                       │  /quit         - Jack out of the matrix      │\n\
                                       │  /help         - This help screen            │\n\
                                       ├──────────────────────────────────────────────┤\n\
                                       │  Context compaction: When token count is     │\n\
                                       │  high, ask Meow-chan to compact the context  │\n\
                                       │  to free up memory nya~!                     │\n\
                                       └──────────────────────────────────────────────┘\n");
            (CommandResult::Continue, Some(output))
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

pub enum StreamResponse {
    /// Response completed normally (server sent done signal)
    Complete(String),
    /// Response was interrupted mid-stream (connection closed before done signal)
    Partial(String),
}

/// Attempt to send request with retries and exponential backoff
pub fn send_with_retry(
    model: &str,
    provider: &Provider,
    history: &[Message],
    is_continuation: bool,
) -> Result<StreamResponse, &'static str> {
    let mut backoff_ms: u64 = 500;

    if is_continuation {
        print("[continuing");
    } else {
        print("[jacking in");
    }

    let start_time = libakuma::uptime();

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            print(&format!(" retry {}", attempt));
            libakuma::sleep_ms(backoff_ms);
            backoff_ms *= 2;
        }

        print(".");

        // Connect (TCP for both HTTP and HTTPS)
        let stream = match connect_to_provider(provider) {
            Ok(s) => s,
            Err(e) => {
                if attempt == MAX_RETRIES - 1 {
                    print(&format!("] {}", e));
                    return Err("Connection failed");
                }
                continue;
            }
        };

        print(".");

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
                        print(&format!("] TLS error: {:?}", e));
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
                    print("] ");
                    return Err("Failed to send request");
                }
                continue;
            }
            
            print("] waiting");
            
            match read_streaming_with_http_stream_tls(&mut http_stream, start_time, provider) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e == "Request cancelled" {
                        return Err(e);
                    }
                    if attempt == MAX_RETRIES - 1 {
                        return Err(e);
                    }
                    print(&format!(" ({})", e));
                    continue;
                }
            }
        } else {
            // HTTP path (existing code)
            if let Err(e) = send_post_request(&stream, &path, &request_body, provider) {
                if attempt == MAX_RETRIES - 1 {
                    print("] ");
                    return Err(e);
                }
                continue;
            }

            print("] waiting");

            match read_streaming_response_with_progress(&stream, start_time, provider) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e == "Request cancelled" {
                        return Err(e);
                    }
                    if attempt == MAX_RETRIES - 1 {
                        return Err(e);
                    }
                    print(&format!(" ({})", e));
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
    for iteration in 0..MAX_TOOL_ITERATIONS {
        let mut total_tools_called: usize = 0;
        let mut all_responses = String::new();
    
        let stream_result = send_with_retry(model, provider, history, iteration > 0)?;
        
        // Handle partial responses (stream interrupted before completion)
        let assistant_response = match stream_result {
            StreamResponse::Complete(response) => response,
            StreamResponse::Partial(partial) => {
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

        // First check for CompactContext tool (handled specially)
        if let Some(compact_result) = try_execute_compact_context(&assistant_response, history, system_prompt) {
            print("\n\n[*] ");
            if compact_result.success {
                print("Context compacted successfully nya~!\n");
                print(&compact_result.output);
            } else {
                print("Failed to compact context nya...\n");
                print(&compact_result.output);
            }
            print("\n\n");
            total_tools_called += 1;
            return Ok(());
        }

        let (text_before_tool, tool_result) = tools::find_and_execute_tool(&assistant_response);

        if let Some(result) = tool_result {
            total_tools_called += 1;
            
            if !text_before_tool.is_empty() {
                history.push(Message::new("assistant", &text_before_tool));
            }

            print("\n\n[*] ");
            if result.success {
                print("Tool executed successfully nya~!\n");
            } else {
                print("Tool failed nya...\n");
            }
            print(&result.output);
            print("\n\n");

            // Include current cwd in tool results so LLM always knows where it is
            let current_cwd = tools::get_working_dir();
            let tool_result_msg = format!(
                "[Tool Result]\n{}\n[End Tool Result]\n[Current Directory: {}]\n\nPlease continue your response based on this result.",
                result.output, current_cwd
            );
            history.push(Message::new("user", &tool_result_msg));
                        
            // Compact after tool execution to release memory
            compact_history(history);

            continue;
        }

        // No tool found - this is the final response
        if !assistant_response.is_empty() {
            history.push(Message::new("assistant", &assistant_response));
        }

        // Extract intent phrases from all accumulated responses
        let intent_phrases = extract_intent_phrases(&all_responses);

        print(&format!("Intent phrases: {:?}, tools called: {:?}\n", intent_phrases.len(), total_tools_called));

        // Check for mismatch: stated intentions but no tool calls
        if !intent_phrases.is_empty() && total_tools_called == 0 {
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
) -> Result<StreamResponse, &'static str> {
    let mut full_response = String::new();
    let mut pending_lines = String::new();
    let mut first_token_received = false;
    let mut stream_completed = false;

    const RESPONSE_WARNING_THRESHOLD: usize = 64 * 1024;
    let mut warned_large_response = false;

    // Note: Dots are printed by the TLS transport layer while waiting for data

    loop {
        if check_escape_pressed() {
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
                                    let elapsed_ms = (libakuma::uptime() - start_time) / 1000;
                                    print(" ");
                                    print_elapsed(elapsed_ms);
                                    print("\n");
                                }
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
                                return Ok(StreamResponse::Complete(full_response));
                            }
                        }
                    }
                    
                    // Remove processed line
                    pending_lines = String::from(&pending_lines[newline_pos + 1..]);
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
                                let elapsed_ms = (libakuma::uptime() - start_time) / 1000;
                                print(" ");
                                print_elapsed(elapsed_ms);
                                print("\n");
                            }
                            print(&content);
                            full_response.push_str(&content);
                        }
                        if done {
                            stream_completed = true;
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

    // Check if stream completed properly
    if !stream_completed && !full_response.is_empty() {
        // Return partial response for continuation
        print("\n[!] Stream interrupted, will continue...\n");
        full_response.shrink_to_fit();
        return Ok(StreamResponse::Partial(full_response));
    }

    // Compact the response to release excess capacity
    full_response.shrink_to_fit();
    Ok(StreamResponse::Complete(full_response))
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

    const RESPONSE_WARNING_THRESHOLD: usize = 64 * 1024;
    let mut warned_large_response = false;

    loop {
        if check_escape_pressed() {
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
                                        let elapsed_ms = (libakuma::uptime() - start_time) / 1000;
                                        if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
                                            print(" ");
                                            print_elapsed(elapsed_ms);
                                            print("\n");
                                        } else {
                                            for _ in 0..(7 + dots_printed) {
                                                print("\x08 \x08");
                                            }
                                            print_elapsed(elapsed_ms);
                                            print("\n");
                                        }
                                    }
                                    print(&content);
                                    full_response.push_str(&content);
                                }
                                if done {
                                    stream_completed = true;
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
                                    let elapsed_ms = (libakuma::uptime() - start_time) / 1000;
                                    if tui_app::TUI_ACTIVE.load(Ordering::SeqCst) {
                                        print(" ");
                                        print_elapsed(elapsed_ms);
                                        print("\n");
                                    } else {
                                        for _ in 0..(7 + dots_printed) {
                                            print("\x08 \x08");
                                        }
                                        print_elapsed(elapsed_ms);
                                        print("\n");
                                    }
                                }
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
                        return Ok(StreamResponse::Complete(full_response));
                    }
                }
            }
            Err(e) => {
                if e.kind == libakuma::net::ErrorKind::WouldBlock
                    || e.kind == libakuma::net::ErrorKind::TimedOut
                {
                    read_attempts += 1;

                    if read_attempts % 50 == 0 && !first_token_received {
                        print(".");
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

    // Check if stream completed properly
    if !stream_completed && !full_response.is_empty() {
        // Return partial response for continuation
        print("\n[!] Stream interrupted, will continue...\n");
        full_response.shrink_to_fit();
        return Ok(StreamResponse::Partial(full_response));
    }

    // Compact the response to release excess capacity
    full_response.shrink_to_fit();
    Ok(StreamResponse::Complete(full_response))
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

/// Print elapsed time in a cute format
fn print_elapsed(ms: u64) {
    if ms < 1000 {
        print(&format!("~(=^‥^)ノ [{}ms]", ms));
    } else {
        let secs = ms / 1000;
        let remainder = (ms % 1000) / 100; // one decimal
        print(&format!("~(=^‥^)ノ [{}.{}s]", secs, remainder));
    }
}

// ============================================================================
// Input Handling
// ============================================================================

/// Check if escape key was pressed (non-blocking)
/// Returns true if ESC (0x1B) was detected
fn check_escape_pressed() -> bool {
    let mut buf = [0u8; 8];
    let n = read(fd::STDIN, &mut buf);
    if n > 0 {
        // Check for escape key (0x1B)
        for i in 0..(n as usize) {
            if buf[i] == 0x1B {
                return true;
            }
        }
    }
    false
}

/// Read a line from stdin (blocking with polling)
/// Returns None on EOF (Ctrl+D on empty line)
fn read_line() -> Option<String> {
    let mut line = String::new();
    let mut buf = [0u8; 1];
    let mut consecutive_empty_reads = 0u32;

    loop {
        let n = read(fd::STDIN, &mut buf);
        
        if n <= 0 {
            // No data available - poll with backoff
            consecutive_empty_reads += 1;
            
            // After many empty reads, increase sleep time
            let sleep_time = if consecutive_empty_reads < 10 {
                10 // 10ms
            } else if consecutive_empty_reads < 100 {
                50 // 50ms
            } else {
                100 // 100ms
            };
            
            libakuma::sleep_ms(sleep_time);
            continue;
        }
        
        // Got data - reset counter
        consecutive_empty_reads = 0;

        let c = buf[0];
        if c == b'\n' || c == b'\r' {
            // Echo newline
            print("\n");
            break;
        }
        if c == 4 {
            // Ctrl+D
            if line.is_empty() {
                return None;
            }
            break;
        }
        // Handle backspace
        if c == 8 || c == 127 {
            if !line.is_empty() {
                line.pop();
                // Echo backspace: move back, space, move back
                print("\x08 \x08");
            }
            continue;
        }
        // Regular character
        if c >= 32 && c < 127 {
            line.push(c as char);
            // Echo the character
            let echo = [c];
            if let Ok(s) = core::str::from_utf8(&echo) {
                print(s);
            }
        }
    }

    Some(line)
}
