use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

#[derive(Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: String::from(role),
            content: String::from(content),
        }
    }

    pub fn write_json(&self, out: &mut String) {
        out.push_str("{\"role\":\"");
        out.push_str(&self.role);
        out.push_str("\",\"content\":\"");
        json_escape_to(&self.content, out);
        out.push_str("\"}");
    }
}

pub const MAX_HISTORY_SIZE: usize = 10;

pub fn trim_history(history: &mut Vec<Message>) {
    if history.len() > MAX_HISTORY_SIZE {
        let to_remove = history.len() - MAX_HISTORY_SIZE;
        history.drain(1..1 + to_remove);
    }
}

pub fn compact_history(history: &mut Vec<Message>) {
    for msg in history.iter_mut() {
        msg.role.shrink_to_fit();
        msg.content.shrink_to_fit();
    }
    history.shrink_to_fit();
}

pub fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

pub fn calculate_history_tokens(history: &[Message]) -> usize {
    history
        .iter()
        .map(|msg| estimate_tokens(&msg.content) + estimate_tokens(&msg.role) + 4)
        .sum()
}

fn json_escape_to(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let code = c as u32;
                out.push_str(&format!("\\u{:04x}", code));
            }
            _ => out.push(c),
        }
    }
}