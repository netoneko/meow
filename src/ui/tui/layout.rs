use alloc::string::String;
use alloc::format;
use core::fmt::Write;
use core::sync::atomic::{Ordering, AtomicU16};
use libakuma::{set_cursor_position, write as akuma_write, fd};

use crate::config::{COLOR_GRAY_DIM, COLOR_RESET, COLOR_YELLOW};

// ANSI escapes
pub const CLEAR_TO_EOL: &str = "\x1b[K";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiState {
    Idle,
    Connecting,
    WaitingForResponse,
    Streaming,
    Processing,
    Exiting,
}

pub struct PaneLayout {
    pub state: TuiState,
    pub term_width: u16,
    pub term_height: u16,
    pub output_top: u16,
    pub output_bottom: u16,
    pub output_row: u16,
    pub output_col: u16,
    pub status_row: u16,
    pub status_text: String,
    pub status_color: &'static str,
    pub status_dots: u8,
    pub status_time_ms: Option<u64>,
    pub status_start_us: u64,
    pub footer_top: u16,
    pub footer_height: u16,
    pub prompt_scroll: u16,
    pub cursor_idx: u16,
    pub input_prefix_len: u16,
    pub repaint_counter: u16,
}

impl PaneLayout {
    pub fn new(width: u16, height: u16) -> Self {
        let footer_height = 4;
        let gap = Self::calculate_gap(height);
        let separator_row = height.saturating_sub(footer_height);
        let status_row = separator_row.saturating_sub(1);
        let output_bottom = status_row.saturating_sub(gap);
        
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
            status_color: "\x1b[38;5;242m",
            status_dots: 0,
            status_time_ms: None,
            status_start_us: 0,
            footer_top: separator_row,
            footer_height,
            prompt_scroll: 0,
            cursor_idx: 0,
            input_prefix_len: 0,
            repaint_counter: 0,
        }
    }

    fn calculate_gap(height: u16) -> u16 {
        if height >= 40 { 5 } else if height >= 30 { 4 } else { 3 }
    }

    pub fn gap(&self) -> u16 { Self::calculate_gap(self.term_height) }

    pub fn recalculate(&mut self, new_footer_height: u16) {
        let gap = self.gap();
        self.footer_height = new_footer_height;
        let separator_row = self.term_height.saturating_sub(new_footer_height);
        self.status_row = separator_row.saturating_sub(1);
        self.output_bottom = self.status_row.saturating_sub(gap);
        self.footer_top = separator_row;
        if self.output_row > self.output_bottom { self.output_row = self.output_bottom; }
    }

    pub fn set_scroll_region(&self) {
        let mut stdout = Stdout;
        let _ = write!(stdout, "\x1b[{};{}r", self.output_top, self.output_bottom + 1);
    }

    pub fn reset_scroll_region(&self) {
        let mut stdout = Stdout;
        let _ = write!(stdout, "\x1b[1;{}r", self.term_height);
    }

    pub fn update_status(&mut self, text: &str, dots: u8, time_ms: Option<u64>) {
        if self.status_text != text {
            self.status_text = String::from(text);
            self.status_dots = if dots > 0 { dots } else { 1 };
            self.status_start_us = libakuma::uptime();
            self.repaint_counter = 0;
            self.status_color = if text.contains("error") || text.contains("failed") || text.contains("retry") || text.contains("cancelled") {
                "\x1b[38;5;203m"
            } else if text.contains("streaming") {
                "\x1b[38;5;120m"
            } else if text.contains("waiting") && !text.contains("awaiting") {
                "\x1b[38;5;215m"
            } else {
                "\x1b[38;5;242m"
            };
        }
        self.status_time_ms = time_ms;
    }

    pub fn clear_status(&mut self) {
        self.status_text.clear();
        self.status_color = "\x1b[38;5;242m";
        self.status_dots = 1;
        self.status_time_ms = None;
        self.status_start_us = 0;
    }
}

pub struct Stdout;
impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        akuma_write(fd::STDOUT, s.as_bytes());
        Ok(())
    }
}

pub static TERM_WIDTH: AtomicU16 = AtomicU16::new(100);
pub static TERM_HEIGHT: AtomicU16 = AtomicU16::new(25);

static mut PANE_LAYOUT: Option<PaneLayout> = None;

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
