use core::sync::atomic::Ordering;
use libakuma::{set_cursor_position, hide_cursor, show_cursor, write as akuma_write, fd};
use crate::util::StackBuffer;
use core::fmt::Write;

use crate::config::{COLOR_YELLOW, COLOR_RESET, COLOR_VIOLET, COLOR_BOLD, COLOR_GRAY_DIM};
use crate::app::state::{self, STREAMING};
use super::layout::{get_pane_layout, TERM_WIDTH, TERM_HEIGHT, CLEAR_TO_EOL, Stdout};
use super::input::{self, INPUT_LEN, CURSOR_IDX, PROMPT_SCROLL_TOP};

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

pub fn tui_print(s: &str) {
    tui_print_with_indent(s, "", 9, None);
}

pub fn tui_print_assistant(s: &str) {
    tui_print_with_indent(s, "", 9, Some(crate::config::COLOR_MEOW));
}

pub static mut TEST_CAPTURE: Option<alloc::vec::Vec<alloc::string::String>> = None;

pub fn tui_print_with_indent(s: &str, prefix: &str, indent: u16, color: Option<&str>) {
    unsafe {
        if let Some(ref mut v) = TEST_CAPTURE {
            v.push(alloc::string::String::from(s));
            return;
        }
    }
    if s.is_empty() && prefix.is_empty() { return; }
    let w = TERM_WIDTH.load(Ordering::SeqCst);
    let h = TERM_HEIGHT.load(Ordering::SeqCst);
    let mut col = crate::tui_app::CUR_COL.load(Ordering::SeqCst);
    let mut row = crate::tui_app::CUR_ROW.load(Ordering::SeqCst);
    
    let layout = get_pane_layout();
    let gap = layout.gap();
    let max_row = h.saturating_sub(layout.footer_height + 1 + gap);

    set_cursor_position(col as u64, row as u64);
    if let Some(c) = color { akuma_write(fd::STDOUT, c.as_bytes()); }
    if col == 0 {
        if !prefix.is_empty() {
            akuma_write(fd::STDOUT, prefix.as_bytes());
            col = input::visual_length(prefix) as u16;
        } else if indent > 0 {
            for _ in 0..indent { akuma_write(fd::STDOUT, b" "); }
            col = indent;
        }
    }
    
    let mut word_buf: alloc::vec::Vec<char> = alloc::vec::Vec::with_capacity(64);
    let mut word_display_len: u16 = 0;
    let mut in_esc = false;
    let mut esc_buf: alloc::vec::Vec<char> = alloc::vec::Vec::with_capacity(16);
    
    let wrap_line = |row: &mut u16, col: &mut u16, max_row: u16| {
        *row += 1;
        if *row > max_row { *row = max_row; akuma_write(fd::STDOUT, b"\n"); }
        else { set_cursor_position(0, *row as u64); }
        for _ in 0..indent { akuma_write(fd::STDOUT, b" "); }
        *col = indent;
    };
    
    let flush_word = |word_buf: &mut alloc::vec::Vec<char>, word_display_len: &mut u16, col: &mut u16, row: &mut u16, max_row: u16, w: u16, indent: u16| {
        if word_buf.is_empty() { return; }
        if *col + *word_display_len > w.saturating_sub(1) && *col > indent { wrap_line(row, col, max_row); }
        for c in word_buf.iter() {
            let mut buf = [0u8; 4];
            akuma_write(fd::STDOUT, c.encode_utf8(&mut buf).as_bytes());
        }
        *col += *word_display_len;
        word_buf.clear();
        *word_display_len = 0;
    };

    let is_delimiter = |c: char| c == ' ' || c == '\t' || c == '-' || c == '/' || c == '\\' || c == ':';
    let is_punctuation = |c: char| c == ',' || c == '.' || c == '!' || c == '?' || c == ';' || c == ':';
    
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
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
        
        if c == '\n' {
            flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
            wrap_line(&mut row, &mut col, max_row);
        } else if c == '\x08' {
            if col > indent { col -= 1; akuma_write(fd::STDOUT, b"\x08"); }
        } else if is_delimiter(c) {
            word_buf.push(c);
            if c != '\t' { word_display_len += 1; } else { word_display_len += 4; } // Basic tab handling
            
            // If it's a space, we flush AFTER the space to allow wrapping.
            // If it's a hyphen/slash, we also flush to allow wrapping there.
            flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
            
            if col >= w.saturating_sub(1) { wrap_line(&mut row, &mut col, max_row); }
        } else {
            word_buf.push(c);
            word_display_len += 1;
            
            // Peek next to see if it's punctuation. If so, don't flush yet even if we are at the end of line.
            let next_is_punct = chars.peek().map(|&next| is_punctuation(next)).unwrap_or(false);
            
            if word_display_len >= w.saturating_sub(indent) && !next_is_punct {
                flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
            }
        }
    }
    flush_word(&mut word_buf, &mut word_display_len, &mut col, &mut row, max_row, w, indent);
    if color.is_some() { akuma_write(fd::STDOUT, COLOR_RESET.as_bytes()); }

    crate::tui_app::CUR_COL.store(col, Ordering::SeqCst);
    crate::tui_app::CUR_ROW.store(row, Ordering::SeqCst);
    layout.output_col = col; layout.output_row = row;

    state::with_global_input(|input_str| {
        let (cx, cy_off) = input::calculate_input_cursor(input_str, CURSOR_IDX.load(Ordering::SeqCst) as usize, INPUT_LEN.load(Ordering::SeqCst) as usize, w as usize);
        let scroll_top = PROMPT_SCROLL_TOP.load(Ordering::SeqCst) as u64;
        let prompt_start_row = h as u64 - layout.footer_height as u64 + 2;
        let final_cy = prompt_start_row + (cy_off - scroll_top);
        let clamped_cy = if final_cy >= h as u64 { h as u64 - 1 } else { final_cy };
        set_cursor_position(cx, clamped_cy);
    });
}

pub fn render_footer(current_tokens: usize, token_limit: usize, mem_kb: usize) {
    let mut stdout = Stdout;
    let layout = get_pane_layout();
    let (w, h) = (layout.term_width as usize, layout.term_height as u64);
    layout.repaint_counter = layout.repaint_counter.wrapping_add(1) % 10000;
    let is_streaming = STREAMING.load(Ordering::SeqCst);
    if layout.repaint_counter % 10 == 0 { layout.status_dots = (layout.status_dots % 5) + 1; }
    if !is_streaming && layout.status_text.is_empty() { layout.update_status("[MEOW] awaiting user input", 0, None); }

    let mut t_disp_buf_data = [0u8; 16];
    let mut t_disp_buf = StackBuffer::new(&mut t_disp_buf_data);
    let t_disp = if current_tokens >= 1000 { let _ = write!(t_disp_buf, "{}k", current_tokens / 1000); t_disp_buf.as_str() } else { let _ = write!(t_disp_buf, "{}", current_tokens); t_disp_buf.as_str() };
    
    let mut l_disp_buf_data = [0u8; 16];
    let mut l_disp_buf = StackBuffer::new(&mut l_disp_buf_data);
    let _ = write!(l_disp_buf, "{}k", token_limit / 1000);
    let l_disp = l_disp_buf.as_str();

    let mut m_disp_buf_data = [0u8; 16];
    let mut m_disp_buf = StackBuffer::new(&mut m_disp_buf_data);
    let m_disp = if mem_kb >= 1024 { let _ = write!(m_disp_buf, "{}M", mem_kb / 1024); m_disp_buf.as_str() } else { let _ = write!(m_disp_buf, "{}K", mem_kb); m_disp_buf.as_str() };
    let h_kb = state::get_last_history_kb();
    let color = if h_kb > 256 { "\x1b[38;5;196m" } else if h_kb > 128 { "\x1b[38;5;226m" } else { COLOR_YELLOW };
    let mut hist_disp_buf_data = [0u8; 64];
    let mut hist_disp_buf = StackBuffer::new(&mut hist_disp_buf_data);
    let _ = write!(hist_disp_buf, "|{}Hist: {}K{}", color, h_kb, COLOR_RESET);
    let hist_disp = hist_disp_buf.as_str();
    let q_len = state::message_queue_len();
    let mut q_disp_buf_data = [0u8; 32];
    let mut q_disp_buf = StackBuffer::new(&mut q_disp_buf_data);
    if q_len > 0 {
        let _ = write!(q_disp_buf, " [QUEUED: {}]", q_len);
    }
    let q_disp = q_disp_buf.as_str();

    let mut prompt_prefix_buf_data = [0u8; 128]; // Choose a size that's large enough
    let mut prompt_prefix_buf = StackBuffer::new(&mut prompt_prefix_buf_data);
    let _ = write!(prompt_prefix_buf, "  {}[{}/{}|{}{}{}] {}(=^･ω･^=) > ", COLOR_YELLOW, t_disp, l_disp, m_disp, hist_disp, COLOR_YELLOW, q_disp);
    let prompt_prefix = prompt_prefix_buf.as_str();
    let p_len = input::visual_length(&prompt_prefix);
    INPUT_LEN.store(p_len as u16, Ordering::SeqCst);
    layout.input_prefix_len = p_len as u16;

    state::with_global_input(|input_str| {
        let wrapped = input::count_wrapped_lines(input_str, p_len, w);
        let n_f_h = (core::cmp::min(wrapped, core::cmp::min(10, (h / 3) as usize)) + 2) as u16;
        let o_f_h = layout.footer_height;
        let eff_f_h = if is_streaming && n_f_h < o_f_h { o_f_h } else { n_f_h };
        let eff_p_l = (eff_f_h as usize).saturating_sub(2);
        
        if eff_f_h != o_f_h {
            let (o_o_b, o_s_r) = (layout.output_bottom, layout.status_row);
            if eff_f_h > o_f_h {
                let growth = eff_f_h - o_f_h;
                for r in o_s_r..=(o_s_r + growth) { set_cursor_position(0, r as u64); let _ = akuma_write(fd::STDOUT, CLEAR_TO_EOL.as_bytes()); }
                set_cursor_position(0, o_o_b as u64); for _ in 0..growth { akuma_write(fd::STDOUT, b"\n"); }
                layout.recalculate(eff_f_h); layout.set_scroll_region();
                layout.output_row = layout.output_row.saturating_sub(growth);
                if layout.output_row > layout.output_bottom { layout.output_row = layout.output_bottom; }
                crate::tui_app::CUR_ROW.store(layout.output_row, Ordering::SeqCst);
            } else {
                for r in (h as u16 - o_f_h)..(h as u16 - eff_f_h) { set_cursor_position(0, r as u64); let _ = akuma_write(fd::STDOUT, CLEAR_TO_EOL.as_bytes()); }
                layout.recalculate(eff_f_h); layout.set_scroll_region();
            }
        }

        let idx = CURSOR_IDX.load(Ordering::SeqCst) as usize;
        let (_, cy_abs) = input::calculate_input_cursor(input_str, idx, p_len, w);
        let mut s_t = layout.prompt_scroll;
        if cy_abs < s_t as u64 { s_t = cy_abs as u16; } 
        else if cy_abs >= (s_t as u64 + eff_p_l as u64) { s_t = (cy_abs - eff_p_l as u64 + 1) as u16; }
        layout.prompt_scroll = s_t; PROMPT_SCROLL_TOP.store(s_t, Ordering::SeqCst);

        hide_cursor();
        let s_r = h - eff_f_h as u64;
        if eff_f_h < o_f_h {
            let o_st_r = (h - o_f_h as u64).saturating_sub(1);
            for r in o_st_r..s_r { set_cursor_position(0, r); let _ = akuma_write(fd::STDOUT, CLEAR_TO_EOL.as_bytes()); }
        }
        
        set_cursor_position(0, s_r.saturating_sub(1)); let _ = akuma_write(fd::STDOUT, CLEAR_TO_EOL.as_bytes());
        if !layout.status_text.is_empty() {
            let _ = write!(stdout, "  {}{}", layout.status_color, layout.status_text);
            for _ in 0..layout.status_dots { let _ = write!(stdout, "."); }
            for _ in layout.status_dots..5 { let _ = write!(stdout, " "); }
            let ms = if let Some(ms) = layout.status_time_ms { Some(ms) } else if layout.status_start_us > 0 && !layout.status_text.contains("awaiting") { Some((libakuma::uptime() - layout.status_start_us) / 1000) } else { None };
            if let Some(ms) = ms { if ms < 1000 { let _ = write!(stdout, "~(=^‥^)ノ [{}ms]", ms); } else { let _ = write!(stdout, "~(=^‥^)ノ [{}.{}s]", ms / 1000, (ms % 1000) / 100); } }
            let _ = write!(stdout, "{}", COLOR_RESET);
        }
        
        set_cursor_position(0, s_r);
        akuma_write(fd::STDOUT, COLOR_GRAY_DIM.as_bytes());
        for _ in 0..w { akuma_write(fd::STDOUT, "━".as_bytes()); }
        akuma_write(fd::STDOUT, COLOR_RESET.as_bytes());

        let p_r = s_r + 1; set_cursor_position(0, p_r); let _ = akuma_write(fd::STDOUT, CLEAR_TO_EOL.as_bytes());
        
        state::with_model_and_provider(|mod_n, prov_n| {
            let mut stdout = Stdout;
            let _ = write!(stdout, "  {}{}[Provider: {}] [Model: {}]{}", COLOR_GRAY_DIM, COLOR_RESET, prov_n, mod_n, COLOR_RESET);
        });

        for i in 0..eff_p_l { set_cursor_position(0, p_r + 1 + i as u64); let _ = akuma_write(fd::STDOUT, CLEAR_TO_EOL.as_bytes()); }
        if s_t == 0 { set_cursor_position(0, p_r + 1); let _ = write!(stdout, "{}{}{}{}", COLOR_VIOLET, COLOR_BOLD, prompt_prefix, COLOR_RESET); }
        let _ = akuma_write(fd::STDOUT, COLOR_VIOLET.as_bytes());
        let (mut c_l, mut c_c) = (0, p_len);
        for c in input_str.chars() {
            if c == '\n' { c_l += 1; c_c = 4; }
            else {
                if c_l >= s_t as usize && c_l < (s_t as usize + eff_p_l) {
                    set_cursor_position(c_c as u64, p_r + 1 + (c_l as u64 - s_t as u64));
                    let mut b = [0u8; 4]; let _ = akuma_write(fd::STDOUT, c.encode_utf8(&mut b).as_bytes());
                }
                c_c += 1; if c_c >= w { c_l += 1; c_c = 4; }
            }
        }
        let _ = akuma_write(fd::STDOUT, COLOR_RESET.as_bytes());
        let (cx, cy_off) = input::calculate_input_cursor(input_str, idx, p_len, w);
        set_cursor_position(cx, p_r + 1 + (cy_off - s_t as u64));
        show_cursor();
    });
}

pub fn print_greeting() {
    let mut stdout = Stdout;
    use core::fmt::Write;
    let _ = write!(stdout, "\n{}\x1b[38;5;236m", COLOR_RESET);
    let _ = write!(stdout, "{}", CAT_ASCII);
    let _ = write!(stdout, "{}\n  {}MEOW!{} ~(=^‥^)ノ\n\n", COLOR_RESET, COLOR_BOLD, COLOR_RESET);
}
