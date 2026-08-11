use alloc::vec::Vec;
use alloc::format;
use libakuma::net::{TcpStream, resolve};
use libakuma_tls::{find_headers_end, parse_status_line, parse_url};

use super::mod_types::ToolResult;

// Maximum response size for HTTP fetch (64KB)
const MAX_FETCH_SIZE: usize = 64 * 1024;

/// HTTP/HTTPS GET fetch tool
pub fn tool_http_fetch(url: &str) -> ToolResult {
    let parsed = match parse_url(url) {
        Some(p) => p,
        None => return ToolResult::err("Invalid URL format. Use: http(s)://host[:port]/path"),
    };

    if parsed.is_https {
        match libakuma_tls::https_fetch(url, true, Some(MAX_FETCH_SIZE)) {
            Ok(body) => {
                match core::str::from_utf8(&body) {
                    Ok(text) => {
                        let truncated = if body.len() >= MAX_FETCH_SIZE { " (truncated)" } else { "" };
                        ToolResult::ok(format!(
                            "Fetched {} ({} bytes{}):
```
{}
```",
                            url, body.len(), truncated, text
                        ))
                    }
                    Err(_) => ToolResult::err("Response contains non-UTF8 data (binary content)"),
                }
            }
            Err(e) => ToolResult::err(format!("HTTPS fetch failed: {:?}", e)),
        }
    } else {
        let ip = match resolve(parsed.host) {
            Ok(ip) => ip,
            Err(_) => return ToolResult::err(format!("DNS resolution failed for: {}", parsed.host)),
        };

        let addr_str = format!("{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], parsed.port);
        let stream = match TcpStream::connect(&addr_str) {
            Ok(s) => s,
            Err(_) => return ToolResult::err(format!("Connection failed to: {}", addr_str)),
        };

        let request = format!(
            "GET {} HTTP/1.0\r\n\
             Host: {}\r\n\
             User-Agent: meow/1.0 (Akuma)\r\n\
             Connection: close\r\n\
             \r\n",
            parsed.path,
            parsed.host
        );

        if stream.write_all(request.as_bytes()).is_err() {
            return ToolResult::err("Failed to send HTTP request");
        }

        let mut response = Vec::new();
        let mut buf = [0u8; 1024];

        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if response.len() + n > MAX_FETCH_SIZE {
                        let remaining = MAX_FETCH_SIZE - response.len();
                        response.extend_from_slice(&buf[..remaining]);
                        break;
                    }
                    response.extend_from_slice(&buf[..n]);
                }
                Err(e) => {
                    if e.kind == libakuma::net::ErrorKind::WouldBlock {
                        libakuma::sleep_ms(1);
                        continue;
                    }
                    break;
                }
            }
        }

        if response.is_empty() {
            return ToolResult::err("Empty response from server");
        }

        let (status, body) = match parse_http_response(&response) {
            Some(r) => r,
            None => return ToolResult::err("Failed to parse HTTP response"),
        };

        if !(200..300).contains(&status) {
            return ToolResult::err(format!("HTTP error: status {}", status));
        }

        match core::str::from_utf8(body) {
            Ok(text) => {
                let truncated = if response.len() >= MAX_FETCH_SIZE { " (truncated)" } else { "" };
                ToolResult::ok(format!(
                    "Fetched {} ({} bytes{}):
```
{}
```",
                    url, body.len(), truncated, text
                ))
            }
            Err(_) => ToolResult::err("Response contains non-UTF8 data (binary content)"),
        }
    }
}

fn parse_http_response(data: &[u8]) -> Option<(u16, &[u8])> {
    let headers_end = find_headers_end(data)?;
    let header_str = core::str::from_utf8(&data[..headers_end]).ok()?;
    let status = parse_status_line(header_str)?;
    Some((status, &data[headers_end..]))
}
