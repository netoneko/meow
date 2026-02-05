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

/// Calculate gap between LLM output area and footer based on terminal size.
/// Returns 3-5 lines depending on terminal height to prevent repaint interference.
fn output_footer_gap() -> u16 {
    let h = TERM_HEIGHT.load(Ordering::SeqCst);
    if h >= 40 {
        5 // Large terminals get more buffer
    } else if h >= 30 {
        4
    } else {
        3 // Minimum gap for smaller terminals
    }
}

pub static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static CANCELLED: AtomicBool = AtomicBool::new(false);
pub static STREAMING: AtomicBool = AtomicBool::new(false);
pub static TERM_WIDTH: AtomicU16 = AtomicU16::new(100);
pub static TERM_HEIGHT: AtomicU16 = AtomicU16::new(25);
pub static CUR_COL: AtomicU16 = AtomicU16::new(0);
pub static CUR_ROW: AtomicU16 = AtomicU16::new(0);
pub static INPUT_LEN: AtomicU16 = AtomicU16::new(0);
pub static CURSOR_IDX: AtomicU16 = AtomicU16::new(0);
pub static PROMPT_SCROLL_TOP: AtomicU16 = AtomicU16::new(0);
pub static FOOTER_HEIGHT: AtomicU16 = AtomicU16::new(4);

static mut GLOBAL_INPUT: Option<String> = None;
static mut MESSAGE_QUEUE: Option<VecDeque<String>> = None;
static mut COMMAND_HISTORY: Option<Vec<String>> = None;
static mut HISTORY_INDEX: usize = 0;
static mut SAVED_INPUT: Option<String> = None;
static mut MODEL_NAME: Option<String> = None;
static mut PROVIDER_NAME: Option<String> = None;
static mut RAW_INPUT_QUEUE: Option<VecDeque<u8>> = None;
static mut LAST_INPUT_TIME: u64 = 0;

fn get_raw_input_queue() -> &'static mut VecDeque<u8> {
    unsafe {
        if RAW_INPUT_QUEUE.is_none() {
            RAW_INPUT_QUEUE = Some(VecDeque::with_capacity(64));
        }
        RAW_INPUT_QUEUE.as_mut().unwrap()
    }
}

fn count_wrapped_lines(input: &str, prompt_width: usize, width: usize) -> usize {
    if width == 0 { return 1; }
    let mut lines = 1;
    let mut current_col = prompt_width;
    for c in input.chars() {
        if c == '\n' {
            lines += 1;
            current_col = 0;
        } else {
            current_col += 1;
            if current_col >= width {
                lines += 1;
                current_col = 0;
            }
        }
    }
    lines
}

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
            let f_h = FOOTER_HEIGHT.load(Ordering::SeqCst);
            let gap = output_footer_gap();
            let max_row = h - (f_h + 1 + gap);
            if row > max_row {
                row = max_row;
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
                let f_h = FOOTER_HEIGHT.load(Ordering::SeqCst);
                let gap = output_footer_gap();
                let max_row = h - (f_h + 1 + gap);
                if row > max_row {
                    row = max_row;
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

    // Return cursor to prompt
    let input = get_global_input();
    let prompt_prefix_len = INPUT_LEN.load(Ordering::SeqCst) as usize;
    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let (cx, cy_off) = calculate_input_cursor(input, idx, prompt_prefix_len, w as usize);
    
    let f_h = FOOTER_HEIGHT.load(Ordering::SeqCst);
    let scroll_top = PROMPT_SCROLL_TOP.load(Ordering::SeqCst) as u64;
    
    // Prompt area starts at Row h - f_h + 2
    let prompt_start_row = h as u64 - f_h as u64 + 2;
    let final_cy = prompt_start_row + (cy_off - scroll_top);
    
    // Clamp to prompt area bounds
    let clamped_cy = if final_cy >= h as u64 { h as u64 - 1 } else { final_cy };
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
    if width == 0 { return (0, 0); }
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
    Interrupt,
    Unknown,
}

/// Parse raw input bytes into an InputEvent
fn parse_input(buf: &[u8]) -> (InputEvent, usize) {
    if buf.is_empty() { return (InputEvent::Unknown, 0); }
    
    match buf[0] {
        0x03 => (InputEvent::Interrupt, 1), // Ctrl+C
        0x0D => {
            if buf.len() >= 2 && buf[1] == 0x0A {
                return (InputEvent::ShiftEnter, 2);
            }
            (InputEvent::Enter, 1)
        }
        0x0A => (InputEvent::ShiftEnter, 1),
        0x1B => { // ESC
            if buf.len() == 1 {
                let now = libakuma::uptime();
                unsafe {
                    if now.saturating_sub(LAST_INPUT_TIME) > 50000 {
                        return (InputEvent::Esc, 1);
                    } else {
                        return (InputEvent::Unknown, 0); 
                    }
                }
            }
            
            if buf[1] == 0x5B { // CSI ESC [
                let mut i = 2;
                while i < buf.len() {
                    let c = buf[i];
                    if (0x40..=0x7E).contains(&c) {
                        let len = i + 1;
                        let seq = &buf[2..len-1];
                        match c {
                            b'A' => return (InputEvent::Up, len),
                            b'B' => return (InputEvent::Down, len),
                            b'C' => {
                                if seq == b"1;3" { return (InputEvent::AltRight, len); }
                                return (InputEvent::Right, len);
                            }
                            b'D' => {
                                if seq == b"1;3" { return (InputEvent::AltLeft, len); }
                                return (InputEvent::Left, len);
                            }
                            b'H' => return (InputEvent::Home, len),
                            b'F' => return (InputEvent::End, len),
                            b'~' => {
                                if seq == b"3" { return (InputEvent::Delete, len); }
                                return (InputEvent::Unknown, len);
                            }
                            b'u' => {
                                if seq == b"13;2" || seq == b"13;5" { return (InputEvent::ShiftEnter, len); }
                                return (InputEvent::Unknown, len);
                            }
                            _ => return (InputEvent::Unknown, len),
                        }
                    }
                    i += 1;
                }
                if buf.len() >= 8 { return (InputEvent::Unknown, 1); }
                return (InputEvent::Unknown, 0); 
            }
            
            if buf[1] == 0x4F { // SS3 ESC O
                if buf.len() >= 3 {
                    let len = 3;
                    match buf[2] {
                        b'A' => return (InputEvent::Up, len),
                        b'B' => return (InputEvent::Down, len),
                        b'C' => return (InputEvent::Right, len),
                        b'D' => return (InputEvent::Left, len),
                        b'H' => return (InputEvent::Home, len),
                        b'F' => return (InputEvent::End, len),
                        b'M' => return (InputEvent::Enter, len),
                        _ => return (InputEvent::Unknown, len),
                    }
                }
                return (InputEvent::Unknown, 0);
            }
            
            if buf[1] == b'\r' || buf[1] == b'\n' { return (InputEvent::ShiftEnter, 2); }
            if buf[1] == b'b' { return (InputEvent::AltLeft, 2); }
            if buf[1] == b'f' { return (InputEvent::AltRight, 2); }

            return (InputEvent::Esc, 1);
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
    let mut idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let w = TERM_WIDTH.load(Ordering::SeqCst) as usize;
    let prompt_prefix_len = INPUT_LEN.load(Ordering::SeqCst) as usize;

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
            let (cx, cy) = calculate_input_cursor(input, idx, prompt_prefix_len, w);
            if cy > 0 {
                let mut new_idx = 0;
                let mut cur_cx = prompt_prefix_len;
                let mut cur_cy = 0;
                for (i, c) in input.chars().enumerate() {
                    if cur_cy == cy - 1 && cur_cx as u64 == cx {
                        new_idx = i;
                        break;
                    }
                    if c == '\n' {
                        if cur_cy == cy - 1 { new_idx = i; }
                        cur_cx = 0; cur_cy += 1;
                    } else {
                        cur_cx += 1;
                        if cur_cx >= w {
                            if cur_cy == cy - 1 { new_idx = i; }
                            cur_cx = 0; cur_cy += 1;
                        }
                    }
                    if cur_cy > cy - 1 { break; }
                }
                CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst);
                *redraw = true;
            } else {
                let history = get_command_history();
                if !history.is_empty() {
                    unsafe {
                        if HISTORY_INDEX == history.len() { *get_saved_input() = input.clone(); }
                        if HISTORY_INDEX > 0 {
                            HISTORY_INDEX -= 1;
                            *input = history[HISTORY_INDEX].clone();
                            CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst);
                            *redraw = true;
                        }
                    }
                }
            }
        }
        InputEvent::Down => {
            let (cx, cy) = calculate_input_cursor(input, idx, prompt_prefix_len, w);
            let total_lines = count_wrapped_lines(input, prompt_prefix_len, w);
            if cy < (total_lines as u64).saturating_sub(1) {
                let mut new_idx = input.chars().count();
                let mut cur_cx = prompt_prefix_len;
                let mut cur_cy = 0;
                for (i, c) in input.chars().enumerate() {
                    if cur_cy == cy + 1 && cur_cx as u64 == cx {
                        new_idx = i;
                        break;
                    }
                    if c == '\n' {
                        cur_cx = 0; cur_cy += 1;
                    } else {
                        cur_cx += 1;
                        if cur_cx >= w { cur_cx = 0; cur_cy += 1; }
                    }
                }
                CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst);
                *redraw = true;
            } else {
                let history = get_command_history();
                if !history.is_empty() {
                    unsafe {
                        if HISTORY_INDEX < history.len() {
                            HISTORY_INDEX += 1;
                            if HISTORY_INDEX == history.len() { *input = get_saved_input().clone(); }
                            else { *input = history[HISTORY_INDEX].clone(); }
                            CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst);
                            *redraw = true;
                        }
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
                PROMPT_SCROLL_TOP.store(0, Ordering::SeqCst);
                get_message_queue().push_back(msg);
                *redraw = true;
            }
        }
        InputEvent::CtrlU => {
            input.clear();
            CURSOR_IDX.store(0, Ordering::SeqCst);
            PROMPT_SCROLL_TOP.store(0, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::CtrlW => {
            let old_idx = idx;
            let mut new_idx = idx;
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b == b' ') { new_idx -= 1; }
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b != b' ') { new_idx -= 1; }
            for _ in 0..(old_idx - new_idx) {
                if new_idx < input.len() { input.remove(new_idx); }
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
            set_scroll_region(1, nh - FOOTER_HEIGHT.load(Ordering::SeqCst) - output_footer_gap());
            *redraw = true;
        }
        InputEvent::Esc | InputEvent::Interrupt => {
            // Always set CANCELLED to stop any ongoing LLM request
            CANCELLED.store(true, Ordering::SeqCst);
            // Quit the input loop if exit_on_escape is set OR if Ctrl+C was pressed
            if exit_on_escape || event == InputEvent::Interrupt { 
                *quit = true; 
            }
        }
        _ => {}
    }
}

pub fn tui_handle_input(current_tokens: usize, token_limit: usize, mem_kb: usize) {
    if !TUI_ACTIVE.load(Ordering::SeqCst) { return; }
    
    let mut event_buf = [0u8; 16];
    let bytes_read = poll_input_event(0, &mut event_buf);
    
    let queue = get_raw_input_queue();
    if bytes_read > 0 {
        for i in 0..bytes_read as usize { queue.push_back(event_buf[i]); }
        unsafe { LAST_INPUT_TIME = libakuma::uptime(); }
    }

    let input = get_global_input();
    let mut redraw = false;
    let mut quit = false;

    while !queue.is_empty() {
        let mut temp_buf = [0u8; 16];
        let n_copy = core::cmp::min(queue.len(), 16);
        for i in 0..n_copy { temp_buf[i] = queue[i]; }
        
        let (event, n) = parse_input(&temp_buf[..n_copy]);
        if n == 0 { break; }
        for _ in 0..n { queue.pop_front(); }
        handle_input_event(event, input, &mut redraw, &mut quit, false);
    }
    
    if redraw { render_footer_internal(input, current_tokens, token_limit, mem_kb); }
}

pub struct App {
    pub history: Vec<Message>,
    pub terminal_width: u16,
    pub terminal_height: u16,
}

impl App {
    pub fn new() -> Self {
        Self { history: Vec::new(), terminal_width: 100, terminal_height: 25 }
    }
    pub fn render_footer(&self, current_tokens: usize, token_limit: usize, mem_kb: usize) {
        let input = get_global_input();
        render_footer_internal(input, current_tokens, token_limit, mem_kb);
    }
}

fn render_footer_internal(input: &str, current_tokens: usize, token_limit: usize, mem_kb: usize) {
    let mut stdout = Stdout;
    let w = TERM_WIDTH.load(Ordering::SeqCst) as usize;
    let h = TERM_HEIGHT.load(Ordering::SeqCst) as u64;

    let token_display = if current_tokens >= 1000 { format!("{}k", current_tokens / 1000) } else { format!("{}", current_tokens) };
    let limit_display = format!("{}k", token_limit / 1000);
    let mem_display = if mem_kb >= 1024 { format!("{}M", mem_kb / 1024) } else { format!("{}K", mem_kb) };

    let queue_len = get_message_queue().len();
    let queue_display = if queue_len > 0 { format!(" [QUEUED: {}]", queue_len) } else { String::new() };

    let prompt_prefix = format!("  [{}/{}|{}]{} (=^･ω･^=) > ", token_display, limit_display, mem_display, queue_display);
    let prompt_prefix_len = prompt_prefix.chars().count();
    INPUT_LEN.store(prompt_prefix_len as u16, Ordering::SeqCst);

    let wrapped_lines = count_wrapped_lines(input, prompt_prefix_len, w);
    let max_prompt_lines = core::cmp::min(10, (h / 3) as usize);
    let display_prompt_lines = core::cmp::min(wrapped_lines, max_prompt_lines);
    let new_footer_height = (display_prompt_lines + 2) as u16; 
    let old_footer_height = FOOTER_HEIGHT.load(Ordering::SeqCst);
    let is_streaming = STREAMING.load(Ordering::SeqCst);
    let gap = output_footer_gap();
    
    // During streaming: allow footer to GROW but not SHRINK
    // (shrinking would leave old separator lines that we can't safely clear during streaming)
    let effective_footer_height = if is_streaming && new_footer_height < old_footer_height {
        old_footer_height
    } else {
        new_footer_height
    };
    let effective_prompt_lines = (effective_footer_height as usize).saturating_sub(2);
    
    // Handle footer height changes
    if effective_footer_height != old_footer_height {
        FOOTER_HEIGHT.store(effective_footer_height, Ordering::SeqCst);
        
        if effective_footer_height > old_footer_height {
            // Footer is GROWING - update scroll region and clamp CUR_ROW
            set_scroll_region(1, (h as u16) - effective_footer_height - gap);
            
            let cur_row = CUR_ROW.load(Ordering::SeqCst);
            let max_row = h as u16 - (effective_footer_height + 1 + gap);
            if cur_row > max_row {
                CUR_ROW.store(max_row, Ordering::SeqCst);
            }
        } else {
            // Footer is SHRINKING (only happens when not streaming)
            set_scroll_region(1, (h as u16) - effective_footer_height - gap);
        }
    }

    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let (_cx_abs, cy_off_abs) = calculate_input_cursor(input, idx, prompt_prefix_len, w);
    let mut scroll_top = PROMPT_SCROLL_TOP.load(Ordering::SeqCst);
    if cy_off_abs < scroll_top as u64 { scroll_top = cy_off_abs as u16; } 
    else if cy_off_abs >= (scroll_top as u64 + effective_prompt_lines as u64) { scroll_top = (cy_off_abs - effective_prompt_lines as u64 + 1) as u64 as u16; }
    PROMPT_SCROLL_TOP.store(scroll_top, Ordering::SeqCst);

    hide_cursor();
    let separator_row = h - effective_footer_height as u64;
    
    // When footer shrinks, clear old footer lines that are now in the gap area.
    // Only clear from old separator position to new separator position.
    // This never touches the scroll region (LLM output area).
    if effective_footer_height < old_footer_height {
        let old_separator_row = h - old_footer_height as u64;
        for row in old_separator_row..separator_row {
            set_cursor_position(0, row);
            let _ = write!(stdout, "{}", CLEAR_TO_EOL);
        }
    }
    
    set_cursor_position(0, separator_row);
    let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, "━".repeat(w), COLOR_RESET);

    let status_row = separator_row + 1;
    set_cursor_position(0, status_row);
    let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    let (model, provider) = get_model_and_provider();
    let status_info = format!("  [Provider: {}] [Model: {}]", provider, model);
    let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, status_info, COLOR_RESET);

    for i in 0..effective_prompt_lines {
        set_cursor_position(0, status_row + 1 + i as u64);
        let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    }

    let mut current_line = 0;
    let mut current_col = prompt_prefix_len;
    if scroll_top == 0 {
        set_cursor_position(0, status_row + 1);
        let _ = write!(stdout, "{}{}{}{}", COLOR_VIOLET, COLOR_BOLD, prompt_prefix, COLOR_RESET);
    }
    let _ = write!(stdout, "{}", COLOR_VIOLET);
    for c in input.chars() {
        if c == '\n' {
            current_line += 1; current_col = 0;
        } else {
            if current_line >= scroll_top as usize && current_line < (scroll_top as usize + effective_prompt_lines) {
                let target_row = status_row + 1 + (current_line as u64 - scroll_top as u64);
                set_cursor_position(current_col as u64, target_row);
                let mut buf = [0u8; 4];
                let _ = write!(stdout, "{}", c.encode_utf8(&mut buf));
            }
            current_col += 1;
            if current_col >= w { current_line += 1; current_col = 0; }
        }
    }
    let _ = write!(stdout, "{}", COLOR_RESET);

    let (cx, cy_off) = calculate_input_cursor(input, idx, prompt_prefix_len, w);
    let final_cy = status_row + 1 + (cy_off - scroll_top as u64);
    set_cursor_position(cx, final_cy);
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
    let _ = write!(stdout, "\x1b[>1u");
    let _ = write!(stdout, "\x1b[?1049h");
    clear_screen();
    let f_h = FOOTER_HEIGHT.load(Ordering::SeqCst);
    let gap = output_footer_gap();
    set_scroll_region(1, h - f_h - gap);
    print_greeting();
    let _ = write!(stdout, "  {}TIP:{} Type {}/hotkeys{} to see input shortcuts nya~! ♪(=^･ω･^)ﾉ\n\n", COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_YELLOW, COLOR_RESET);

    CUR_ROW.store(h - (f_h + 1 + gap), Ordering::SeqCst);
    CUR_COL.store(0, Ordering::SeqCst);

    loop {
        let current_tokens = crate::calculate_history_tokens(&app.history);
        let mem_kb = libakuma::memory_usage() / 1024;
        app.render_footer(current_tokens, context_window, mem_kb);

        let mut event_buf = [0u8; 16];
        let bytes_read = poll_input_event(50, &mut event_buf);
        let queue = get_raw_input_queue();
        if bytes_read > 0 {
            for i in 0..bytes_read as usize { queue.push_back(event_buf[i]); }
            unsafe { LAST_INPUT_TIME = libakuma::uptime(); }
        }

        let input = get_global_input();
        let mut quit_loop = false;
        let mut redraw = false;

        while !queue.is_empty() {
            let mut temp_buf = [0u8; 16];
            let n_copy = core::cmp::min(queue.len(), 16);
            for i in 0..n_copy { temp_buf[i] = queue[i]; }
            let (event, n) = parse_input(&temp_buf[..n_copy]);
            if n == 0 { break; }
            for _ in 0..n { queue.pop_front(); }
            handle_input_event(event, input, &mut redraw, &mut quit_loop, config.exit_on_escape);
            if event == InputEvent::Enter { break; }
        }
        if quit_loop { break; }

        if let Some(user_input) = get_message_queue().pop_front() {
            app.render_footer(current_tokens, context_window, mem_kb);
            let f_h = FOOTER_HEIGHT.load(Ordering::SeqCst);
            let gap = output_footer_gap();
            let mut stdout = Stdout;
            
            // Position cursor in the output area before printing
            let cur_row = CUR_ROW.load(Ordering::SeqCst);
            set_cursor_position(0, cur_row as u64);
            
            let _ = write!(stdout, "\n {}> {}{}{}\n", COLOR_VIOLET, COLOR_BOLD, user_input, COLOR_RESET);
            if user_input.starts_with('/') {
                let (res, output) = crate::handle_command(&user_input, model, provider, config, history, system_prompt);
                if let Some(out) = output {
                    let _ = write!(stdout, "  \n{}{}{}\n\n", COLOR_GRAY_BRIGHT, out, COLOR_RESET);
                    let msg = Message::new("system", &out);
                    history.push(msg.clone()); app.history.push(msg);
                }
                if let CommandResult::Quit = res { break; }
                app.history = history.clone();
                CUR_ROW.store(app.terminal_height - (f_h + 1 + gap), Ordering::SeqCst);
                CUR_COL.store(0, Ordering::SeqCst);
            } else {
                app.history.push(Message::new("user", &user_input));
                history.clear(); history.extend(app.history.iter().cloned());
                let _ = write!(stdout, "  {}[MEOW] {}", COLOR_MEOW, COLOR_RESET);
                let _ = write!(stdout, "{}", COLOR_MEOW);
                CUR_ROW.store(app.terminal_height - (f_h + 1 + gap), Ordering::SeqCst);
                CUR_COL.store(9, Ordering::SeqCst);
                STREAMING.store(true, Ordering::SeqCst);
                let _ = crate::chat_once(model, provider, &user_input, history, Some(context_window), system_prompt);
                STREAMING.store(false, Ordering::SeqCst);
                CANCELLED.store(false, Ordering::SeqCst); // Clear cancel flag after streaming ends
                let _ = write!(stdout, "{}\n", COLOR_RESET);
                app.history = history.clone(); crate::compact_history(&mut app.history);
            }
        }
    }

    set_scroll_region(1, app.terminal_height);
    let mut stdout = Stdout;
    let _ = write!(stdout, "\x1b[<u");
    set_terminal_attributes(fd::STDIN, 0, old_mode_flags);
    clear_screen();
    set_cursor_position(0, 0);
    show_cursor();
    Ok(())
}