use alloc::string::String;

/// Extract a string field from a tool call's flat, single-level `args_json`.
pub fn extract_string_field(json: &str, field: &str) -> Option<String> {
    crate::json::string_at(json, &[field])
}

/// Extract a number field from a tool call's flat, single-level `args_json`.
pub fn extract_number_field(json: &str, field: &str) -> Option<usize> {
    usize::try_from(crate::json::number_at(json, &[field])?).ok()
}
