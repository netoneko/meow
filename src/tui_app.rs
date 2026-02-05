use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use libakuma::{
    get_terminal_attributes, set_terminal_attributes, 
    set_cursor_position, hide_cursor, show_cursor, 
    clear_screen, poll_input_event, write as akuma_write, fd
};

use crate::config::{Provider, Config, COLOR_MEOW, COLOR_GRAY_DIM, COLOR_GRAY_BRIGHT, COLOR_YELLOW, COLOR_RESET, COLOR_BOLD, COLOR_VIOLET};
use crate::{Message, CommandResult};

// ANSI escapes
const CLEAR_TO_EOL: &str = "\x1b[K";

pub static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static CANCELLED: AtomicBool = AtomicBool::new(false);
pub static TERM_WIDTH: AtomicU16 = AtomicU16::new(100);
pub static TERM_HEIGHT: AtomicU16 = AtomicU16::new(25);
pub static CUR_COL: AtomicU16 = AtomicU16::new(0);
pub static CUR_ROW: AtomicU16 = AtomicU16::new(0);
pub static INPUT_LEN: AtomicU16 = AtomicU16::new(0);
pub static CURSOR_IDX: AtomicU16 = AtomicU16::new(0);

static mut GLOBAL_INPUT: Option<String> = None;
static mut MESSAGE_QUEUE: Option<VecDeque<String>> = None;
static mut COMMAND_HISTORY: Option<Vec<String>> = None;
static mut HISTORY_INDEX: usize = 0;
static mut SAVED_INPUT: Option<String> = None;
static mut MODEL_NAME: Option<String> = None;
static mut PROVIDER_NAME: Option<String> = None;

fn get_global_input() -> &'static mut String {
    unsafe {
        if GLOBAL_INPUT.is_none() {
            GLOBAL_INPUT = Some(String::new());
        }
        GLOBAL_INPUT.as_mut().unwrap()
    }
}

pub fn get_message_queue() -> &'static mut VecDeque<String> {
    unsafe {
        if MESSAGE_QUEUE.is_none() {
            MESSAGE_QUEUE = Some(VecDeque::new());
        }
        MESSAGE_QUEUE.as_mut().unwrap()
    }
}

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
    
    let w = TERM_WIDTH.load(Ordering::SeqCst);
    let h = TERM_HEIGHT.load(Ordering::SeqCst);
    let mut col = CUR_COL.load(Ordering::SeqCst);
    let mut row = CUR_ROW.load(Ordering::SeqCst);

    // Jump to the current AI position in the scroll area.
    set_cursor_position(col as u64, row as u64);
    
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            let mut buf = [0u8; 4];
            akuma_write(fd::STDOUT, c.encode_utf8(&mut buf).as_bytes());
            // CSI sequence starts with '[', which is in the @ to ~ range but NOT a terminator.
            if c != '[' && c >= '@' && c <= '~' {
                in_esc = false;
            }
            continue;
        }
        
        if c == '\x1b' {
            in_esc = true;
            let mut buf = [0u8; 4];
            akuma_write(fd::STDOUT, c.encode_utf8(&mut buf).as_bytes());
            continue;
        }

        if c == '\n' {
            row += 1;
            // Limit to h-5 (Line 21 if h=25)
            if row > h - 5 {
                row = h - 5;
                akuma_write(fd::STDOUT, b"\n");
            } else {
                set_cursor_position(0, row as u64);
            }
            akuma_write(fd::STDOUT, b"         ");
            col = 9;
        } else if c == '\x08' { // Backspace
            if col > 9 {
                col -= 1;
                akuma_write(fd::STDOUT, b"\x08");
            }
        } else {
            if col >= w - 1 {
                row += 1;
                if row > h - 5 {
                    row = h - 5;
                    akuma_write(fd::STDOUT, b"\n");
                }
                set_cursor_position(0, row as u64);
                akuma_write(fd::STDOUT, b"         ");
                col = 9;
            }
            let mut buf = [0u8; 4];
            akuma_write(fd::STDOUT, c.encode_utf8(&mut buf).as_bytes());
            col += 1;
        }
    }
    
    CUR_COL.store(col, Ordering::SeqCst);
    CUR_ROW.store(row, Ordering::SeqCst);

    // Return cursor to prompt (handles wrapping and multiline)
    let input = get_global_input();
    let prompt_width = INPUT_LEN.load(Ordering::SeqCst) as usize;
    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let (cx, cy_off) = calculate_input_cursor(input, idx, prompt_width, w as usize);
    let cy = (h as u64 - 2) + cy_off;
    
    // Clamp cy to h-1 to prevent scroll trigger
    let clamped_cy = if cy >= h as u64 { h as u64 - 1 } else { cy };
    set_cursor_position(cx, clamped_cy);
}

/// Check if a cancellation request (ESC key) was received.
pub fn get_command_history() -> &'static mut Vec<String> {
    unsafe {
        if COMMAND_HISTORY.is_none() {
            COMMAND_HISTORY = Some(Vec::new());
        }
        COMMAND_HISTORY.as_mut().unwrap()
    }
}

pub fn get_saved_input() -> &'static mut String {
    unsafe {
        if SAVED_INPUT.is_none() {
            SAVED_INPUT = Some(String::new());
        }
        SAVED_INPUT.as_mut().unwrap()
    }
}

pub fn set_model_and_provider(model: &str, provider: &str) {
    unsafe {
        MODEL_NAME = Some(String::from(model));
        PROVIDER_NAME = Some(String::from(provider));
    }
}

pub fn get_model_and_provider() -> (String, String) {
    unsafe {
        (
            MODEL_NAME.as_ref().cloned().unwrap_or_else(|| String::from("unknown")),
            PROVIDER_NAME.as_ref().cloned().unwrap_or_else(|| String::from("unknown")),
        )
    }
}

/// Calculate the (x, y) coordinates of the cursor within the input box,
/// accounting for wrapping and explicit newlines.
fn calculate_input_cursor(input: &str, idx: usize, prompt_width: usize, width: usize) -> (u64, u64) {
    let mut cx = prompt_width;
    let mut cy = 0;
    
    for (i, c) in input.chars().enumerate() {
        if i >= idx { break; }
        
        if c == '\n' {
            cx = 0;
            cy += 1;
        } else {
            cx += 1;
            if cx >= width {
                cx = 0;
                cy += 1;
            }
        }
    }
    
    (cx as u64, cy as u64)
}

pub fn add_to_history(cmd: &str) {
    let history = get_command_history();
    // Don't add if it's the same as the last entry
    if history.is_empty() || history.last().unwrap() != cmd {
        history.push(String::from(cmd));
        if history.len() > 50 {
            history.remove(0);
        }
    }
    unsafe {
        HISTORY_INDEX = history.len();
    }
}

pub fn tui_is_cancelled() -> bool {
    CANCELLED.swap(false, Ordering::SeqCst)
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum InputEvent {
    Char(char),
    Backspace,
    Delete,
    Enter,
    ShiftEnter,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    AltLeft,
    AltRight,
    CtrlA,
    CtrlE,
    CtrlU,
    CtrlW,
    CtrlL,
    Esc,
    Unknown,
}

/// Parse raw input bytes into an InputEvent
fn parse_input(buf: &[u8]) -> (InputEvent, usize) {
    if buf.is_empty() { return (InputEvent::Unknown, 0); }
    
    match buf[0] {
        0x0D => {
            // Check for CRLF (\r\n) - often sent for Shift+Enter or just Enter
            if buf.len() >= 2 && buf[1] == 0x0A {
                return (InputEvent::ShiftEnter, 2);
            }
            (InputEvent::Enter, 1)
        }
        0x0A => (InputEvent::ShiftEnter, 1), // LF alone is almost always Shift+Enter in raw mode
        0x1B => { // ESC
            if buf.len() == 1 {
                return (InputEvent::Esc, 1);
            }
            
            // CSI sequences: ESC [ ...
            if buf.len() >= 3 && buf[1] == 0x5B {
                match buf[2] {
                    0x41 => return (InputEvent::Up, 3),
                    0x42 => return (InputEvent::Down, 3),
                    0x43 => return (InputEvent::Right, 3),
                    0x44 => return (InputEvent::Left, 3),
                    0x48 => return (InputEvent::Home, 3),
                    0x46 => return (InputEvent::End, 3),
                    b'3' if buf.len() >= 4 && buf[3] == b'~' => return (InputEvent::Delete, 4),
                    b'1' if buf.len() >= 6 && &buf[2..6] == b"1;3D" => return (InputEvent::AltLeft, 6),
                    b'1' if buf.len() >= 6 && &buf[2..6] == b"1;3C" => return (InputEvent::AltRight, 6),
                    b'1' if buf.len() >= 7 && &buf[2..7] == b"13;2u" => return (InputEvent::ShiftEnter, 7),
                    b'1' if buf.len() >= 7 && &buf[2..7] == b"13;5u" => return (InputEvent::ShiftEnter, 7), // Ctrl+Enter as ShiftEnter
                    b'2' if buf.len() >= 9 && &buf[2..9] == b"7;2;13~" => return (InputEvent::ShiftEnter, 9), // \x1b[27;2;13~
                    _ => {}
                }
            }
            
            // Alt+Enter / Alt+LF
            if buf.len() >= 2 && (buf[1] == b'\r' || buf[1] == b'\n') {
                return (InputEvent::ShiftEnter, 2);
            }
            
            // ESC O ... sequences (SS3)
            if buf.len() >= 3 && buf[1] == b'O' {
                match buf[2] {
                    b'A' => return (InputEvent::Up, 3),
                    b'B' => return (InputEvent::Down, 3),
                    b'C' => return (InputEvent::Right, 3),
                    b'D' => return (InputEvent::Left, 3),
                    b'H' => return (InputEvent::Home, 3),
                    b'F' => return (InputEvent::End, 3),
                    b'M' => return (InputEvent::Enter, 3), // Keypad Enter
                    _ => {}
                }
            }
            
            // Alt+b / Alt+f
            if buf.len() >= 2 && buf[1] == b'b' { return (InputEvent::AltLeft, 2); }
            if buf.len() >= 2 && buf[1] == b'f' { return (InputEvent::AltRight, 2); }
            
            // Unknown or incomplete sequence
            if buf.len() > 1 && buf[1] != 0x5B && buf[1] != b'O' {
                return (InputEvent::Esc, 1); // Treat as Esc and let next byte be processed
            }
            
            (InputEvent::Unknown, 1)
        }
        0x01 => (InputEvent::CtrlA, 1),
        0x05 => (InputEvent::CtrlE, 1),
        0x08 | 0x7F => (InputEvent::Backspace, 1),
        0x0C => (InputEvent::CtrlL, 1),
        0x15 => (InputEvent::CtrlU, 1),
        0x17 => (InputEvent::CtrlW, 1),
        c if c >= 0x20 && c <= 0x7E => (InputEvent::Char(c as char), 1),
        _ => (InputEvent::Unknown, 1),
    }
}

/// Unified handler for InputEvents
fn handle_input_event(
    event: InputEvent, 
    input: &mut String, 
    redraw: &mut bool, 
    quit: &mut bool, 
    exit_on_escape: bool,
) {
    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    match event {
        InputEvent::Char(c) => {
            input.insert(idx, c);
            CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::Backspace => {
            if idx > 0 && !input.is_empty() {
                input.remove(idx - 1);
                CURSOR_IDX.store((idx - 1) as u16, Ordering::SeqCst);
                *redraw = true;
            }
        }
        InputEvent::Delete => {
            if idx < input.chars().count() {
                input.remove(idx);
                *redraw = true;
            }
        }
        InputEvent::Left => {
            if idx > 0 {
                CURSOR_IDX.store((idx - 1) as u16, Ordering::SeqCst);
                *redraw = true;
            }
        }
        InputEvent::Right => {
            if idx < input.chars().count() {
                CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst);
                *redraw = true;
            }
        }
        InputEvent::Up => {
            let history = get_command_history();
            if !history.is_empty() {
                unsafe {
                    if HISTORY_INDEX == history.len() {
                        *get_saved_input() = input.clone();
                    }
                    if HISTORY_INDEX > 0 {
                        HISTORY_INDEX -= 1;
                        *input = history[HISTORY_INDEX].clone();
                        CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst);
                        *redraw = true;
                    }
                }
            }
        }
        InputEvent::Down => {
            let history = get_command_history();
            if !history.is_empty() {
                unsafe {
                    if HISTORY_INDEX < history.len() {
                        HISTORY_INDEX += 1;
                        if HISTORY_INDEX == history.len() {
                            *input = get_saved_input().clone();
                        } else {
                            *input = history[HISTORY_INDEX].clone();
                        }
                        CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst);
                        *redraw = true;
                    }
                }
            }
        }
        InputEvent::Home | InputEvent::CtrlA => {
            CURSOR_IDX.store(0, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::End | InputEvent::CtrlE => {
            CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::ShiftEnter => {
            input.insert(idx, '\n');
            CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::Enter => {
            if !input.is_empty() {
                let msg = input.clone();
                add_to_history(&msg);
                input.clear();
                CURSOR_IDX.store(0, Ordering::SeqCst);
                get_message_queue().push_back(msg);
                *redraw = true;
            }
        }
        InputEvent::CtrlU => {
            input.clear();
            CURSOR_IDX.store(0, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::CtrlW => {
            let old_idx = idx;
            let mut new_idx = idx;
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b == b' ') { new_idx -= 1; }
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b != b' ') { new_idx -= 1; }
            for _ in 0..(old_idx - new_idx) {
                if new_idx < input.len() {
                    input.remove(new_idx);
                }
            }
            CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::AltLeft => {
            let mut new_idx = idx;
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b == b' ') { new_idx -= 1; }
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b != b' ') { new_idx -= 1; }
            CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::AltRight => {
            let mut new_idx = idx;
            let len = input.chars().count();
            while new_idx < len && input.as_bytes().get(new_idx).map_or(false, |&b| b == b' ') { new_idx += 1; }
            while new_idx < len && input.as_bytes().get(new_idx).map_or(false, |&b| b != b' ') { new_idx += 1; }
            CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::CtrlL => {
            let (nw, nh) = probe_terminal_size();
            TERM_WIDTH.store(nw, Ordering::SeqCst);
            TERM_HEIGHT.store(nh, Ordering::SeqCst);
            clear_screen();
            print_greeting();
            set_scroll_region(1, nh - 4);
            // We can't update App's width/height from here directly easily, 
            // but the atomics will be used for future renders.
            *redraw = true;
        }
        InputEvent::Esc => {
            if exit_on_escape {
                *quit = true;
            } else {
                CANCELLED.store(true, Ordering::SeqCst);
            }
        }
        _ => {}
    }
}

/// Non-blocking check for input and redraw of footer during AI streaming.
pub fn tui_handle_input(current_tokens: usize, token_limit: usize, mem_kb: usize) {
    if !TUI_ACTIVE.load(Ordering::SeqCst) { return; }
    
    let mut event_buf = [0u8; 16];
    // Use a tiny timeout to allow multi-byte sequences to arrive
    let bytes_read = poll_input_event(10, &mut event_buf);
    
    if bytes_read > 0 {
        let mut consumed = 0usize;
        let bytes_read = bytes_read as usize;
        let input = get_global_input();
        let mut redraw = false;
        let mut quit = false;

        while consumed < bytes_read {
            let (event, n) = parse_input(&event_buf[consumed..bytes_read]);
            if n == 0 { break; }
            consumed += n;
            
            handle_input_event(event, input, &mut redraw, &mut quit, false);
        }
        
        if redraw {
            render_footer_internal(input, current_tokens, token_limit, mem_kb);
        }
    }
}

pub struct App {
    pub history: Vec<Message>,
    pub terminal_width: u16,
    pub terminal_height: u16,
}

impl App {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
            terminal_width: 100,
            terminal_height: 25,
        }
    }

    /// Renders the fixed bottom footer (separator, status, prompt).
    pub fn render_footer(&self, current_tokens: usize, token_limit: usize, mem_kb: usize) {
        let input = get_global_input();
        render_footer_internal(input, current_tokens, token_limit, mem_kb);
    }
}

fn render_footer_internal(input: &str, current_tokens: usize, token_limit: usize, mem_kb: usize) {
    let mut stdout = Stdout;
    let w = TERM_WIDTH.load(Ordering::SeqCst) as usize;
    let h = TERM_HEIGHT.load(Ordering::SeqCst) as u64;

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
    let queue_len = get_message_queue().len();
    let queue_display = if queue_len > 0 {
        format!(" [QUEUED: {}]", queue_len)
    } else {
        String::new()
    };

    let prompt_prefix = format!("  [{}/{}|{}]{} (=^･ω･^=) > ", 
        token_display, limit_display, mem_display, queue_display);
    
    let prompt_width = prompt_prefix.chars().count();
    INPUT_LEN.store(prompt_width as u16, Ordering::SeqCst);

    // Hide cursor during footer render to prevent flickering
    hide_cursor();

    // 1. Draw Separator (at Row h-4)
    set_cursor_position(0, h - 4);
    let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, "━".repeat(w), COLOR_RESET);

    // 2. Draw Status Bar (Row h-3) - Model and Provider info
    set_cursor_position(0, h - 3);
    let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    let (model, provider) = get_model_and_provider();
    let status_info = format!("  [Provider: {}] [Model: {}]", provider, model);
    let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, status_info, COLOR_RESET);

    // 3. Draw Prompt (starting at Row h-2, can wrap to Row h-1)
    set_cursor_position(0, h - 2);
    let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    set_cursor_position(0, h - 1);
    let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    
    set_cursor_position(0, h - 2);
    let _ = write!(stdout, "{}{}", COLOR_VIOLET, prompt_prefix);
    let _ = write!(stdout, "{}{}", COLOR_RESET, COLOR_VIOLET);
    
    // Manually render input to handle wraps and newlines
    let mut cur_cx = prompt_width;
    let mut cur_cy = h - 2;
    
    for c in input.chars() {
        if c == '\n' {
            cur_cx = 0;
            cur_cy += 1;
            if cur_cy < h {
                set_cursor_position(cur_cx as u64, cur_cy);
            }
        } else {
            let mut buf = [0u8; 4];
            let _ = write!(stdout, "{}", c.encode_utf8(&mut buf));
            cur_cx += 1;
            if cur_cx >= w {
                cur_cx = 0;
                cur_cy += 1;
                if cur_cy < h {
                    set_cursor_position(cur_cx as u64, cur_cy);
                }
            }
        }
        if cur_cy >= h { break; } // Out of footer bounds
    }
    let _ = write!(stdout, "{}", COLOR_RESET);

    // 4. Position Cursor (handles wrapping on Row h-2 and h-1)
    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let (cx, cy_off) = calculate_input_cursor(input, idx, prompt_width, w);
    let cy = (h - 2) + cy_off;
    
    // Clamp cy to h-1 to prevent scroll trigger
    let clamped_cy = if cy >= h { h - 1 } else { cy };
    set_cursor_position(cx, clamped_cy);
    show_cursor();
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
    // Increased timeout to 500ms for better reliability
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
    
    set_model_and_provider(model, &provider.name);
    
    let mut stdout = Stdout;
    // Enable Kitty keyboard protocol (for Shift+Enter detection in supported terminals)
    // Mode 1 = disambiguate escape codes, enables CSI u sequences for modified keys
    let _ = write!(stdout, "\x1b[>1u");
    let _ = write!(stdout, "\x1b[?1049h"); // Enter alternate screen
    clear_screen();
    // Scroll region ends at h-4 to leave room for 4-line footer
    set_scroll_region(1, h - 4);
    print_greeting();
    
    // Show initial hotkeys tip
    let _ = write!(stdout, "  {}TIP:{} Type {}/hotkeys{} to see input shortcuts nya~! ♪(=^･ω･^)ﾉ\n\n", 
        COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_YELLOW, COLOR_RESET);

    // Start AI cursor at the bottom of the scroll region (Line h-4 is row h-5)
    CUR_ROW.store(h - 5, Ordering::SeqCst);
    CUR_COL.store(0, Ordering::SeqCst);

    loop {
        let current_tokens = crate::calculate_history_tokens(&app.history);
        let mem_kb = libakuma::memory_usage() / 1024;

        app.render_footer(current_tokens, context_window, mem_kb);

        // Poll for input with a short timeout to allow for queue processing
        let mut event_buf = [0u8; 16];
        let bytes_read = poll_input_event(100, &mut event_buf);

        if bytes_read > 0 {
            let mut consumed = 0usize;
            let bytes_read = bytes_read as usize;
            let input = get_global_input();
            let mut quit_loop = false;
            let mut redraw = false;

            while consumed < bytes_read {
                let (event, n) = parse_input(&event_buf[consumed..bytes_read]);
                if n == 0 { break; }
                consumed += n;
                
                handle_input_event(event, input, &mut redraw, &mut quit_loop, config.exit_on_escape);
                if event == InputEvent::Enter { break; }
            }
            if quit_loop { break; }
        }

        // Process one message from the queue if available
        if let Some(user_input) = get_message_queue().pop_front() {
            let mut stdout = Stdout;
            
            // Redraw footer IMMEDIATELY to clear input box/show updated queue
            app.render_footer(current_tokens, context_window, mem_kb);
            
            // 1. Move to scroll area and print user message
            let row = (app.terminal_height - 5) as u64; 
            set_cursor_position(0, row);
            let _ = write!(stdout, "\n {}> {}{}{}\n", COLOR_VIOLET, COLOR_BOLD, user_input, COLOR_RESET);
            
            if user_input.starts_with('/') {
                // 2. Handle Command
                let (res, output) = crate::handle_command(&user_input, model, provider, config, history, system_prompt);
                if let Some(out) = output {
                    let _ = write!(stdout, "  \n{}{}{}\n\n", COLOR_GRAY_BRIGHT, out, COLOR_RESET);
                    // Add to both history vectors
                    let msg = Message::new("system", &out);
                    history.push(msg.clone());
                    app.history.push(msg);
                }
                if let CommandResult::Quit = res { break; }
                
                // Ensure history is synced if command modified it (e.g. /clear)
                app.history = history.clone();
                
                CUR_ROW.store(app.terminal_height - 5, Ordering::SeqCst);
                CUR_COL.store(0, Ordering::SeqCst);
            } else {
                // 3. Handle Chat
                app.history.push(Message::new("user", &user_input));
                history.clear();
                history.extend(app.history.iter().cloned());

                // 2 spaces prefix for [MEOW]
                let _ = write!(stdout, "  {}[MEOW] {}", COLOR_MEOW, COLOR_RESET);
                let _ = write!(stdout, "{}", COLOR_MEOW);
                
                // Track AI position for streaming (bottom of scroll region)
                CUR_ROW.store(app.terminal_height - 5, Ordering::SeqCst);
                CUR_COL.store(9, Ordering::SeqCst); // "  [MEOW] " is 9 chars
                
                // Note: chat_once will stream to current position (in scroll area)
                let _ = crate::chat_once(model, provider, &user_input, history, Some(context_window), system_prompt);
                
                let _ = write!(stdout, "{}\n", COLOR_RESET);
                app.history = history.clone();
                crate::compact_history(&mut app.history);
            }
        }
    }

    // Reset scroll region and attributes
    set_scroll_region(1, app.terminal_height);
    // Disable Kitty keyboard protocol
    let _ = write!(stdout, "\x1b[<u");
    set_terminal_attributes(fd::STDIN, 0, old_mode_flags);
    clear_screen();
    set_cursor_position(0, 0);
    show_cursor();
    Ok(())
}
