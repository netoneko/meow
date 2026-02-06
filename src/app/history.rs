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

    pub fn to_json(&self) -> String {
        let escaped_content = json_escape(&self.content);
        format!(
            "{{\"role\":\"{}\",\"content\":\"{}\"}}",
            self.role, escaped_content
        )
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

fn json_escape(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => {
                let code = c as u32;
                // Use double backslash for the format string itself
                result.push_str(&format!("\\u{:04x}", code));
            }
            _ => result.push(c),
        }
    }
    result
}