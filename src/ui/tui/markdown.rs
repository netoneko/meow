use alloc::string::String;
use alloc::format;
use crate::config::{COLOR_BOLD, COLOR_RESET, COLOR_GRAY_DIM, COLOR_VIOLET};
use super::render::tui_print_with_indent;

pub struct MarkdownRenderer {
    indent: u16,
}

impl MarkdownRenderer {
    pub fn new(indent: u16, _prefix: &str) -> Self {
        Self { indent }
    }

    pub fn render(&self, markdown: &str) {
        let mut in_code_block = false;
        
        for line in markdown.lines() {
            let trimmed = line.trim();
            
            // Handle Code Blocks
            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
                tui_print_with_indent("\n", "", self.indent, None);
                continue;
            }
            
            if in_code_block {
                tui_print_with_indent(line, "", self.indent + 2, Some(COLOR_GRAY_DIM));
                tui_print_with_indent("\n", "", self.indent, None);
                continue;
            }

            // Handle Horizontal Rule
            if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                tui_print_with_indent("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n", "", self.indent, Some(COLOR_GRAY_DIM));
                continue;
            }

            // Handle Headers
            if trimmed.starts_with('#') {
                let level = trimmed.chars().take_while(|&c| c == '#').count();
                let content = trimmed[level..].trim();
                let style = format!("{}{}", COLOR_BOLD, COLOR_VIOLET);
                tui_print_with_indent("\n", "", self.indent, None);
                self.render_inline(content, Some(&style));
                tui_print_with_indent("\n", "", self.indent, None);
                continue;
            }

            // Handle Lists
            if trimmed.starts_with("* ") || trimmed.starts_with("- ") {
                tui_print_with_indent(" • ", "", self.indent, Some(COLOR_VIOLET));
                self.render_inline(&trimmed[2..], None);
                tui_print_with_indent("\n", "", self.indent, None);
                continue;
            }
            
            if trimmed.len() > 2 && trimmed.chars().next().unwrap().is_ascii_digit() && trimmed.contains(". ") {
                if let Some(pos) = trimmed.find(". ") {
                    let num = &trimmed[..pos+2];
                    tui_print_with_indent(num, "", self.indent, Some(COLOR_VIOLET));
                    self.render_inline(&trimmed[pos+2..], None);
                    tui_print_with_indent("\n", "", self.indent, None);
                    continue;
                }
            }

            // Regular paragraph line
            if !trimmed.is_empty() {
                self.render_inline(line, None);
                tui_print_with_indent(" ", "", self.indent, None); // Space between lines in same paragraph
            } else {
                tui_print_with_indent("\n\n", "", self.indent, None);
            }
        }
    }

    fn render_inline(&self, text: &str, base_style: Option<&str>) {
        let mut i = 0;
        let chars: alloc::vec::Vec<char> = text.chars().collect();
        let mut current_bold = false;
        let mut current_italic = false;
        let mut current_code = false;

        let apply_styles = |bold: bool, italic: bool, code: bool| {
            let mut s = String::from(COLOR_RESET);
            if let Some(base) = base_style { s.push_str(base); }
            if bold { s.push_str(COLOR_BOLD); }
            if italic { s.push_str("\x1b[3m"); }
            if code { s.push_str(COLOR_GRAY_DIM); }
            s
        };

        if let Some(base) = base_style {
            tui_print_with_indent("", "", self.indent, Some(base));
        }

        while i < chars.len() {
            // Bold **
            if i + 1 < chars.len() && chars[i] == '*' && chars[i+1] == '*' {
                current_bold = !current_bold;
                tui_print_with_indent("", "", self.indent, Some(&apply_styles(current_bold, current_italic, current_code)));
                i += 2;
                continue;
            }
            
            // Inline Code `
            if chars[i] == '`' {
                current_code = !current_code;
                tui_print_with_indent("", "", self.indent, Some(&apply_styles(current_bold, current_italic, current_code)));
                i += 1;
                continue;
            }

            // Italic *
            if chars[i] == '*' {
                current_italic = !current_italic;
                tui_print_with_indent("", "", self.indent, Some(&apply_styles(current_bold, current_italic, current_code)));
                i += 1;
                continue;
            }

            let mut buf = [0u8; 4];
            tui_print_with_indent(chars[i].encode_utf8(&mut buf), "", self.indent, None);
            i += 1;
        }

        tui_print_with_indent("", "", self.indent, Some(COLOR_RESET));
    }
}