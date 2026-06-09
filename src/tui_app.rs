use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

use libakuma::{
    get_terminal_attributes, set_terminal_attributes, 
    set_cursor_position, clear_screen, poll_input_event, write as akuma_write, show_cursor, fd
};

use crate::config::{Provider, Config, COLOR_GRAY_BRIGHT, COLOR_YELLOW, COLOR_RESET, COLOR_BOLD, COLOR_VIOLET, COLOR_USER};
use crate::app::{self, Message, Conversation, commands::CommandResult, state};
use crate::ui::tui::layout::{get_pane_layout, TERM_WIDTH, TERM_HEIGHT};
use crate::ui::tui::input::{self, InputEvent, CURSOR_IDX};
use crate::ui::tui::render;

pub static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static CANCELLED: AtomicBool = AtomicBool::new(false);
pub static DEBUG_MODE: AtomicBool = AtomicBool::new(false);
pub static CUR_COL: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);
pub static CUR_ROW: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);

struct TuiGuard;
impl TuiGuard {
    fn new() -> Self { state::TUI_ACTIVE.store(true, Ordering::SeqCst); TUI_ACTIVE.store(true, Ordering::SeqCst); Self }
}
impl Drop for TuiGuard {
    fn drop(&mut self) { state::TUI_ACTIVE.store(false, Ordering::SeqCst); TUI_ACTIVE.store(false, Ordering::SeqCst); }
}

pub mod mode_flags {
    pub const RAW_MODE_ENABLE: u64 = 0x01;
}

pub fn tui_print(s: &str) { render::tui_print(s); }
pub fn tui_print_assistant(s: &str) { render::tui_print_assistant(s); }
pub fn tui_print_with_indent(s: &str, prefix: &str, indent: u16, color: Option<&str>) { render::tui_print_with_indent(s, prefix, indent, color); }
pub fn tui_render_markdown(markdown: &str) {
    tui_render_markdown_with_indent(markdown, 9);
}
pub fn tui_render_markdown_with_indent(markdown: &str, indent: u16) {
    let renderer = crate::ui::tui::markdown::MarkdownRenderer::new(indent);
    renderer.render(markdown);
}
pub fn update_streaming_status(text: &str, dots: u8, time_ms: Option<u64>) { 
    state::STREAMING.store(true, Ordering::SeqCst);
    get_pane_layout().update_status(text, dots, time_ms); 
}
pub fn clear_streaming_status() { 
    state::STREAMING.store(false, Ordering::SeqCst);
    get_pane_layout().clear_status(); 
}
pub fn set_model_and_provider(model: &str, provider: &str) { state::set_model_and_provider(model, provider); }
pub fn tui_is_cancelled() -> bool { state::CANCELLED.load(Ordering::SeqCst) }

static mut STREAMING_RENDERER: Option<crate::ui::tui::stream::StreamingRenderer> = None;

pub fn start_streaming(indent: u16) {
    unsafe { *core::ptr::addr_of_mut!(STREAMING_RENDERER) = Some(crate::ui::tui::stream::StreamingRenderer::new(indent)); }
}

pub fn process_streaming_chunk(chunk: &str) {
    unsafe {
        if let Some(r) = (*core::ptr::addr_of_mut!(STREAMING_RENDERER)).as_mut() {
            r.process_chunk(chunk);
        } else {
            render::tui_print_assistant(chunk);
        }
    }
}

pub fn finish_streaming() {
    unsafe {
        if let Some(mut r) = (*core::ptr::addr_of_mut!(STREAMING_RENDERER)).take() {
            r.finalize();
        }
    }
}

fn handle_input_event(event: InputEvent, input: &mut String, redraw: &mut bool, quit: &mut bool, exit_on_escape: bool) {
    let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
    match event {
        InputEvent::Char(c) => { input.insert(idx, c); CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst); *redraw = true; }
        InputEvent::Backspace => { if idx > 0 && !input.is_empty() { input.remove(idx - 1); CURSOR_IDX.store((idx - 1) as u16, Ordering::SeqCst); *redraw = true; } }
        InputEvent::Delete => { if idx < input.chars().count() { input.remove(idx); *redraw = true; } }
        InputEvent::Left => { if idx > 0 { CURSOR_IDX.store((idx - 1) as u16, Ordering::SeqCst); *redraw = true; } }
        InputEvent::Right => { if idx < input.chars().count() { CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst); *redraw = true; } }
        InputEvent::Up => {
            let w = TERM_WIDTH.load(Ordering::SeqCst) as usize;
            let p_w = input::INPUT_LEN.load(Ordering::SeqCst) as usize;
            let (cx, cy) = input::calculate_input_cursor(input, idx, p_w, w);
            if cy > 0 {
                let new_idx = input::get_idx_from_coords(input, cx, cy - 1, p_w, w);
                CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst);
                *redraw = true;
            } else {
                let history_len = state::get_history_len();
                let history_index = state::get_history_index();
                if history_index > 0 {
                    if history_index == history_len { state::set_saved_input(input.clone()); }
                    state::set_history_index(history_index - 1);
                    if let Some(item) = state::get_history_item(history_index - 1) {
                        *input = item;
                        CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst);
                        *redraw = true;
                    }
                }
            }
        }
        InputEvent::Down => {
            let w = TERM_WIDTH.load(Ordering::SeqCst) as usize;
            let p_w = input::INPUT_LEN.load(Ordering::SeqCst) as usize;
            let (cx, cy) = input::calculate_input_cursor(input, idx, p_w, w);
            let num_lines = input::count_wrapped_lines(input, p_w, w);
            if (cy as usize) < num_lines - 1 {
                let new_idx = input::get_idx_from_coords(input, cx, cy + 1, p_w, w);
                CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst);
                *redraw = true;
            } else {
                let history_len = state::get_history_len();
                let history_index = state::get_history_index();
                if history_index < history_len {
                    state::set_history_index(history_index + 1);
                    if history_index + 1 == history_len { *input = state::get_saved_input(); }
                    else if let Some(item) = state::get_history_item(history_index + 1) { *input = item; }
                    CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst);
                    *redraw = true;
                }
            }
        }
        InputEvent::Home | InputEvent::CtrlA => { CURSOR_IDX.store(0, Ordering::SeqCst); *redraw = true; }
        InputEvent::End | InputEvent::CtrlE => { CURSOR_IDX.store(input.chars().count() as u16, Ordering::SeqCst); *redraw = true; }
        InputEvent::ShiftEnter => { input.insert(idx, '\n'); CURSOR_IDX.store((idx + 1) as u16, Ordering::SeqCst); *redraw = true; }
        InputEvent::Enter => { if !input.is_empty() { state::add_to_history(input); state::push_message(input.clone()); input.clear(); CURSOR_IDX.store(0, Ordering::SeqCst); *redraw = true; } }
        InputEvent::CtrlU => { input.clear(); CURSOR_IDX.store(0, Ordering::SeqCst); *redraw = true; }
        InputEvent::CtrlW | InputEvent::AltLeft => {
            let mut new_idx = idx;
            while new_idx > 0 && input.as_bytes().get(new_idx-1).is_some_and(|&b| b == b' ') { new_idx -= 1; }
            if event == InputEvent::CtrlW {
                while new_idx > 0 && input.as_bytes().get(new_idx-1).is_some_and(|&b| b != b' ' && b != b'\n') { new_idx -= 1; }
                for _ in 0..(idx - new_idx) { if new_idx < input.len() { input.remove(new_idx); } }
            } else {
                while new_idx > 0 && input.as_bytes().get(new_idx-1).is_some_and(|&b| b != b' ') { new_idx -= 1; }
            }
            CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst); *redraw = true;
        }
        InputEvent::AltRight => {
            let mut new_idx = idx;
            let len = input.chars().count();
            while new_idx < len && input.as_bytes().get(new_idx).is_some_and(|&b| b == b' ') { new_idx += 1; }
            while new_idx < len && input.as_bytes().get(new_idx).is_some_and(|&b| b != b' ') { new_idx += 1; }
            CURSOR_IDX.store(new_idx as u16, Ordering::SeqCst); *redraw = true;
        }
        InputEvent::CtrlL => {
            let (nw, nh) = probe_terminal_size();
            TERM_WIDTH.store(nw, Ordering::SeqCst); TERM_HEIGHT.store(nh, Ordering::SeqCst);
            let layout = get_pane_layout(); layout.term_width = nw; layout.term_height = nh; layout.recalculate(layout.footer_height);
            clear_screen(); render::print_greeting(); layout.set_scroll_region();
            let o_r = nh.saturating_sub(layout.footer_height + 1 + layout.gap());
            CUR_ROW.store(o_r, Ordering::SeqCst); CUR_COL.store(0, Ordering::SeqCst);
            layout.output_row = o_r; layout.output_col = 0; *redraw = true;
        }
        InputEvent::Esc | InputEvent::Interrupt => { state::CANCELLED.store(true, Ordering::SeqCst); CANCELLED.store(true, Ordering::SeqCst); if exit_on_escape || event == InputEvent::Interrupt { *quit = true; } }
        _ => {}
    }
}

/// Drain the raw input queue, dispatching events. Returns `(redraw, quit)`.
fn drain_queue(exit_on_escape: bool, break_on_enter: bool) -> (bool, bool) {
    let q = input::get_raw_input_queue();
    let mut inp = state::get_global_input();
    let (mut redraw, mut quit) = (false, false);
    while !q.is_empty() {
        let mut t_b = [0u8; 16]; let n_c = q.len().min(16);
        for i in 0..n_c { t_b[i] = q[i]; }
        let (event, n) = input::parse_input(&t_b[..n_c]);
        if n == 0 { break; }
        for _ in 0..n { q.pop_front(); }
        handle_input_event(event, &mut inp, &mut redraw, &mut quit, exit_on_escape);
        if break_on_enter && event == InputEvent::Enter { break; }
    }
    if redraw { state::set_global_input(inp); }
    (redraw, quit)
}

pub fn tui_handle_input(current_tokens: usize, token_limit: usize, mem_kb: usize) {
    if !TUI_ACTIVE.load(Ordering::SeqCst) { return; }
    let mut e_b = [0u8; 16];
    let b_r = poll_input_event(0, &mut e_b);
    let q = input::get_raw_input_queue();
    if b_r > 0 { for &b in e_b.iter().take(b_r as usize) { q.push_back(b); } input::update_last_input_time(); }

    if q.is_empty() {
        if state::STREAMING.load(Ordering::SeqCst) { render::render_footer(current_tokens, token_limit, mem_kb); }
        return;
    }

    let (redraw, _) = drain_queue(false, false);
    if redraw || state::STREAMING.load(Ordering::SeqCst) { render::render_footer(current_tokens, token_limit, mem_kb); }
}

fn probe_terminal_size() -> (u16, u16) {
    akuma_write(fd::STDOUT, b"\x1b[999;999H\x1b[6n");
    let mut buf = [0u8; 32];
    let n = poll_input_event(500, &mut buf);
    if n > 0 {
        if let Ok(resp) = core::str::from_utf8(&buf[..n as usize]) {
            if let Some(start) = resp.find('[') {
                if let Some(end) = resp.find('R') {
                    let parts = &resp[start+1..end];
                    let mut split = parts.split(';');
                    if let (Some(r_str), Some(c_str)) = (split.next(), split.next()) {
                        return (c_str.parse::<u16>().unwrap_or(100), r_str.parse::<u16>().unwrap_or(25));
                    }
                }
            }
        }
    }
    (100, 25)
}

pub fn run_tui(model: &mut String, provider: &mut Provider, config: &mut Config, conversation: &mut Conversation, context_window: usize, system_prompt: &str) -> Result<(), &'static str> {
    let _guard = TuiGuard::new();
    let mut old_mode: u64 = 0;
    get_terminal_attributes(fd::STDIN, &mut old_mode as *mut u64 as u64);
    set_terminal_attributes(fd::STDIN, 0, mode_flags::RAW_MODE_ENABLE);

    let (w, h) = probe_terminal_size();
    TERM_WIDTH.store(w, Ordering::SeqCst); TERM_HEIGHT.store(h, Ordering::SeqCst);
    state::set_model_and_provider(model, &provider.name);
    state::set_render_markdown(config.render_markdown);
    
    let layout = get_pane_layout();
    layout.term_width = w; layout.term_height = h; layout.recalculate(4);
    
    akuma_write(fd::STDOUT, b"\x1b[>1u\x1b[?1049h");
    clear_screen();
    layout.set_scroll_region();
    render::print_greeting();
    use crate::ui::tui::layout::Stdout;
    use core::fmt::Write;
    let mut stdout = Stdout;
    let _ = write!(stdout, "  {}TIP:{} Type {}/hotkeys{} to see input shortcuts\n\n", COLOR_GRAY_BRIGHT, COLOR_RESET, COLOR_YELLOW, COLOR_RESET);

    let o_r = h.saturating_sub(layout.footer_height + 1 + layout.gap());
    CUR_ROW.store(o_r, Ordering::SeqCst); CUR_COL.store(0, Ordering::SeqCst);
    layout.output_row = o_r; layout.output_col = 0;

    loop {
        let c_t = conversation.tokens();
        let m_kb = libakuma::memory_usage() / 1024;
        state::set_last_history_kb(c_t / 1024);
        render::render_footer(c_t, context_window, m_kb);

        let mut e_b = [0u8; 16];
        let b_r = poll_input_event(50, &mut e_b);
        let q = input::get_raw_input_queue();
        if b_r > 0 { for &b in e_b.iter().take(b_r as usize) { q.push_back(b); } input::update_last_input_time(); }

        if !q.is_empty() {
            let (_, quit) = drain_queue(config.exit_on_escape, true);
            if quit { break; }
        }

        if let Some(u_i) = state::pop_message() {
            render::render_footer(c_t, context_window, m_kb);
            set_cursor_position(0, CUR_ROW.load(Ordering::SeqCst) as u64);
            tui_print_with_indent("\n\n", "", 0, None);
            let mut color_buf_data = [0u8; 32];
            let mut color_buf = crate::util::StackBuffer::new(&mut color_buf_data);
            let _ = write!(color_buf, "{}{}", COLOR_VIOLET, COLOR_BOLD);
            tui_print_with_indent(" >  ", "", 0, Some(color_buf.as_str()));
            
            if config.render_markdown {
                tui_render_markdown_with_indent(&u_i, 4);
            } else {
                tui_print_with_indent(&u_i, "", 4, Some(COLOR_USER));
            }
            tui_print("\n");

            if u_i.starts_with('/') {
                let (res, out) = app::commands::handle_command(&u_i, model, provider, config, conversation, system_prompt);
                if let Some(o) = out {
                    tui_print_with_indent("\n", "", 0, None);
                    if config.render_markdown {
                        tui_render_markdown(&o);
                    } else {
                        tui_print_assistant(&o);
                    }
                    tui_print_with_indent("\n\n", "", 0, None);
                    conversation.append(&Message::new("system", &o));
                }
                if let CommandResult::Quit = res { break; }
            } else {
                state::STREAMING.store(true, Ordering::SeqCst);
                layout.update_status("[MEOW] jacking in", 1, None);
                tui_print("\n\n");
                let _ = app::chat::chat_once(model, provider, &u_i, conversation, Some(context_window), system_prompt);
                state::STREAMING.store(false, Ordering::SeqCst); state::CANCELLED.store(false, Ordering::SeqCst);
                layout.clear_status();
                let _ = writeln!(stdout, "{}", COLOR_RESET);
            }
        }
    }

    get_pane_layout().reset_scroll_region();
    // Exit kitty keyboard enhancement, then exit alternate screen buffer.
    // Alternate screen exit restores the previous terminal content automatically,
    // so we must NOT clear_screen() after this.
    akuma_write(fd::STDOUT, b"\x1b[<u\x1b[?1049l");
    set_terminal_attributes(fd::STDIN, 0, old_mode);
    show_cursor();
    Ok(())
}
