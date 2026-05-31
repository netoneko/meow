use alloc::string::String;
use alloc::vec::Vec;

pub struct StreamStats {
    pub ttft_us: u64,
    pub stream_us: u64,
    pub total_bytes: usize,
    pub fakes: usize,
}

pub struct ToolCallData {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

pub enum StreamResponse {
    /// Response completed normally (server sent done signal)
    Complete(String, StreamStats),
    /// Response completed with structured tool calls (OpenAI tool calling)
    CompleteWithTools(String, Vec<ToolCallData>, StreamStats),
    /// Response was interrupted mid-stream (connection closed before done signal)
    Partial(String, StreamStats),
}

#[derive(Debug)]
pub struct ModelInfo {
    pub name: String,
    pub _size: Option<u64>,
    pub _parameter_size: Option<String>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum ProviderError {
    ConnectionFailed(String),
    RequestFailed(String),
    ParseError(String),
}
