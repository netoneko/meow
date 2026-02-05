use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use libakuma::{
    get_terminal_attributes, set_terminal_attributes, 
    set_cursor_position, hide_cursor, show_cursor, 
    clear_screen, poll_input_event, write as akuma_write, fd,
    open, close, read_fd, open_flags
};

use crate::config::{Provider, Config, COLOR_USER, COLOR_MEOW, COLOR_GRAY_DIM, COLOR_GRAY_BRIGHT, COLOR_GREEN, COLOR_YELLOW, COLOR_RESET, COLOR_BOLD};
use crate::{Message, CommandResult};

// ANSI escapes
const SAVE_CURSOR: &str = "\x1b[s";
const RESTORE_CURSOR: &str = "\x1b[u";
const CLEAR_TO_EOL: &str = "\x1b[K";

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
    pub terminal_width: u16,
    pub terminal_height: u16,
}

impl App {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            history: Vec::new(),
            terminal_width: 100,
            terminal_height: 25,
        }
    }

    /// Renders the fixed bottom footer (separator, status, prompt).
    pub fn render_footer(&self, token_info: &str) {
        let mut stdout = Stdout;
        let h = self.terminal_height as u64;
        let w = self.terminal_width as usize;

        // 1. Draw Separator
        set_cursor_position(0, h - 3);
        let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, "─".repeat(w), COLOR_RESET);

        // 2. Draw Status Bar
        set_cursor_position(0, h - 2);
        let _ = write!(stdout, "{}", CLEAR_TO_EOL);
        let _ = write!(stdout, " {}{}{}", COLOR_YELLOW, token_info, COLOR_RESET);

        // 3. Draw Prompt
        set_cursor_position(0, h - 1);
        let _ = write!(stdout, "{}", CLEAR_TO_EOL);
        let _ = write!(stdout, "{}> {}{}{}", COLOR_BOLD, COLOR_USER, self.input, COLOR_RESET);

        // 4. Position Cursor at end of input
        set_cursor_position((2 + self.input.len()) as u64, h - 1);
    }
}

fn set_scroll_region(top: u16, bottom: u16) {
    let mut stdout = Stdout;
    let _ = write!(stdout, "\x1b[{};{}r", top, bottom);
}

fn print_greeting() {
    let mut stdout = Stdout;
    let mut buf = [0u8; 4096];
    let fd = open("src/akuma_40.txt", open_flags::O_RDONLY);
    if fd >= 0 {
        let n = read_fd(fd, &mut buf);
        close(fd);
        if n > 0 {
            let _ = write!(stdout, "\n{}\x1b[38;5;236m", COLOR_RESET);
            if let Ok(s) = core::str::from_utf8(&buf[..n as usize]) {
                let _ = write!(stdout, "{}", s);
            }
            let _ = write!(stdout, "{}\n  {}MEOW!{} ~(=^‥^)ノ\n\n", COLOR_RESET, COLOR_BOLD, COLOR_RESET);
        }
    }
}

fn probe_terminal_size() -> (u16, u16) {
    let mut stdout = Stdout;
    let _ = write!(stdout, "\x1b[999;999H\x1b[6n");
    let mut buf = [0u8; 32];
    let n = poll_input_event(200, &mut buf);
    if n > 0 {
        if let Ok(resp) = core::str::from_utf8(&buf[..n as usize]) {
            if let Some(start) = resp.find('[') {
                if let Some(end) = resp.find('R') {
                    let parts = &resp[start+1..end];
                    let mut split = parts.split(';');
                    if let (Some(r_str), Some(c_str)) = (split.next(), split.next()) {
                        let r = r_str.parse::<u16>().unwrap_or(25);
                        let c = c_str.parse::<u16>().unwrap_or(100);
                        return (c, r);
                    }
                }
            }
        }
    }
    (100, 25)
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
    let (w, h) = probe_terminal_size();
    app.terminal_width = w; app.terminal_height = h;
    
    clear_screen();
    print_greeting();
    set_scroll_region(1, h - 3);

    loop {
        let current_tokens = crate::calculate_history_tokens(&app.history);
        let mem_kb = libakuma::memory_usage() / 1024;
        let token_info = format!("[ TOKENS: {} / {}k ] [ MEM: {}k ]", 
            current_tokens, context_window / 1000, mem_kb);

        app.render_footer(&token_info);

        let mut event_buf = [0u8; 16];
        let bytes_read = poll_input_event(u64::MAX, &mut event_buf);

        if bytes_read > 0 {
            let key_code = event_buf[0];
            match key_code {
                0x1B => { 
                    if bytes_read > 1 { continue; }
                    if config.exit_on_escape { break; }
                },
                b'\r' | b'\n' => {
                    if !app.input.is_empty() {
                        let mut stdout = Stdout;
                        let user_input = app.input.clone();
                        app.input.clear();
                        
                        // 1. Move to scroll area and print user message
                        set_cursor_position(0, (app.terminal_height - 4) as u64);
                        let _ = write!(stdout, "\n{}> {}{}{}\n", COLOR_USER, COLOR_BOLD, user_input, COLOR_RESET);
                        
                        if user_input.starts_with('/') {
                            // 2. Handle Command
                            let (res, output) = crate::handle_command(&user_input, model, provider, config, history, system_prompt);
                            if let Some(out) = output {
                                let _ = write!(stdout, "{}{}{}\n\n", COLOR_GRAY_BRIGHT, out, COLOR_RESET);
                                app.history.push(Message::new("system", &out));
                            }
                            if let CommandResult::Quit = res { break; }
                        } else {
                            // 3. Handle Chat
                            app.history.push(Message::new("user", &user_input));
                            history.clear();
                            history.extend(app.history.iter().cloned());

                            let _ = write!(stdout, "{}[MEOW] {}", COLOR_MEOW, COLOR_RESET);
                            let _ = write!(stdout, "{}", COLOR_MEOW);
                            
                            // Note: chat_once will stream to current position (in scroll area)
                            let _ = crate::chat_once(model, provider, &user_input, history, Some(context_window), system_prompt);
                            
                            let _ = write!(stdout, "{}\n", COLOR_RESET);
                            app.history = history.clone();
                            crate::compact_history(&mut app.history);
                        }
                    }
                },
                0x7F | 0x08 => {
                    app.input.pop();
                },
                12 => { // Ctrl-L: Re-probe
                    let (nw, nh) = probe_terminal_size();
                    app.terminal_width = nw; app.terminal_height = nh;
                    clear_screen();
                    print_greeting();
                    set_scroll_region(1, nh - 3);
                },
                c if c >= 0x20 && c <= 0x7E => {
                    app.input.push(c as char);
                },
                _ => {}
            }
        }
    }

    // Reset scroll region and attributes
    set_scroll_region(1, app.terminal_height);
    set_terminal_attributes(fd::STDIN, 0, old_mode_flags);
    show_cursor();
    Ok(())
}