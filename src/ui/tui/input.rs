use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};

pub static INPUT_LEN: AtomicU16 = AtomicU16::new(0);
pub static CURSOR_IDX: AtomicU16 = AtomicU16::new(0);
pub static PROMPT_SCROLL_TOP: AtomicU16 = AtomicU16::new(0);
static LAST_INPUT_TIME: AtomicU64 = AtomicU64::new(0);

static mut RAW_INPUT_QUEUE: Option<VecDeque<u8>> = None;

pub fn get_raw_input_queue() -> &'static mut VecDeque<u8> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(RAW_INPUT_QUEUE);
        if (*ptr).is_none() {
            *ptr = Some(VecDeque::with_capacity(64));
        }
        (*ptr).as_mut().unwrap()
    }
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

pub fn parse_input(buf: &[u8]) -> (InputEvent, usize) {
    if buf.is_empty() { return (InputEvent::Unknown, 0); }
    match buf[0] {
        0x03 => (InputEvent::Interrupt, 1),
        0x0D => {
            if buf.len() >= 2 && buf[1] == 0x0A { return (InputEvent::ShiftEnter, 2); }
            (InputEvent::Enter, 1)
        }
        0x0A => (InputEvent::ShiftEnter, 1),
        0x1B => {
            if buf.len() == 1 {
                let now = libakuma::uptime();
                if now.saturating_sub(LAST_INPUT_TIME.load(Ordering::Relaxed)) > 50000 { return (InputEvent::Esc, 1); }
                else { return (InputEvent::Unknown, 0); }
            }
            if buf[1] == 0x5B {
                let mut i = 2;
                while i < buf.len() {
                    let c = buf[i];
                    if (0x40..=0x7E).contains(&c) {
                        let len = i + 1;
                        let seq = &buf[2..len-1];
                        match c {
                            b'A' => return (InputEvent::Up, len),
                            b'B' => return (InputEvent::Down, len),
                            b'C' => { if seq == b"1;3" { return (InputEvent::AltRight, len); } return (InputEvent::Right, len); }
                            b'D' => { if seq == b"1;3" { return (InputEvent::AltLeft, len); } return (InputEvent::Left, len); }
                            b'H' => return (InputEvent::Home, len),
                            b'F' => return (InputEvent::End, len),
                            b'~' => { if seq == b"3" { return (InputEvent::Delete, len); } return (InputEvent::Unknown, len); }
                            b'u' => {
                                if seq == b"13;2" || seq == b"13;5" { return (InputEvent::ShiftEnter, len); }
                                if let Some(semi_pos) = seq.iter().position(|&b| b == b';') {
                                    if let Ok(keycode_str) = core::str::from_utf8(&seq[..semi_pos]) {
                                        if let Ok(keycode) = keycode_str.parse::<u32>() {
                                            if let Ok(mod_str) = core::str::from_utf8(&seq[semi_pos+1..]) {
                                                if let Ok(modifier) = mod_str.parse::<u32>() {
                                                    let ctrl = (modifier.saturating_sub(1) & 4) != 0;
                                                    let alt = (modifier.saturating_sub(1) & 2) != 0;
                                                    if ctrl && !alt {
                                                        match keycode {
                                                            97 => return (InputEvent::CtrlA, len),
                                                            99 => return (InputEvent::Interrupt, len),
                                                            101 => return (InputEvent::CtrlE, len),
                                                            106 => return (InputEvent::ShiftEnter, len),
                                                            108 => return (InputEvent::CtrlL, len),
                                                            117 => return (InputEvent::CtrlU, len),
                                                            119 => return (InputEvent::CtrlW, len),
                                                            _ => {}
                                                        }
                                                    }
                                                    if alt && !ctrl {
                                                        match keycode {
                                                            98 => return (InputEvent::AltLeft, len),
                                                            102 => return (InputEvent::AltRight, len),
                                                            _ => {}
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    if let Ok(keycode_str) = core::str::from_utf8(seq) {
                                        if let Ok(keycode) = keycode_str.parse::<u32>() {
                                            match keycode {
                                                27 => return (InputEvent::Esc, len),
                                                13 => return (InputEvent::Enter, len),
                                                127 => return (InputEvent::Backspace, len),
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
            if buf[1] == 0x4F {
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

pub fn update_last_input_time() {
    LAST_INPUT_TIME.store(libakuma::uptime(), Ordering::Relaxed);
}

pub fn calculate_input_cursor(input: &str, idx: usize, prompt_width: usize, width: usize) -> (u64, u64) {
    if width == 0 { return (0, 0); }
    let (mut cx, mut cy) = (prompt_width, 0);
    for (i, c) in input.chars().enumerate() {
        if i >= idx { break; }
        if c == '\n' { cx = 4; cy += 1; }
        else { cx += 1; if cx >= width { cx = 4; cy += 1; } }
    }
    (cx as u64, cy as u64)
}

pub fn get_idx_from_coords(input: &str, target_cx: u64, target_cy: u64, prompt_width: usize, width: usize) -> usize {
    if width == 0 { return 0; }
    let (mut cx, mut cy) = (prompt_width as u64, 0u64);
    let mut best_idx = 0;
    
    for (i, c) in input.chars().enumerate() {
        if cy == target_cy {
            best_idx = i;
            if cx >= target_cx {
                return i;
            }
        }
        if cy > target_cy {
            return best_idx;
        }
        
        if c == '\n' { cx = 4; cy += 1; }
        else { cx += 1; if cx >= width as u64 { cx = 4; cy += 1; } }
    }
    
    if cy == target_cy {
        return input.chars().count();
    }
    
    best_idx
}

pub fn count_wrapped_lines(input: &str, prompt_width: usize, width: usize) -> usize {
    if width == 0 { return 1; }
    let (mut lines, mut current_col) = (1, prompt_width);
    for c in input.chars() {
        if c == '\n' { lines += 1; current_col = 4; }
        else { current_col += 1; if current_col >= width { lines += 1; current_col = 4; } }
    }
    lines
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
