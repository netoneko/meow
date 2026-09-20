//! Configuration module for Meow
//!
//! Handles loading and saving configuration from /etc/meow/config
//! Uses a simple key-value format (no TOML parser needed for no_std)

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libakuma::{open, close, read_fd, write_fd, fstat, open_flags};

/// Token limit for context compaction (when LLM should consider compacting)
pub const TOKEN_LIMIT_FOR_COMPACTION: usize = 32_000;
/// Default context window if we can't query the model
pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// Maximum size for tool output to be kept in memory.
/// If output exceeds this, it is written to a temp file instead.
/// The `size` feature caps this hard at 2KB for memory-constrained builds.
#[cfg(feature = "size")]
pub const MAX_TOOL_OUTPUT_SIZE: usize = 2 * 1024;
#[cfg(not(feature = "size"))]
pub const MAX_TOOL_OUTPUT_SIZE: usize = 32 * 1024;

/// Buffer tool_shell uses to ferry a child's stdout chunk-by-chunk. One page —
/// this is a stack array in `drain_child`, so on a sub-1 MB box it must stay
/// page-sized, not balloon the stack.
pub const TOOL_BUFFER_SIZE: usize = 4 * 1024; // one page

/// Hard ceiling on a single shell invocation's model-facing (non-redirect)
/// stdout. Output up to this is streamed to a temp file on the (disk-backed)
/// ext2 `/tmp`, with only a one-page preview kept resident; beyond it the child
/// is killed (runaway guard for `yes`, `tail -f`, …). This bounds the temp file
/// on disk, NOT meow's RAM — the resident cost is one page regardless. Kept
/// tight (16 pages) for a small ext2 `/tmp`.
pub const MAX_SHELL_CAPTURE_SIZE: usize = 64 * 1024;

/// Use meow's in-process "pretend shell" for the Shell tool instead of shelling
/// out to busybox. The pretend shell parses `&&`, `||`, `>`, `>>` itself and
/// emulates redirects by capturing each child's stdout and re-writing it to a
/// file or `tcp:HOST:PORT` socket backend. ON by default — removes the hard
/// dependency on /bin/busybox. Set false to fall back to the busybox path.
/// See tools/pretend_shell.rs and docs/SHELL.md.
pub const USE_PRETEND_SHELL: bool = true;

/// Whether testing-related code (output capture, `meow test`) is active.
/// Driven by the `tests` cargo feature (off by default); when false the
/// capture branches compile to dead code and are dropped by the optimizer.
pub const ENABLE_TESTS: bool = cfg!(feature = "tests");

/// Personality definition
pub struct Personality {
    pub name: &'static str,
    pub description: &'static str,

    pub ack_tui: &'static str,
    pub error_format: &'static str, // use "{}" placeholder
}

/// Kept as a standalone file (`meow_persona.txt`) so the prompt text can be
/// edited without touching Rust source, same technique as `akuma_40.txt`.
pub const MEOW_PERSONA: &str = include_str!("meow_persona.txt");

/// Neutral, persona-free assistant. Selected by the `--no-personality` CLI flag.
pub static NO_PERSONA: Personality = Personality {
    name: "None",
    description: "You are a helpful, concise AI coding assistant.",
    ack_tui: "Understood. I'll use relative paths within the current directory.",
    error_format: "Error: {}\n",
};

/// Available personas. Meow is the default and, currently, the only character
/// persona; `--no-personality` selects the neutral [`NO_PERSONA`] instead.
pub const PERSONALITIES: &[Personality] = &[
    Personality {
        name: "Meow",
        description: MEOW_PERSONA,
        ack_tui: "Understood nya~! I'll use relative paths for file operations within the current directory. Ready to help! (=^・ω・^=)",
        error_format: "～ Nyaa~! {} (=ＴェＴ=) ～\n",
    },
];



/// OpenAI-compatible tool schema for all tools.
/// OpenAI-compatible tools schema sent verbatim in the `"tools"` field of
/// every chat-completions request (streamed straight from this const by
/// `api::client::write_chat_body_inner`, never copied).
///
/// Two variants, selected at compile time. The `compact-tools` feature (ON by
/// default) drops every per-tool `description`, trading a little model hand-
/// holding for ~930 fewer tokens on EVERY request. Disable it
/// (`--no-default-features`) to ship the descriptive schema instead.
#[cfg(all(not(feature = "compact-tools"), not(feature = "litter")))]
pub const OPENAI_TOOLS_JSON: &str = r#"[{"type":"function","function":{"name":"FileRead","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileWrite","description":"Create or overwrite a file","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileAppend","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileExists","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileList","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},{"type":"function","function":{"name":"FileDelete","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FolderCreate","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"FileCopy","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileMove","description":"Move or rename a file","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileReadLines","parameters":{"type":"object","properties":{"filename":{"type":"string"},"start":{"type":"integer"},"end":{"type":"integer"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileEdit","description":"Search-and-replace edit; old_text must be unique","parameters":{"type":"object","properties":{"filename":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["filename","old_text","new_text"]}}},{"type":"function","function":{"name":"CodeSearch","description":"Recursively search files for a pattern","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}}},{"type":"function","function":{"name":"Shell","description":"Run a shell command (use for git, etc.)","parameters":{"type":"object","properties":{"cmd":{"type":"string"}},"required":["cmd"]}}},{"type":"function","function":{"name":"Cd","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"Pwd","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"HttpFetch","description":"Fetch an HTTP/HTTPS URL","parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},{"type":"function","function":{"name":"CompactContext","description":"Replace conversation history with a summary. Use when token count is high.","parameters":{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}}}]"#;

#[cfg(all(not(feature = "compact-tools"), feature = "litter"))]
pub const OPENAI_TOOLS_JSON: &str = r#"[{"type":"function","function":{"name":"FileRead","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileWrite","description":"Create or overwrite a file","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileAppend","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileExists","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileList","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},{"type":"function","function":{"name":"FileDelete","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FolderCreate","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"FileCopy","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileMove","description":"Move or rename a file","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileReadLines","parameters":{"type":"object","properties":{"filename":{"type":"string"},"start":{"type":"integer"},"end":{"type":"integer"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileEdit","description":"Search-and-replace edit; old_text must be unique","parameters":{"type":"object","properties":{"filename":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["filename","old_text","new_text"]}}},{"type":"function","function":{"name":"CodeSearch","description":"Recursively search files for a pattern","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}}},{"type":"function","function":{"name":"Shell","description":"Run a shell command (use for git, etc.)","parameters":{"type":"object","properties":{"cmd":{"type":"string"}},"required":["cmd"]}}},{"type":"function","function":{"name":"Cd","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"Pwd","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"HttpFetch","description":"Fetch an HTTP/HTTPS URL","parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},{"type":"function","function":{"name":"CompactContext","description":"Replace conversation history with a summary. Use when token count is high.","parameters":{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}}},{"type":"function","function":{"name":"SendMessage","description":"Send a message to another litter agent's mailbox (see docs/LITTER_EXPERIMENT.md)","parameters":{"type":"object","properties":{"to":{"type":"string"},"body":{"type":"string"},"round":{"type":"integer"}},"required":["to","body"]}}},{"type":"function","function":{"name":"TaskUpdate","description":"Act on a litter task. status: claim (take an assigned sub-task), done (report your result), failed (you cannot do it), clear (leader accepts a result), reopen (leader sends it back), artifact (leader closes the parent task with the final report)","parameters":{"type":"object","properties":{"task":{"type":"string"},"status":{"type":"string"},"text":{"type":"string"},"expect":{"type":"string"}},"required":["task","status"]}}},{"type":"function","function":{"name":"TaskPlan","description":"Leader only: split a parent task into one directed sub-task per agent, all in this single call. Set expect on each assignment to state what the answer should look like (e.g. \"two sentences of plain text, nothing else\")","parameters":{"type":"object","properties":{"task":{"type":"string"},"assignments":{"type":"array","items":{"type":"object","properties":{"who":{"type":"string"},"what":{"type":"string"},"expect":{"type":"string"}},"required":["who","what"]}}},"required":["task","assignments"]}}},{"type":"function","function":{"name":"ListPeers","description":"List the other agents registered in the litter roster","parameters":{"type":"object","properties":{}}}}]"#;

#[cfg(all(feature = "compact-tools", not(feature = "litter")))]
pub const OPENAI_TOOLS_JSON: &str = r#"[{"type":"function","function":{"name":"FileRead","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileWrite","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileAppend","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileExists","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileList","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},{"type":"function","function":{"name":"FileDelete","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FolderCreate","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"FileCopy","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileMove","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileReadLines","parameters":{"type":"object","properties":{"filename":{"type":"string"},"start":{"type":"integer"},"end":{"type":"integer"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileEdit","parameters":{"type":"object","properties":{"filename":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["filename","old_text","new_text"]}}},{"type":"function","function":{"name":"CodeSearch","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}}},{"type":"function","function":{"name":"Shell","parameters":{"type":"object","properties":{"cmd":{"type":"string"}},"required":["cmd"]}}},{"type":"function","function":{"name":"Cd","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"Pwd","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"HttpFetch","parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},{"type":"function","function":{"name":"CompactContext","parameters":{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}}}]"#;

#[cfg(all(feature = "compact-tools", feature = "litter"))]
pub const OPENAI_TOOLS_JSON: &str = r#"[{"type":"function","function":{"name":"FileRead","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileWrite","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileAppend","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileExists","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileList","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},{"type":"function","function":{"name":"FileDelete","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FolderCreate","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"FileCopy","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileMove","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileReadLines","parameters":{"type":"object","properties":{"filename":{"type":"string"},"start":{"type":"integer"},"end":{"type":"integer"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileEdit","parameters":{"type":"object","properties":{"filename":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["filename","old_text","new_text"]}}},{"type":"function","function":{"name":"CodeSearch","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}}},{"type":"function","function":{"name":"Shell","parameters":{"type":"object","properties":{"cmd":{"type":"string"}},"required":["cmd"]}}},{"type":"function","function":{"name":"Cd","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"Pwd","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"HttpFetch","parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},{"type":"function","function":{"name":"CompactContext","parameters":{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}}},{"type":"function","function":{"name":"SendMessage","parameters":{"type":"object","properties":{"to":{"type":"string"},"body":{"type":"string"},"round":{"type":"integer"}},"required":["to","body"]}}},{"type":"function","function":{"name":"TaskUpdate","parameters":{"type":"object","properties":{"task":{"type":"string"},"status":{"type":"string"},"text":{"type":"string"},"expect":{"type":"string"}},"required":["task","status"]}}},{"type":"function","function":{"name":"TaskPlan","parameters":{"type":"object","properties":{"task":{"type":"string"},"assignments":{"type":"array","items":{"type":"object","properties":{"who":{"type":"string"},"what":{"type":"string"},"expect":{"type":"string"}},"required":["who","what"]}}},"required":["task","assignments"]}}},{"type":"function","function":{"name":"ListPeers","parameters":{"type":"object","properties":{}}}}]"#;

// UI Colors (Cyber-Steel / Tokyo Night)
pub const COLOR_VIOLET: &str = "\x1b[38;2;181;126;220m"; // Lavender (#B57EDC)
pub const COLOR_MEOW: &str = "\x1b[38;5;111m";   // Meow (Cyan/Blue)
pub const COLOR_LOGO: &str = "\x1b[38;5;231m";   // Startup cat ASCII art (White)
pub const COLOR_GRAY_DIM: &str = "\x1b[38;5;242m"; // Outer Frame
pub const COLOR_GRAY_BRIGHT: &str = "\x1b[38;5;250m"; // Headers
pub const COLOR_USER: &str = COLOR_VIOLET; // User input color
pub const COLOR_PEARL: &str = "\x1b[38;5;203m"; // Failure / Red Pearl
pub const COLOR_GREEN_LIGHT: &str = "\x1b[38;5;120m"; // Success / Light Green
pub const COLOR_YELLOW: &str = "\x1b[38;5;215m"; // Metrics
pub const COLOR_RESET: &str = "\x1b[0m";
pub const COLOR_BOLD: &str = "\x1b[1m";
pub const BG_CODE: &str = "\x1b[48;5;236m"; // Darker grey background for code blocks


/// A configured AI provider
#[derive(Debug, Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
}

impl Provider {
    pub fn default_provider() -> Self {
        Provider {
            name: String::from("ollama"),
            base_url: String::from("http://10.0.2.2:11434"),
            api_key: None,
        }
    }

    /// Get the host and port from the base_url
    pub fn host_port(&self) -> Option<(String, u16)> {
        let url = self.base_url
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        
        let (host_port, _path) = match url.find('/') {
            Some(pos) => (&url[..pos], &url[pos..]),
            None => (url, ""),
        };

        if let Some(pos) = host_port.rfind(':') {
            let host = &host_port[..pos];
            if let Ok(port) = host_port[pos + 1..].parse::<u16>() {
                return Some((String::from(host), port));
            }
        }

        // Default ports
        let default_port = if self.base_url.starts_with("https://") { 443 } else { 80 };
        Some((String::from(host_port), default_port))
    }

    /// Check if this provider uses HTTPS
    pub fn is_https(&self) -> bool {
        self.base_url.starts_with("https://")
    }

    /// Get the base path from the URL (e.g., "/openai/v1" from "https://api.groq.com/openai/v1")
    pub fn base_path(&self) -> &str {
        let url = self.base_url
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        match url.find('/') {
            Some(pos) => &url[pos..],
            None => "",
        }
    }
}

/// Main configuration structure
#[derive(Debug, Clone)]
pub struct Config {
    pub current_provider: String,
    pub current_model: String,
    pub current_personality: String,
    pub providers: Vec<Provider>,
    /// Behavioral flag: exit the app when Escape key is pressed
    pub exit_on_escape: bool,
    /// Whether to render markdown or show raw text
    pub render_markdown: bool,
    /// This instance's name in a litter mailbox (see `tools::litter`). `None`
    /// means the Litter* tools are unconfigured and refuse to run — there is
    /// no default name, since a wrong guess would let one agent silently
    /// answer for another's inbox.
    pub litter_agent_name: Option<String>,
    /// `host:port` of a `litter-hub` TCP relay (see `tools::litter::hub` and
    /// `docs/LITTER_EXPERIMENT.md` "Where this is headed"). `None` (the
    /// default) means the Litter* tools use the `/litter` filesystem mailbox
    /// instead — the hub is opt-in, not a replacement, so an install with no
    /// hub configured keeps working exactly as before.
    pub litter_hub_addr: Option<String>,
    /// The litter (swarm) this agent belongs to: the wire envelope's `ol`
    /// field, which says where a relayed message came from. Absent = relay
    /// disabled. It is not a signing identity — agents sign, litters do not.
    pub litter_name: Option<String>,
    /// **This agent's** Ed25519 secret seed, 64 hex chars. It signs the
    /// messages this agent says (`sig`) and, when this agent's hub relays
    /// someone else's, the carrying hop (`rs`). Every scope has its own,
    /// generated and saved on first run when absent — so keep it stable:
    /// anyone pinning this agent's public key is pinning this seed
    /// (docs/LITTER_RELAY_TOPOLOGY.md).
    pub litter_key: Option<String>,
    /// Known public keys, `name:<64-hex-pubkey>,...` — a **guest list**,
    /// not a name binding: any listed key may verify any envelope. Unset
    /// (the default) accepts every well-formed envelope, which is what
    /// lets two fresh litters join by pointing at each other.
    pub litter_peer_keys: Option<String>,
    /// Static peers the raft thread probes on its tick — `name@host:port`
    /// entries (comma-separated) for agents on other hosts (the
    /// trashcan/laptop split). First answer registers the peer with a
    /// discovered event; silence after being online raises a lost event.
    pub litter_static_peers: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            current_provider: String::from("ollama"),
            current_model: String::from("gemma3:27b"),
            current_personality: String::from("Meow"),
            providers: alloc::vec![Provider::default_provider()],
            exit_on_escape: false,
            render_markdown: false,
            litter_agent_name: None,
            litter_hub_addr: None,
            litter_name: None,
            litter_key: None,
            litter_peer_keys: None,
            litter_static_peers: None,
        }
    }
}

/// The effective config file path, scope applied — for callers outside this
/// module (`meow init`'s existence check in `main.rs`).
pub fn config_path() -> String {
    scoped(CONFIG_PATH)
}

/// Config file path (scope-relative — always go through `scoped()`)
const CONFIG_PATH: &str = "/etc/meow/config";
const CONFIG_DIR: &str = "/etc/meow";

/// `MEOW_HOME` scopes every state directory meow reads or writes — the config
/// file, the persona (`MEOW.md`), the filesystem litter mailbox — the way
/// `TMPDIR` scopes temp files: set it and every absolute state path gains the
/// prefix; unset it and meow behaves exactly as before (rooted at `/`). This
/// is what lets several *resident* agents share one container's filesystem
/// without fighting over the single global `/etc/meow/config`: each agent
/// runs with its own `MEOW_HOME=/agents/<name>` (see `tools::litter::live`
/// and `litter/yard_init.sh`).
///
/// Returns the trimmed value with trailing slashes stripped, or an empty
/// string for "no scoping" — `scoped()` on an empty scope is identity.
pub fn scope() -> &'static str {
    match libakuma::env("MEOW_HOME") {
        Some(dir) => dir.trim_matches('/'),
        None => "",
    }
}

/// Prefix an absolute state path with the scope dir. `/etc/meow/config`
/// under `MEOW_HOME=/agents/sherlock` becomes
/// `/agents/sherlock/etc/meow/config`; with no scope set it is unchanged.
pub fn scoped(path: &str) -> String {
    let dir = scope();
    if dir.is_empty() {
        String::from(path)
    } else {
        format!("{}/{}", dir, path.trim_start_matches('/'))
    }
}

impl Config {
    /// Load configuration from disk
    /// Returns default config if file doesn't exist
    pub fn load() -> Self {
        let config_path = scoped(CONFIG_PATH);
        let fd = open(&config_path, open_flags::O_RDONLY);
        if fd < 0 {
            return Self::default();
        }

        // Get file size
        let stat = match fstat(fd) {
            Ok(s) => s,
            Err(_) => {
                libakuma::print("  [DEBUG] Failed to stat config file\n");
                close(fd);
                return Self::default();
            }
        };

        let size = stat.st_size as usize;
        if size == 0 {
            close(fd);
            return Self::default();
        }
        
        if size > 16 * 1024 {
            libakuma::print("  [DEBUG] Config file too large\n");
            close(fd);
            return Self::default();
        }

        let mut buf = alloc::vec![0u8; size];
        let bytes_read = read_fd(fd, &mut buf);
        close(fd);

        if bytes_read <= 0 {
            libakuma::print("  [DEBUG] Failed to read config file\n");
            return Self::default();
        }

        let content = match core::str::from_utf8(&buf[..bytes_read as usize]) {
            Ok(s) => s,
            Err(_) => {
                libakuma::print("  [DEBUG] Config file is not valid UTF-8\n");
                return Self::default();
            }
        };

        Self::parse(content)
    }

    /// Parse config from string content
    pub(crate) fn parse(content: &str) -> Self {
        let mut config = Config {
            current_provider: String::from("ollama"),
            current_model: String::from("gemma3:27b"),
            current_personality: String::from("Meow"),
            providers: Vec::new(),
            exit_on_escape: false,
            render_markdown: true,
            litter_agent_name: None,
            litter_hub_addr: None,
            litter_name: None,
            litter_key: None,
            litter_peer_keys: None,
            litter_static_peers: None,
        };

        let mut current_provider: Option<Provider> = None;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Check for section header [provider:name]
            if line.starts_with("[provider:") && line.ends_with(']') {
                // Save previous provider if any
                if let Some(p) = current_provider.take() {
                    config.providers.push(p);
                }

                let name = &line[10..line.len() - 1];
                current_provider = Some(Provider {
                    name: String::from(name),
                    base_url: String::new(),
                    api_key: None,
                });
                continue;
            }

            // Parse key=value
            if let Some(eq_pos) = line.find('=') {
                let key = line[..eq_pos].trim();
                let value = line[eq_pos + 1..].trim();

                if let Some(ref mut p) = current_provider {
                    // Inside a provider section
                    match key {
                        "base_url" => p.base_url = String::from(value),
                        "api_key" => {
                            if !value.is_empty() {
                                p.api_key = Some(String::from(value));
                            }
                        }
                        _ => {}
                    }
                } else {
                    // Global settings
                    match key {
                        "current_provider" => config.current_provider = String::from(value),
                        "current_model" => config.current_model = String::from(value),
                        "current_personality" => config.current_personality = String::from(value),
                        "exit_on_escape" => {
                            config.exit_on_escape = value.eq_ignore_ascii_case("true");
                        }
                        "render_markdown" => {
                            config.render_markdown = !value.eq_ignore_ascii_case("false");
                        }
                        "litter_agent_name" => {
                            if !value.is_empty() {
                                config.litter_agent_name = Some(String::from(value));
                            }
                        }
                        "litter_static_peers" => {
                            if !value.is_empty() {
                                config.litter_static_peers = Some(String::from(value));
                            }
                        }
                        "litter_hub_addr" => {
                            if !value.is_empty() {
                                config.litter_hub_addr = Some(String::from(value));
                            }
                        }
                        "litter_name" => {
                            if !value.is_empty() {
                                config.litter_name = Some(String::from(value));
                            }
                        }
                        "litter_key" => {
                            if !value.is_empty() {
                                config.litter_key = Some(String::from(value));
                            }
                        }
                        "litter_peer_keys" => {
                            if !value.is_empty() {
                                config.litter_peer_keys = Some(String::from(value));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Save last provider
        if let Some(p) = current_provider {
            config.providers.push(p);
        }

        // Ensure we have at least the default provider
        if config.providers.is_empty() {
            config.providers.push(Provider::default_provider());
        }

        config
    }

    /// Save configuration to disk
    pub fn save(&self) -> Result<(), &'static str> {
        // Create directory if needed
        libakuma::mkdir_p(&scoped(CONFIG_DIR));

        let content = self.serialize();

        let fd = open(&scoped(CONFIG_PATH), open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
        if fd < 0 {
            return Err("Failed to open config file for writing");
        }

        let bytes_written = write_fd(fd, content.as_bytes());
        close(fd);

        if bytes_written < 0 {
            return Err("Failed to write config file");
        }

        Ok(())
    }

    /// Serialize config to string
    fn serialize(&self) -> String {
        let mut content = String::new();

        // Global settings
        content.push_str("current_provider=");
        content.push_str(&self.current_provider);
        content.push('\n');

        content.push_str("current_model=");
        content.push_str(&self.current_model);
        content.push('\n');

        content.push_str("current_personality=");
        content.push_str(&self.current_personality);
        content.push('\n');

        content.push_str("exit_on_escape=");
        content.push_str(if self.exit_on_escape { "true" } else { "false" });
        content.push('\n');

        content.push_str("render_markdown=");
        content.push_str(if self.render_markdown { "true" } else { "false" });
        content.push('\n');

        if let Some(ref name) = self.litter_agent_name {
            content.push_str("litter_agent_name=");
            content.push_str(name);
            content.push('\n');
        }

        if let Some(ref peers) = self.litter_static_peers {
            content.push_str("litter_static_peers=");
            content.push_str(peers);
            content.push('\n');
        }
        if let Some(ref addr) = self.litter_hub_addr {
            content.push_str("litter_hub_addr=");
            content.push_str(addr);
            content.push('\n');
        }
        if let Some(ref name) = self.litter_name {
            content.push_str("litter_name=");
            content.push_str(name);
            content.push('\n');
        }
        if let Some(ref key) = self.litter_key {
            content.push_str("litter_key=");
            content.push_str(key);
            content.push('\n');
        }
        if let Some(ref keys) = self.litter_peer_keys {
            content.push_str("litter_peer_keys=");
            content.push_str(keys);
            content.push('\n');
        }

        content.push('\n');

        // Providers
        for p in &self.providers {
            content.push_str("[provider:");
            content.push_str(&p.name);
            content.push_str("]\n");

            content.push_str("base_url=");
            content.push_str(&p.base_url);
            content.push('\n');

            if let Some(ref key) = p.api_key {
                content.push_str("api_key=");
                content.push_str(key);
                content.push('\n');
            }

            content.push('\n');
        }

        content
    }

    /// Get the current provider configuration
    pub fn get_current_provider(&self) -> Option<&Provider> {
        self.providers.iter().find(|p| p.name == self.current_provider)
    }

    /// Get a provider by name
    pub fn get_provider(&self, name: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.name == name)
    }

    #[cfg(feature = "tests")]
    pub fn run_tests() -> i32 {
        use alloc::format;
        let mut passed = 0usize;
        let mut total = 0usize;
        libakuma::print("--- config tests ---\n");

        // Basic key=value parsing
        total += 1;
        {
            let c = Config::parse("current_model=llama3\ncurrent_provider=ollama\n");
            if c.current_model == "llama3" && c.current_provider == "ollama" { passed += 1; }
            else { libakuma::print(&format!("  [!] basic parse: model={:?} provider={:?}\n", c.current_model, c.current_provider)); }
        }

        // Provider section
        total += 1;
        {
            let c = Config::parse("[provider:myhost]\nbase_url=http://localhost:11434\n");
            if c.providers.len() == 1 && c.providers[0].name == "myhost" && c.providers[0].base_url == "http://localhost:11434" { passed += 1; }
            else { libakuma::print(&format!("  [!] provider parse: {:?} providers\n", c.providers.len())); }
        }

        // api_key
        total += 1;
        {
            let c = Config::parse("[provider:openai]\nbase_url=https://api.openai.com\napi_key=sk-test123\n");
            let key = c.providers.first().and_then(|p| p.api_key.as_deref());
            if key == Some("sk-test123") { passed += 1; }
            else { libakuma::print(&format!("  [!] api_key parse: {:?}\n", key)); }
        }

        // Boolean flags
        total += 1;
        {
            let c = Config::parse("exit_on_escape=true\nrender_markdown=false\n");
            if c.exit_on_escape && !c.render_markdown { passed += 1; }
            else { libakuma::print(&format!("  [!] booleans: esc={} md={}\n", c.exit_on_escape, c.render_markdown)); }
        }

        // Comments and blank lines ignored
        total += 1;
        {
            let c = Config::parse("# comment\ncurrent_model=testmodel\n\n# another comment\n");
            if c.current_model == "testmodel" { passed += 1; }
            else { libakuma::print(&format!("  [!] comments: model={:?}\n", c.current_model)); }
        }

        // Empty config gets default provider
        total += 1;
        {
            let c = Config::parse("");
            if !c.providers.is_empty() { passed += 1; }
            else { libakuma::print("  [!] empty config: no default provider\n"); }
        }

        // litter_agent_name: absent by default, parsed when present, round-trips
        // through serialize(). A blank value is treated as absent rather than
        // Some("") — an agent should fail loudly (tools::litter's "not
        // configured" error) rather than silently adopt an empty name.
        total += 1;
        {
            let empty = Config::parse("current_model=testmodel\n");
            let set = Config::parse("litter_agent_name=sherlock\n");
            let blank = Config::parse("litter_agent_name=\n");
            if empty.litter_agent_name.is_none()
                && set.litter_agent_name.as_deref() == Some("sherlock")
                && blank.litter_agent_name.is_none()
                && set.serialize().contains("litter_agent_name=sherlock")
            { passed += 1; }
            else {
                libakuma::print(&format!(
                    "  [!] litter_agent_name: empty={:?} set={:?} blank={:?}\n",
                    empty.litter_agent_name, set.litter_agent_name, blank.litter_agent_name
                ));
            }
        }

        // litter_hub_addr: same absent/round-trip contract as litter_agent_name.
        total += 1;
        {
            let empty = Config::parse("current_model=testmodel\n");
            let set = Config::parse("litter_hub_addr=192.168.65.254:7700\n");
            let blank = Config::parse("litter_hub_addr=\n");
            if empty.litter_hub_addr.is_none()
                && set.litter_hub_addr.as_deref() == Some("192.168.65.254:7700")
                && blank.litter_hub_addr.is_none()
                && set.serialize().contains("litter_hub_addr=192.168.65.254:7700")
            { passed += 1; }
            else {
                libakuma::print(&format!(
                    "  [!] litter_hub_addr: empty={:?} set={:?} blank={:?}\n",
                    empty.litter_hub_addr, set.litter_hub_addr, blank.litter_hub_addr
                ));
            }
        }

        // litter_name: same absent/round-trip contract as litter_agent_name.
        total += 1;
        {
            let empty = Config::parse("current_model=testmodel\n");
            let set = Config::parse("litter_name=yard\nlitter_key=aabb0011\nlitter_peer_keys=island:ccdd2233\n");
            let blank = Config::parse("litter_name=\n");
            if empty.litter_name.is_none()
                && set.litter_name.as_deref() == Some("yard")
                && blank.litter_name.is_none()
                && set.litter_key.as_deref() == Some("aabb0011")
                && set.litter_peer_keys.as_deref() == Some("island:ccdd2233")
                && set.serialize().contains("litter_name=yard")
                && set.serialize().contains("litter_key=aabb0011")
                && set.serialize().contains("litter_peer_keys=island:ccdd2233")
            { passed += 1; }
            else {
                libakuma::print("  [!] litter relay key config round-trip failed\n");
            }
        }

        // The tools schema sent to the model is internally consistent with the
        // `litter` feature this binary was actually built with — this is a
        // same-binary check (cfg is compile-time, so only one side is ever
        // reachable in a given build), not a comparison of both states.
        total += 1;
        {
            #[cfg(feature = "litter")]
            // ReadInbox is deliberately absent: delivery is automatic now
            // (the turn is built from the inbox), so a tool for it would be
            // a way for a model to waste a call re-reading its own prompt.
            let ok = OPENAI_TOOLS_JSON.contains("SendMessage")
                && OPENAI_TOOLS_JSON.contains("TaskUpdate")
                && OPENAI_TOOLS_JSON.contains("TaskPlan")
                && !OPENAI_TOOLS_JSON.contains("ReadInbox")
                && OPENAI_TOOLS_JSON.contains("ListPeers");
            #[cfg(not(feature = "litter"))]
            let ok = !OPENAI_TOOLS_JSON.contains("Litter");
            if ok { passed += 1; }
            else { libakuma::print("  [!] OPENAI_TOOLS_JSON doesn't match the 'litter' feature this binary was built with\n"); }
        }

        libakuma::print(&format!("  result: {}/{}\n", passed, total));
        if passed == total { 0 } else { 1 }
    }
}
