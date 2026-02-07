use alloc::string::String;
use alloc::format;
use crate::config::{COLOR_BOLD, COLOR_RESET, COLOR_GRAY_DIM, COLOR_VIOLET, COLOR_YELLOW, BG_CODE};
use super::render::tui_print_with_indent;

pub struct MarkdownRenderer {
    indent: u16,
    base_style: Option<&'static str>, // Store base style
}

impl MarkdownRenderer {
    pub fn new(indent: u16, _prefix: &str, base_style: Option<&'static str>) -> Self {
        Self { indent, base_style }
    }

    pub fn render(&self, markdown: &str) {
        let mut in_code_block = false;
        
        for line in markdown.lines() {
            let trimmed = line.trim();
            
            // Handle Code Blocks
            if trimmed.starts_with("```") {
                if !in_code_block {
                    // Entering code block
                    let lang = &trimmed[3..].trim();
                    let style = format!("{}{}", BG_CODE, COLOR_YELLOW);
                    tui_print_with_indent("  ", "", 0, Some(BG_CODE));
                    if !lang.is_empty() {
                        tui_print_with_indent(format!("{}\n", lang).as_str(), "", self.indent + 2, Some(style.as_str()));
                    } else {
                        tui_print_with_indent("\n", "", self.indent + 2, Some(BG_CODE));
                    }
                    in_code_block = true;
                } else {
                    // Leaving code block
                    in_code_block = false;
                    tui_print_with_indent(COLOR_RESET, "", 0, None);
                    tui_print_with_indent("\n", "", self.indent, None);
                }
                continue;
            }
            
            if in_code_block {
                let mut styled_line = String::from(BG_CODE);
                styled_line.push_str(COLOR_GRAY_DIM);
                styled_line.push_str(line);
                
                tui_print_with_indent(&styled_line, "", self.indent + 2, None);
                // Reset color at end of line and prepare indentation for next line
                tui_print_with_indent(format!("{}\n", COLOR_RESET).as_str(), "", self.indent + 2, None);
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
                if level > 0 && level <= 6 {
                    let content = trimmed[level..].trim();
                    let style = format!("{}{}", COLOR_BOLD, COLOR_VIOLET);
                    tui_print_with_indent("\n", "", self.indent, None);
                    self.render_inline(content, Some(&style));
                    tui_print_with_indent("\n", "", self.indent, None);
                    continue;
                }
            }

            // Handle Lists
            if trimmed.starts_with("* ") || trimmed.starts_with("- ") {
                tui_print_with_indent(" • ", "", self.indent, Some(COLOR_VIOLET));
                self.render_inline(&trimmed[2..], None);
                tui_print_with_indent("\n", "", self.indent, None);
                continue;
            }
            
            if trimmed.len() > 2 && trimmed.chars().next().unwrap().is_ascii_digit() {
                if let Some(pos) = trimmed.find(". ") {
                    let num_part = &trimmed[..pos+2];
                    if num_part.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ' ') {
                        tui_print_with_indent(num_part, "", self.indent, Some(COLOR_VIOLET));
                        self.render_inline(&trimmed[pos+2..], None);
                        tui_print_with_indent("\n", "", self.indent, None);
                        continue;
                    }
                }
            }

            // Regular paragraph line
            if !trimmed.is_empty() {
                self.render_inline(line, None);
                tui_print_with_indent(" ", "", self.indent, None);
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
        let mut text_buf = String::new();

        let apply_styles = |bold: bool, italic: bool, code: bool| {
            let mut s = String::from(COLOR_RESET);
            if let Some(base) = base_style { s.push_str(base); }
            if bold { s.push_str(COLOR_BOLD); }
            if italic { s.push_str("\x1b[3m"); }
            if code { 
                s.push_str(BG_CODE);
                s.push_str(COLOR_GRAY_DIM); 
            }
            s
        };

        let mut current_style = apply_styles(false, false, false);
        tui_print_with_indent("", "", self.indent, Some(&current_style));

        while i < chars.len() {
            let mut style_changed = false;
            
            // Bold **
            if i + 1 < chars.len() && chars[i] == '*' && chars[i+1] == '*' {
                current_bold = !current_bold;
                style_changed = true;
                i += 2;
            }
            // Inline Code `
            else if chars[i] == '`' {
                current_code = !current_code;
                style_changed = true;
                i += 1;
            }
            // Italic *
            else if chars[i] == '*' {
                current_italic = !current_italic;
                style_changed = true;
                i += 1;
            }
            else {
                text_buf.push(chars[i]);
                i += 1;
            }

            if style_changed || i == chars.len() {
                if !text_buf.is_empty() {
                    tui_print_with_indent(&text_buf, "", self.indent, None);
                    text_buf.clear();
                }
                if style_changed {
                    current_style = apply_styles(current_bold, current_italic, current_code);
                    tui_print_with_indent("", "", self.indent, Some(&current_style));
                }
            }
        }

        tui_print_with_indent("", "", self.indent, Some(COLOR_RESET));
    }
}
