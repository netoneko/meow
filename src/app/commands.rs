use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

use crate::config::{Config, Provider, ApiType, TOKEN_LIMIT_FOR_COMPACTION};
use crate::api;
use crate::tui_app;
use super::history::{Message, calculate_history_tokens};

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
                    let mut output = String::from("～ Available neural links: ～
");
                    match api::list_models(provider) {
                        Ok(models) => {
                            if models.is_empty() {
                                (CommandResult::Continue, Some(String::from("～ No models found nya...")))
                            } else {
                                for (i, m) in models.iter().enumerate() {
                                    let current_marker = if m.name == *model { " (current)" } else { "" };
                                    let size_info = m.parameter_size.as_ref().map(|s| format!(" [{}]", s)).unwrap_or_default();
                                    output.push_str(&format!("  {}. {}{}{}
", i + 1, m.name, size_info, current_marker));
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
                    tui_app::set_model_and_provider(model, &provider.name);
                    (CommandResult::Continue, Some(format!("～ *ears twitch* Neural link reconfigured to: {} nya~!", new_model)))
                }
                None => {
                    (CommandResult::Continue, Some(format!("～ Current neural link: {}
  Tip: Use '/model list' to see available models nya~!", model)))
                }
            }
        }
        "/provider" => {
            match arg {
                Some("?") | Some("list") => {
                    let mut output = String::from("～ Configured providers: ～
");
                    for (i, p) in config.providers.iter().enumerate() {
                        let current_marker = if p.name == provider.name { " (current)" } else { "" };
                        let api_type = match p.api_type {
                            ApiType::Ollama => "Ollama",
                            ApiType::OpenAI => "OpenAI",
                        };
                        output.push_str(&format!("  {}. {} ({}) [{}]{}
", i + 1, p.name, p.base_url, api_type, current_marker));
                    }
                    (CommandResult::Continue, Some(output))
                }
                Some(prov_name) => {
                    if let Some(p) = config.get_provider(prov_name) {
                        *provider = p.clone();
                        config.current_provider = String::from(prov_name);
                        let _ = config.save();
                        tui_app::set_model_and_provider(model, &provider.name);
                        (CommandResult::Continue, Some(format!("～ *ears twitch* Switched to provider: {} nya~!", prov_name)))
                    } else {
                        (CommandResult::Continue, Some(format!("～ Unknown provider: {} ...Run 'meow init' to add it nya~", prov_name)))
                    }
                }
                None => {
                    (CommandResult::Continue, Some(format!("～ Current provider: {} ({})
  Tip: Use '/provider list' to see configured providers nya~!", provider.name, provider.base_url)))
                }
            }
        }
        "/tokens" => {
            let current = calculate_history_tokens(history);
            (CommandResult::Continue, Some(format!("～ Current token usage: {} / {} 
  Tip: Ask Meow to 'compact the context' when tokens are high nya~!", current, TOKEN_LIMIT_FOR_COMPACTION)))
        }
        "/hotkeys" | "/shortcuts" => {
            let output = String::from("# Meow's Input Shortcuts

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
            let output = String::from("# Meow's Command Protocol

* `/clear`: Wipe memory banks nya~
* `/model [NAME]`: Check/switch neural link
* `/model list`: List available models
* `/provider`: Check/switch provider
* `/provider list`: List configured providers
* `/tokens`: Show current token usage
* `/hotkeys`: Show input shortcuts
* `/quit`: Jack out of the matrix
* `/help`: This help screen

**Context compaction**: When token count is high, ask Meow to compact the context to free up memory nya~!
");
            (CommandResult::Continue, Some(output))
        }
        _ => {
            (CommandResult::Continue, Some(format!("～ Nyaa? Unknown command: {} ...Meow-chan is confused (=｀ω´=)", command)))
        }
    }
}
