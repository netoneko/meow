use alloc::string::String;
use alloc::format;
use core::sync::atomic::Ordering;
use crate::config::{COLOR_GREEN_LIGHT, COLOR_RESET, COLOR_BOLD, COLOR_GRAY_DIM, COLOR_VIOLET, COLOR_YELLOW, BG_CODE};
use super::render::tui_print_with_indent;
use crate::tui_app::CUR_COL;

pub enum StreamState {
    Text,
    BufferingJson {
        buffer: String,
        depth: usize,
    },
    SkippingJson {
        depth: usize,
    },
}

pub struct StreamingRenderer {
    state: StreamState,
    indent: u16,
    in_bold: bool,
    in_italic: bool,
    in_code: bool,
    in_code_block: bool,
    markdown_buf: String,
}

impl StreamingRenderer {
    pub fn new(indent: u16) -> Self {
        Self {
            state: StreamState::Text,
            indent,
            in_bold: false,
            in_italic: false,
            in_code: false,
            in_code_block: false,
            markdown_buf: String::new(),
        }
    }

    pub fn process_chunk(&mut self, chunk: &str) {
        for c in chunk.chars() {
            let mut next_state = None;
            let mut chars_to_process = String::new();

            match &mut self.state {
                StreamState::Text => {
                    if c == '{' && !self.in_code {
                        next_state = Some(StreamState::BufferingJson {
                            buffer: alloc::format!("{}", c),
                            depth: 1,
                        });
                    } else {
                        chars_to_process.push(c);
                    }
                }
                StreamState::BufferingJson { buffer, depth } => {
                    buffer.push(c);
                    if c == '{' {
                        *depth += 1;
                    } else if c == '}' {
                        *depth -= 1;
                        if *depth == 0 {
                            chars_to_process = buffer.clone();
                            next_state = Some(StreamState::Text);
                        }
                    }
                    
                    if next_state.is_none() {
                        if buffer.contains("\"command\"") {
                            if let Some(cmd) = extract_command_name(buffer) {
                                print_tool_notification(&cmd, self.indent);
                                next_state = Some(StreamState::SkippingJson { depth: *depth });
                            }
                        } else if buffer.len() > 1024 {
                            chars_to_process = buffer.clone();
                            next_state = Some(StreamState::Text);
                        }
                    }
                }
                StreamState::SkippingJson { depth } => {
                    if c == '{' {
                        *depth += 1;
                    } else if c == '}' {
                        *depth -= 1;
                        if *depth == 0 {
                            next_state = Some(StreamState::Text);
                        }
                    }
                }
            }

            if let Some(ns) = next_state {
                self.state = ns;
            }
            
            for bc in chars_to_process.chars() {
                self.process_markdown_char(bc);
            }
        }
    }

    fn process_markdown_char(&mut self, c: char) {
        self.markdown_buf.push(c);
        
        // Handle newline and look for block elements
        if c == '\n' {
            let buf = self.markdown_buf.clone();
            self.markdown_buf.clear();
            let trimmed = buf.trim();
            
            if trimmed.starts_with("```") {
                if !self.in_code_block {
                    self.in_code_block = true;
                    let lang = trimmed[3..].trim();
                    let style = format!("{}{}", BG_CODE, COLOR_YELLOW);
                    tui_print_with_indent("  ", "", 0, Some(BG_CODE));
                    if !lang.is_empty() {
                        tui_print_with_indent(format!("{}          \n", lang).as_str(), "", self.indent + 2, Some(style.as_str()));
                    } else {
                        tui_print_with_indent("          \n", "", self.indent + 2, Some(style.as_str()));
                    }
                    return;
                } else {
                    self.in_code_block = false;
                    tui_print_with_indent(COLOR_RESET, "", 0, None);
                    tui_print_with_indent("\n", "", self.indent, None);
                    return;
                }
            }

            if self.in_code_block {
                let content = &buf[..buf.len().saturating_sub(1)]; // Remove newline
                let mut styled = String::from(BG_CODE);
                styled.push_str(COLOR_GRAY_DIM);
                styled.push_str(content);
                
                tui_print_with_indent(&styled, "", self.indent + 2, None);
                tui_print_with_indent(format!("{}\n", COLOR_RESET).as_str(), "", self.indent + 2, None);
                return;
            }
            
            if trimmed.starts_with('#') {
                let level = trimmed.chars().take_while(|&c| c == '#').count();
                if level > 0 && level <= 6 {
                    let style = format!("{}{}", COLOR_BOLD, COLOR_VIOLET);
                    tui_print_with_indent("", "", self.indent, Some(&style));
                    tui_print_with_indent(trimmed[level..].trim(), "", self.indent, None);
                    tui_print_with_indent("\n", "", self.indent, Some(COLOR_RESET));
                    return;
                }
            } else if trimmed.starts_with("* ") || trimmed.starts_with("- ") {
                if CUR_COL.load(Ordering::SeqCst) > self.indent {
                    tui_print_with_indent("\n", "", self.indent, None);
                }
                tui_print_with_indent(" • ", "", self.indent, Some(COLOR_VIOLET));
                tui_print_with_indent(&trimmed[2..], "", self.indent, None);
                tui_print_with_indent("\n", "", self.indent, None);
                return;
            }
            
            tui_print_with_indent(&buf, "", self.indent, None);
            return;
        }

        if self.markdown_buf.ends_with("**") {
            self.markdown_buf.truncate(self.markdown_buf.len() - 2);
            self.flush_markdown_buf();
            self.in_bold = !self.in_bold;
            self.apply_style();
        } else if self.markdown_buf.ends_with("`") {
            if self.markdown_buf == "```" {
                // Potential code block, don't toggle in_code yet
                return;
            }
            self.markdown_buf.truncate(self.markdown_buf.len() - 1);
            self.flush_markdown_buf();
            self.in_code = !self.in_code;
            self.apply_style();
        } else if self.markdown_buf.len() > 1 {
            let mut chars = self.markdown_buf.chars();
            let first = chars.next().unwrap();
            let second = chars.next().unwrap();
            
            if first == '*' && second != '*' {
                self.markdown_buf.remove(0);
                self.flush_markdown_buf();
                self.in_italic = !self.in_italic;
                self.apply_style();
            } else if first != '*' && first != '#' && first != '-' && first != '`' { // Don't flush if it could be a header, list, or code
                self.flush_first_char();
            }
        }
    }

    fn flush_first_char(&mut self) {
        if self.markdown_buf.is_empty() { return; }
        if self.markdown_buf.starts_with("```") { return; }
        let c = self.markdown_buf.remove(0);
        let mut s = String::new();
        s.push(c);
        tui_print_with_indent(&s, "", self.indent, None);
    }

    fn flush_markdown_buf(&mut self) {
        if !self.markdown_buf.is_empty() {
            tui_print_with_indent(&self.markdown_buf, "", self.indent, None);
            self.markdown_buf.clear();
        }
    }

    fn apply_style(&mut self) {
        let mut style = String::from(COLOR_RESET);
        if self.in_bold { style.push_str(COLOR_BOLD); }
        if self.in_italic { style.push_str("\x1b[3m"); }
        if self.in_code { 
            style.push_str(BG_CODE);
            style.push_str(COLOR_GRAY_DIM); 
        }
        style.push_str(crate::config::COLOR_MEOW);
        tui_print_with_indent("", "", self.indent, Some(&style));
    }
    
    pub fn finalize(&mut self) {
        self.flush_markdown_buf();
        match &mut self.state {
            StreamState::BufferingJson { buffer, .. } => {
                tui_print_with_indent(buffer, "", self.indent, None);
            }
            _ => {}
        }
        self.state = StreamState::Text;
        tui_print_with_indent("", "", self.indent, Some(COLOR_RESET));
    }
}

fn extract_command_name(json: &str) -> Option<String> {
    if let Some(pos) = json.find("\"command\"") {
        let after = &json[pos + 9..];
        if let Some(start_quote) = after.find('"') {
            let after_start = &after[start_quote + 1..];
            if let Some(end_quote) = after_start.find('"') {
                return Some(String::from(&after_start[..end_quote]));
            }
        }
    }
    None
}

fn print_tool_notification(cmd: &str, indent: u16) {
    let content = format!("--- [TOOL CALL: {}] ---\n", cmd);
    tui_print_with_indent(&content, "", indent, Some(COLOR_GREEN_LIGHT));
}
