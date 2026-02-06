//! Meow-chan - Cyberpunk Neko AI Assistant
//!
//! A cute cybernetically-enhanced catgirl AI that connects to Ollama LLMs.

#![no_std]
#![no_main]

extern crate alloc;

mod api;
mod app;
mod code_search;
mod config;
mod tools;
mod tui_app;
mod ui;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use config::{Config, Provider, DEFAULT_CONTEXT_WINDOW, SYSTEM_PROMPT_BASE};
use libakuma::{arg, argc, exit};
use app::Message;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let code = main();
    exit(code);
}

fn main() -> i32 {
    let mut app_config = Config::load();
    let mut model_override: Option<String> = None;
    let mut provider_override: Option<String> = None;
    let mut one_shot_message: Option<String> = None;
    let mut use_tui = true;

    let mut i = 1;
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
                if let Some(m) = arg(i) { model_override = Some(String::from(m)); }
                else { libakuma::print("meow: -m requires a model name\n"); return 1; }
            } else if arg_str == "-p" || arg_str == "--provider" {
                i += 1;
                if let Some(p) = arg(i) { provider_override = Some(String::from(p)); }
                else { libakuma::print("meow: --provider requires a provider name\n"); return 1; }
            } else if arg_str == "--tui" {
                use_tui = true;
            } else if arg_str == "-h" || arg_str == "--help" {
                print_usage();
                return 0;
            } else if !arg_str.starts_with('-') {
                one_shot_message = Some(String::from(arg_str));
                use_tui = false;
            }
        }
        i += 1;
    }

    if let Some(ref prov_name) = provider_override {
        if app_config.get_provider(prov_name).is_some() {
            app_config.current_provider = prov_name.clone();
        } else {
            libakuma::print(&format!("meow: unknown provider '{}'. Run 'meow init' to configure.\n", prov_name));
            return 1;
        }
    }

    if let Some(ref m) = model_override {
        app_config.current_model = m.clone();
    }

    let current_provider = app_config
        .get_current_provider()
        .cloned()
        .unwrap_or_else(Provider::ollama_default);

    let model = app_config.current_model.clone();
    let mut system_prompt = String::from(SYSTEM_PROMPT_BASE);
    if tools::chainlink_available() {
        system_prompt.push_str(CHAINLINK_TOOLS_SECTION);
    }

    if use_tui || one_shot_message.is_none() {
        let mut history: Vec<Message> = Vec::new();
        history.push(Message::new("system", &system_prompt));
        
        let initial_cwd = tools::get_working_dir();
        let sandbox_root = tools::get_sandbox_root();
        let cwd_context = if sandbox_root == "/" {
            format!("[System Context] Your current working directory is: {}\nNo sandbox restrictions - you can access any path.", initial_cwd)
        } else {
            format!("[System Context] Your current working directory is: {}\nSandbox root: {} (you cannot access paths outside this directory)\nUse relative paths like 'docs/' instead of absolute paths like '/docs/'.", initial_cwd, sandbox_root)
        };
        history.push(Message::new("user", &cwd_context));
        history.push(Message::new("assistant", "Understood nya~! I'll use relative paths for file operations within the current directory. Ready to help! (=^・ω・^=)"));

        let context_window = match api::query_model_info(&model, &current_provider) {
            Some(ctx) => ctx,
            None => DEFAULT_CONTEXT_WINDOW,
        };

        let mut current_model = model;
        let mut current_provider = current_provider;

        if let Err(e) = tui_app::run_tui(&mut current_model, &mut current_provider, &mut app_config, &mut history, context_window, &system_prompt) {
            libakuma::print(&format!("TUI Error: {}\n", e));
            return 1;
        }
        return 0;
    }

    if let Some(msg) = one_shot_message {
        let mut history = Vec::new();
        history.push(Message::new("system", &system_prompt));
        let initial_cwd = tools::get_working_dir();
        let sandbox_root = tools::get_sandbox_root();
        let cwd_context = if sandbox_root == "/" {
            format!("[System Context] Current working directory: {}\nNo sandbox restrictions.", initial_cwd)
        } else {
            format!("[System Context] Current working directory: {}\nSandbox root: {} - use relative paths.", initial_cwd, sandbox_root)
        };
        history.push(Message::new("user", &cwd_context));
        history.push(Message::new("assistant", "Understood nya~!"));
        
        return match app::chat_once(&model, &current_provider, &msg, &mut history, None, &system_prompt) {
            Ok(_) => { libakuma::print("\n"); 0 }
            Err(e) => { libakuma::print(&format!("～ Nyaa~! {} (=ＴェＴ=) ～\n", e)); 1 }
        };
    }

    0
}

fn print_usage() {
    libakuma::print("  /\\_/\\\n ( o.o )  ～ MEOW-CHAN PROTOCOL ～\n  > ^ <   Cyberpunk Neko AI Assistant\n\nUsage: meow [OPTIONS] [MESSAGE]\n       meow init              # Configure providers\n\nOptions:\n  -m, --model <NAME>      Neural link override\n  -p, --provider <NAME>   Use specific provider\n  --tui                   Interactive TUI (default)\n  -h, --help              Display this transmission\n\nInteractive Commands:\n  /clear              Wipe memory banks nya~\n  /model [NAME]       Check/switch/list neural links\n  /provider [NAME]    Check/switch providers\n  /tokens             Show current token usage\n  /help               Command protocol\n  /quit               Jack out\n");
}

fn run_init(config: &mut Config) -> i32 {
    libakuma::print("\n  /\\_/\\  ╔══════════════════════════════════════╗\n ( o.o ) ║  M E O W - C H A N   I N I T         ║\n  > ^ <  ║  ～ Provider Configuration ～        ║\n /|   |\\ ╚══════════════════════════════════════╝\n\n～ Current providers: ～\n");
    if config.providers.is_empty() {
        libakuma::print("  (none configured)\n");
    } else {
        for p in &config.providers {
            let current = if p.name == config.current_provider { " (current)" } else { "" };
            let api_type = match p.api_type { config::ApiType::Ollama => "Ollama", config::ApiType::OpenAI => "OpenAI" };
            libakuma::print(&format!("  - {} [{}]: {}{}\n", p.name, api_type, p.base_url, current));
        }
    }
    libakuma::print(&format!("\n  Current model: {}\n  Config file: /etc/meow/config\n\n～ To add a provider, edit /etc/meow/config manually ～\n   Format:\n   [provider:name]\n   base_url=http://host:port\n   api_type=ollama|openai\n   api_key=your-key-here (optional)\n\n", config.current_model));
    0
}

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