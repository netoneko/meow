use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::fmt::Write;

use crate::config::{Config, Provider, TOKEN_LIMIT_FOR_COMPACTION};
use crate::api;
use crate::tui_app;
use super::history::{Message, Conversation};

pub enum CommandResult {
    Continue,
    Quit,
}

pub fn handle_command(
    cmd: &str,
    model: &mut String,
    provider: &mut Provider,
    config: &mut Config,
    conversation: &mut Conversation,
    system_prompt: &str,
) -> (CommandResult, Option<String>) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let arg = parts.get(1).map(|s| s.trim());

    match command {
        "/quit" | "/exit" | "/q" => {
            (CommandResult::Quit, Some(String::from("Goodbye.")))
        }
        "/clear" | "/reset" => {
            conversation.reseed(&[Message::new("system", system_prompt)]);
            (CommandResult::Continue, Some(String::from("History cleared.")))
        }
        "/session" => {
            let info = format!(
                "Current session: {}\n  Path: {}\n  Messages: {}\n  Tokens: {}",
                conversation.session_id(),
                conversation.path(),
                conversation.len(),
                conversation.tokens(),
            );
            (CommandResult::Continue, Some(info))
        }
        "/new" => {
            let id = conversation.start_new(&[Message::new("system", system_prompt)]);
            (CommandResult::Continue, Some(format!("Started new session: {}", id)))
        }
        "/model" => {
            match arg {
                Some("?") | Some("list") => {
                    let mut output = String::from("Available models:\n");
                    match api::list_models(provider) {
                        Ok(models) => {
                            if models.is_empty() {
                                (CommandResult::Continue, Some(String::from("No models found.")))
                            } else {
                                for (i, m) in models.iter().enumerate() {
                                    let current_marker = if m.name == *model { " (current)" } else { "" };
                                    let size_info = m._parameter_size.as_ref().map(|s| format!(" [{}]", s)).unwrap_or_default();
                                    let _ = writeln!(output, "  {}. {}{}{}", i + 1, m.name, size_info, current_marker);
                                }
                                (CommandResult::Continue, Some(output))
                            }
                        }
                        Err(e) => {
                            (CommandResult::Continue, Some(format!("Failed to fetch models: {:?}", e)))
                        }
                    }
                }
                Some(new_model) => {
                    *model = String::from(new_model);
                    config.current_model = String::from(new_model);
                    let _ = config.save();
                    tui_app::set_model_and_provider(model, &provider.name);
                    (CommandResult::Continue, Some(format!("Model set to: {}", new_model)))
                }
                None => {
                    (CommandResult::Continue, Some(format!("Current model: {}\n  Use '/model list' to see available models.", model)))
                }
            }
        }
        "/provider" => {
            match arg {
                Some("?") | Some("list") => {
                    let mut output = String::from("Configured providers:\n");
                    for (i, p) in config.providers.iter().enumerate() {
                        let current_marker = if p.name == provider.name { " (current)" } else { "" };
                        let _ = writeln!(output, "  {}. {} ({}){}", i + 1, p.name, p.base_url, current_marker);
                    }
                    (CommandResult::Continue, Some(output))
                }
                Some(prov_name) => {
                    if let Some(p) = config.get_provider(prov_name) {
                        *provider = p.clone();
                        config.current_provider = String::from(prov_name);
                        let _ = config.save();
                        tui_app::set_model_and_provider(model, &provider.name);
                        (CommandResult::Continue, Some(format!("Switched to provider: {}", prov_name)))
                    } else {
                        (CommandResult::Continue, Some(format!("Unknown provider: {}. Run 'meow init' to add it.", prov_name)))
                    }
                }
                None => {
                    (CommandResult::Continue, Some(format!("Current provider: {} ({})\n  Use '/provider list' to see configured providers.", provider.name, provider.base_url)))
                }
            }
        }
        "/tokens" => {
            let current = conversation.tokens();
            (CommandResult::Continue, Some(format!("Current token usage: {} / {}\n  Ask the AI to 'compact the context' when tokens are high.", current, TOKEN_LIMIT_FOR_COMPACTION)))
        }
        "/personality" => {
            match arg {
                Some("list") | Some("?") => {
                    let mut output = String::from("Available personalities:\n");
                    for p in crate::config::PERSONALITIES {
                        let current_marker = if p.name == config.current_personality { " (current)" } else { "" };
                        let _ = writeln!(output, "  - {}{}", p.name, current_marker);
                    }
                    (CommandResult::Continue, Some(output))
                }
                Some(new_p) => {
                    if crate::config::PERSONALITIES.iter().any(|p| p.name == new_p) {
                        config.current_personality = String::from(new_p);
                        let _ = config.save();
                        (CommandResult::Continue, Some(format!("Personality set to {}. (Use /clear to apply)", new_p)))
                    } else {
                        (CommandResult::Continue, Some(format!("Unknown personality: {}. Use '/personality list' to see available ones.", new_p)))
                    }
                }
                None => {
                    (CommandResult::Continue, Some(format!("Current personality: {}", config.current_personality)))
                }
            }
        }
        "/markdown" => {
            config.render_markdown = !config.render_markdown;
            crate::app::state::set_render_markdown(config.render_markdown);
            let _ = config.save();
            let status = if config.render_markdown { "enabled" } else { "disabled" };
            (CommandResult::Continue, Some(format!("Markdown rendering {}", status)))
        }
        "/hotkeys" | "/shortcuts" => {
            let output = String::from("# Input Shortcuts

* **Shift+Enter** / **Ctrl+J**: Insert newline
* **Ctrl+A** / **Home**: Move to start of line
* **Ctrl+E** / **End**: Move to end of line
* **Ctrl+W**: Delete previous word
* **Ctrl+U**: Clear entire input line
* **Alt+B** / **Opt+Left**: Move back one word
* **Alt+F** / **Opt+Right**: Move forward one word
* **Arrows**: Navigate history and line
* **ESC** / **Ctrl+C**: Cancel current AI request

*Note: Some terminals intercept Ctrl+W/U/C.*
");
            (CommandResult::Continue, Some(output))
        }
        "/help" | "/?" => {
            let output = String::from("# Commands

* `/clear`: Clear history
* `/session`: Describe the current session
* `/new`: Start a new session
* `/model [NAME]`: Check/switch model
* `/model list`: List available models
* `/provider`: Check/switch provider
* `/provider list`: List configured providers
* `/personality [NAME]`: Check/switch personality
* `/tokens`: Show current token usage
* `/markdown`: Toggle Markdown rendering
* `/hotkeys`: Show input shortcuts
* `/test`: Run built-in tests
* `/quit`: Quit
* `/help`: This help screen

**Context compaction**: When token count is high, ask the AI to compact the context to free up memory.
");
            (CommandResult::Continue, Some(output))
        }
        #[cfg(feature = "tests")]
        "/test" | "/test_stream" => {
            let res = crate::app::history::run_tests()
                + crate::config::Config::run_tests()
                + crate::app::chat::run_tests()
                + crate::ui::tui::stream::run_tests();
            let msg = if res == 0 {
                String::from("All tests passed.")
            } else {
                format!("{} test suite(s) failed. Check output above.", res)
            };
            (CommandResult::Continue, Some(msg))
        }
        _ => {
            (CommandResult::Continue, Some(format!("Unknown command: {}. Type /help for a list.", command)))
        }
    }
}
