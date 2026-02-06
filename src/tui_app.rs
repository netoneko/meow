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

use crate::config::{Provider, Config, COLOR_GRAY_DIM, COLOR_GRAY_BRIGHT, COLOR_YELLOW, COLOR_RESET, COLOR_BOLD, COLOR_VIOLET};
use crate::{Message, CommandResult};

// ANSI escapes
const CLEAR_TO_EOL: &str = "\x1b[K";

// =============================================================================
// PaneLayout - Three-pane TUI state management
// =============================================================================

/// The current state of the TUI, driving what is displayed and how input is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiState {
    /// Waiting for user to type and press Enter.
    Idle,
    /// DNS resolution and TCP connection in progress.
    Connecting,
    /// Waiting for the first byte of the LLM response.
    WaitingForResponse,
    /// Streaming the LLM response.
    Streaming,
    /// A local command (e.g. /help) or tool is processing.
    Processing,
    /// The application is shutting down.
    Exiting,
}

/// Manages the three-pane TUI layout:
/// - Output pane (top): scrollable LLM output
/// - Status pane (middle): connection status, timing
/// - Footer pane (bottom): prompt input, model info
pub struct PaneLayout {
    pub state: TuiState,
    pub term_width: u16,
    pub term_height: u16,
    
    // Output pane - scrollable region
    pub output_top: u16,        // Always 1
    pub output_bottom: u16,     // Calculated based on footer size
    pub output_row: u16,        // Current cursor row in output
    pub output_col: u16,        // Current cursor col in output
    
    // Status pane - single fixed row
    pub status_row: u16,        // Row for status display
    pub status_text: String,    // Current status text (e.g., "[MEOW]")
    pub status_color: &'static str, // Color for status text
    pub status_dots: u8,        // Number of dots for progress
    pub status_time_ms: Option<u64>, // Timing info
    pub status_start_us: u64,   // Start time for current status
    
    // Footer pane - prompt and info
    pub footer_top: u16,        // Separator row
    pub footer_height: u16,     // Total footer height (2 + prompt lines)
    pub prompt_scroll: u16,     // Scroll offset within prompt
    pub cursor_idx: u16,        // Cursor position in input
    pub input_prefix_len: u16,  // Length of prompt prefix
    
    // Animation state
    pub repaint_counter: u16,   // Counter for slow animations (dots update every 10th repaint)
}

impl PaneLayout {
    /// Create a new pane layout for the given terminal dimensions.
    /// Layout from top to bottom:
    ///   - Output pane (rows 1 to output_bottom, scrollable)
    ///   - Gap (buffer zone)
    ///   - Status pane (1 row, just above footer separator)
    ///   - Footer (separator + provider info + prompt)
    pub fn new(width: u16, height: u16) -> Self {
        let footer_height = 4; // Initial: separator + provider + 2 prompt lines
        let gap = Self::calculate_gap(height);
        // Status row is 1 row above footer separator
        let separator_row = height.saturating_sub(footer_height);
        let status_row = separator_row.saturating_sub(1);
        // Output bottom is above the gap
        let output_bottom = status_row.saturating_sub(gap);
        let footer_top = separator_row;
        
        Self {
            state: TuiState::Idle,
            term_width: width,
            term_height: height,
            output_top: 1,
            output_bottom,
            output_row: output_bottom,
            output_col: 0,
            status_row,
            status_text: String::new(),
            status_color: "\x1b[38;5;242m", // COLOR_GRAY_DIM
            status_dots: 0,
            status_time_ms: None,
            status_start_us: 0,
            footer_top,
            footer_height,
            prompt_scroll: 0,
            cursor_idx: 0,
            input_prefix_len: 0,
            repaint_counter: 0,
        }
    }
    
    /// Calculate the gap between output and status based on terminal height.
    fn calculate_gap(height: u16) -> u16 {
        if height >= 40 {
            5 // Large terminals get more buffer
        } else if height >= 30 {
            4
        } else {
            3 // Minimum gap for smaller terminals
        }
    }
    
    /// Get the current gap value.
    pub fn gap(&self) -> u16 {
        Self::calculate_gap(self.term_height)
    }
    
    /// Recalculate pane boundaries when footer height changes.
    pub fn recalculate(&mut self, new_footer_height: u16) {
        let gap = self.gap();
        self.footer_height = new_footer_height;
        // Status row is 1 row above footer separator
        let separator_row = self.term_height.saturating_sub(new_footer_height);
        self.status_row = separator_row.saturating_sub(1);
        // Output bottom is above the gap
        self.output_bottom = self.status_row.saturating_sub(gap);
        self.footer_top = separator_row;
        
        // Clamp output cursor if needed
        if self.output_row > self.output_bottom {
            self.output_row = self.output_bottom;
        }
    }
    
    /// Set scroll region to only include output pane.
    pub fn set_scroll_region(&self) {
        let mut stdout = Stdout;
        // Scroll region covers from top to output_bottom (inclusive)
        // Use output_bottom + 1 for the ANSI escape which is inclusive
        let _ = write!(stdout, "\x1b[{};{}r", self.output_top, self.output_bottom + 1);
    }
    
    /// Reset scroll region to full terminal.
    pub fn reset_scroll_region(&self) {
        let mut stdout = Stdout;
        let _ = write!(stdout, "\x1b[1;{}r", self.term_height);
    }
    
    /// Render the status pane at status_row (just above separator).
    /// Does NOT restore cursor position - caller should handle cursor placement.
    pub fn render_status(&self) {
        let mut stdout = Stdout;
        
        // Move to status row and clear it
        set_cursor_position(0, self.status_row as u64);
        let _ = write!(stdout, "{}", CLEAR_TO_EOL);
        
        // Draw status content
        if !self.status_text.is_empty() {
            let _ = write!(stdout, "  {}{}", COLOR_GRAY_DIM, self.status_text);
            
            // Add dots for progress
            for _ in 0..self.status_dots {
                let _ = write!(stdout, ".");
            }
            
            // Add timing if available
            if let Some(ms) = self.status_time_ms {
                if ms < 1000 {
                    let _ = write!(stdout, " ~(=^‥^)ノ [{}ms]", ms);
                } else {
                    let secs = ms / 1000;
                    let remainder = (ms % 1000) / 100;
                    let _ = write!(stdout, " ~(=^‥^)ノ [{}.{}s]", secs, remainder);
                }
            }
            
            let _ = write!(stdout, "{}", COLOR_RESET);
        }
        
        // Note: Cursor is left at end of status line. Caller should reposition if needed.
    }
    
    /// Update status text (does NOT render - rendering happens via tui_handle_input or render_footer).
    pub fn update_status(&mut self, text: &str, dots: u8, time_ms: Option<u64>) {
        // Only reset dots/counter if the status text changes
        if self.status_text != text {
            self.status_text = String::from(text);
            self.status_dots = if dots > 0 { dots } else { 1 }; // Start at 1 if not specified
            self.status_start_us = libakuma::uptime();
            self.repaint_counter = 0; // Reset counter for fresh animation
            
            // Auto-pick color based on text if not specified otherwise
            self.status_color = if text.contains("error") || text.contains("failed") || text.contains("retry") || text.contains("cancelled") {
                "\x1b[38;5;203m" // COLOR_PEARL
            } else if text.contains("streaming") {
                "\x1b[38;5;120m" // COLOR_GREEN_LIGHT
            } else if text.contains("waiting") && !text.contains("awaiting") {
                "\x1b[38;5;215m" // COLOR_YELLOW
            } else {
                "\x1b[38;5;242m" // COLOR_GRAY_DIM
            };
        }
        self.status_time_ms = time_ms;
    }
    
    /// Clear the status pane (does NOT render).
    pub fn clear_status(&mut self) {
        self.status_text.clear();
        self.status_color = "\x1b[38;5;242m"; // COLOR_GRAY_DIM
        self.status_dots = 1; // Reset to 1 for idle animation
        self.status_time_ms = None;
        self.status_start_us = 0;
    }
    
    /// Get max output row (alias for output_bottom).
    #[allow(dead_code)]
    pub fn output_max_row(&self) -> u16 {
        self.output_bottom
    }
}

// Global pane layout (initialized on TUI start)
static mut PANE_LAYOUT: Option<PaneLayout> = None;

/// Get the global pane layout.
pub fn get_pane_layout() -> &'static mut PaneLayout {
    unsafe {
        if PANE_LAYOUT.is_none() {
            let w = TERM_WIDTH.load(Ordering::SeqCst);
            let h = TERM_HEIGHT.load(Ordering::SeqCst);
            PANE_LAYOUT = Some(PaneLayout::new(w, h));
        }
        PANE_LAYOUT.as_mut().unwrap()
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
static mut LAST_HISTORY_KB: usize = 0;

fn get_raw_input_queue() -> &'static mut VecDeque<u8> {
    unsafe {
        if RAW_INPUT_QUEUE.is_none() {
            RAW_INPUT_QUEUE = Some(VecDeque::with_capacity(64));
        }
        RAW_INPUT_QUEUE.as_mut().unwrap()
    }
}

const PROMPT_MULTILINE_INDENT: usize = 4;

fn count_wrapped_lines(input: &str, prompt_width: usize, width: usize) -> usize {
    if width == 0 { return 1; }
    let mut lines = 1;
    let mut current_col = prompt_width;
    for c in input.chars() {
        if c == '\n' {
            lines += 1;
            current_col = PROMPT_MULTILINE_INDENT;
        } else {
            current_col += 1;
            if current_col >= width {
                lines += 1;
                current_col = PROMPT_MULTILINE_INDENT;
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

/// TUI-aware print function that handles word-wrapping, indentation, and cursor management.
/// Uses atomics as source of truth, with PaneLayout for boundary calculations.
pub fn tui_print(s: &str) {
    tui_print_with_indent(s, "", 9, None);
}

/// Print with custom prefix, indentation for wrapped lines, and optional color.
pub fn tui_print_with_indent(s: &str, prefix: &str, indent: u16, color: Option<&str>) {
    if s.is_empty() && prefix.is_empty() { return; }
    
    // Read from atomics (source of truth)
    let w = TERM_WIDTH.load(Ordering::SeqCst);
    let h = TERM_HEIGHT.load(Ordering::SeqCst);
    let mut col = CUR_COL.load(Ordering::SeqCst);
    let mut row = CUR_ROW.load(Ordering::SeqCst);
    let f_h = FOOTER_HEIGHT.load(Ordering::SeqCst);
    
    // Calculate max_row using layout's gap calculation
    let layout = get_pane_layout();
    let gap = layout.gap();
    let max_row = h.saturating_sub(f_h + 1 + gap);

    // Jump to the current output position in the scroll area.
    set_cursor_position(col as u64, row as u64);
    
    // Apply initial color if provided
    if let Some(c) = color {
        akuma_write(fd::STDOUT, c.as_bytes());
    }

    // Print initial prefix if we are at start of line
    if col == 0 && !prefix.is_empty() {
        akuma_write(fd::STDOUT, prefix.as_bytes());
        col = visual_length(prefix) as u16;
    }
    
    // Word buffer for word-wrapping (buffer the current word)
    let mut word_buf: alloc::vec::Vec<char> = alloc::vec::Vec::with_capacity(64);
    let mut word_display_len: u16 = 0; // Visual length (excludes escape codes)
    
    let mut in_esc = false;
    let mut esc_buf: alloc::vec::Vec<char> = alloc::vec::Vec::with_capacity(16);
    
    // Helper closure to wrap to next line
    let mut wrap_line = |row: &mut u16, col: &mut u16, max_row: u16| {
        *row += 1;
        if *row > max_row {
            *row = max_row;
            akuma_write(fd::STDOUT, b"\n");
        } else {
            set_cursor_position(0, *row as u64);
        }
        
        // Apply indent
        for _ in 0..indent {
            akuma_write(fd::STDOUT, b" ");
        }
        *col = indent;
    };
    
    // Helper to flush the word buffer
    let mut flush_word = |word_buf: &mut alloc::vec::Vec<char>, word_display_len: &mut u16, 
                      col: &mut u16, row: &mut u16, max_row: u16, w: u16, indent: u16| {
        if word_buf.is_empty() { return; }
        
        // Check if word fits on current line
        if *col + *word_display_len > w - 1 && *col > indent {
            // Word doesn't fit - wrap first
            wrap_line(row, col, max_row);
        }
        
        // Print the buffered word
        for c in word_buf.iter() {
            let mut buf = [0u8; 4];
            akuma_write(fd::STDOUT, c.encode_utf8(&mut buf).as_bytes());
        }
        *col += *word_display_len;
        
        word_buf.clear();
        *word_display_len = 0;
    };
    
    for c in s.chars() {
        // Handle escape sequences - pass through without affecting word buffer
        if in_esc {
            esc_buf.push(c);
            if c != '[' && c >= '@' && c <= '~' {
                // End of escape sequence - print it
                for ec in esc_buf.iter() {
                    let mut buf = [0u8; 4];
                    akuma_write(fd::STDOUT, ec.encode_utf8(&mut buf).as_bytes());
                }
                esc_buf.clear();
                in_esc = false;
            }
            continue;
        }
        
        if c == '\x1b' {
            in_esc = true;
            esc_buf.clear();
            esc_buf.push(c);
            continue;
        }

        if c == '\n' {
            // Flush word buffer before newline
            flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
            wrap_line(&mut row, &mut col, max_row);
        } else if c == '\x08' {
            // Backspace
            if col > indent {
                col -= 1;
                akuma_write(fd::STDOUT, b"\x08");
            }
        } else if c == ' ' || c == '\t' {
            // Space/tab - flush word buffer first, then print space
            flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
            
            // Check if space fits
            if col >= w - 1 {
                wrap_line(&mut row, &mut col, max_row);
            }
            akuma_write(fd::STDOUT, b" ");
            col += 1;
        } else {
            // Regular character - add to word buffer
            word_buf.push(c);
            word_display_len += 1;
            
            // If word is too long for a line, flush it anyway (force break)
            if word_display_len >= w - indent {
                flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
            }
        }
    }
    
    // Flush any remaining word
    flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
    
    // Reset color at end
    if color.is_some() {
        akuma_write(fd::STDOUT, COLOR_RESET.as_bytes());
    }

    // Update atomics (source of truth)
    CUR_COL.store(col, Ordering::SeqCst);
    CUR_ROW.store(row, Ordering::SeqCst);
    
    // Also update layout for consistency
    layout.output_col = col;
    layout.output_row = row;

    // Return cursor to prompt position
    let input = get_global_input();
    let prompt_prefix_len = INPUT_LEN.load(Ordering::SeqCst) as usize;
    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let (cx, cy_off) = calculate_input_cursor(input, idx, prompt_prefix_len, w as usize);
    
    let scroll_top = PROMPT_SCROLL_TOP.load(Ordering::SeqCst) as u64;
    
    // Prompt area starts at Row h - f_h + 2
    let prompt_start_row = h as u64 - f_h as u64 + 2;
    let final_cy = prompt_start_row + (cy_off - scroll_top);
    
    // Clamp to prompt area bounds
    let clamped_cy = if final_cy >= h as u64 { h as u64 - 1 } else { final_cy };
    set_cursor_position(cx, clamped_cy);
}

/// Update the status pane with connection/streaming status.
/// Call this from main.rs during streaming to show progress.
pub fn update_streaming_status(text: &str, dots: u8, time_ms: Option<u64>) {
    if !TUI_ACTIVE.load(Ordering::SeqCst) { return; }
    let layout = get_pane_layout();
    layout.update_status(text, dots, time_ms);
}

/// Clear the status pane after streaming ends.
pub fn clear_streaming_status() {
    if !TUI_ACTIVE.load(Ordering::SeqCst) { return; }
    let layout = get_pane_layout();
    layout.clear_status();
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
            cx = PROMPT_MULTILINE_INDENT;
            cy += 1;
        } else {
            cx += 1;
            if cx >= width {
                cx = PROMPT_MULTILINE_INDENT;
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
    CANCELLED.load(Ordering::SeqCst)
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
                // Single ESC byte - could be standalone ESC or start of escape sequence
                // Use time since last input to decide:
                // - If >50ms since last input, treat as standalone ESC (user pressed ESC key)
                // - Otherwise, wait for more bytes (might be escape sequence coming)
                let now = libakuma::uptime();
                unsafe {
                    if now.saturating_sub(LAST_INPUT_TIME) > 50000 {
                        return (InputEvent::Esc, 1);
                    } else {
                        // Potential escape sequence starting - wait for more bytes
                        // Return 0 consumed to break out of parse loop and wait for more input
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
                                // CSI u (Kitty keyboard protocol)
                                // Format: ESC [ keycode ; modifier u
                                // Modifier: 1=Shift, 2=Alt, 4=Ctrl (value is 1 + bits)
                                if seq == b"13;2" || seq == b"13;5" { return (InputEvent::ShiftEnter, len); }
                                
                                // Parse keycode and optional modifier
                                if let Some(semi_pos) = seq.iter().position(|&b| b == b';') {
                                    // Has modifier: keycode;modifier
                                    if let Ok(keycode_str) = core::str::from_utf8(&seq[..semi_pos]) {
                                        if let Ok(keycode) = keycode_str.parse::<u32>() {
                                            if let Ok(mod_str) = core::str::from_utf8(&seq[semi_pos+1..]) {
                                                if let Ok(modifier) = mod_str.parse::<u32>() {
                                                    let ctrl = (modifier.saturating_sub(1) & 4) != 0;
                                                    let alt = (modifier.saturating_sub(1) & 2) != 0;
                                                    
                                                    // Ctrl + key
                                                    if ctrl && !alt {
                                                        match keycode {
                                                            97 => return (InputEvent::CtrlA, len),  // a
                                                            99 => return (InputEvent::Interrupt, len), // c
                                                            101 => return (InputEvent::CtrlE, len), // e
                                                            106 => return (InputEvent::ShiftEnter, len), // j (Ctrl+J = LF)
                                                            108 => return (InputEvent::CtrlL, len), // l
                                                            117 => return (InputEvent::CtrlU, len), // u
                                                            119 => return (InputEvent::CtrlW, len), // w
                                                            _ => {}
                                                        }
                                                    }
                                                    // Alt + key
                                                    if alt && !ctrl {
                                                        match keycode {
                                                            98 => return (InputEvent::AltLeft, len),  // b
                                                            102 => return (InputEvent::AltRight, len), // f
                                                            _ => {}
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    // No modifier: just keycode
                                    if let Ok(keycode_str) = core::str::from_utf8(seq) {
                                        if let Ok(keycode) = keycode_str.parse::<u32>() {
                                            match keycode {
                                                27 => return (InputEvent::Esc, len), // ESC
                                                13 => return (InputEvent::Enter, len), // Enter
                                                127 => return (InputEvent::Backspace, len), // Backspace
                                                _ => {}
                                            }
                                        }
                                    }
                                }
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
    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
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
            
            // Update PaneLayout with new dimensions
            let layout = get_pane_layout();
            layout.term_width = nw;
            layout.term_height = nh;
            layout.recalculate(FOOTER_HEIGHT.load(Ordering::SeqCst));
            
            clear_screen();
            print_greeting();
            
            // Set scroll region using layout
            layout.set_scroll_region();
            
            // Reset output cursor position
            layout.output_row = layout.output_bottom;
            layout.output_col = 0;
            CUR_ROW.store(layout.output_row, Ordering::SeqCst);
            CUR_COL.store(0, Ordering::SeqCst);
            
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
    
    // Always render during streaming to show status updates
    // Otherwise only render when input changed
    let is_streaming = STREAMING.load(Ordering::SeqCst);
    if redraw || is_streaming { 
        render_footer_internal(input, current_tokens, token_limit, mem_kb); 
    }
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
        let _ = calculate_history_bytes(&self.history);
        render_footer_internal(input, current_tokens, token_limit, mem_kb);
    }
}

/// Calculate the total byte size of message history
pub fn calculate_history_bytes(history: &[Message]) -> usize {
    let bytes = history
        .iter()
        .map(|msg| msg.role.len() + msg.content.len() + 32) // +32 for metadata/structure overhead
        .sum();
    
    // Update the static for UI persistence
    unsafe {
        LAST_HISTORY_KB = bytes / 1024;
    }
    bytes
}

/// Calculate the visual length of a string, excluding ANSI escape sequences
pub fn visual_length(s: &str) -> usize {
    let mut len = 0;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c != '[' && c >= '@' && c <= '~' {
                in_esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_esc = true;
            continue;
        }
        len += 1;
    }
    len
}

fn render_footer_internal(input: &str, current_tokens: usize, token_limit: usize, mem_kb: usize) {
    let mut stdout = Stdout;
    let layout = get_pane_layout();
    let w = layout.term_width as usize;
    let h = layout.term_height as u64;
    
    // Increment repaint counter (wraps at 10000)
    layout.repaint_counter = layout.repaint_counter.wrapping_add(1) % 10000;
    let is_streaming = STREAMING.load(Ordering::SeqCst);
    
    // Update dots every 10th repaint (~500ms at 50ms poll rate)
    if layout.repaint_counter % 10 == 0 {
        layout.status_dots = (layout.status_dots % 5) + 1;
    }
    
    // Set idle status when not streaming
    if !is_streaming && layout.status_text.is_empty() {
        layout.update_status("[MEOW] awaiting user input", 0, None);
    }

    let token_display = if current_tokens >= 1000 { format!("{}k", current_tokens / 1000) } else { format!("{}", current_tokens) };
    let limit_display = format!("{}k", token_limit / 1000);
    let mem_display = if mem_kb >= 1024 { format!("{}M", mem_kb / 1024) } else { format!("{}K", mem_kb) };
    
    // History size with color coding (using persisted static)
    let history_kb = unsafe { LAST_HISTORY_KB };
    let color = if history_kb > 256 {
        "\x1b[38;5;196m" // Red
    } else if history_kb > 128 {
        "\x1b[38;5;226m" // Yellow
    } else {
        COLOR_YELLOW
    };
    let hist_display = format!("|{}Hist: {}K{}", color, history_kb, COLOR_RESET);
    
    let queue_len = get_message_queue().len();
    let queue_display = if queue_len > 0 { format!(" [QUEUED: {}]", queue_len) } else { String::new() };

    let prompt_prefix = format!("  {}[{}/{}|{}{}{}] {}(=^･ω･^=) > ", 
        COLOR_YELLOW, token_display, limit_display, mem_display, hist_display, COLOR_YELLOW, queue_display);
    let prompt_prefix_len = visual_length(&prompt_prefix);
    INPUT_LEN.store(prompt_prefix_len as u16, Ordering::SeqCst);
    layout.input_prefix_len = prompt_prefix_len as u16;

    let wrapped_lines = count_wrapped_lines(input, prompt_prefix_len, w);
    let max_prompt_lines = core::cmp::min(10, (h / 3) as usize);
    let display_prompt_lines = core::cmp::min(wrapped_lines, max_prompt_lines);
    let new_footer_height = (display_prompt_lines + 2) as u16; 
    let old_footer_height = layout.footer_height;
    
    // During streaming: allow footer to GROW but not SHRINK
    // (shrinking would leave old separator lines that we can't safely clear during streaming)
    let effective_footer_height = if is_streaming && new_footer_height < old_footer_height {
        old_footer_height
    } else {
        new_footer_height
    };
    let effective_prompt_lines = (effective_footer_height as usize).saturating_sub(2);
    
    // Handle footer height changes using PaneLayout coordination
    if effective_footer_height != old_footer_height {
        let old_output_bottom = layout.output_bottom;
        let old_status_row = layout.status_row;
        
        if effective_footer_height > old_footer_height {
            // Footer is GROWING - need to scroll content up BEFORE changing scroll region
            let growth = effective_footer_height - old_footer_height;
            
            // First, clear the old status row and separator area that will become part of output/gap
            for row in old_status_row..=(old_status_row + growth) {
                set_cursor_position(0, row as u64);
                let _ = write!(stdout, "{}", CLEAR_TO_EOL);
            }
            
            // Position cursor at the bottom of the scroll region and print newlines
            // to scroll the LLM output up, making room for the larger footer
            set_cursor_position(0, old_output_bottom as u64);
            for _ in 0..growth {
                akuma_write(fd::STDOUT, b"\n");
            }
            
            // Recalculate all pane boundaries
            layout.recalculate(effective_footer_height);
            
            // Now update scroll region to the new smaller size
            layout.set_scroll_region();
            
            // Adjust output cursor to account for the scroll (subtract the growth)
            layout.output_row = layout.output_row.saturating_sub(growth);
            if layout.output_row > layout.output_bottom {
                layout.output_row = layout.output_bottom;
            }
            
            // Keep atomics in sync
            CUR_ROW.store(layout.output_row, Ordering::SeqCst);
        } else {
            // Footer is SHRINKING (only happens when not streaming)
            // Clear the old footer area before shrinking
            let old_separator_row = h as u16 - old_footer_height;
            let new_separator_row = h as u16 - effective_footer_height;
            for row in old_separator_row..new_separator_row {
                set_cursor_position(0, row as u64);
                let _ = write!(stdout, "{}", CLEAR_TO_EOL);
            }
            
            layout.recalculate(effective_footer_height);
            layout.set_scroll_region();
        }
        
        // Keep atomic in sync
        FOOTER_HEIGHT.store(effective_footer_height, Ordering::SeqCst);
    }

    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let (_cx_abs, cy_off_abs) = calculate_input_cursor(input, idx, prompt_prefix_len, w);
    let mut scroll_top = layout.prompt_scroll;
    if cy_off_abs < scroll_top as u64 { scroll_top = cy_off_abs as u16; } 
    else if cy_off_abs >= (scroll_top as u64 + effective_prompt_lines as u64) { scroll_top = (cy_off_abs - effective_prompt_lines as u64 + 1) as u64 as u16; }
    layout.prompt_scroll = scroll_top;
    PROMPT_SCROLL_TOP.store(scroll_top, Ordering::SeqCst);

    hide_cursor();
    let separator_row = h - effective_footer_height as u64;
    
    // Streaming status row is just above the separator
    let streaming_status_row = separator_row.saturating_sub(1);
    
    // When footer shrinks, clear old footer lines and old status row that are now in the gap area.
    // This never touches the scroll region (LLM output area).
    if effective_footer_height < old_footer_height {
        let old_separator_row = h - old_footer_height as u64;
        let old_status_row = old_separator_row.saturating_sub(1);
        // Clear from old status row through to the new separator
        for row in old_status_row..separator_row {
            set_cursor_position(0, row);
            let _ = write!(stdout, "{}", CLEAR_TO_EOL);
        }
    }
    
    // Draw streaming status above separator
    set_cursor_position(0, streaming_status_row);
    let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    if !layout.status_text.is_empty() {
        let _ = write!(stdout, "  {}{}", layout.status_color, layout.status_text);
        // Animated dots (1-5), padded to 5 chars so everything stays in place
        for _ in 0..layout.status_dots {
            let _ = write!(stdout, ".");
        }
        for _ in layout.status_dots..5 {
            let _ = write!(stdout, " ");
        }
        
        // Use status_time_ms if available, otherwise calculate from status_start_us
        let ms = if let Some(ms) = layout.status_time_ms {
            Some(ms)
        } else if layout.status_start_us > 0 && !layout.status_text.contains("awaiting") {
            Some((libakuma::uptime() - layout.status_start_us) / 1000)
        } else {
            None
        };

        if let Some(ms) = ms {
            if ms < 1000 {
                let _ = write!(stdout, "~(=^‥^)ノ [{}ms]", ms);
            } else {
                let secs = ms / 1000;
                let remainder = (ms % 1000) / 100;
                let _ = write!(stdout, "~(=^‥^)ノ [{}.{}s]", secs, remainder);
            }
        }
        let _ = write!(stdout, "{}", COLOR_RESET);
    }
    
    // Draw separator line
    set_cursor_position(0, separator_row);
    let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, "━".repeat(w), COLOR_RESET);

    // Provider/model info row (inside footer, after separator)
    let provider_row = separator_row + 1;
    set_cursor_position(0, provider_row);
    let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    let (model, provider) = get_model_and_provider();
    let provider_info = format!("  [Provider: {}] [Model: {}]", provider, model);
    let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, provider_info, COLOR_RESET);

    // Clear and draw prompt lines
    for i in 0..effective_prompt_lines {
        set_cursor_position(0, provider_row + 1 + i as u64);
        let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    }

    let mut current_line = 0;
    let mut current_col = prompt_prefix_len;
    if scroll_top == 0 {
        set_cursor_position(0, provider_row + 1);
        let _ = write!(stdout, "{}{}{}{}", COLOR_VIOLET, COLOR_BOLD, prompt_prefix, COLOR_RESET);
    }
    let _ = write!(stdout, "{}", COLOR_VIOLET);
    for c in input.chars() {
        if c == '\n' {
            current_line += 1;
            current_col = PROMPT_MULTILINE_INDENT;
        } else {
            if current_line >= scroll_top as usize && current_line < (scroll_top as usize + effective_prompt_lines) {
                let target_row = provider_row + 1 + (current_line as u64 - scroll_top as u64);
                set_cursor_position(current_col as u64, target_row);
                let mut buf = [0u8; 4];
                let _ = write!(stdout, "{}", c.encode_utf8(&mut buf));
            }
            current_col += 1;
            if current_col >= w {
                current_line += 1;
                current_col = PROMPT_MULTILINE_INDENT;
            }
        }
    }
    let _ = write!(stdout, "{}", COLOR_RESET);

    let (cx, cy_off) = calculate_input_cursor(input, idx, prompt_prefix_len, w);
    let final_cy = provider_row + 1 + (cy_off - scroll_top as u64);
    layout.cursor_idx = idx as u16;
    set_cursor_position(cx, final_cy);
    show_cursor();
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
    
    // Initialize PaneLayout
    let layout = get_pane_layout();
    layout.term_width = w;
    layout.term_height = h;
    layout.recalculate(FOOTER_HEIGHT.load(Ordering::SeqCst));
    
    let mut stdout = Stdout;
    let _ = write!(stdout, "\x1b[>1u");
    let _ = write!(stdout, "\x1b[?1049h");
    clear_screen();
    
    // Set scroll region using PaneLayout
    layout.set_scroll_region();
    
    print_greeting();
    let _ = write!(stdout, "  {}TIP:{} Type {}/hotkeys{} to see input shortcuts nya~! ♪(=^･ω･^)ﾉ\n\n", COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_YELLOW, COLOR_RESET);

    // Initialize cursor position using layout
    layout.output_row = layout.output_bottom;
    layout.output_col = 0;
    CUR_ROW.store(layout.output_row, Ordering::SeqCst);
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
            let layout = get_pane_layout();
            let gap = layout.gap();
            let mut stdout = Stdout;
            
            // Position cursor in the output area before printing
            let cur_row = CUR_ROW.load(Ordering::SeqCst);
            set_cursor_position(0, cur_row as u64);
            
            tui_print_with_indent("\n\n", "", 0, None);
            tui_print_with_indent(&user_input, " >  ", 4, Some(&format!("{}{}", COLOR_VIOLET, COLOR_BOLD)));
            tui_print("\n");

            if user_input.starts_with('/') {
                let (res, output) = crate::handle_command(&user_input, model, provider, config, history, system_prompt);
                if let Some(out) = output {
                    let _ = write!(stdout, "  \n{}{}{}\n\n", COLOR_GRAY_BRIGHT, out, COLOR_RESET);
                    let msg = Message::new("system", &out);
                    history.push(msg.clone()); app.history.push(msg);
                }
                if let CommandResult::Quit = res { break; }
                app.history = history.clone();
                
                // Reset output position using atomics (source of truth)
                let output_row = app.terminal_height.saturating_sub(f_h + 1 + gap);
                CUR_ROW.store(output_row, Ordering::SeqCst);
                CUR_COL.store(0, Ordering::SeqCst);
                layout.output_row = output_row;
                layout.output_col = 0;
            } else {
                app.history.push(Message::new("user", &user_input));
                history.clear(); history.extend(app.history.iter().cloned());
                
                // Initialize output position using atomics (source of truth)
                let output_row = app.terminal_height.saturating_sub(f_h + 1 + gap);
                CUR_ROW.store(output_row, Ordering::SeqCst);
                CUR_COL.store(0, Ordering::SeqCst);
                layout.output_row = output_row;
                layout.output_col = 0;
                
                STREAMING.store(true, Ordering::SeqCst);
                
                // Update status pane with connection status (dots start at 1)
                layout.update_status("[MEOW] jacking in", 1, None);
                
                // Add some spacing before LLM response (status shown in status bar, not inline)
                tui_print("\n\n");
                
                let _ = crate::chat_once(model, provider, &user_input, history, Some(context_window), system_prompt);
                STREAMING.store(false, Ordering::SeqCst);
                CANCELLED.store(false, Ordering::SeqCst); // Clear cancel flag after streaming ends
                
                // Clear status after streaming ends
                layout.clear_status();
                
                let _ = write!(stdout, "{}\n", COLOR_RESET);
                app.history = history.clone(); crate::compact_history(&mut app.history);
            }
        }
    }

    // Reset scroll region using layout
    let layout = get_pane_layout();
    layout.reset_scroll_region();
    
    let mut stdout = Stdout;
    let _ = write!(stdout, "\x1b[<u");
    set_terminal_attributes(fd::STDIN, 0, old_mode_flags);
    clear_screen();
    set_cursor_position(0, 0);
    show_cursor();
    Ok(())
}