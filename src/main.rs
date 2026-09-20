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
mod json;
#[cfg(feature = "linux-net")]
mod linux_net;
#[cfg(feature = "litter")]
mod rt;
mod tools;
mod tui_app;
mod ui;
mod util;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

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

    #[cfg(feature = "litter")]
    if let Some(ref name) = app_config.litter_agent_name {
        tools::litter::set_agent_name(name.clone());
    }
    #[cfg(feature = "litter")]
    tools::litter::hub::set_hub_addr(app_config.litter_hub_addr.clone());
    #[cfg(feature = "litter")]
    tools::litter::set_static_peers_spec(app_config.litter_static_peers.clone());
    #[cfg(feature = "litter")]
    tools::litter::set_litter_name(app_config.litter_name.clone());
    #[cfg(feature = "litter")]
    {
        // No signing key configured → generate one now and persist it: a
        // litter's key must be stable or peers pinning its public key see
        // every signature break on the next boot. One line of warning, once.
        if app_config.litter_key.is_none() {
            match tools::litter::sig::generate_seed() {
                Some(seed) => {
                    app_config.litter_key = Some(seed.clone());
                    match app_config.save() {
                        Ok(()) => libakuma::print("litter: no signing key found — generated a new litter signing key and saved it to the config\n"),
                        Err(e) => libakuma::print(&format!("litter: generated a signing key but could NOT save it ({}): signatures will rotate every boot!\n", e)),
                    }
                }
                None => libakuma::print("litter: no signing key configured and key generation failed — relay disabled\n"),
            }
        }
        tools::litter::sig::set_our_key(app_config.litter_key.as_deref());
        tools::litter::sig::set_peer_keys(app_config.litter_peer_keys.as_deref());
    }
    // Bootstrap: a hub-backed litter member joins the hub's roster as part of
    // its own startup, every invocation — see `tools::litter::hub::bootstrap`.
    // Filesystem-mode litter (no `litter_hub_addr`) needs no equivalent step:
    // `roster.json` is the launcher's file, never meow's to write.
    #[cfg(feature = "litter")]
    if let (Some(ref name), Some(ref addr)) = (&app_config.litter_agent_name, &app_config.litter_hub_addr) {
        tools::litter::hub::bootstrap(addr, name);
    }

    let mut i = 1;
    #[cfg_attr(not(feature = "litter"), allow(unused_mut))]
    let mut litter_chase = false;
    #[cfg_attr(not(feature = "litter"), allow(unused_mut))]
    let mut litter_live = false;
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
            #[cfg(feature = "litter")]
            if first_arg == "litter" {
                let sub = arg(2);
                match sub {
                    Some("peers") => exit(run_litter_inspect(tools::litter::tool_list_peers())),
                    Some("inbox") => exit(run_litter_inspect(tools::litter::tool_read_inbox())),
                    Some("chase") => {
                        litter_chase = true;
                        i = 3; // resume normal flag parsing after "litter chase"
                    }
                    Some("live") => {
                        litter_live = true;
                        i = 3; // resume normal flag parsing after "litter live"
                    }
                    Some("send") => exit(run_litter_send()),
                    Some("task") => exit(run_litter_task()),
                    Some("observe") => exit(run_litter_observe()),
                    Some(other) => {
                        libakuma::print(&format!("meow: unknown 'meow litter' subcommand '{}'\n", other));
                        libakuma::print("Usage: meow litter {peers|inbox|send|task|chase|live|observe} [args]\n");
                        exit(1);
                    }
                    None => {
                        libakuma::print("Usage: meow litter {peers|inbox|send|task|chase|live|observe} [args]\n");
                        exit(1);
                    }
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
            } else if arg_str == "--debug" {
                tui_app::DEBUG_MODE.store(true, core::sync::atomic::Ordering::SeqCst);
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

    #[cfg(feature = "litter")]
    if litter_chase {
        system_prompt.push_str("\n\nYou are one member of a litter of meow agents working the same task. Before anything else, call ListPeers to see who else is here and ReadInbox to see what's already been said. Do the task, then call SendMessage to share your findings with the litter — don't just answer in isolation.");
    }
    #[cfg(feature = "litter")]
    if litter_live {
        // Same preamble as chase: a live turn IS a chase turn, just woken by
        // inbox activity instead of the command line (see `tools::litter::live`).
        system_prompt.push_str("\n\nYou are one member of a litter of meow agents working the same task. Before anything else, call ListPeers to see who else is here and ReadInbox to see what's already been said. Do the task, then call SendMessage to share your findings with the litter — don't just answer in isolation.");
    }
    #[cfg(not(feature = "litter"))]
    let _ = litter_chase;
    #[cfg(not(feature = "litter"))]
    let _ = litter_live;

    #[cfg(feature = "litter")]
    if litter_live {
        // Resident mode: never returns (see `tools::litter::live`).
        tools::litter::live::run(model.clone(), current_provider.clone(), system_prompt.clone());
    }

    system_prompt.push_str("\n\n");


    if cgi_mode {
        // Check QUERY_STRING for per-request model override (?model=<name>)
        let model = {
            let mut m = model;
            if let Some(qs) = libakuma::env("QUERY_STRING") {
                if let Some(val) = util::parse_query_param(qs, "model") {
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

        // Session id as the first line of the body, for later debugging.
        let session_id = app::session::generate_session_id();
        libakuma::print(&format!("session: {}\n", session_id));

        let mut conversation = Conversation::new_session(session_id);
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

    // `use_tui` alone, NOT `use_tui || one_shot_message.is_none()`.
    //
    // That `||` made `--no-tui` silently ineffective: without `-c` the
    // `is_none()` arm was true, so the flag was accepted, ignored, and the full
    // TUI started anyway — alternate screen, scroll region and all. A flag that
    // reports nothing and does nothing is worse than one that errors. The
    // `else` arms below now cover both remaining cases: a one-shot message, or
    // an interactive line-mode session.
    if use_tui {
        let session_id = app::session::generate_session_id();
        let mut conversation = Conversation::new_session(session_id);
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
        let session_id = app::session::generate_session_id();
        libakuma::print(&format!("session: {}\n", session_id));
        let mut conversation = Conversation::new_session(session_id);
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

    // Interactive, line-mode: `meow --no-tui` with no `-c`.
    //
    // This is what makes the terminal's own scrollback work. The TUI cannot
    // scroll back at all — it runs in the alternate screen (no scrollback by
    // definition) and paints the transcript inside a DECSTBM scroll region,
    // and lines scrolled out of a region are *discarded* rather than kept. Here
    // there is no alternate screen, no scroll region and no raw mode, so output
    // is ordinary terminal output: scrollback, mouse-wheel scrolling and
    // click-drag selection are the terminal's job and all simply work.
    //
    // It is also the quiet mode. The terminal does the line editing, so meow
    // writes nothing per keystroke and repaints nothing — which matters on this
    // kernel well beyond tidiness: every syscall takes the BKL at entry on
    // amd64 (`amd64/src/usermode.rs`, no per-syscall opt-out), the console is a
    // framebuffer, and the netpoll daemon contends for that same lock, so
    // console traffic measurably costs receive latency.
    // `docs/archive/AMD64_TRASHCAN_ISSUES.md` §5.5.
    interactive_line_mode(
        &model,
        &current_provider,
        &app_config,
        &system_prompt,
        no_personality,
    );
    exit(0);
}

/// Read one line from stdin, stripping the newline. `None` at EOF (Ctrl-D).
///
/// No raw mode and no escape parsing: stdin is left in its normal line
/// discipline, so the kernel returns a whole line on Enter and handles echo and
/// backspace itself. That is the entire reason this mode is quiet.
fn read_line_stdin() -> Option<String> {
    let mut line = String::new();
    let mut buf = [0u8; 256];
    loop {
        let n = libakuma::read(libakuma::fd::STDIN, &mut buf);
        if n <= 0 {
            // EOF with nothing buffered is Ctrl-D on an empty prompt; EOF with a
            // partial line still yields that line.
            return if line.is_empty() { None } else { Some(line) };
        }
        let chunk = &buf[..n as usize];
        for &b in chunk {
            if b == b'\n' || b == b'\r' {
                return Some(line);
            }
            // Cooked mode has already applied backspace, but a terminal that
            // sends DEL through anyway should not leave a stray glyph.
            if b == 0x7F {
                line.pop();
            } else if b >= 0x20 || b == b'\t' {
                line.push(b as char);
            }
        }
        // A read that filled the buffer without a newline: keep going.
        if (n as usize) < buf.len() {
            // Short read with no newline seen — a terminal in line mode should
            // not do this, but returning what we have beats blocking forever.
            return Some(line);
        }
    }
}

/// The `--no-tui` interactive loop: read a line, answer it, repeat.
fn interactive_line_mode(
    model: &str,
    provider: &Provider,
    app_config: &Config,
    system_prompt: &str,
    no_personality: bool,
) {
    let session_id = app::session::generate_session_id();
    let mut conversation = Conversation::new_session(session_id.clone());
    conversation.append(&Message::new("system", system_prompt));

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

    let persona = get_active_personality(app_config, no_personality);
    conversation.append(&Message::new("assistant", persona.ack_tui));

    libakuma::print(&format!(
        "meow (line mode) - session {}\nBlank line or Ctrl-D to quit. Terminal scrollback works here.\n\n",
        session_id
    ));

    loop {
        libakuma::print("> ");
        let line = match read_line_stdin() {
            Some(l) => l,
            None => break,
        };
        let msg = line.trim();
        if msg.is_empty() {
            break;
        }
        if msg == "/quit" || msg == "/exit" {
            break;
        }
        match app::chat_once(model, provider, msg, &mut conversation, None, system_prompt) {
            Ok(_) => libakuma::print("\n\n"),
            Err(e) => {
                let err_msg = persona.error_format.replace("{}", e);
                libakuma::print(&err_msg);
                libakuma::print("\n");
            }
        }
    }
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
    // Scoped MEOW.md (`$MEOW_HOME/MEOW.md`) wins over the CWD's, so a
    // resident agent sharing a filesystem with its litter-mates can carry its
    // own persona without each one needing a different working directory.
    let path = config::scoped("MEOW.md");
    let fd = open(&path, open_flags::O_RDONLY);
    if fd < 0 {
        // Fall back to the CWD's MEOW.md — the pre-scoping behavior.
        let fd = open("MEOW.md", open_flags::O_RDONLY);
        if fd < 0 {
            return None;
        }
        return read_prompt_fd(fd);
    }
    read_prompt_fd(fd)
}

fn read_prompt_fd(fd: i32) -> Option<String> {
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
        "meow - AI assistant\n\nUsage:\n  meow                        Interactive TUI mode (default)\n  meow -c \"message\"           Non-interactive: send message and exit\n  meow init                   Configure providers\n  meow test                   Run built-in tests\n\nOptions:\n  -c, --command <MSG>     Non-interactive: send MSG and print response to stdout\n  -m, --model <NAME>      Override the active model\n  -p, --provider <NAME>   Override the active provider\n  -P, --personality <NAM> Switch persona (default: Meow)\n  -N, --no-personality    Disable the persona; use a neutral assistant prompt\n  --tui                   Force interactive TUI mode\n  --no-tui                Line mode: no alt screen, no repainting, so the\n                          terminal's own scrollback and selection work\n  --debug                 Log connection and HTTP details (non-TUI only)\n  -h, --help              Show this help\n\nNon-interactive mode (-c) prints streaming output directly to stdout with\nANSI color codes but without cursor repositioning or the 3-pane layout.\nSuitable for scripting, pipes, and low-memory environments.\n\n--no-tui without -c is an interactive line-mode session: one prompt per\nline, no alternate screen and no scroll region, so scrolling back is the\nterminal's job and works normally. Use it when you want scrollback, text\nselection, or the least possible console traffic.\n\nInteractive Commands (TUI mode):\n  /clear              Wipe memory banks\n  /session            Describe the current session\n  /new                Start a new session\n  /model [NAME]       Check/switch/list models\n  /provider [NAME]    Check/switch providers\n  /personality [NAME] Check/switch personality\n  /tokens             Show current token usage\n  /help               Command list\n  /quit               Quit\n",
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
    #[cfg(feature = "litter")]
    { failures += tools::litter::run_tests(); }
    #[cfg(feature = "litter")]
    { failures += tools::litter::raft::run_tests(); }
    #[cfg(feature = "litter")]
    { failures += crate::rt::run_tests(); }
    if failures == 0 {
        libakuma::print("=== All tests passed ===\n");
    } else {
        libakuma::print(&format!("=== {} test suite(s) failed ===\n", failures));
    }
    if failures == 0 { 0 } else { 1 }
}

/// `meow litter peers` / `meow litter inbox`: run one message-tool directly
/// and print its result, with no LLM call involved. This is the "manage and
/// inspect" half of `meow litter`; `chase` (handled inline in `main`, since it
/// needs the full LLM/system-prompt setup) is the "put it to work" half.
#[cfg(feature = "litter")]
fn run_litter_inspect(result: tools::ToolResult) -> i32 {
    libakuma::print(&result.output);
    libakuma::print("\n");
    if result.success { 0 } else { 1 }
}

/// `meow litter send --to <name> --body "<text>" [--round N] [--from <name>]`:
/// the operator's own direct line into a peer's mailbox, using the exact same
/// `tool_send_message` an agent's `SendMessage` tool call reaches — this
/// process is not itself a litter agent (`litter_agent_name` need not be set),
/// so `from` defaults to `"root"` rather than being read from config, unless
/// overridden with `--from` (e.g. to send as one specific litter agent from
/// the operator's shell without configuring that agent's identity).
#[cfg(feature = "litter")]
fn run_litter_send() -> i32 {
    let mut to: Option<String> = None;
    let mut body: Option<String> = None;
    let mut round: i64 = 0;
    let mut from = String::from("root");
    let mut j = 3;
    while j < argc() {
        match arg(j) {
            Some("--to") => { j += 1; to = arg(j).map(String::from); }
            Some("--body") => { j += 1; body = arg(j).map(String::from); }
            Some("--round") => {
                j += 1;
                round = arg(j).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            }
            Some("--from") => {
                j += 1;
                if let Some(f) = arg(j) { from = String::from(f); }
            }
            _ => {}
        }
        j += 1;
    }

    let (to, body) = match (to, body) {
        (Some(t), Some(b)) => (t, b),
        _ => {
            libakuma::print("Usage: meow litter send --to <name> --body \"<text>\" [--round N] [--from <name>]\n");
            return 1;
        }
    };

    tools::litter::set_agent_name(from);
    run_litter_inspect(tools::litter::tool_send_message(&to, &body, round))
}

/// `meow litter task`: submit one task record as the operator.
///
/// The operator's route into the workflow. It exists because opening a
/// parent task is a *record*, not chat — `litter send --body "[task] …"`
/// used to work and deliberately no longer does, since task traffic that
/// travels as prose is task traffic anyone can forge by typing.
///
/// `--from` defaults to `root`, which is the identity the hub grants
/// operator authority to. That is a string today, and should become a
/// signature (`docs/LITTER_WORKFLOW.md` § Future work): right now anyone
/// who can reach the socket can pass `--from root`.
fn run_litter_task() -> i32 {
    let mut status = String::from("open");
    let mut task = String::new();
    let mut text = String::new();
    let mut expect = String::new();
    let mut from = String::from("root");
    let mut j = 3;
    while j < argc() {
        match arg(j) {
            Some("--status") => { j += 1; if let Some(v) = arg(j) { status = String::from(v); } }
            Some("--expect") => { j += 1; if let Some(v) = arg(j) { expect = String::from(v); } }
            Some("--task") => { j += 1; if let Some(v) = arg(j) { task = String::from(v); } }
            Some("--text") => { j += 1; if let Some(v) = arg(j) { text = String::from(v); } }
            Some("--from") => { j += 1; if let Some(v) = arg(j) { from = String::from(v); } }
            _ => {}
        }
        j += 1;
    }
    if text.is_empty() && status == "open" {
        libakuma::print("Usage: meow litter task --text \"<what to do>\" [--expect \"<shape of the answer>\"] [--status open|clear|reopen|artifact] [--task tN] [--from <name>]\n");
        return 1;
    }
    tools::litter::set_agent_name(from);
    run_litter_inspect(tools::litter::tool_task_update(&task, &status, &text, &expect))
}

/// `meow litter observe`: print every participant's messages merged into one
/// transcript, each with a small colored avatar per sender (see
/// `tools::litter::observe`). Read-only, no LLM call, no agent identity
/// required — this is the operator watching the litter, not a litter member.
#[cfg(feature = "litter")]
fn run_litter_observe() -> i32 {
    use litter_wire::Response;
    use tools::litter::observe::Entry;

    let mut entries: Vec<Entry> = Vec::new();

    if let Some(addr) = tools::litter::hub::hub_addr() {
        let names = match tools::litter::hub::peers(&addr, 0) {
            Ok(Response::Peers { names, .. }) => names,
            Ok(_) => {
                libakuma::print("observe: hub returned an unexpected response to 'peers'\n");
                return 1;
            }
            Err(e) => {
                libakuma::print(&format!("observe: failed to list peers: {}\n", e));
                return 1;
            }
        };
        for name in names {
            match tools::litter::hub::inbox_messages(&addr, &name) {
                Ok(messages) => {
                    for m in messages {
                        entries.push(Entry { from: m.from, round: m.round, body: m.body });
                    }
                }
                Err(e) => {
                    libakuma::print(&format!("observe: failed to read inbox for '{}': {}\n", name, e));
                }
            }
        }
        // Group fan-out (a Send addressed to `litter` lands in every
        // member's inbox — see `serve::GROUP_NAME`) means one message
        // exists several times; the transcript shows each distinct message
        // once. Compaction markers are bookkeeping, not conversation —
        // skip them.
        if !entries.is_empty() {
            entries.sort_by(|a, b| (&a.from, a.round, &a.body).cmp(&(&b.from, b.round, &b.body)));
            entries.dedup_by(|a, b| a.from == b.from && a.round == b.round && a.body == b.body);
        }
    }

    if entries.is_empty() {
        libakuma::print("No litter messages found yet.\n");
        return 0;
    }

    let entries = tools::litter::observe::merge_chronological(entries);
    let mut colors = tools::litter::observe::ColorAssigner::new();
    for entry in &entries {
        let color = colors.color_for(&entry.from);
        libakuma::print(&tools::litter::observe::render(entry, color));
    }
    0
}

fn run_init(config: &mut Config) -> i32 {
    libakuma::print("meow init - Provider Configuration\n\nCurrent providers:\n");

    // Try to create the config file if it's missing
    let config_path = config::config_path();
    let fd = libakuma::open(&config_path, libakuma::open_flags::O_RDONLY);
    if fd < 0 {
        libakuma::print("  [*] Config file missing, initializing with defaults...\n");
        if let Err(e) = config.save() {
            libakuma::print(&format!("  [!] Failed to save default config: {}\n", e));
        } else {
            libakuma::print(&format!("  [*] Default config created at {}\n", config_path));
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
        "\n  Current model: {}\n  Current personality: {}\n  Config file: {}\n\nTo add a provider, edit the config file:\n   [provider:name]\n   base_url=http://host:port\n   api_key=sk-test (optional)\n\n",
        config.current_model, config.current_personality, config_path
    ));
    0
}
