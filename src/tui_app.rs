use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use libakuma::{
    get_terminal_attributes, set_terminal_attributes, 
    set_cursor_position, hide_cursor, show_cursor, 
    clear_screen, poll_input_event, write as akuma_write, fd,
};

// Mode flags (from kernel's terminal.rs)
// Must match src/terminal.rs `mode_flags`
pub mod mode_flags {
    pub const RAW_MODE_ENABLE: u64 = 0x01;
    pub const RAW_MODE_DISABLE: u64 = 0x02;
    // Add other flags as needed
}

// Simple abstraction for writing to stdout
struct Stdout;

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        akuma_write(fd::STDOUT, s.as_bytes());
        Ok(())
    }
}

/// Represents the state of the TUI application.
pub struct App {
    pub input: String,
    pub history: Vec<String>,
    pub scroll_offset: usize, // For scrolling chat history
    pub terminal_width: u16,
    pub terminal_height: u16,
    pub input_dirty: AtomicBool, // Indicates if the input line needs to be redrawn
    pub history_dirty: AtomicBool, // Indicates if the chat history needs to be redrawn
}

impl App {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            history: Vec::new(),
            scroll_offset: 0,
            terminal_width: 80, // Default, will try to determine actual size
            terminal_height: 24, // Default
            input_dirty: AtomicBool::new(true), // Initially, the input needs to be drawn
            history_dirty: AtomicBool::new(true), // Initially, the history needs to be drawn
        }
    }

    /// Renders the chat history area.
    pub fn render_history(&mut self) {
        let mut stdout = Stdout;

        let chat_area_height = self.terminal_height.saturating_sub(4) as usize; // 1 for input, 1 for border, 2 for padding
        let chat_area_width = self.terminal_width.saturating_sub(2) as usize; // for border

        // Clear the chat area by overwriting with spaces before drawing new content
        for y in 0..chat_area_height {
            set_cursor_position(0, y as u64);
            let _ = write!(stdout, "{:width$}", "", width = self.terminal_width as usize);
        }

        // Draw chat history
        let num_history_lines = self.history.len();
        let display_start_index = if num_history_lines > chat_area_height {
            num_history_lines.saturating_sub(chat_area_height).saturating_sub(self.scroll_offset)
        } else {
            0
        };
        
        let mut current_y = 0;
        for msg_idx in display_start_index..num_history_lines {
            if current_y as u16 >= chat_area_height as u16 {
                break;
            }
            let msg = &self.history[msg_idx];
            
            // Simple word wrap for messages
            let mut line_start_idx = 0;
            while line_start_idx < msg.len() {
                let mut line_end_idx = (line_start_idx + chat_area_width).min(msg.len());

                // Try to break at word boundary if not at end of message
                if line_end_idx < msg.len() {
                    if let Some(space_idx) = msg[line_start_idx..line_end_idx].rfind(' ') {
                        line_end_idx = line_start_idx + space_idx;
                    }
                }
                
                let line_to_print = &msg[line_start_idx..line_end_idx];
                
                set_cursor_position(0, current_y as u64);
                let _ = write!(stdout, "{}", line_to_print);
                current_y += 1;
                line_start_idx = line_end_idx;
                if line_start_idx < msg.len() && msg.chars().nth(line_start_idx) == Some(' ') {
                    line_start_idx += 1; // Skip space if it was the break point
                }

                if current_y as u16 >= chat_area_height as u16 {
                    break;
                }
            }
        }
    }

    /// Renders the input line and positions the cursor.
    pub fn render_input(&mut self) {
        let mut stdout = Stdout;

        // Clear the input line area
        let input_line_start = self.terminal_height.saturating_sub(2) as u64;
        set_cursor_position(0, input_line_start);
        let _ = write!(stdout, "{:width$}", "", width = self.terminal_width as usize);

        // Draw input box
        set_cursor_position(0, input_line_start);
        let _ = write!(stdout, "{}", "> ".to_string() + &self.input);

        // Position cursor at the end of input
        let cursor_col = 2 + self.input.len() as u64;
        set_cursor_position(cursor_col, input_line_start);
    }
}

/// The main entry point for the TUI application.
pub fn run_tui() -> Result<(), &'static str> {
    let mut old_mode_flags: u64 = 0;
    
    // Save current terminal attributes
    let result = get_terminal_attributes(fd::STDIN, &mut old_mode_flags as *mut u64 as u64);
    if result < 0 {
        return Err("Failed to get terminal attributes");
    }

    // Enable raw mode
    let result = set_terminal_attributes(fd::STDIN, 0, mode_flags::RAW_MODE_ENABLE);
    if result < 0 {
        return Err("Failed to set terminal attributes to raw mode");
    }

    let mut app = App::new();
    // TODO: Dynamically determine terminal_width and terminal_height

    // Clear screen once at the beginning
    clear_screen();

    app.history.push("Welcome to Meow-chan TUI!".to_string());
    app.history.push("Type /help for commands, ESC to quit.".to_string());
    app.history_dirty.store(true, Ordering::Release); // Ensure initial history render
    app.input_dirty.store(true, Ordering::Release); // Ensure initial input render

    loop {
        // Hide cursor during drawing to prevent flicker
        hide_cursor();

        if app.history_dirty.load(Ordering::Acquire) {
            app.render_history();
            app.history_dirty.store(false, Ordering::Release);
        }
        if app.input_dirty.load(Ordering::Acquire) {
            app.render_input();
            app.input_dirty.store(false, Ordering::Release);
        }

        // Show cursor after all drawing is done and input cursor is positioned
        show_cursor();

        let mut event_buf = [0u8; 16]; // Buffer for input events
        let bytes_read = poll_input_event(10, &mut event_buf); // Poll with 10ms timeout

        if bytes_read > 0 {
            let key_code = event_buf[0]; // Assuming single byte key codes for now

            match key_code {
                0x1B => { // ESC key
                    break; 
                },
                b'\r' | b'\n' => { // Enter key
                    if !app.input.is_empty() {
                        let user_message = format!("You: {}", app.input);
                        app.history.push(user_message);
                        app.input.clear();
                        // TODO: Process input, send to LLM
                        app.history.push("Meow-chan: Nya~! Processing...".to_string()); // Placeholder
                        app.history_dirty.store(true, Ordering::Release); // History changed
                        app.input_dirty.store(true, Ordering::Release); // Input cleared
                    }
                },
                0x7F | 0x08 => { // Backspace or Delete
                    app.input.pop();
                    app.input_dirty.store(true, Ordering::Release);
                },
                c if c >= 0x20 && c <= 0x7E => { // Printable ASCII characters
                    if app.input.len() < app.terminal_width as usize - 4 { // Prevent overflow
                        app.input.push(c as char);
                        app.input_dirty.store(true, Ordering::Release);
                    }
                },
                _ => {} // Ignore other control characters for now
            }
        }
    }

    // Restore original terminal attributes
    let result = set_terminal_attributes(fd::STDIN, 0, old_mode_flags);
    if result < 0 {
        // Log error, but still try to clean up terminal
        let mut stdout = Stdout;
        let _ = write!(stdout, "Error restoring terminal attributes: {}\n", result);
    }
    
    show_cursor();
    clear_screen();

    Ok(())
}