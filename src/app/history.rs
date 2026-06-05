use alloc::string::String;
use alloc::vec::Vec;
use crate::util::json_escape_to;

#[derive(Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
    /// JSON array string of tool calls, set on assistant messages that invoke tools
    pub tool_calls_json: Option<String>,
    /// Tool call ID, set on role:"tool" result messages
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: String::from(role),
            content: String::from(content),
            tool_calls_json: None,
            tool_call_id: None,
        }
    }

    pub fn write_json(&self, out: &mut String) {
        out.push_str("{\"role\":\"");
        out.push_str(&self.role);
        out.push('"');

        if let Some(ref tc_json) = self.tool_calls_json {
            out.push_str(",\"content\":null,\"tool_calls\":");
            out.push_str(tc_json);
        } else {
            out.push_str(",\"content\":\"");
            json_escape_to(&self.content, out);
            out.push('"');
        }

        if let Some(ref tc_id) = self.tool_call_id {
            out.push_str(",\"tool_call_id\":\"");
            out.push_str(tc_id);
            out.push('"');
        }

        out.push('}');
    }
}

pub const MAX_HISTORY_SIZE: usize = 100;

pub fn compact_history(history: &mut Vec<Message>) {
    for msg in history.iter_mut() {
        msg.role.shrink_to_fit();
        msg.content.shrink_to_fit();
    }
    history.shrink_to_fit();
}

pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

pub fn calculate_history_tokens(history: &[Message]) -> usize {
    history
        .iter()
        .map(|msg| {
            let tc_tokens = msg.tool_calls_json.as_deref().map(estimate_tokens).unwrap_or(0);
            estimate_tokens(&msg.content) + estimate_tokens(&msg.role) + tc_tokens + 4
        })
        .sum()
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    use alloc::format;
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- history tests ---\n");

    // estimate_tokens
    let token_cases: &[(&str, usize)] = &[
        ("", 0), ("abcd", 1), ("abcde", 2), ("hello world!", 3),
    ];
    for (input, expected) in token_cases {
        total += 1;
        let got = estimate_tokens(input);
        if got == *expected { passed += 1; }
        else { libakuma::print(&format!("  [!] estimate_tokens({:?}): got {} want {}\n", input, got, expected)); }
    }

    // write_json basic
    total += 1;
    {
        let msg = Message::new("user", "hello");
        let mut out = String::new();
        msg.write_json(&mut out);
        let want = "{\"role\":\"user\",\"content\":\"hello\"}";
        if out == want { passed += 1; }
        else { libakuma::print(&format!("  [!] write_json: got {:?}\n", out)); }
    }

    // write_json with escape sequences
    total += 1;
    {
        let msg = Message::new("assistant", "line1\nline2\ttab\"quote");
        let mut out = String::new();
        msg.write_json(&mut out);
        let want = "{\"role\":\"assistant\",\"content\":\"line1\\nline2\\ttab\\\"quote\"}";
        if out == want { passed += 1; }
        else { libakuma::print(&format!("  [!] write_json escape: got {:?}\n", out)); }
    }

    // calculate_history_tokens returns > 0 for non-empty history
    total += 1;
    {
        let h = alloc::vec![Message::new("user", "hello world")];
        let tokens = calculate_history_tokens(&h);
        if tokens > 0 { passed += 1; }
        else { libakuma::print("  [!] calculate_history_tokens returned 0\n"); }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}

