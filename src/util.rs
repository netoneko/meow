use alloc::string::String;
use alloc::format;
use core::fmt::{self, Write};

pub fn json_escape_to(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            _    => out.push(c),
        }
    }
}

pub struct StackBuffer<'a> {
    buffer: &'a mut [u8],
    offset: usize,
}

impl<'a> StackBuffer<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        StackBuffer {
            buffer,
            offset: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.offset]).unwrap_or("")
    }
}

impl<'a> Write for StackBuffer<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let remaining_len = self.buffer.len() - self.offset;
        if bytes.len() > remaining_len {
            // Not enough space, truncate or return an error
            // For now, let's truncate.
            self.buffer[self.offset..self.offset + remaining_len].copy_from_slice(&bytes[..remaining_len]);
            self.offset += remaining_len;
            return Err(fmt::Error); // Indicate that not all bytes were written
        }
        self.buffer[self.offset..self.offset + bytes.len()].copy_from_slice(bytes);
        self.offset += bytes.len();
        Ok(())
    }
}
