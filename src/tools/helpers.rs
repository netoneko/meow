use alloc::string::String;
use alloc::format;

/// Extract a string field from JSON (simple implementation)
pub fn extract_string_field(json: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\"", field);
    let start = json.find(&pattern)?;

    let after_field = &json[start + pattern.len()..];
    let colon_pos = after_field.find(':')?;
    let after_colon = &after_field[colon_pos + 1..];

    let trimmed = after_colon.trim_start();

    if !trimmed.starts_with('"') {
        return None;
    }

    let value_rest = &trimmed[1..];
    let mut result = String::new();
    let mut chars = value_rest.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    match next {
                        'n' => result.push('\n'),
                        'r' => result.push('\r'),
                        't' => result.push('\t'),
                        '"' => result.push('"'),
                        '\\' => result.push('\\'),
                        '/' => result.push('/'),
                        'u' => {
                            let mut codepoint: u32 = 0;
                            for _ in 0..4 {
                                match chars.next() {
                                    Some(h @ '0'..='9') => codepoint = codepoint * 16 + (h as u32 - '0' as u32),
                                    Some(h @ 'a'..='f') => codepoint = codepoint * 16 + (h as u32 - 'a' as u32 + 10),
                                    Some(h @ 'A'..='F') => codepoint = codepoint * 16 + (h as u32 - 'A' as u32 + 10),
                                    _ => { codepoint = 0xFFFD; break; }
                                }
                            }
                            if let Some(ch) = char::from_u32(codepoint) {
                                result.push(ch);
                            }
                        }
                        _ => {
                            result.push('\\');
                            result.push(next);
                        }
                    }
                }
            }
            _ => result.push(c),
        }
    }

    Some(result)
}

/// Extract a number field from JSON
pub fn extract_number_field(json: &str, field: &str) -> Option<usize> {
    let pattern = format!("\"{}\"", field);
    let start = json.find(&pattern)?;

    let after_field = &json[start + pattern.len()..];
    let colon_pos = after_field.find(':')?;
    let after_colon = &after_field[colon_pos + 1..];

    let trimmed = after_colon.trim_start();

    let num_end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if num_end == 0 {
        return None;
    }

    trimmed[..num_end].parse().ok()
}