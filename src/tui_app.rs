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

// Tokyo Night & Cyber-Steel Palette
const COLOR_VIOLET: &str = "\x1b[38;5;177m"; // User
const COLOR_BLUE: &str = "\x1b[38;5;111m";   // Meow
const COLOR_GRAY_DIM: &str = "\x1b[38;5;240m"; // Outer Frame
const COLOR_GRAY_BRIGHT: &str = "\x1b[38;5;250m"; // Headers
const COLOR_GREEN: &str = "\x1b[38;5;120m";  // Online status
const COLOR_YELLOW: &str = "\x1b[38;5;215m"; // Metrics
const COLOR_RESET: &str = "\x1b[0m";
const COLOR_BOLD: &str = "\x1b[1m";

// Box Drawing Characters
const BOX_TL: &str = "╔";
const BOX_TR: &str = "╗";
const BOX_BL: &str = "╚";
const BOX_BR: &str = "╝";
const BOX_H: &str = "═";
const BOX_V: &str = "║";
const BOX_ML: &str = "╠";
const BOX_MR: &str = "╣";
const BOX_SEP: &str = "─";
const BOX_SEPL: &str = "╟";
const BOX_SEPR: &str = "╢";

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
    pub frame_dirty: AtomicBool,
}

impl App {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            history: Vec::new(),
            scroll_offset: 0,
            terminal_width: 100,
            terminal_height: 25,
            input_dirty: AtomicBool::new(true),
            history_dirty: AtomicBool::new(true),
            frame_dirty: AtomicBool::new(true),
        }
    }

    fn draw_horizontal_line(&self, left: &str, mid: &str, right: &str) {
        let mut stdout = Stdout;
        let _ = write!(stdout, "{}", left);
        for _ in 0..(self.terminal_width.saturating_sub(2)) {
            let _ = write!(stdout, "{}", mid);
        }
        let _ = write!(stdout, "{}", right);
    }

    pub fn render_frame(&mut self, model: &str, provider: &str) {
        let mut stdout = Stdout;
        let w = self.terminal_width as usize;

        // 1. Top Bar
        set_cursor_position(0, 0);
        let header = " [ MEOW-CHAN v1.0 // NEURAL LINK ] ";
        let status = " [ ONLINE ] ";
        let _ = write!(stdout, "{}{}", COLOR_GRAY_DIM, BOX_TL);
        let _ = write!(stdout, "{}{}{}", COLOR_BOLD, COLOR_GRAY_BRIGHT, header);
        let h_line_len = w.saturating_sub(header.len() + status.len() + 2);
        let _ = write!(stdout, "{}", COLOR_GRAY_DIM);
        for _ in 0..h_line_len { let _ = write!(stdout, "{}", BOX_H); }
        let _ = write!(stdout, "{}{}{}{}", COLOR_GREEN, status, COLOR_GRAY_DIM, BOX_TR);

        // 2. Grid Info
        set_cursor_position(0, 1);
        let _ = write!(stdout, "{}{}", BOX_V, COLOR_RESET);
        let grid_info = format!(" > GRID: {} | PROV: {} ", model, provider);
        let _ = write!(stdout, "{}{}", COLOR_YELLOW, grid_info);
        let remaining = w.saturating_sub(grid_info.len() + 2);
        for _ in 0..remaining { let _ = write!(stdout, " "); }
        let _ = write!(stdout, "{}{}", COLOR_GRAY_DIM, BOX_V);

        // 3. Separator (Single Line)
        set_cursor_position(0, 2);
        let _ = write!(stdout, "{}", COLOR_GRAY_DIM);
        self.draw_horizontal_line(BOX_SEPL, BOX_SEP, BOX_SEPR);

        // 4. History area vertical borders
        let chat_height = self.terminal_height.saturating_sub(6) as usize;
        for y in 0..chat_height {
            set_cursor_position(0, (y + 3) as u64);
            let _ = write!(stdout, "{}{}", COLOR_GRAY_DIM, BOX_V);
            set_cursor_position((self.terminal_width - 1) as u64, (y + 3) as u64);
            let _ = write!(stdout, "{}{}", BOX_V, COLOR_RESET);
        }

        // 5. Middle Separator
        let mid_y = self.terminal_height.saturating_sub(3) as u64;
        set_cursor_position(0, mid_y);
        let _ = write!(stdout, "{}", COLOR_GRAY_DIM);
        self.draw_horizontal_line(BOX_ML, BOX_H, BOX_MR);

        // 6. Bottom Border
        set_cursor_position(0, (self.terminal_height - 1) as u64);
        self.draw_horizontal_line(BOX_BL, BOX_H, BOX_BR);
        let _ = write!(stdout, "{}", COLOR_RESET);
    }

    pub fn render_history(&mut self) {
        let mut stdout = Stdout;
        let chat_area_height = self.terminal_height.saturating_sub(6) as usize;
        let chat_area_width = self.terminal_width.saturating_sub(4) as usize;

        let mut lines = Vec::new();
        for msg in self.history.iter().skip(3) {
            let (prefix, color) = match msg.role.as_str() {
                "user" => ("> ", COLOR_VIOLET),
                "assistant" => ("[MEOW] ", COLOR_BLUE),
                _ => ("[*] ", COLOR_GRAY_BRIGHT),
            };

            let mut first_line = true;
            for chunk in msg.content.lines() {
                let mut content = chunk;
                if content.is_empty() {
                    lines.push(String::new());
                    continue;
                }

                while !content.is_empty() {
                    let line_prefix = if first_line { prefix } else { "       " };
                    let max_len = chat_area_width.saturating_sub(line_prefix.len());
                    
                    let mut byte_offset = content.len();
                    for (i, (b_idx, _)) in content.char_indices().enumerate() {
                        if i == max_len { byte_offset = b_idx; break; }
                    }

                    if byte_offset < content.len() {
                        if let Some(space_idx) = content[..byte_offset].rfind(' ') {
                            byte_offset = space_idx;
                        }
                    }

                    let line_text = &content[..byte_offset];
                    lines.push(format!("{}{}{}{}", color, line_prefix, line_text, COLOR_RESET));
                    
                    content = content[byte_offset..].trim_start();
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
            set_cursor_position(1, (y + 3) as u64);
            let inner_width = self.terminal_width.saturating_sub(2) as usize;
            let _ = write!(stdout, "{:width$}", "", width = inner_width);
            
            if let Some(line) = lines.get(display_start + y) {
                set_cursor_position(2, (y + 3) as u64);
                let _ = write!(stdout, "{}", line);
            }
            // Restore right border
            set_cursor_position((self.terminal_width - 1) as u64, (y + 3) as u64);
            let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, BOX_V, COLOR_RESET);
        }
    }

    pub fn render_input(&mut self, token_info: &str) {
        let mut stdout = Stdout;
        let input_y = self.terminal_height.saturating_sub(2) as u64;
        let w = self.terminal_width as usize;

        set_cursor_position(1, input_y);
        let _ = write!(stdout, "{:width$}", "", width = w - 2);
        
        set_cursor_position(2, input_y);
        let prompt = format!("{}{} {} > {}", COLOR_BOLD, COLOR_YELLOW, token_info, COLOR_RESET);
        let _ = write!(stdout, "{}{}{}", prompt, COLOR_VIOLET, self.input);

        let prompt_display_len = token_info.len() + 4; 
        let cursor_col = (2 + prompt_display_len + self.input.len()) as u64;
        set_cursor_position(cursor_col, input_y);
        
        set_cursor_position((self.terminal_width - 1) as u64, input_y);
        let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, BOX_V, COLOR_RESET);
    }
}

fn probe_terminal_size() -> (u16, u16) {
    let mut stdout = Stdout;
    // 1. Move cursor to extremely large position
    let _ = write!(stdout, "\x1b[999;999H");
    // 2. Query cursor position
    let _ = write!(stdout, "\x1b[6n");
    
    // 3. Read response: \x1b[row;colR
    let mut buf = [0u8; 32];
    let n = poll_input_event(500, &mut buf);
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
    (100, 25) // Default fallback
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
    
    // Auto-detect size
    let (w, h) = probe_terminal_size();
    app.terminal_width = w;
    app.terminal_height = h;
    
    clear_screen();

    loop {
        let current_tokens = crate::calculate_history_tokens(&app.history);
        let mem_kb = libakuma::memory_usage() / 1024;
        let mem_display = if mem_kb > 1024 { format!("{}M", mem_kb/1024) } else { format!("{}K", mem_kb) };
        let token_info = format!("[ {} / {}k ] [ {} ]", 
            current_tokens, 
            context_window / 1000,
            mem_display
        );

        if app.frame_dirty.load(Ordering::Acquire) {
            app.render_frame(model, &provider.name);
            app.frame_dirty.store(false, Ordering::Release);
            app.history_dirty.store(true, Ordering::Release);
            app.input_dirty.store(true, Ordering::Release);
        }

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
                0x1B => break,
                b'\r' | b'\n' => {
                    if !app.input.is_empty() {
                        let user_input = app.input.clone();
                        app.input.clear();
                        
                        if user_input.starts_with('/') {
                            let (res, output) = crate::handle_command(&user_input, model, provider, config, history, system_prompt);
                            if let Some(out) = output { app.history.push(Message::new("system", &out)); }
                            app.history_dirty.store(true, Ordering::Release);
                            app.input_dirty.store(true, Ordering::Release);
                            if let CommandResult::Quit = res { break; }
                        } else {
                            app.history.push(Message::new("user", &user_input));
                            app.history_dirty.store(true, Ordering::Release);
                            app.input_dirty.store(true, Ordering::Release);
                            app.render_history();
                            app.render_input(&token_info);

                            history.clear();
                            history.extend(app.history.iter().cloned());

                            let chat_area_height = app.terminal_height.saturating_sub(6) as usize;
                            let ai_start_y = (app.history.len() * 2).min(chat_area_height + 2);
                            set_cursor_position(2, ai_start_y as u64);
                            
                            let mut stdout = Stdout;
                            let _ = write!(stdout, "{}[MEOW] {}", COLOR_BLUE, COLOR_RESET);
                            let _ = write!(stdout, "{}", COLOR_BLUE);
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
                12 => { // Ctrl-L: Re-probe size
                    let (w, h) = probe_terminal_size();
                    app.terminal_width = w; app.terminal_height = h;
                    clear_screen();
                    app.frame_dirty.store(true, Ordering::Release);
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
