use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use libakuma::{
    get_terminal_attributes, set_terminal_attributes, 
    set_cursor_position, hide_cursor, show_cursor, 
    clear_screen, poll_input_event, write as akuma_write, fd,
};

use crate::config::{Provider, Config};
use crate::{Message, CommandResult};

// Tokyo Night Color Scheme (256-color ANSI)
const COLOR_VIOLET: &str = "\x1b[38;5;177m"; // User
const COLOR_BLUE: &str = "\x1b[38;5;111m";   // Meow
const COLOR_CYAN: &str = "\x1b[38;5;117m";   // System/Tools
const COLOR_YELLOW: &str = "\x1b[38;5;215m"; // Status
const COLOR_RESET: &str = "\x1b[0m";
const COLOR_BOLD: &str = "\x1b[1m";

// ANSI escape to clear from cursor to end of line
const CLEAR_TO_EOL: &str = "\x1b[K";

// Mode flags (from kernel's terminal.rs)
pub mod mode_flags {
    pub const RAW_MODE_ENABLE: u64 = 0x01;
}

struct Stdout;

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        akuma_write(fd::STDOUT, s.as_bytes());
        Ok(())
    }
}

pub struct App {
    pub input: String,
    pub history: Vec<Message>,
    pub scroll_offset: usize,
    pub terminal_width: u16,
    pub terminal_height: u16,
    pub input_dirty: AtomicBool,
    pub history_dirty: AtomicBool,
}

impl App {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            history: Vec::new(),
            scroll_offset: 0,
            terminal_width: 80,
            terminal_height: 24,
            input_dirty: AtomicBool::new(true),
            history_dirty: AtomicBool::new(true),
        }
    }

    pub fn render_history(&mut self) {
        let mut stdout = Stdout;
        let chat_area_height = self.terminal_height.saturating_sub(4) as usize;
        let chat_area_width = self.terminal_width.saturating_sub(2) as usize;

        // Draw chat history
        let mut lines = Vec::new();
        for msg in self.history.iter().skip(3) {
            let (prefix, color) = match msg.role.as_str() {
                "user" => ("> ", COLOR_VIOLET),
                "assistant" => ("[MEOW] ", COLOR_BLUE),
                _ => ("[*] ", COLOR_CYAN),
            };

            let mut first_line = true;
            for chunk in msg.content.lines() {
                let mut content = chunk;
                if content.is_empty() && first_line {
                    lines.push(format!("{}{}{}", color, prefix, COLOR_RESET));
                    first_line = false;
                    continue;
                } else if content.is_empty() {
                    lines.push(String::new());
                    continue;
                }

                while !content.is_empty() {
                    let line_prefix = if first_line { prefix } else { "       " };
                    let max_len = chat_area_width.saturating_sub(line_prefix.len());
                    
                    let mut line_len = content.len().min(max_len);
                    if line_len < content.len() {
                        if let Some(space_idx) = content[..line_len].rfind(' ') {
                            line_len = space_idx;
                        }
                    }

                    let line_text = &content[..line_len];
                    lines.push(format!("{}{}{}{}", color, line_prefix, line_text, COLOR_RESET));
                    
                    content = content[line_len..].trim_start();
                    first_line = false;
                }
            }
            lines.push(String::new());
        }

        let num_lines = lines.len();
        let display_start = if num_lines > chat_area_height {
            num_lines.saturating_sub(chat_area_height).saturating_sub(self.scroll_offset)
        } else {
            0
        };

        for y in 0..chat_area_height {
            set_cursor_position(0, y as u64);
            let _ = write!(stdout, "{}", CLEAR_TO_EOL); // Erase line content
            if let Some(line) = lines.get(display_start + y) {
                let _ = write!(stdout, "{}", line);
            }
        }
    }

    pub fn render_input(&mut self, token_info: &str) {
        let mut stdout = Stdout;
        let input_line_start = self.terminal_height.saturating_sub(2) as u64;

        set_cursor_position(0, input_line_start);
        let _ = write!(stdout, "{}", CLEAR_TO_EOL);
        
        let prompt = format!("{}{} {} > {}", COLOR_BOLD, COLOR_YELLOW, token_info, COLOR_RESET);
        let _ = write!(stdout, "{}{}", prompt, self.input);

        // Position cursor (prompt text is like "[123/128k|1.2M] > ")
        let prompt_len = token_info.len() + 4; 
        let cursor_col = (prompt_len + self.input.len()) as u64;
        set_cursor_position(cursor_col, input_line_start);
    }
}

pub fn run_tui(
    model: &mut String, 
    provider: &mut Provider, 
    config: &mut Config,
    history: &mut Vec<Message>,
    context_window: usize,
    system_prompt: &str
) -> Result<(), &'static str> {
    let mut old_mode_flags: u64 = 0;
    get_terminal_attributes(fd::STDIN, &mut old_mode_flags as *mut u64 as u64);
    set_terminal_attributes(fd::STDIN, 0, mode_flags::RAW_MODE_ENABLE);

    let mut app = App::new();
    app.history = history.clone();
    
    clear_screen();

    loop {
        let current_tokens = crate::calculate_history_tokens(&app.history);
        let mem_kb = libakuma::memory_usage() / 1024;
        let token_info = format!("[{}/{}k|{}k]", 
            current_tokens, 
            context_window / 1000,
            mem_kb
        );

        let needs_render = app.history_dirty.load(Ordering::Acquire) || app.input_dirty.load(Ordering::Acquire);

        if needs_render {
            hide_cursor();
            if app.history_dirty.load(Ordering::Acquire) {
                app.render_history();
                app.history_dirty.store(false, Ordering::Release);
            }
            if app.input_dirty.load(Ordering::Acquire) {
                app.render_input(&token_info);
                app.input_dirty.store(false, Ordering::Release);
            }
            show_cursor();
        }

        let poll_timeout = if needs_render { 1 } else { u64::MAX };
        let mut event_buf = [0u8; 16];
        let bytes_read = poll_input_event(poll_timeout, &mut event_buf);

        if bytes_read > 0 {
            let key_code = event_buf[0];

            match key_code {
                0x1B => break, // ESC
                b'\r' | b'\n' => {
                    if !app.input.is_empty() {
                        let user_input = app.input.clone();
                        app.input.clear();
                        
                        if user_input.starts_with('/') {
                            let (res, output) = crate::handle_command(&user_input, model, provider, config, history, system_prompt);
                            if let Some(out) = output {
                                app.history.push(Message::new("system", &out));
                            }
                            app.history_dirty.store(true, Ordering::Release);
                            app.input_dirty.store(true, Ordering::Release);
                            if let CommandResult::Quit = res { break; }
                        } else {
                            app.history.push(Message::new("user", &user_input));
                            app.history_dirty.store(true, Ordering::Release);
                            app.input_dirty.store(true, Ordering::Release);
                            
                            // Re-render before blocking call to chat_once
                            app.render_history();
                            app.render_input(&token_info);

                            history.clear();
                            history.extend(app.history.iter().cloned());

                            // Position for streaming
                            let ai_start_y = (app.history.len() * 2).min(app.terminal_height as usize - 5);
                            set_cursor_position(0, ai_start_y as u64);
                            let mut stdout = Stdout;
                            let _ = write!(stdout, "{}[MEOW] {}", COLOR_BLUE, COLOR_RESET);
                            let _ = write!(stdout, "{}", COLOR_BLUE); // Keep color for content

                            let _ = crate::chat_once(model, provider, &user_input, history, Some(context_window), system_prompt);
                            
                            let _ = write!(stdout, "{}", COLOR_RESET);

                            app.history = history.clone();
                            crate::compact_history(&mut app.history);
                            app.history_dirty.store(true, Ordering::Release);
                        }
                    }
                },
                0x7F | 0x08 => {
                    app.input.pop();
                    app.input_dirty.store(true, Ordering::Release);
                },
                c if c >= 0x20 && c <= 0x7E => {
                    app.input.push(c as char);
                    app.input_dirty.store(true, Ordering::Release);
                },
                _ => {}
            }
        }
    }

    set_terminal_attributes(fd::STDIN, 0, old_mode_flags);
    show_cursor();
    clear_screen();

    Ok(())
}