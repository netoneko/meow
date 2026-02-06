use core::fmt::{self, Write};

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

    pub fn len(&self) -> usize {
        self.offset
    }

    pub fn is_empty(&self) -> bool {
        self.offset == 0
    }

    pub fn clear(&mut self) {
        self.offset = 0;
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
