use alloc::string::String;
use alloc::format;
use crate::config::COLOR_MEOW;
use super::render::tui_print_with_indent;

pub enum StreamState {
    Text,
    BufferingPotentialTool {
        buffer: String,
    },
    BufferingJson {
        buffer: String,
        depth: usize,
        in_string: bool,
        escape: bool,
    },
}

pub struct StreamingRenderer {
    state: StreamState,
    indent: u16,
    line_buf: String,
    at_line_start: bool,
}

impl StreamingRenderer {
    pub fn new(indent: u16) -> Self {
        Self {
            state: StreamState::Text,
            indent,
            line_buf: String::new(),
            at_line_start: true,
        }
    }

    pub fn process_chunk(&mut self, chunk: &str) {
        for c in chunk.chars() {
            let mut next_state = None;
            let mut chars_to_flush = String::new();

            match &mut self.state {
                StreamState::Text => {
                    if c == '\n' {
                        chars_to_flush = core::mem::take(&mut self.line_buf);
                        chars_to_flush.push('\n');
                        self.at_line_start = true;
                    } else if self.at_line_start {
                        if c.is_whitespace() {
                            self.line_buf.push(c);
                        } else {
                            self.line_buf.push(c);
                            let trimmed = self.line_buf.trim_start();
                            if trimmed == "```" {
                                next_state = Some(StreamState::BufferingPotentialTool {
                                    buffer: core::mem::take(&mut self.line_buf),
                                });
                            } else if trimmed == "{" {
                                next_state = Some(StreamState::BufferingJson {
                                    buffer: core::mem::take(&mut self.line_buf),
                                    depth: 1,
                                    in_string: false,
                                    escape: false,
                                });
                            } else if ! "```".starts_with(trimmed) && ! "{".starts_with(trimmed) {
                                // Clearly not a tool, flush line_buf and disable line-start buffering
                                chars_to_flush = core::mem::take(&mut self.line_buf);
                                self.at_line_start = false;
                            }
                        }
                    } else {
                        // Fluent streaming for non-line-start text
                        chars_to_flush.push(c);
                        if c == '\n' {
                            self.at_line_start = true;
                        }
                    }
                }
                StreamState::BufferingPotentialTool { buffer } => {
                    buffer.push(c);
                    let trimmed = buffer.trim_start();
                    
                    // Logic to see if we should ABORT buffering because it's clearly not a tool
                    if next_state.is_none() && trimmed.len() > 3 {
                        let after_bt = &trimmed[3..];
                        if let Some(nl) = after_bt.find('\n') {
                            let tag = after_bt[..nl].trim();
                            if !tag.is_empty() && tag != "json" {
                                // It's like ```rust\n... flush it
                                chars_to_flush = core::mem::take(buffer);
                                next_state = Some(StreamState::Text);
                                self.at_line_start = buffer.ends_with('\n');
                            }
                        } else {
                            // No newline yet, check if we are still typing "json"
                            if ! "json".starts_with(after_bt) {
                                chars_to_flush = core::mem::take(buffer);
                                next_state = Some(StreamState::Text);
                                self.at_line_start = buffer.ends_with('\n');
                            }
                        }
                    }
                    
                    // Check if block is complete
                    if next_state.is_none() && buffer.ends_with("```") && buffer.len() > 10 {
                        if let Some((tool, args)) = extract_tool_info(buffer) {
                            print_tool_notification(&tool, &args, self.indent);
                            next_state = Some(StreamState::Text);
                            self.at_line_start = true;
                        } else {
                            // Not a tool call, flush the whole markdown block
                            chars_to_flush = core::mem::take(buffer);
                            next_state = Some(StreamState::Text);
                            self.at_line_start = buffer.ends_with('\n');
                        }
                    } else if next_state.is_none() && buffer.len() > 16384 {
                        // Safety fallback
                        chars_to_flush = core::mem::take(buffer);
                        next_state = Some(StreamState::Text);
                        self.at_line_start = buffer.ends_with('\n');
                    }
                }
                StreamState::BufferingJson { buffer, depth, in_string, escape } => {
                    buffer.push(c);
                    if *escape { *escape = false; }
                    else if c == '\\' && *in_string { *escape = true; }
                    else if c == '"' { *in_string = !*in_string; }
                    else if !*in_string {
                        if c == '{' { *depth += 1; }
                        else if c == '}' {
                            *depth -= 1;
                            if *depth == 0 {
                                if (buffer.contains("\"command\"") || buffer.contains("command")) && 
                                   (buffer.contains("\"tool\"") || buffer.contains("tool")) {
                                    if let Some((tool, args)) = extract_tool_info(buffer) {
                                        print_tool_notification(&tool, &args, self.indent);
                                        self.at_line_start = true;
                                    } else {
                                        chars_to_flush = core::mem::take(buffer);
                                        self.at_line_start = buffer.ends_with('\n');
                                    }
                                } else {
                                    chars_to_flush = core::mem::take(buffer);
                                    self.at_line_start = buffer.ends_with('\n');
                                }
                                next_state = Some(StreamState::Text);
                            }
                        }
                    }
                }
            }

            if let Some(ns) = next_state {
                self.state = ns;
            }

            if !chars_to_flush.is_empty() {
                tui_print_with_indent(&chars_to_flush, "", self.indent, Some(COLOR_MEOW));
            }
        }
    }

    pub fn finalize(&mut self) {
        let to_flush = match &mut self.state {
            StreamState::Text => core::mem::take(&mut self.line_buf),
            StreamState::BufferingPotentialTool { buffer } => core::mem::take(buffer),
            StreamState::BufferingJson { buffer, .. } => core::mem::take(buffer),
        };
        if !to_flush.is_empty() {
            tui_print_with_indent(&to_flush, "", self.indent, Some(COLOR_MEOW));
        }
        self.state = StreamState::Text;
        self.at_line_start = true;
    }
}

fn extract_tool_info(json: &str) -> Option<(String, String)> {
    let tool = extract_field_value(json, "tool")?;
    if tool.is_empty() { return None; }
    
    let mut args = String::new();
    let fields = [
        "filename", "path", "cmd", "url", "message", "branch", 
        "source", "destination", "source_filename", "destination_filename",
        "pattern", "content", "old_text", "new_text", "id", "status"
    ];
    
    for field in fields {
        if let Some(val) = extract_field_value(json, field) {
            if field == "tool" || field == "command" || field == "args" { continue; }
            if !args.is_empty() { args.push_str(", "); }
            args.push_str(field);
            args.push_str("=\"");
            args.push_str(&val);
            args.push('"');
        }
    }
    Some((tool, args))
}

/// Find `field` in `json` wrapped by `open`/`close` (e.g. `"field"` via
/// `Some('"'), '"'`, or a bare `field:` via `None, ':'`), returning the byte
/// offset right past the closing wrapper char. Avoids `format!`-ing the three
/// quoting-style patterns `extract_field_value` used to allocate per call.
fn find_wrapped(json: &str, field: &str, open: Option<char>, close: char) -> Option<usize> {
    for (p, _) in json.match_indices(field) {
        if let Some(open) = open {
            if p == 0 || json.as_bytes()[p - 1] != open as u8 { continue; }
        }
        let end = p + field.len();
        if !json[end..].starts_with(close) { continue; }
        return Some(end + 1);
    }
    None
}

fn extract_field_value(json: &str, field: &str) -> Option<String> {
    let pattern_ends = [
        find_wrapped(json, field, Some('"'), '"'),
        find_wrapped(json, field, Some('\''), '\''),
        find_wrapped(json, field, None, ':'),
    ];

    for after_pattern in pattern_ends.into_iter().flatten() {
        let after = &json[after_pattern..];
        if let Some(colon_pos) = after.find(':') {
            let after_colon = after[colon_pos + 1..].trim_start();
            if after_colon.starts_with('"') || after_colon.starts_with('\'') {
                let quote = after_colon.chars().next().unwrap();
                let after_quote = &after_colon[1..];
                let mut val = String::new();
                let mut escape = false;
                for c in after_quote.chars() {
                    if escape {
                        match c {
                            'n' => val.push('\n'),
                            'r' => val.push('\r'),
                            't' => val.push('\t'),
                            _ => val.push(c),
                        }
                        escape = false;
                    } else if c == '\\' { escape = true; }
                    else if c == quote { return Some(val); }
                    else { val.push(c); }
                }
            } else {
                let end = after_colon.find(|c: char| c == ',' || c == '}' || c == ']' || c.is_whitespace())
                    .unwrap_or(after_colon.len());
                if end > 0 {
                    let val = after_colon[..end].trim();
                    if !val.is_empty() && val != "{" { return Some(String::from(val)); }
                }
            }
        }
    }
    None
}

fn print_tool_notification(tool: &str, args: &str, indent: u16) {
    tui_print_with_indent("\n", "", 0, None);
    let content = if args.is_empty() {
        format!("ToolCalled: {}\n", tool)
    } else {
        format!("ToolCalled: {} | Arguments {}\n", tool, args)
    };
    tui_print_with_indent(&content, "     --- ", 9, Some(crate::config::COLOR_GRAY_DIM));
    tui_print_with_indent("", "", indent, None);
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {

    if !crate::config::ENABLE_TESTS {

        libakuma::print("Tests are disabled. Set ENABLE_TESTS=true in config.rs to run them.\n");

        return 0;

    }

    libakuma::print("--- Meow StreamingRenderer Tests ---\n");

    let test_cases: [(&str, &str, &[&str]); 4] = [

        ("Normal text", "Hello nya~!\n", &["Hello nya~!\n"]),

        ("Simple tool call", "```json\n{\n  \"command\": {\n    \"tool\": \"FileRead\",\n    \"args\": {\n      \"filename\": \"test.txt\"\n    }\n  }\n}\n```", &["\n", "ToolCalled: FileRead | Arguments filename=\"test.txt\"\n"]),

        ("Tool with text before", "Sure! Here it is:\n\n```json\n{\n  \"command\": {\n    \"tool\": \"FileList\",\n    \"args\": {\"path\": \"/\"}\n  }\n}\n```", &["Sure! Here it is:\n\n", "\n", "ToolCalled: FileList | Arguments path=\"/\"\n"]),

        ("Tool call without code block", "{\n  \"command\": {\n    \"tool\": \"Pwd\",\n    \"args\": {}\n  }\n}", &["\n", "ToolCalled: Pwd\n"])

    ];

    let mut passed = 0;

    for (name, input, expected) in test_cases {

        libakuma::print(&format!("[*] Testing: {}\n", name));

        unsafe { super::render::TEST_CAPTURE = Some(alloc::vec::Vec::new()); }

        let mut renderer = StreamingRenderer::new(9);

        for chunk in input.as_bytes().chunks(1) {

            if let Ok(s) = core::str::from_utf8(chunk) { renderer.process_chunk(s); }

        }

        renderer.finalize();

        let captured = unsafe { (*core::ptr::addr_of_mut!(super::render::TEST_CAPTURE)).take().unwrap_or_default() };

        let filtered: alloc::vec::Vec<_> = captured.into_iter().filter(|s| !s.is_empty()).collect();

        let mut match_count = 0;

        for (i, exp) in expected.iter().enumerate() {

            if let Some(got) = filtered.get(i) {

                if got == *exp { match_count += 1; }

                else { libakuma::print(&format!("  [!] Mismatch at index {}: expected {:?}, got {:?}\n", i, exp, got)); }

            }

        }

        if match_count == expected.len() && filtered.len() == expected.len() { passed += 1; libakuma::print("  [+] Passed!\n"); }

        else {

            libakuma::print(&format!("  [!] Failed: {}/{} matches, {} total outputs\n", match_count, expected.len(), filtered.len()));

            for (i, s) in filtered.iter().enumerate() { libakuma::print(&format!("    {:2}: {:?}\n", i, s)); }

        }

    }

    libakuma::print(&format!("--- Results: {}/{} passed ---\n", passed, test_cases.len()));

    if passed == test_cases.len() { 0 } else { 1 }

}
