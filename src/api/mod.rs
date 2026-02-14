pub mod types;
pub mod client;

pub use types::*;
pub use client::send_with_retry;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use libakuma::net::{TcpStream, resolve};
use libakuma_tls::{https_get, HttpHeaders};
use crate::config::{ApiType, Provider};

/// Connect to a provider (HTTP only)
fn connect(provider: &Provider) -> Result<TcpStream, ProviderError> {
    let (host, port) = provider.host_port()
        .ok_or_else(|| ProviderError::ConnectionFailed(String::from("Invalid URL")))?;

    let ip = resolve(&host).map_err(|_| {
        ProviderError::ConnectionFailed(format!("DNS resolution failed for: {}", host))
    })?;

    let addr_str = format!("{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], port);

    TcpStream::connect(&addr_str).map_err(|_| {
        ProviderError::ConnectionFailed(format!("Connection failed to: {}", addr_str))
    })
}

fn read_response(stream: &TcpStream) -> Result<String, ProviderError> {
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    let start_time = libakuma::uptime();
    let timeout_us = 5_000_000; // 5 seconds timeout for metadata queries

    loop {
        if libakuma::uptime() - start_time > timeout_us {
            return Err(ProviderError::RequestFailed(String::from("Read timeout")));
        }

        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&buf[..n]);
                if response.len() > 256 * 1024 { break; }
            }
            Err(e) => {
                if e.kind == libakuma::net::ErrorKind::WouldBlock
                    || e.kind == libakuma::net::ErrorKind::TimedOut
                {
                    libakuma::sleep_ms(10);
                    continue;
                }
                break;
            }
        }
    }

    Ok(String::from_utf8_lossy(&response).into_owned())
}

pub fn list_models(provider: &Provider) -> Result<Vec<ModelInfo>, ProviderError> {
    match provider.api_type {
        ApiType::Ollama => list_ollama_models(provider),
        ApiType::OpenAI => list_openai_models(provider),
    }
}

fn list_ollama_models(provider: &Provider) -> Result<Vec<ModelInfo>, ProviderError> {
    let (host, port) = provider.host_port()
        .ok_or_else(|| ProviderError::ConnectionFailed(String::from("Invalid URL")))?;

    let stream = connect(provider)?;

    let request = format!(
        "GET /api/tags HTTP/1.0\r\n\
         Host: {}:{}\r\n\
         Connection: close\r\n\
         \r\n",
        host, port
    );

    stream.write_all(request.as_bytes())
        .map_err(|_| ProviderError::RequestFailed(String::from("Write failed")))?;

    let response_str = read_response(&stream)?;

    let body = response_str
        .find("\r\n\r\n")
        .map(|pos| &response_str[pos + 4..])
        .ok_or_else(|| ProviderError::ParseError(String::from("Invalid HTTP response")))?;

    parse_ollama_models(body)
}

fn parse_ollama_models(json: &str) -> Result<Vec<ModelInfo>, ProviderError> {
    let mut models = Vec::new();

    let models_start = json.find("\"models\"")
        .ok_or_else(|| ProviderError::ParseError(String::from("No models field found")))?;

    let json = &json[models_start..];
    let array_start = json.find('[')
        .ok_or_else(|| ProviderError::ParseError(String::from("No models array found")))?;

    let json = &json[array_start..];

    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut obj_start = None;

    for (i, c) in json.chars().enumerate() {
        if escape_next { escape_next = false; continue; }
        match c {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => { if depth == 0 { obj_start = Some(i); } depth += 1; }
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = obj_start {
                        let obj = &json[start..=i];
                        if let Some(model) = parse_model_object(obj) { models.push(model); }
                    }
                    obj_start = None;
                }
            }
            ']' if !in_string && depth == 0 => break,
            _ => {}
        }
    }
    Ok(models)
}

fn parse_model_object(json: &str) -> Option<ModelInfo> {
    let name = extract_json_string(json, "name")?;
    let size = extract_json_number(json, "size");
    let parameter_size = extract_json_string(json, "parameter_size");
    Some(ModelInfo { name, _size: size, _parameter_size: parameter_size })
}

fn list_openai_models(provider: &Provider) -> Result<Vec<ModelInfo>, ProviderError> {
    let base_url = &provider.base_url;
    let base = provider.base_path();
    
    let url = if base.ends_with("/v1") {
        format!("{}/models", base_url.trim_end_matches('/'))
    } else {
        format!("{}/v1/models", base_url.trim_end_matches('/'))
    };

    let mut headers = HttpHeaders::new();
    if let Some(key) = &provider.api_key { headers.bearer_auth(key); }

    let response = https_get(&url, &headers)
        .map_err(|_| ProviderError::RequestFailed(String::from("TLS/HTTP request failed")))?;

    let body = core::str::from_utf8(&response)
        .map_err(|_| ProviderError::ParseError(String::from("Invalid UTF-8 response")))?;

    parse_openai_models(body)
}

fn parse_openai_models(json: &str) -> Result<Vec<ModelInfo>, ProviderError> {
    let mut models = Vec::new();
    let data_start = json.find("\"data\"")
        .ok_or_else(|| ProviderError::ParseError(String::from("No data field found")))?;

    let json = &json[data_start..];
    let array_start = json.find('[')
        .ok_or_else(|| ProviderError::ParseError(String::from("No data array found")))?;

    let json = &json[array_start..];

    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut obj_start = None;

    for (i, c) in json.chars().enumerate() {
        if escape_next { escape_next = false; continue; }
        match c {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => { if depth == 0 { obj_start = Some(i); } depth += 1; }
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = obj_start {
                        let obj = &json[start..=i];
                        if let Some(id) = extract_json_string(obj, "id") {
                            models.push(ModelInfo { name: id, _size: None, _parameter_size: None });
                        }
                    }
                    obj_start = None;
                }
            }
            ']' if !in_string && depth == 0 => break,
            _ => {}
        }
    }
    Ok(models)
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let start = json.find(&pattern)?;
    let after_key = &json[start + pattern.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = &after_key[colon_pos + 1..];
    let trimmed = after_colon.trim_start();
    if !trimmed.starts_with('"') { return None; }
    let rest = &trimmed[1..];
    let mut result = String::new();
    let mut chars = rest.chars().peekable();
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
                        _ => { result.push('\\'); result.push(next); }
                    }
                }
            }
            _ => result.push(c),
        }
    }
    Some(result)
}

fn extract_json_number(json: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{}\"", key);
    let start = json.find(&pattern)?;
    let after_key = &json[start + pattern.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = &after_key[colon_pos + 1..];
    let trimmed = after_colon.trim_start();
    let end = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(trimmed.len());
    trimmed[..end].parse().ok()
}

pub fn query_model_info(model: &str, provider: &Provider) -> Option<usize> {
    if provider.api_type != ApiType::Ollama { return None; }
    let (host, port) = provider.host_port()?;
    let stream = connect(provider).ok()?;
    let body = format!("{{\"model\":\"{}\"}}", model);
    let request = format!(
        "POST /api/show HTTP/1.0\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        host, port, body.len(), body
    );
    stream.write_all(request.as_bytes()).ok()?;
    let response_str = read_response(&stream).ok()?;
    if let Some(pos) = response_str.find("\"num_ctx\"") {
        let after = &response_str[pos + 9..];
        let num_start = after.find(|c: char| c.is_ascii_digit())?;
        let rest = &after[num_start..];
        let num_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let num_str = &rest[..num_end];
        return num_str.parse().ok();
    }
    None
}