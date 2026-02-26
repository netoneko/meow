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
mod util;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use app::Message;
use config::{COMMON_TOOLS, Config, DEFAULT_CONTEXT_WINDOW, PERSONALITIES, Provider, Personality};
use libakuma::{arg, argc, close, exit, fstat, open, open_flags, read_fd};

#[no_mangle]
pub extern "C" fn main() {
    let mut app_config = Config::load();
    let mut model_override: Option<String> = None;
    let mut provider_override: Option<String> = None;
    let mut personality_override: Option<String> = None;
    let mut one_shot_message: Option<String> = None;
    let mut use_tui = true;

    let mut i = 1;
    if argc() > 1 {
        if let Some(first_arg) = arg(1) {
            if first_arg == "init" {
                exit(run_init(&mut app_config));
            }
            if first_arg == "test_stream" {
                exit(crate::ui::tui::stream::run_tests());
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
                    libakuma::print("meow: -m requires a model name\n");
                    exit(1);
                }
            } else if arg_str == "-p" || arg_str == "--provider" {
                i += 1;
                if let Some(p) = arg(i) {
                    provider_override = Some(String::from(p));
                } else {
                    libakuma::print("meow: --provider requires a provider name\n");
                    exit(1);
                }
            } else if arg_str == "-P" || arg_str == "--personality" {
                i += 1;
                if let Some(p) = arg(i) {
                    personality_override = Some(String::from(p));
                } else {
                    libakuma::print("meow: -P requires a personality name\n");
                    exit(1);
                }
            } else if arg_str == "--tui" {
                use_tui = true;
            } else if arg_str == "-h" || arg_str == "--help" {
                print_usage();
                exit(0);
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
            libakuma::print(&format!(
                "meow: unknown provider '{}'. Run 'meow init' to configure.\n",
                prov_name
            ));
            exit(1);
        }
    }

    if let Some(ref m) = model_override {
        app_config.current_model = m.clone();
    }

    if let Some(ref p) = personality_override {
        app_config.current_personality = p.clone();
    }

    let current_provider = app_config
        .get_current_provider()
        .cloned()
        .unwrap_or_else(Provider::ollama_default);

    let model = app_config.current_model.clone();

    // Assemble system prompt
    let mut system_prompt = String::new();

    // Check for local MEOW.md in current working directory
    let local_prompt = load_local_prompt();
    if let Some(prompt) = local_prompt {
        system_prompt.push_str(&prompt);
    } else {
        // Find personality in registry
        let persona = PERSONALITIES
            .iter()
            .find(|p| p.name == app_config.current_personality);
        if let Some(p) = persona {
            system_prompt.push_str(p.description);
        } else {
            // Fallback to Meow if not found
            system_prompt.push_str(PERSONALITIES[0].description);
        }
    }

    system_prompt.push_str("\n\n");
    system_prompt.push_str(COMMON_TOOLS);

    // if tools::chainlink_available() {
    //     system_prompt.push_str(tools::chainlink::CHAINLINK_TOOLS_SECTION);
    // }

    if use_tui || one_shot_message.is_none() {
        let mut history: Vec<Message> = Vec::new();
        history.push(Message::new("system", &system_prompt));

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

        let persona = get_active_personality(&app_config);
        let ack_msg = persona.ack_tui;
        history.push(Message::new("assistant", ack_msg));

        // Skip blocking model info query on startup to prevent hangs.
        // It can be queried later if needed or configured via commands.
        let context_window = DEFAULT_CONTEXT_WINDOW;

        let mut current_model = model;
        let mut current_provider = current_provider;

        if let Err(e) = tui_app::run_tui(
            &mut current_model,
            &mut current_provider,
            &mut app_config,
            &mut history,
            context_window,
            &system_prompt,
        ) {
            libakuma::print(&format!("TUI Error: {}\n", e));
            exit(1);
        }
        exit(0);
    }

    if let Some(msg) = one_shot_message {
        let mut history = Vec::new();
        history.push(Message::new("system", &system_prompt));
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

        let persona = get_active_personality(&app_config);
        let ack_msg = persona.ack_tui;
        history.push(Message::new("assistant", ack_msg));

        match app::chat_once(
            &model,
            &current_provider,
            &msg,
            &mut history,
            None,
            &system_prompt,
        ) {
            Ok(_) => {
                libakuma::print("\n");
                exit(0);
            }
            Err(e) => {
                let persona = get_active_personality(&app_config);
                let err_msg = format!("Error: {}", e);
                libakuma::print(&err_msg);
                exit(1);
            }
        };
    }

    exit(0);
}

fn get_active_personality<'a>(config: &'a Config) -> &'a Personality {
    PERSONALITIES
        .iter()
        .find(|p| p.name == config.current_personality)
        .unwrap_or(&PERSONALITIES[0]) // fallback to first (Meow)
}

fn load_local_prompt() -> Option<String> {
    let fd = open("MEOW.md", open_flags::O_RDONLY);
    if fd < 0 {
        return None;
    }

    let stat = match fstat(fd) {
        Ok(s) => s,
        Err(_) => {
            close(fd);
            return None;
        }
    };

    let size = stat.st_size as usize;
    if size == 0 || size > 64 * 1024 {
        close(fd);
        return None;
    }

    let mut buf = alloc::vec![0u8; size];
    let bytes_read = read_fd(fd, &mut buf);
    close(fd);

    if bytes_read <= 0 {
        return None;
    }

    match String::from_utf8(buf) {
        Ok(s) => Some(s),
        Err(_) => None,
    }
}

fn print_usage() {
    libakuma::print(
        "  /\\_/\\\n ( o.o )  ～ MEOW-CHAN PROTOCOL ～\n  > ^ <   Cyberpunk Neko AI Assistant\n\nUsage: meow [OPTIONS] [MESSAGE]\n       meow init              # Configure providers\n\nOptions:\n  -m, --model <NAME>      Neural link override\n  -p, --provider <NAME>   Use specific provider\n  -P, --personality <NAM> Switch persona (Meow, Jaffar, Rosie)\n  --tui                   Interactive TUI (default)\n  -h, --help              Display this transmission\n\nInteractive Commands:\n  /clear              Wipe memory banks nya~\n  /model [NAME]       Check/switch/list neural links\n  /provider [NAME]    Check/switch providers\n  /personality [NAME] Check/switch personality\n  /tokens             Show current token usage\n  /help               Command protocol\n  /quit               Jack out\n",
    );
}

fn run_init(config: &mut Config) -> i32 {
    libakuma::print(
        "\n  /\\_/\\  ╔══════════════════════════════════════╗\n ( o.o ) ║  M E O W - C H A N   I N I T         ║\n  > ^ <  ║  ～ Provider Configuration ～        ║\n /|   |\\ ╚══════════════════════════════════════╝\n\n～ Current providers: ～\n",
    );

    // Try to create the config file if it's missing
    let fd = libakuma::open("/etc/meow/config", libakuma::open_flags::O_RDONLY);
    if fd < 0 {
        libakuma::print("  [*] Config file missing, initializing with defaults...\n");
        if let Err(e) = config.save() {
            libakuma::print(&format!("  [!] Failed to save default config: {}\n", e));
        } else {
            libakuma::print("  [*] Default config created at /etc/meow/config\n");
        }
    } else {
        libakuma::close(fd);
    }

    if config.providers.is_empty() {
        libakuma::print("  (none configured)\n");
    } else {
        for p in &config.providers {
            let current = if p.name == config.current_provider {
                " (current)"
            } else {
                ""
            };
            let api_type = match p.api_type {
                config::ApiType::Ollama => "Ollama",
                config::ApiType::OpenAI => "OpenAI",
            };
            libakuma::print(&format!(
                "  - {} [{}]: {}{}\n",
                p.name, api_type, p.base_url, current
            ));
        }
    }
    libakuma::print(&format!(
        "\n  Current model: {}\n  Current personality: {}\n  Config file: /etc/meow/config\n\n～ To add a provider, edit /etc/meow/config manually ～\n   Format:\n   [provider:name]\n   base_url=http://host:port\n   api_type=ollama|openai\n   api_key=your-key-here (optional)\n\n",
        config.current_model, config.current_personality
    ));
    0
}
