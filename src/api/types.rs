use alloc::string::String;

pub struct StreamStats {
    pub ttft_us: u64,
    pub stream_us: u64,
    pub total_bytes: usize,
    pub fakes: usize,
}

pub enum StreamResponse {
    /// Response completed normally (server sent done signal)
    Complete(String, StreamStats),
    /// Response was interrupted mid-stream (connection closed before done signal)
    Partial(String, StreamStats),
}

#[derive(Debug)]
pub struct ModelInfo {
    pub name: String,
    pub size: Option<u64>,
    pub parameter_size: Option<String>,
}

#[derive(Debug)]
pub enum ProviderError {
    ConnectionFailed(String),
    RequestFailed(String),
    ParseError(String),
}
