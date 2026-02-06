use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use libakuma::{
    get_terminal_attributes, set_terminal_attributes, 
    set_cursor_position, hide_cursor, show_cursor, 
    clear_screen, poll_input_event, write as akuma_write, fd
};

use crate::config::{Provider, Config, COLOR_GRAY_DIM, COLOR_GRAY_BRIGHT, COLOR_YELLOW, COLOR_RESET, COLOR_BOLD, COLOR_VIOLET};
use crate::app::{Message, commands::CommandResult, calculate_history_tokens, compact_history};
use crate::ui::tui::layout::{get_pane_layout, TERM_WIDTH, TERM_HEIGHT, CLEAR_TO_EOL, Stdout};
use crate::ui::tui::input::{self, InputEvent, INPUT_LEN, CURSOR_IDX, PROMPT_SCROLL_TOP};

pub static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static CANCELLED: AtomicBool = AtomicBool::new(false);
pub static STREAMING: AtomicBool = AtomicBool::new(false);
pub static CUR_COL: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);
pub static CUR_ROW: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);

static mut GLOBAL_INPUT: Option<String> = None;
static mut MESSAGE_QUEUE: Option<VecDeque<String>> = None;
static mut COMMAND_HISTORY: Option<Vec<String>> = None;
static mut HISTORY_INDEX: usize = 0;
static mut SAVED_INPUT: Option<String> = None;
static mut MODEL_NAME: Option<String> = None;
static mut PROVIDER_NAME: Option<String> = None;
static mut LAST_HISTORY_KB: usize = 0;

fn get_global_input() -> &'static mut String {
    unsafe {
        if GLOBAL_INPUT.is_none() { GLOBAL_INPUT = Some(String::new()); }
        GLOBAL_INPUT.as_mut().unwrap()
    }
}

pub fn get_message_queue() -> &'static mut VecDeque<String> {
    unsafe {
        if MESSAGE_QUEUE.is_none() { MESSAGE_QUEUE = Some(VecDeque::new()); }
        MESSAGE_QUEUE.as_mut().unwrap()
    }
}

pub fn get_command_history() -> &'static mut Vec<String> {
    unsafe {
        if COMMAND_HISTORY.is_none() { COMMAND_HISTORY = Some(Vec::new()); }
        COMMAND_HISTORY.as_mut().unwrap()
    }
}

pub fn get_saved_input() -> &'static mut String {
    unsafe {
        if SAVED_INPUT.is_none() { SAVED_INPUT = Some(String::new()); }
        SAVED_INPUT.as_mut().unwrap()
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
    fn new() -> Self { TUI_ACTIVE.store(true, Ordering::SeqCst); Self }
}
impl Drop for TuiGuard {
    fn drop(&mut self) { TUI_ACTIVE.store(false, Ordering::SeqCst); }
}

pub mod mode_flags {
    pub const RAW_MODE_ENABLE: u64 = 0x01;
}

pub fn tui_print(s: &str) {
    tui_print_with_indent(s, "", 9, None);
}

pub fn tui_print_with_indent(s: &str, prefix: &str, indent: u16, color: Option<&str>) {
    if s.is_empty() && prefix.is_empty() { return; }
    let w = TERM_WIDTH.load(Ordering::SeqCst);
    let h = TERM_HEIGHT.load(Ordering::SeqCst);
    let mut col = CUR_COL.load(Ordering::SeqCst);
    let mut row = CUR_ROW.load(Ordering::SeqCst);
    
    let layout = get_pane_layout();
    let gap = layout.gap();
    let max_row = h.saturating_sub(layout.footer_height + 1 + gap);

    set_cursor_position(col as u64, row as u64);
    if let Some(c) = color { akuma_write(fd::STDOUT, c.as_bytes()); }
    if col == 0 && !prefix.is_empty() {
        akuma_write(fd::STDOUT, prefix.as_bytes());
        col = visual_length(prefix) as u16;
    }
    
    let mut word_buf: Vec<char> = Vec::with_capacity(64);
    let mut word_display_len: u16 = 0;
    let mut in_esc = false;
    let mut esc_buf: Vec<char> = Vec::with_capacity(16);
    
    let mut wrap_line = |row: &mut u16, col: &mut u16, max_row: u16| {
        *row += 1;
        if *row > max_row { *row = max_row; akuma_write(fd::STDOUT, b"\n"); }
        else { set_cursor_position(0, *row as u64); }
        for _ in 0..indent { akuma_write(fd::STDOUT, b" "); }
        *col = indent;
    };
    
    let mut flush_word = |word_buf: &mut Vec<char>, word_display_len: &mut u16, col: &mut u16, row: &mut u16, max_row: u16, w: u16, indent: u16| {
        if word_buf.is_empty() { return; }
        if *col + *word_display_len > w - 1 && *col > indent { wrap_line(row, col, max_row); }
        for c in word_buf.iter() {
            let mut buf = [0u8; 4];
            akuma_write(fd::STDOUT, c.encode_utf8(&mut buf).as_bytes());
        }
        *col += *word_display_len;
        word_buf.clear();
        *word_display_len = 0;
    };
    
    for c in s.chars() {
        if in_esc {
            esc_buf.push(c);
            if c != '[' && c >= '@' && c <= '~' {
                for ec in esc_buf.iter() {
                    let mut buf = [0u8; 4];
                    akuma_write(fd::STDOUT, ec.encode_utf8(&mut buf).as_bytes());
                }
                esc_buf.clear();
                in_esc = false;
            }
            continue;
        }
        if c == '\x1b' { in_esc = true; esc_buf.clear(); esc_buf.push(c); continue; }
        if c == '\n' { flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent); wrap_line(&mut row, &mut col, max_row); }
        else if c == '\x08' { if col > indent { col -= 1; akuma_write(fd::STDOUT, b"\x08"); } }
        else if c == ' ' || c == '\t' { flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent); if col >= w - 1 { wrap_line(&mut row, &mut col, max_row); } akuma_write(fd::STDOUT, b" "); col += 1; }
        else { word_buf.push(c); word_display_len += 1; if word_display_len >= w - indent { flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent); } }
    }
    flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
    if color.is_some() { akuma_write(fd::STDOUT, COLOR_RESET.as_bytes()); }

    CUR_COL.store(col, Ordering::SeqCst);
    CUR_ROW.store(row, Ordering::SeqCst);
    layout.output_col = col; layout.output_row = row;

    let input = get_global_input();
    let (cx, cy_off) = calculate_input_cursor(input, CURSOR_IDX.load(Ordering::SeqCst) as usize, INPUT_LEN.load(Ordering::SeqCst) as usize, w as usize);
    let scroll_top = PROMPT_SCROLL_TOP.load(Ordering::SeqCst) as u64;
    let prompt_start_row = h as u64 - layout.footer_height as u64 + 2;
    let final_cy = prompt_start_row + (cy_off - scroll_top);
    let clamped_cy = if final_cy >= h as u64 { h as u64 - 1 } else { final_cy };
    set_cursor_position(cx, clamped_cy);
}

pub fn update_streaming_status(text: &str, dots: u8, time_ms: Option<u64>) {
    if !TUI_ACTIVE.load(Ordering::SeqCst) { return; }
    get_pane_layout().update_status(text, dots, time_ms);
}

pub fn clear_streaming_status() {
    if !TUI_ACTIVE.load(Ordering::SeqCst) { return; }
    get_pane_layout().clear_status();
}

pub fn set_model_and_provider(model: &str, provider: &str) {
    unsafe { MODEL_NAME = Some(String::from(model)); PROVIDER_NAME = Some(String::from(provider)); }
}

pub fn get_model_and_provider() -> (String, String) {
    unsafe { (MODEL_NAME.as_ref().cloned().unwrap_or_else(|| String::from("unknown")), PROVIDER_NAME.as_ref().cloned().unwrap_or_else(|| String::from("unknown"))) }
}

fn calculate_input_cursor(input: &str, idx: usize, prompt_width: usize, width: usize) -> (u64, u64) {
    if width == 0 { return (0, 0); }
    let (mut cx, mut cy) = (prompt_width, 0);
    for (i, c) in input.chars().enumerate() {
        if i >= idx { break; }
        if c == '\n' { cx = 4; cy += 1; }
        else { cx += 1; if cx >= width { cx = 4; cy += 1; } }
    }
    (cx as u64, cy as u64)
}

fn count_wrapped_lines(input: &str, prompt_width: usize, width: usize) -> usize {
    if width == 0 { return 1; }
    let (mut lines, mut current_col) = (1, prompt_width);
    for c in input.chars() {
        if c == '\n' { lines += 1; current_col = 4; }
        else { current_col += 1; if current_col >= width { lines += 1; current_col = 4; } }
    }
    lines
}

pub fn add_to_history(cmd: &str) {
    let history = get_command_history();
    if history.is_empty() || history.last().unwrap() != cmd {
        history.push(String::from(cmd));
        if history.len() > 50 { history.remove(0); }
    }
    unsafe { HISTORY_INDEX = history.len(); }
}

pub fn tui_is_cancelled() -> bool { CANCELLED.load(Ordering::SeqCst) }

fn handle_input_event(event: InputEvent, input: &mut String, redraw: &mut bool, quit: &mut bool, exit_on_escape: bool) {
    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let w = TERM_WIDTH.load(Ordering::SeqCst) as usize;
    let prompt_prefix_len = INPUT_LEN.load(Ordering::SeqCst) as usize;

    match event {
        InputEvent::Char(c) => { input.insert(idx, c); CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst); *redraw = true; }
        InputEvent::Backspace => { if idx > 0 && !input.is_empty() { input.remove(idx - 1); CURSOR_IDX.store((idx - 1) as u16, Ordering::SeqCst); *redraw = true; } }
        InputEvent::Delete => { if idx < input.chars().count() { input.remove(idx); *redraw = true; } }
        InputEvent::Left => { if idx > 0 { CURSOR_IDX.store((idx - 1) as u16, Ordering::SeqCst); *redraw = true; } }
        InputEvent::Right => { if idx < input.chars().count() { CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst); *redraw = true; } }
        InputEvent::Up => {
            let (cx, cy) = calculate_input_cursor(input, idx, prompt_prefix_len, w);
            if cy > 0 {
                let (mut new_idx, mut cur_cx, mut cur_cy) = (0, prompt_prefix_len, 0);
                for (i, c) in input.chars().enumerate() {
                    if cur_cy == cy - 1 && cur_cx as u64 == cx { new_idx = i; break; }
                    if c == '\n' { if cur_cy == cy - 1 { new_idx = i; } cur_cx = 0; cur_cy += 1; }
                    else { cur_cx += 1; if cur_cx >= w { if cur_cy == cy - 1 { new_idx = i; } cur_cx = 0; cur_cy += 1; } }
                    if cur_cy > cy - 1 { break; }
                }
                CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst); *redraw = true;
            } else {
                let history = get_command_history();
                if !history.is_empty() {
                    unsafe {
                        if HISTORY_INDEX == history.len() { *get_saved_input() = input.clone(); }
                        if HISTORY_INDEX > 0 { HISTORY_INDEX -= 1; *input = history[HISTORY_INDEX].clone(); CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst); *redraw = true; }
                    }
                }
            }
        }
        InputEvent::Down => {
            let (cx, cy) = calculate_input_cursor(input, idx, prompt_prefix_len, w);
            let total_lines = count_wrapped_lines(input, prompt_prefix_len, w);
            if cy < (total_lines as u64).saturating_sub(1) {
                let (mut new_idx, mut cur_cx, mut cur_cy) = (input.chars().count(), prompt_prefix_len, 0);
                for (i, c) in input.chars().enumerate() {
                    if cur_cy == cy + 1 && cur_cx as u64 == cx { new_idx = i; break; }
                    if c == '\n' { cur_cx = 0; cur_cy += 1; }
                    else { cur_cx += 1; if cur_cx >= w { cur_cx = 0; cur_cy += 1; } }
                }
                CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst); *redraw = true;
            } else {
                let history = get_command_history();
                if !history.is_empty() {
                    unsafe {
                        if HISTORY_INDEX < history.len() { HISTORY_INDEX += 1; if HISTORY_INDEX == history.len() { *input = get_saved_input().clone(); } else { *input = history[HISTORY_INDEX].clone(); } CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst); *redraw = true; }
                    }
                }
            }
        }
        InputEvent::Home | InputEvent::CtrlA => { CURSOR_IDX.store(0, Ordering::SeqCst); *redraw = true; }
        InputEvent::End | InputEvent::CtrlE => { CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst); *redraw = true; }
        InputEvent::ShiftEnter => { input.insert(idx, '\n'); CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst); *redraw = true; }
        InputEvent::Enter => { if !input.is_empty() { let msg = input.clone(); add_to_history(&msg); input.clear(); CURSOR_IDX.store(0, Ordering::SeqCst); PROMPT_SCROLL_TOP.store(0, Ordering::SeqCst); get_message_queue().push_back(msg); *redraw = true; } }
        InputEvent::CtrlU => { input.clear(); CURSOR_IDX.store(0, Ordering::SeqCst); PROMPT_SCROLL_TOP.store(0, Ordering::SeqCst); *redraw = true; }
        InputEvent::CtrlW => {
            let (old_idx, mut new_idx) = (idx, idx);
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b == b' ') { new_idx -= 1; }
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b != b' ') { new_idx -= 1; }
            for _ in 0..(old_idx - new_idx) { if new_idx < input.len() { input.remove(new_idx); } }
            CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst); *redraw = true;
        }
        InputEvent::AltLeft => {
            let mut new_idx = idx;
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b == b' ') { new_idx -= 1; }
            while new_idx > 0 && input.as_bytes().get(new_idx-1).map_or(false, |&b| b != b' ') { new_idx -= 1; }
            CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst); *redraw = true;
        }
        InputEvent::AltRight => {
            let mut new_idx = idx;
            let len = input.chars().count();
            while new_idx < len && input.as_bytes().get(new_idx).map_or(false, |&b| b == b' ') { new_idx += 1; }
            while new_idx < len && input.as_bytes().get(new_idx).map_or(false, |&b| b != b' ') { new_idx += 1; }
            CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst); *redraw = true;
        }
        InputEvent::CtrlL => {
            let (nw, nh) = probe_terminal_size();
            TERM_WIDTH.store(nw, Ordering::SeqCst); TERM_HEIGHT.store(nh, Ordering::SeqCst);
            let layout = get_pane_layout(); layout.term_width = nw; layout.term_height = nh; layout.recalculate(layout.footer_height);
            clear_screen(); print_greeting(); layout.set_scroll_region();
            layout.output_row = layout.output_bottom; layout.output_col = 0;
            CUR_ROW.store(layout.output_row, Ordering::SeqCst); CUR_COL.store(0, Ordering::SeqCst);
            *redraw = true;
        }
        InputEvent::Esc | InputEvent::Interrupt => { CANCELLED.store(true, Ordering::SeqCst); if exit_on_escape || event == InputEvent::Interrupt { *quit = true; } }
        _ => {}
    }
}

pub fn tui_handle_input(current_tokens: usize, token_limit: usize, mem_kb: usize) {
    if !TUI_ACTIVE.load(Ordering::SeqCst) { return; }
    let mut event_buf = [0u8; 16];
    let bytes_read = poll_input_event(0, &mut event_buf);
    let queue = input::get_raw_input_queue();
    if bytes_read > 0 { for i in 0..bytes_read as usize { queue.push_back(event_buf[i]); } input::update_last_input_time(); }
    let input = get_global_input();
    let (mut redraw, mut quit) = (false, false);
    while !queue.is_empty() {
        let mut temp_buf = [0u8; 16];
        let n_copy = core::cmp::min(queue.len(), 16);
        for i in 0..n_copy { temp_buf[i] = queue[i]; }
        let (event, n) = input::parse_input(&temp_buf[..n_copy]);
        if n == 0 { break; }
        for _ in 0..n { queue.pop_front(); }
        handle_input_event(event, input, &mut redraw, &mut quit, false);
    }
    if redraw || STREAMING.load(Ordering::SeqCst) { render_footer_internal(input, current_tokens, token_limit, mem_kb); }
}

pub fn calculate_history_bytes(history: &[Message]) -> usize {
    let bytes = history.iter().map(|msg| msg.role.len() + msg.content.len() + 32).sum();
    unsafe { LAST_HISTORY_KB = bytes / 1024; }
    bytes
}

pub fn visual_length(s: &str) -> usize {
    let (mut len, mut in_esc) = (0, false);
    for c in s.chars() {
        if in_esc { if c != '[' && c >= '@' && c <= '~' { in_esc = false; } continue; }
        if c == '\x1b' { in_esc = true; continue; }
        len += 1;
    }
    len
}

fn render_footer_internal(input: &str, current_tokens: usize, token_limit: usize, mem_kb: usize) {
    let mut stdout = Stdout;
    let layout = get_pane_layout();
    let (w, h) = (layout.term_width as usize, layout.term_height as u64);
    layout.repaint_counter = layout.repaint_counter.wrapping_add(1) % 10000;
    let is_streaming = STREAMING.load(Ordering::SeqCst);
    if layout.repaint_counter % 10 == 0 { layout.status_dots = (layout.status_dots % 5) + 1; }
    if !is_streaming && layout.status_text.is_empty() { layout.update_status("[MEOW] awaiting user input", 0, None); }

    let t_disp = if current_tokens >= 1000 { format!("{}k", current_tokens / 1000) } else { format!("{}", current_tokens) };
    let l_disp = format!("{}k", token_limit / 1000);
    let m_disp = if mem_kb >= 1024 { format!("{}M", mem_kb / 1024) } else { format!("{}K", mem_kb) };
    let h_kb = unsafe { LAST_HISTORY_KB };
    let color = if h_kb > 256 { "\x1b[38;5;196m" } else if h_kb > 128 { "\x1b[38;5;226m" } else { COLOR_YELLOW };
    let hist_disp = format!("|{}Hist: {}K{}", color, h_kb, COLOR_RESET);
    let q_len = get_message_queue().len();
    let q_disp = if q_len > 0 { format!(" [QUEUED: {}]", q_len) } else { String::new() };

    let prompt_prefix = format!("  {}[{}/{}|{}{}{}] {}(=^･ω･^=) > ", COLOR_YELLOW, t_disp, l_disp, m_disp, hist_disp, COLOR_YELLOW, q_disp);
    let p_len = visual_length(&prompt_prefix);
    INPUT_LEN.store(p_len as u16, Ordering::SeqCst);
    layout.input_prefix_len = p_len as u16;

    let wrapped = count_wrapped_lines(input, p_len, w);
    let n_f_h = (core::cmp::min(wrapped, core::cmp::min(10, (h / 3) as usize)) + 2) as u16;
    let o_f_h = layout.footer_height;
    let eff_f_h = if is_streaming && n_f_h < o_f_h { o_f_h } else { n_f_h };
    let eff_p_l = (eff_f_h as usize).saturating_sub(2);
    
    if eff_f_h != o_f_h {
        let (o_o_b, o_s_r) = (layout.output_bottom, layout.status_row);
        if eff_f_h > o_f_h {
            let growth = eff_f_h - o_f_h;
            for r in o_s_r..=(o_s_r + growth) { set_cursor_position(0, r as u64); let _ = write!(stdout, "{}", CLEAR_TO_EOL); }
            set_cursor_position(0, o_o_b as u64); for _ in 0..growth { akuma_write(fd::STDOUT, b"\n"); }
            layout.recalculate(eff_f_h); layout.set_scroll_region();
            layout.output_row = layout.output_row.saturating_sub(growth);
            if layout.output_row > layout.output_bottom { layout.output_row = layout.output_bottom; }
            CUR_ROW.store(layout.output_row, Ordering::SeqCst);
        } else {
            for r in (h as u16 - o_f_h)..(h as u16 - eff_f_h) { set_cursor_position(0, r as u64); let _ = write!(stdout, "{}", CLEAR_TO_EOL); }
            layout.recalculate(eff_f_h); layout.set_scroll_region();
        }
    }

    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    let (_, cy_abs) = calculate_input_cursor(input, idx, p_len, w);
    let mut s_t = layout.prompt_scroll;
    if cy_abs < s_t as u64 { s_t = cy_abs as u16; } 
    else if cy_abs >= (s_t as u64 + eff_p_l as u64) { s_t = (cy_abs - eff_p_l as u64 + 1) as u16; }
    layout.prompt_scroll = s_t; PROMPT_SCROLL_TOP.store(s_t, Ordering::SeqCst);

    hide_cursor();
    let s_r = h - eff_f_h as u64;
    if eff_f_h < o_f_h {
        let o_st_r = (h - o_f_h as u64).saturating_sub(1);
        for r in o_st_r..s_r { set_cursor_position(0, r); let _ = write!(stdout, "{}", CLEAR_TO_EOL); }
    }
    
    set_cursor_position(0, s_r.saturating_sub(1)); let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    if !layout.status_text.is_empty() {
        let _ = write!(stdout, "  {}{}", layout.status_color, layout.status_text);
        for _ in 0..layout.status_dots { let _ = write!(stdout, "."); }
        for _ in layout.status_dots..5 { let _ = write!(stdout, " "); }
        let ms = if let Some(ms) = layout.status_time_ms { Some(ms) } else if layout.status_start_us > 0 && !layout.status_text.contains("awaiting") { Some((libakuma::uptime() - layout.status_start_us) / 1000) } else { None };
        if let Some(ms) = ms { if ms < 1000 { let _ = write!(stdout, "~(=^‥^)ノ [{}ms]", ms); } else { let _ = write!(stdout, "~(=^‥^)ノ [{}.{}s]", ms / 1000, (ms % 1000) / 100); } }
        let _ = write!(stdout, "{}", COLOR_RESET);
    }
    
    set_cursor_position(0, s_r); let _ = write!(stdout, "{}{}{}", COLOR_GRAY_DIM, "━".repeat(w), COLOR_RESET);
    let p_r = s_r + 1; set_cursor_position(0, p_r); let _ = write!(stdout, "{}", CLEAR_TO_EOL);
    let (mod_n, prov_n) = get_model_and_provider();
    let _ = write!(stdout, "  {}{}[Provider: {}] [Model: {}]{}", COLOR_GRAY_DIM, COLOR_RESET, prov_n, mod_n, COLOR_RESET);

    for i in 0..eff_p_l { set_cursor_position(0, p_r + 1 + i as u64); let _ = write!(stdout, "{}", CLEAR_TO_EOL); }
    if s_t == 0 { set_cursor_position(0, p_r + 1); let _ = write!(stdout, "{}{}{}{}", COLOR_VIOLET, COLOR_BOLD, prompt_prefix, COLOR_RESET); }
    let _ = write!(stdout, "{}", COLOR_VIOLET);
    let (mut c_l, mut c_c) = (0, p_len);
    for c in input.chars() {
        if c == '\n' { c_l += 1; c_c = 4; }
        else {
            if c_l >= s_t as usize && c_l < (s_t as usize + eff_p_l) {
                set_cursor_position(c_c as u64, p_r + 1 + (c_l as u64 - s_t as u64));
                let mut b = [0u8; 4]; let _ = write!(stdout, "{}", c.encode_utf8(&mut b));
            }
            c_c += 1; if c_c >= w { c_l += 1; c_c = 4; }
        }
    }
    let _ = write!(stdout, "{}", COLOR_RESET);
    let (cx, cy_off) = calculate_input_cursor(input, idx, p_len, w);
    set_cursor_position(cx, p_r + 1 + (cy_off - s_t as u64));
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

pub fn run_tui(model: &mut String, provider: &mut Provider, config: &mut Config, history: &mut Vec<Message>, context_window: usize, system_prompt: &str) -> Result<(), &'static str> {
    let _guard = TuiGuard::new();
    let mut old_mode: u64 = 0;
    get_terminal_attributes(fd::STDIN, &mut old_mode as *mut u64 as u64);
    set_terminal_attributes(fd::STDIN, 0, mode_flags::RAW_MODE_ENABLE);

    let (w, h) = probe_terminal_size();
    TERM_WIDTH.store(w, Ordering::SeqCst); TERM_HEIGHT.store(h, Ordering::SeqCst);
    set_model_and_provider(model, &provider.name);
    
    let layout = get_pane_layout();
    layout.term_width = w; layout.term_height = h; layout.recalculate(4);
    
    let mut stdout = Stdout;
    let _ = write!(stdout, "\x1b[>1u\x1b[?1049h");
    clear_screen();
    layout.set_scroll_region();
    print_greeting();
    let _ = write!(stdout, "  {}TIP:{} Type {}/hotkeys{} to see input shortcuts nya~! ♪(=^･ω･^)ﾉ\n\n", COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_YELLOW, COLOR_RESET);

    layout.output_row = layout.output_bottom; layout.output_col = 0;
    CUR_ROW.store(layout.output_row, Ordering::SeqCst); CUR_COL.store(0, Ordering::SeqCst);

    loop {
        let c_t = calculate_history_tokens(history);
        let m_kb = libakuma::memory_usage() / 1024;
        calculate_history_bytes(history);
        render_footer_internal(get_global_input(), c_t, context_window, m_kb);

        let mut e_b = [0u8; 16];
        let b_r = poll_input_event(50, &mut e_b);
        let q = input::get_raw_input_queue();
        if b_r > 0 { for i in 0..b_r as usize { q.push_back(e_b[i]); } input::update_last_input_time(); }

        let inp = get_global_input();
        let (mut q_l, mut red) = (false, false);
        while !q.is_empty() {
            let mut t_b = [0u8; 16]; let n_c = core::cmp::min(q.len(), 16);
            for i in 0..n_c { t_b[i] = q[i]; }
            let (ev, n) = input::parse_input(&t_b[..n_c]);
            if n == 0 { break; }
            for _ in 0..n { q.pop_front(); }
            handle_input_event(ev, inp, &mut red, &mut q_l, config.exit_on_escape);
            if ev == InputEvent::Enter { break; }
        }
        if q_l { break; }

        if let Some(u_i) = get_message_queue().pop_front() {
            render_footer_internal(inp, c_t, context_window, m_kb);
            let layout = get_pane_layout();
            set_cursor_position(0, CUR_ROW.load(Ordering::SeqCst) as u64);
            tui_print_with_indent("\n\n", "", 0, None);
            tui_print_with_indent(&u_i, " >  ", 4, Some(&format!("{}{}", COLOR_VIOLET, COLOR_BOLD)));
            tui_print("\n");

            if u_i.starts_with('/') {
                let (res, out) = crate::app::handle_command(&u_i, model, provider, config, history, system_prompt);
                if let Some(o) = out {
                    let _ = write!(stdout, "  \n{}{}{}\n\n", COLOR_GRAY_BRIGHT, o, COLOR_RESET);
                    history.push(Message::new("system", &o));
                }
                if let CommandResult::Quit = res { break; }
                let o_r = TERM_HEIGHT.load(Ordering::SeqCst).saturating_sub(layout.footer_height + 1 + layout.gap());
                CUR_ROW.store(o_r, Ordering::SeqCst); CUR_COL.store(0, Ordering::SeqCst);
                layout.output_row = o_r; layout.output_col = 0;
            } else {
                STREAMING.store(true, Ordering::SeqCst);
                layout.update_status("[MEOW] jacking in", 1, None);
                tui_print("\n\n");
                let _ = crate::app::chat_once(model, provider, &u_i, history, Some(context_window), system_prompt);
                STREAMING.store(false, Ordering::SeqCst); CANCELLED.store(false, Ordering::SeqCst);
                layout.clear_status();
                let _ = write!(stdout, "{}\n", COLOR_RESET);
                compact_history(history);
            }
        }
    }

    get_pane_layout().reset_scroll_region();
    let _ = write!(stdout, "\x1b[<u");
    set_terminal_attributes(fd::STDIN, 0, old_mode);
    clear_screen(); set_cursor_position(0, 0); show_cursor();
    Ok(())
}
