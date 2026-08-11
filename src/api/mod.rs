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
    if !crate::json::exists(json, &["data"]) {
        return Err(ProviderError::ParseError(String::from("No data field found")));
    }
    Ok(crate::json::strings_at(json, &["data", "*", "id"])
        .into_iter()
        .map(|name| ModelInfo { name, _size: None, _parameter_size: None })
        .collect())
}

