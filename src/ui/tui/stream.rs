use alloc::string::String;
use alloc::format;
use crate::config::{COLOR_GRAY_DIM, COLOR_MEOW, COLOR_RESET};
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
                    if self.at_line_start && c.is_whitespace() && c != '\n' {
                        self.line_buf.push(c);
                    } else if self.at_line_start && c == '`' {
                        self.line_buf.push(c);
                        if self.line_buf.trim_start().starts_with("```") {
                            // Potentially a tool block
                        }
                    } else if c == '{' && self.at_line_start {
                        next_state = Some(StreamState::BufferingJson {
                            buffer: alloc::format!("{}", c),
                            depth: 1,
                            in_string: false,
                            escape: false,
                        });
                        self.at_line_start = false;
                    } else if c == '\n' {
                        chars_to_flush = self.line_buf.clone();
                        chars_to_flush.push('\n');
                        self.line_buf.clear();
                        self.at_line_start = true;
                    } else {
                        self.line_buf.push(c);
                        self.at_line_start = false;
                        
                        let trimmed = self.line_buf.trim_start();
                        if trimmed.starts_with("```json") {
                            next_state = Some(StreamState::BufferingPotentialTool {
                                buffer: self.line_buf.clone(),
                            });
                            self.line_buf.clear();
                        } else if self.line_buf.len() > 128 {
                            // Not a tool start, flush it
                            chars_to_flush = self.line_buf.clone();
                            self.line_buf.clear();
                        }
                    }
                }
                StreamState::BufferingPotentialTool { buffer } => {
                    buffer.push(c);
                    if buffer.ends_with("```") && buffer.len() > 15 {
                        if let Some((tool, args)) = extract_tool_info(buffer) {
                            print_tool_notification(&tool, &args, self.indent);
                            next_state = Some(StreamState::Text);
                            self.at_line_start = true;
                        } else {
                            chars_to_flush = buffer.clone();
                            next_state = Some(StreamState::Text);
                            self.at_line_start = buffer.ends_with('\n');
                        }
                    } else if buffer.len() > 16384 {
                        chars_to_flush = buffer.clone();
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
                                    } else {
                                        chars_to_flush = buffer.clone();
                                    }
                                } else {
                                    chars_to_flush = buffer.clone();
                                }
                                next_state = Some(StreamState::Text);
                                self.at_line_start = buffer.ends_with('\n');
                            }
                        }
                    }
                }
            }

            if let Some(ns) = next_state {
                self.state = ns;
            }

            if !chars_to_flush.is_empty() {
                tui_print_with_indent(&chars_to_flush, "", self.indent, Some(crate::config::COLOR_MEOW));
            }
        }
    }

    pub fn finalize(&mut self) {
        let to_flush = match &mut self.state {
            StreamState::Text => {
                if !self.line_buf.is_empty() {
                    let s = self.line_buf.clone();
                    self.line_buf.clear();
                    s
                } else {
                    String::new()
                }
            }
            StreamState::BufferingPotentialTool { buffer } => {
                let s = buffer.clone();
                buffer.clear();
                s
            }
            StreamState::BufferingJson { buffer, .. } => {
                let s = buffer.clone();
                buffer.clear();
                s
            }
        };
        if !to_flush.is_empty() {
            tui_print_with_indent(&to_flush, "", self.indent, Some(COLOR_MEOW));
        }
        self.state = StreamState::Text;
        tui_print_with_indent("", "", self.indent, Some(COLOR_RESET));
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
            if field == "tool" || field == "command" { continue; }
            if !args.is_empty() { args.push_str(", "); }
            args.push_str(field);
            args.push_str("=\"");
            args.push_str(&val);
            args.push_str("\"");
        }
    }
    Some((tool, args))
}

fn extract_field_value(json: &str, field: &str) -> Option<String> {
    let pattern = alloc::format!("\"{}\"", field);
    if let Some(pos) = json.find(&pattern) {
        let after = &json[pos + pattern.len()..];
        if let Some(colon_pos) = after.find(':') {
            let after_colon = after[colon_pos + 1..].trim_start();
            if after_colon.starts_with('"') {
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
                    else if c == '"' { return Some(val); }
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
    // We want to be at col 0 on a new line.
    tui_print_with_indent("\n", "", 0, None);
    
    let content = if args.is_empty() {
        format!("ToolCalled: {}\n", tool)
    } else {
        format!("ToolCalled: {} | Arguments {}\n", tool, args)
    };
    
    // Print with 1 space indent as requested.
    tui_print_with_indent(&content, "", 1, Some(crate::config::COLOR_GRAY_DIM));
    
    // Ensure the next content starts with the original assistant indentation.
    tui_print_with_indent("", "", indent, Some(crate::config::COLOR_MEOW));
}

