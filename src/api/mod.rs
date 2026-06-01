pub mod types;
pub mod client;

pub use types::*;
pub use client::send_with_retry;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use libakuma_tls::{https_get, HttpHeaders};
use crate::config::Provider;

pub fn list_models(provider: &Provider) -> Result<Vec<ModelInfo>, ProviderError> {
    list_openai_models(provider)
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

    for (i, c) in json.char_indices() {
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


