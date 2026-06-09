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

use app::{Message, Conversation};
use config::{Config, DEFAULT_CONTEXT_WINDOW, NO_PERSONA, PERSONALITIES, Provider};
use libakuma::{arg, argc, close, exit, fstat, open, open_flags, read_fd};

#[no_mangle]
pub extern "C" fn main() {
    let mut app_config = Config::load();
    let mut model_override: Option<String> = None;
    let mut provider_override: Option<String> = None;
    let mut personality_override: Option<String> = None;
    let mut no_personality = false;
    let mut one_shot_message: Option<String> = None;
    let mut use_tui = true;

    let mut cgi_mode = false;

    let mut i = 1;
    if argc() > 1 {
        if let Some(first_arg) = arg(1) {
            if first_arg == "init" {
                exit(run_init(&mut app_config));
            }
            if first_arg == "test" || first_arg == "test_stream" {
                #[cfg(feature = "tests")]
                exit(run_all_tests());
                #[cfg(not(feature = "tests"))]
                {
                    libakuma::print("meow: built without the test suite (rebuild with --features tests)\n");
                    exit(1);
                }
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
            } else if arg_str == "-N" || arg_str == "--no-personality" {
                no_personality = true;
            } else if arg_str == "--cgi" {
                cgi_mode = true;
                use_tui = false;
            } else if arg_str == "-c" || arg_str == "--command" {
                i += 1;
                if let Some(msg) = arg(i) {
                    one_shot_message = Some(String::from(msg));
                    use_tui = false;
                } else {
                    libakuma::print("meow: -c requires a message\n");
                    exit(1);
                }
            } else if arg_str == "--tui" {
                use_tui = true;
            } else if arg_str == "--no-tui" {
                use_tui = false;
            } else if arg_str == "-h" || arg_str == "--help" {
                print_usage();
                exit(0);
            } else if !arg_str.starts_with('-') {
                // Positional argument: non-interactive message (legacy form)
                one_shot_message = Some(String::from(arg_str));
                use_tui = false;
            }
        }
        i += 1;
    }

    // CGI mode: triggered by REQUEST_METHOD env var (set by httpd for all CGI
    // spawns) or the explicit --cgi flag.
    if !cgi_mode {
        cgi_mode = libakuma::env("REQUEST_METHOD").is_some();
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
        .unwrap_or_else(Provider::default_provider);

    let model = app_config.current_model.clone();

    // Assemble system prompt
    let mut system_prompt = String::new();

    // Check for local MEOW.md in current working directory
    let local_prompt = load_local_prompt();
    if let Some(prompt) = local_prompt {
        system_prompt.push_str(&prompt);
    } else {
        system_prompt.push_str(get_active_personality(&app_config, no_personality).description);
    }

    system_prompt.push_str("\n\n");


    if cgi_mode {
        // Check QUERY_STRING for per-request model override (?model=<name>)
        let model = {
            let mut m = model;
            if let Some(qs) = libakuma::env("QUERY_STRING") {
                if let Some(val) = util::parse_query_param(&qs, "model") {
                    let val = String::from(val.trim());
                    if val.is_empty() {
                        libakuma::print("Content-Type: text/plain\r\n\r\nError: unsupported model (empty model name)\n");
                        exit(1);
                    }
                    m = val;
                }
            }
            m
        };

        // Read prompt from POST body on stdin (fd 0, provided by httpd)
        let mut stdin_buf = alloc::vec![0u8; 32 * 1024];
        let n = read_fd(0, &mut stdin_buf);
        if n <= 0 {
            libakuma::print("Content-Type: text/plain\r\n\r\nError: no prompt in POST body\n");
            exit(1);
        }
        let raw = String::from_utf8_lossy(&stdin_buf[..n as usize]);
        let trimmed = raw.trim();
        let prompt = String::from(trimmed);
        if prompt.is_empty() {
            libakuma::print("Content-Type: text/plain\r\n\r\nError: empty prompt\n");
            exit(1);
        }

        // Print CGI response headers before any model output
        libakuma::print("Content-Type: text/plain\r\n\r\n");

        let mut conversation = Conversation::new(app::conversation_path());
        conversation.append(&Message::new("system", &system_prompt));
        let cwd_context = "[System Context] Current working directory: /\nNo sandbox restrictions.";
        conversation.append(&Message::new("user", cwd_context));
        let persona = get_active_personality(&app_config, no_personality);
        conversation.append(&Message::new("assistant", persona.ack_tui));

        match app::chat_once(
            &model,
            &current_provider,
            &prompt,
            &mut conversation,
            None,
            &system_prompt,
        ) {
            Ok(_) => { libakuma::print("\n"); exit(0); }
            Err(e) => {
                let err_persona = get_active_personality(&app_config, no_personality);
                let err_msg = err_persona.error_format.replace("{}", e);
                libakuma::print(&err_msg);
                exit(1);
            }
        }
    }

    if use_tui || one_shot_message.is_none() {
        let mut conversation = Conversation::new(app::conversation_path());
        conversation.append(&Message::new("system", &system_prompt));

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
        conversation.append(&Message::new("user", &cwd_context));

        let persona = get_active_personality(&app_config, no_personality);
        let ack_msg = persona.ack_tui;
        conversation.append(&Message::new("assistant", ack_msg));

        // Skip blocking model info query on startup to prevent hangs.
        // It can be queried later if needed or configured via commands.
        let context_window = DEFAULT_CONTEXT_WINDOW;

        let mut current_model = model;
        let mut current_provider = current_provider;

        if let Err(e) = tui_app::run_tui(
            &mut current_model,
            &mut current_provider,
            &mut app_config,
            &mut conversation,
            context_window,
            &system_prompt,
        ) {
            libakuma::print(&format!("TUI Error: {}\n", e));
            exit(1);
        }
        exit(0);
    }

    if let Some(msg) = one_shot_message {
        let mut conversation = Conversation::new(app::conversation_path());
        conversation.append(&Message::new("system", &system_prompt));
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
        conversation.append(&Message::new("user", &cwd_context));

        let persona = get_active_personality(&app_config, no_personality);
        let ack_msg = persona.ack_tui;
        conversation.append(&Message::new("assistant", ack_msg));

        match app::chat_once(
            &model,
            &current_provider,
            &msg,
            &mut conversation,
            None,
            &system_prompt,
        ) {
            Ok(_) => {
                libakuma::print("\n");
                exit(0);
            }
            Err(e) => {
                let persona = get_active_personality(&app_config, no_personality);
                let err_msg = persona.error_format.replace("{}", e);
                libakuma::print(&err_msg);
                exit(1);
            }
        };
    }

    exit(0);
}

fn get_active_personality(config: &Config, no_personality: bool) -> &'static crate::config::Personality {
    if no_personality {
        return &NO_PERSONA;
    }
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

    String::from_utf8(buf).ok()
}

fn print_usage() {
    libakuma::print(
        "meow - AI assistant\n\nUsage:\n  meow                        Interactive TUI mode (default)\n  meow -c \"message\"           Non-interactive: send message and exit\n  meow init                   Configure providers\n  meow test                   Run built-in tests\n\nOptions:\n  -c, --command <MSG>     Non-interactive: send MSG and print response to stdout\n  -m, --model <NAME>      Override the active model\n  -p, --provider <NAME>   Override the active provider\n  -P, --personality <NAM> Switch persona (default: Meow)\n  -N, --no-personality    Disable the persona; use a neutral assistant prompt\n  --tui                   Force interactive TUI mode\n  --no-tui                Force non-interactive mode (no repainting)\n  -h, --help              Show this help\n\nNon-interactive mode (-c) prints streaming output directly to stdout with\nANSI color codes but without cursor repositioning or the 3-pane layout.\nSuitable for scripting, pipes, and low-memory environments.\n\nInteractive Commands (TUI mode):\n  /clear              Wipe memory banks\n  /model [NAME]       Check/switch/list models\n  /provider [NAME]    Check/switch providers\n  /personality [NAME] Check/switch personality\n  /tokens             Show current token usage\n  /help               Command list\n  /quit               Quit\n",
    );
}

#[cfg(feature = "tests")]
fn run_all_tests() -> i32 {
    libakuma::print("=== Meow Test Suite ===\n");
    let mut failures = 0i32;
    failures += app::history::run_tests();
    failures += Config::run_tests();
    failures += app::chat::run_tests();
    failures += crate::ui::tui::stream::run_tests();
    if failures == 0 {
        libakuma::print("=== All tests passed ===\n");
    } else {
        libakuma::print(&format!("=== {} test suite(s) failed ===\n", failures));
    }
    if failures == 0 { 0 } else { 1 }
}

fn run_init(config: &mut Config) -> i32 {
    libakuma::print("meow init - Provider Configuration\n\nCurrent providers:\n");

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
            libakuma::print(&format!(
                "  - {}: {}{}\n",
                p.name, p.base_url, current
            ));
        }
    }
    libakuma::print(&format!(
        "\n  Current model: {}\n  Current personality: {}\n  Config file: /etc/meow/config\n\nTo add a provider, edit /etc/meow/config:\n   [provider:name]\n   base_url=http://host:port\n   api_key=your-key-here (optional)\n\n",
        config.current_model, config.current_personality
    ));
    0
}
