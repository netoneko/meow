use alloc::vec::Vec;
use alloc::format;
use libakuma::net::{TcpStream, resolve};

use super::mod_types::ToolResult;

// Maximum response size for HTTP fetch (64KB)
const MAX_FETCH_SIZE: usize = 64 * 1024;

/// HTTP/HTTPS GET fetch tool
pub fn tool_http_fetch(url: &str) -> ToolResult {
    let parsed = match parse_http_url(url) {
        Some(p) => p,
        None => return ToolResult::err("Invalid URL format. Use: http(s)://host[:port]/path"),
    };

    if parsed.is_https {
        match libakuma_tls::https_fetch(url, true) {
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
            Err(e) => ToolResult::err(&format!("HTTPS fetch failed: {:?}", e)),
        }
    } else {
        let ip = match resolve(parsed.host) {
            Ok(ip) => ip,
            Err(_) => return ToolResult::err(&format!("DNS resolution failed for: {}", parsed.host)),
        };

        let addr_str = format!("{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], parsed.port);
        let stream = match TcpStream::connect(&addr_str) {
            Ok(s) => s,
            Err(_) => return ToolResult::err(&format!("Connection failed to: {}", addr_str)),
        };

        let request = format!(
            "GET {} HTTP/1.0

             Host: {}

             User-Agent: meow/1.0 (Akuma)

             Connection: close

             
",
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

        if status < 200 || status >= 300 {
            return ToolResult::err(&format!("HTTP error: status {}", status));
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

struct ParsedUrl<'a> {
    is_https: bool,
    host: &'a str,
    port: u16,
    path: &'a str,
}

fn parse_http_url(url: &str) -> Option<ParsedUrl<'_>> {
    let (is_https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return None;
    };
    
    let default_port = if is_https { 443 } else { 80 };
    
    let (host_port, path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], &rest[pos..]),
        None => (rest, "/"),
    };
    
    let (host, port) = match host_port.rfind(':') {
        Some(pos) => {
            let h = &host_port[..pos];
            let p = host_port[pos + 1..].parse::<u16>().ok()?;
            (h, p)
        }
        None => (host_port, default_port),
    };
    
    Some(ParsedUrl { is_https, host, port, path })
}

fn parse_http_response(data: &[u8]) -> Option<(u16, &[u8])> {
    let headers_end = find_headers_end(data)?;
    let header_str = core::str::from_utf8(&data[..headers_end]).ok()?;
    let first_line = header_str.lines().next()?;
    
    let mut parts = first_line.split_whitespace();
    let _version = parts.next()?;
    let status: u16 = parts.next()?.parse().ok()?;
    
    Some((status, &data[headers_end..]))
}

fn find_headers_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"

" {
            return Some(i + 4);
        }
    }
    None
}
