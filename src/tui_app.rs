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

// Mode flags
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
    pub input_dirty: AtomicBool,
}

impl App {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            history: Vec::new(),
            terminal_width: 100,
            terminal_height: 25,
            input_dirty: AtomicBool::new(true),
        }
    }

    /// Renders the bottom status bar and input box.
    pub fn render_footer(&mut self, token_info: &str) {
        let mut stdout = Stdout;
        let w = self.terminal_width as usize;
        let input_lines = (self.input.len() / (w - 10)) + 1;
        let start_y = (self.terminal_height as usize).saturating_sub(input_lines + 1);

        // 1. Draw Status Bar
        set_cursor_position(0, start_y as u64);
        let bar_text = format!(" [ {} ] ", token_info);
        let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, "─".repeat(w), COLOR_RESET);
        set_cursor_position(2, start_y as u64);
        let _ = write!(stdout, "{}{}{}{}", COLOR_BOLD, COLOR_YELLOW, bar_text, COLOR_RESET);

        // 2. Draw Input Box (Expanding)
        for i in 0..input_lines {
            let y = start_y + 1 + i;
            if y >= self.terminal_height as usize { break; }
            set_cursor_position(0, y as u64);
            let _ = write!(stdout, "{}", CLEAR_TO_EOL);
            if i == 0 {
                let _ = write!(stdout, "{}{}{} > {}", COLOR_BOLD, COLOR_YELLOW, "PROMPT", COLOR_RESET);
                let _ = write!(stdout, "{}{}", COLOR_USER, &self.input[..self.input.len().min(w - 10)]);
            } else {
                let start = (w - 10) + (i - 1) * w;
                if start < self.input.len() {
                    let _ = write!(stdout, "{}{}", COLOR_USER, &self.input[start..self.input.len().min(start + w)]);
                }
            }
        }

        // 3. Position Cursor
        let cursor_y = start_y + 1 + (self.input.len() / (w - 10));
        let cursor_x = if self.input.len() < (w - 10) { 9 + self.input.len() } else { self.input.len() % w };
        set_cursor_position(cursor_x as u64, cursor_y as u64);
    }
}

fn print_greeting() {
    let mut stdout = Stdout;
    let mut buf = [0u8; 4096];
    let fd = open("src/akuma_40.txt", open_flags::O_RDONLY);
    if fd >= 0 {
        let n = read_fd(fd, &mut buf);
        close(fd);
        if n > 0 {
            let _ = write!(stdout, "\n{}\x1b[38;5;236m", COLOR_RESET); // Dark grey background
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
    
    print_greeting();

    loop {
        let current_tokens = crate::calculate_history_tokens(&app.history);
        let mem_kb = libakuma::memory_usage() / 1024;
        let token_info = format!("TOKENS: {}/{}k | MEM: {}k", current_tokens, context_window / 1000, mem_kb);

        if app.input_dirty.load(Ordering::Acquire) {
            hide_cursor();
            app.render_footer(&token_info);
            app.input_dirty.store(false, Ordering::Release);
            show_cursor();
        }

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
                        
                        // 1. Move cursor above footer and print user message
                        let footer_height = 2; // Rough estimate
                        set_cursor_position(0, (app.terminal_height - footer_height - 1) as u64);
                        let _ = write!(stdout, "\n{}> {}{}{}\n", COLOR_USER, COLOR_BOLD, user_input, COLOR_RESET);
                        
                        // 2. Clear footer and sync history
                        app.input_dirty.store(true, Ordering::Release);
                        app.history.push(Message::new("user", &user_input));
                        history.clear();
                        history.extend(app.history.iter().cloned());

                        // 3. Start AI response
                        let _ = write!(stdout, "{}[MEOW] {}", COLOR_MEOW, COLOR_RESET);
                        let _ = write!(stdout, "{}", COLOR_MEOW);
                        
                        // chat_once will stream to current cursor position (above footer)
                        let _ = crate::chat_once(model, provider, &user_input, history, Some(context_window), system_prompt);
                        
                        let _ = write!(stdout, "{}\n", COLOR_RESET);

                        app.history = history.clone();
                        crate::compact_history(&mut app.history);
                        app.input_dirty.store(true, Ordering::Release);
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
    Ok(())
}
