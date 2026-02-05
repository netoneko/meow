use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use libakuma::{
    get_terminal_attributes, set_terminal_attributes, 
    set_cursor_position, hide_cursor, show_cursor, 
    clear_screen, poll_input_event, write as akuma_write, fd
};

use crate::config::{Provider, Config, COLOR_USER, COLOR_MEOW, COLOR_GRAY_DIM, COLOR_GRAY_BRIGHT, COLOR_YELLOW, COLOR_RESET, COLOR_BOLD};
use crate::{Message, CommandResult};

// ANSI escapes
pub const SAVE_CURSOR: &str = "\x1b[s";
pub const RESTORE_CURSOR: &str = "\x1b[u";
const CLEAR_TO_EOL: &str = "\x1b[K";

pub static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static TERM_WIDTH: AtomicU16 = AtomicU16::new(100);
pub static TERM_HEIGHT: AtomicU16 = AtomicU16::new(25);
pub static CUR_COL: AtomicU16 = AtomicU16::new(0);
pub static INPUT_LEN: AtomicU16 = AtomicU16::new(0);

const CAT_ASCII: &str = r#"
                      =#=      .-
                      +*#*:.:-**
                      +%%#%##***
                      +%%%#%%#**.
                      +%@@@%%%+*:
          :::::--=+++*%@%@%%%%*-
     :-+##%%%%%%%%@%@#%%@%%%%##%+
  .=##%%%%%%%%%@@@@@%#%%@%%%@@@%%-
.*%%%%%%%@%%%%@%@@@@%%%%@%%%%%@@#-
%@%%%%%%%%%%@%%%%%@@@%%%@@@@@%%%#+
*%%%%%@%@@%@@@%%%%#%@@@@@@@%%@@%@@@*+--
 ::=+*#@@@@@@@@@@@%%%%%%@%%@#----=**@%@#
         .--+**%@@@@@%@@%%@@%*       :-.
                  ::::---#@%%*"#;

struct TuiGuard;
impl TuiGuard {
    fn new() -> Self {
        TUI_ACTIVE.store(true, Ordering::SeqCst);
        Self
    }
}
impl Drop for TuiGuard {
    fn drop(&mut self) {
        TUI_ACTIVE.store(false, Ordering::SeqCst);
    }
}

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

/// TUI-aware print function that handles wrapping, indentation, and cursor management.
pub fn tui_print(s: &str) {
    if s.is_empty() { return; }
    let mut stdout = Stdout;
    
    let w = TERM_WIDTH.load(Ordering::SeqCst);
    let mut col = CUR_COL.load(Ordering::SeqCst);

    // AI is currently at some position in the scroll area.
    // Restore that position before printing.
    let _ = write!(stdout, "{}", RESTORE_CURSOR);
    
    for c in s.chars() {
        if c == '\n' {
            let _ = write!(stdout, "\n         "); // 9 spaces indent on wrap/newline
            col = 9;
        } else if c == '\x08' { // Backspace
            if col > 9 { // Don't backspace into indentation
                col -= 1;
                let _ = akuma_write(fd::STDOUT, b"\x08");
            }
        } else {
            if col >= w - 1 {
                let _ = write!(stdout, "\n         ");
                col = 9;
            }
            let mut buf = [0u8; 4];
            let s_char = c.encode_utf8(&mut buf);
            let _ = akuma_write(fd::STDOUT, s_char.as_bytes());
            col += 1;
        }
    }
    CUR_COL.store(col, Ordering::SeqCst);

    // Save current AI position (for next token)
    let _ = write!(stdout, "{}", SAVE_CURSOR);
    
    // Move to prompt window
    let h = TERM_HEIGHT.load(Ordering::SeqCst);
    let input_pos = INPUT_LEN.load(Ordering::SeqCst);
    set_cursor_position(input_pos as u64, (h - 1) as u64);
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
    pub fn render_footer(&self, current_tokens: usize, token_limit: usize, mem_kb: usize) {
        let mut stdout = Stdout;
        let h = self.terminal_height as u64;
        let w = self.terminal_width as usize;

        // Format metrics
        let token_display = if current_tokens >= 1000 {
            format!("{}k", current_tokens / 1000)
        } else {
            format!("{}", current_tokens)
        };
        let limit_display = format!("{}k", token_limit / 1000);
        let mem_display = if mem_kb >= 1024 {
            format!("{}M", mem_kb / 1024)
        } else {
            format!("{}K", mem_kb)
        };

        // Construct prompt string
        let prompt_prefix = format!("[{}/{}|{}] (=^･ω･^=) > ", 
            token_display, limit_display, mem_display);
        
        // Update global input len for tui_print
        // We need the ACTUAL col position where the user is typing
        let prompt_width = prompt_prefix.chars().count();
        INPUT_LEN.store((prompt_width + self.input.chars().count()) as u16, Ordering::SeqCst);

        // Hide cursor during footer render to prevent flickering
        hide_cursor();

        // 1. Draw Separator (at Row h-2) - use heavy line ━
        set_cursor_position(0, h - 3);
        let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, "━".repeat(w), COLOR_RESET);

        // 2. Draw Status Bar (Row h-1) - now empty as info is in prompt
        set_cursor_position(0, h - 2);
        let _ = write!(stdout, "{}", CLEAR_TO_EOL);

        // 3. Draw Prompt (at Row h)
        set_cursor_position(0, h - 1);
        let _ = write!(stdout, "{}", CLEAR_TO_EOL);
        let _ = write!(stdout, "{}{}{}{}{}", COLOR_YELLOW, prompt_prefix, COLOR_RESET, COLOR_USER, self.input);

        // 4. Position Cursor at end of input
        set_cursor_position((prompt_width + self.input.chars().count()) as u64, h - 1);
        show_cursor();
    }
}

fn set_scroll_region(top: u16, bottom: u16) {
    let mut stdout = Stdout;
    let _ = write!(stdout, "\x1b[{};{}r", top, bottom);
}

fn print_greeting() {
    let mut stdout = Stdout;
    let _ = write!(stdout, "\n{}\x1b[38;5;236m", COLOR_RESET);
    let _ = write!(stdout, "{}", CAT_ASCII);
    let _ = write!(stdout, "{}\n  {}MEOW!{} ~(=^‥^)ノ\n\n", COLOR_RESET, COLOR_BOLD, COLOR_RESET);
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
    let _guard = TuiGuard::new();
    let mut old_mode_flags: u64 = 0;
    get_terminal_attributes(fd::STDIN, &mut old_mode_flags as *mut u64 as u64);
    set_terminal_attributes(fd::STDIN, 0, mode_flags::RAW_MODE_ENABLE);

    let mut app = App::new();
    app.history = history.clone();
    let (w, h) = probe_terminal_size();
    app.terminal_width = w; app.terminal_height = h;
    TERM_WIDTH.store(w, Ordering::SeqCst);
    TERM_HEIGHT.store(h, Ordering::SeqCst);
    
    clear_screen();
    // Scroll region ends at h-3 to leave room for 3-line footer
    set_scroll_region(1, h - 3);
    print_greeting();

    loop {
        let current_tokens = crate::calculate_history_tokens(&app.history);
        let mem_kb = libakuma::memory_usage() / 1024;

        app.render_footer(current_tokens, context_window, mem_kb);

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
                        
                        // Redraw footer IMMEDIATELY to clear input box while AI is thinking/streaming
                        app.render_footer(current_tokens, context_window, mem_kb);
                        
                        // 1. Move to scroll area and print user message
                        // We move to h-4 (bottom of scroll region)
                        set_cursor_position(0, (app.terminal_height - 4) as u64);
                        let _ = write!(stdout, "\n {}> {}{}{}\n", COLOR_USER, COLOR_BOLD, user_input, COLOR_RESET);
                        CUR_COL.store(0, Ordering::SeqCst);
                        
                        if user_input.starts_with('/') {
                            // 2. Handle Command
                            let (res, output) = crate::handle_command(&user_input, model, provider, config, history, system_prompt);
                            if let Some(out) = output {
                                let _ = write!(stdout, "  {}{}{}\n\n", COLOR_GRAY_BRIGHT, out, COLOR_RESET);
                                app.history.push(Message::new("system", &out));
                            }
                            if let CommandResult::Quit = res { break; }
                        } else {
                            // 3. Handle Chat
                            app.history.push(Message::new("user", &user_input));
                            history.clear();
                            history.extend(app.history.iter().cloned());

                            // 2 spaces prefix for [MEOW]
                            let _ = write!(stdout, "  {}[MEOW] {}", COLOR_MEOW, COLOR_RESET);
                            let _ = write!(stdout, "{}", COLOR_MEOW);
                            CUR_COL.store(9, Ordering::SeqCst); // "  [MEOW] " is 9 chars
                            let _ = write!(stdout, "{}", SAVE_CURSOR);
                            
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
                    TERM_WIDTH.store(nw, Ordering::SeqCst);
                    TERM_HEIGHT.store(nh, Ordering::SeqCst);
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