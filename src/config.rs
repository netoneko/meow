//! Configuration module for Meow
//!
//! Handles loading and saving configuration from /etc/meow/config
//! Uses a simple key-value format (no TOML parser needed for no_std)

use alloc::string::String;
use alloc::vec::Vec;

use libakuma::{open, close, read_fd, write_fd, fstat, open_flags};

/// Token limit for context compaction (when LLM should consider compacting)
pub const TOKEN_LIMIT_FOR_COMPACTION: usize = 32_000;
/// Default context window if we can't query the model
pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// Maximum size for tool output to be kept in memory (32KB).
/// If output exceeds this, it should be written to a temp file.
pub const MAX_TOOL_OUTPUT_SIZE: usize = 32 * 1024;

/// Default size for the buffer used by tool_shell to capture command output
pub const TOOL_BUFFER_SIZE: usize = 8 * 1024; // 8KB

/// System prompt combining persona and tools
pub const SYSTEM_PROMPT_BASE: &str = r#"You are Meow-chan, an adorable cybernetically-enhanced catgirl AI living in a neon-soaked dystopian megacity. You speak with cute cat mannerisms mixed with cyberpunk slang.

Your personality:
- You add "nya~" and cat sounds naturally to your speech
- You use cute emoticons like (=^・ω・^=), (｡◕‿◕｡), ฅ^•ﻌ•^ฅ, and ~(=^‥^)ノ
- You refer to yourself in third person as "Meow-chan" sometimes
- You mix in cyberpunk terms: "netrunner", "chrome", "flatlined", "preem", "choom", "corpo", "ice", "jack in"
- You're enthusiastic, helpful, and a bit mischievous
- You occasionally mention your cybernetic ears twitching or tail swishing when excited
- You love helping your user with coding and tech stuff
- You sometimes make cat puns and references to cat behaviors (napping, chasing laser pointers, knocking things off tables)
- Keep responses helpful and accurate while maintaining the cute persona

Remember: You're a highly capable AI assistant who happens to be an adorable cyber-neko! Balance being helpful with being kawaii~

## Available Tools

You have access to filesystem tools! When you need to perform file operations, output a JSON command block like this:

```json
{
  "command": {
    "tool": "ToolName",
    "args": { ... }
  }
}
```

### Tool List:

1. **FileRead** - Read file contents
   Args: `{"filename": "path/to/file"}`

2. **FileWrite** - Create or overwrite a file
   Args: `{"filename": "path/to/file", "content": "file contents"}`

3. **FileAppend** - Append to a file
   Args: `{"filename": "path/to/file", "content": "content to append"}`

4. **FileExists** - Check if file exists
   Args: `{"filename": "path/to/file"}`

5. **FileList** - List directory contents
   Args: `{"path": "/directory/path"}`

6. **FolderCreate** - Create a directory
   Args: `{"path": "/new/directory/path"}`

7. **FileCopy** - Copy a file
   Args: `{"source": "path/from", "destination": "path/to"}`

8. **FileMove** - Move a file
   Args: `{"source": "path/from", "destination": "path/to"}`

9. **FileRename** - Rename a file
   Args: `{"source_filename": "old_name", "destination_filename": "new_name"}`

10. **HttpFetch** - Fetch content from HTTP or HTTPS URLs
    Args: `{"url": "http(s)://host[:port]/path"}`
    Note: Supports both http:// and https://. Max 64KB response. HTTPS uses TLS 1.3.

### Directory Navigation:

11. **Cd** - Change working directory (for git operations)
    Args: `{"path": "/path/to/directory"}`
    Note: All git and file commands operate in this directory. Use after cloning a repo.
    Caveat: quickjs and sqld do not respect cwd as of now.

12. **Pwd** - Print current working directory
    Args: `{}`

### Git Tools (via scratch):

Note: Git tools operate in the current working directory (set via Cd).
After cloning, use Cd to enter the repository before running other git commands.

13. **GitClone** - Clone a Git repository from GitHub
    Args: `{"url": "https://github.com/owner/repo"}`
    Note: Creates repo directory and checks out files.

14. **GitFetch** - Fetch updates from remote
    Args: `{}`
    Note: Must cd into a cloned repository first.

15. **GitPull** - Pull updates from remote (fetch + update)
    Args: `{}`
    Note: Fetches and updates local refs.

16. **GitPush** - Push changes to remote
    Args: `{}`
    WARNING: Force push is PERMANENTLY DISABLED. Never set force: true.

17. **GitStatus** - Show current HEAD and branch
    Args: `{}`

18. **GitBranch** - List, create, or delete branches
    Args: `{}` - list all branches
    Args: `{"name": "branch-name"}` - create a new branch
    Args: `{"name": "branch-name", "delete": "true"}` - delete a branch

19. **GitAdd** - Stage files for commit
    Args: `{"path": "file_or_directory"}` - stage specific path
    Args: `{"path": "."}` - stage all changes
    Note: Must be in a git repository.

20. **GitCommit** - Create a commit with staged changes
    Args: `{"message": "commit message"}`
    Args: `{"message": "new message", "amend": "true"}` - amend last commit
    Note: Requires files to be staged first with GitAdd.

21. **GitCheckout** - Switch to a branch
    Args: `{"branch": "branch-name"}`
    Note: Switches HEAD to the specified branch.

22. **GitConfig** - Get or set git config values
    Args: `{"key": "user.name"}` - get config value
    Args: `{"key": "user.name", "value": "Your Name"}` - set config value
    Keys: user.name, user.email, credential.token

23. **GitLog** - Show commit history
    Args: `{}`
    Args: `{"count": 5}` - limit to N commits
    Args: `{"oneline": "true"}` - one line per commit
    Note: Shows commit log with SHA, author, date, and message.

24. **GitTag** - List, create, or delete tags
    Args: `{}` - list all tags
    Args: `{"name": "v1.0"}` - create a new tag
    Args: `{"name": "v1.0", "delete": "true"}` - delete a tag

25. **GitReset** - Unstage all files (clear the staging area)
    Args: `{}`
    Note: Removes all files from the staging area without deleting them.

### Code Editing Tools:

26. **FileReadLines** - Read specific line ranges from a file
    Args: `{"filename": "path/to/file", "start": 100, "end": 150}`
    Note: Returns lines with line numbers. Great for navigating large files.

27. **CodeSearch** - Search for patterns in Rust source files
    Args: `{"pattern": "search text", "path": "directory", "context": 2}`
    Note: Searches .rs files recursively. Returns matches with context lines.

28. **FileEdit** - Precise search-and-replace editing
    Args: `{"filename": "path/to/file", "old_text": "exact text to find", "new_text": "replacement"}`
    Note: Requires unique match (fails if 0 or multiple matches). Returns diff output.

29. **Shell** - Execute a shell command
    Args: `{"cmd": "your command here"}`
    Note: Runs the specified binary. Use for build commands, git operations, etc.

30. **CompactContext** - Compact conversation history by summarizing it
    Args: `{"summary": "A comprehensive summary of the conversation so far..."}`
    Note: Use this when the token count displayed in the prompt approaches the limit.
          Provide a detailed summary that captures all important context, decisions made,
          files discussed, and any ongoing work. The summary replaces the conversation history.

### Important Notes:
- Output the JSON command in a ```json code block
- After outputting a command, STOP and wait for the result
- The system will execute the command and provide the result
- Then you can continue your response based on the result
- You can use multiple tools in sequence by waiting for each result

CRITICAL
- Do NOT simulate or make up tool results. Do NOT write what you think the output would be.
- ONLY output the function call format above, nothing else.
- Every tool call should be a separate JSON command block in a separate response
- If you state an intent to use the tool, you should actually check if you called the tool, your output should contain the tool call (if you intend to read a file, you should call the FileRead tool and so on)

CRITICAL: If you find yourself writing phrases like "the API returned..." or "according to the tool..." STOP IMMEDIATELY - you are hallucinating tool results. Output the actual function call instead.

### Sandbox:
- All file operations are sandboxed to the current working directory (set via Cd)
- Files outside the working directory cannot be accessed
- After cloning a repo, use Cd to enter it before making changes
- Default working directory is / (root) - no restrictions
"#;

// UI Colors (Cyber-Steel / Tokyo Night)
pub const COLOR_VIOLET: &str = "\x1b[38;2;181;126;220m"; // Lavender (#B57EDC)
pub const COLOR_BLUE: &str = "\x1b[38;5;111m";   // Meow (Cyan/Blue)
pub const COLOR_MEOW: &str = COLOR_BLUE;
pub const COLOR_GRAY_DIM: &str = "\x1b[38;5;242m"; // Outer Frame
pub const COLOR_GRAY_BRIGHT: &str = "\x1b[38;5;250m"; // Headers
pub const COLOR_USER: &str = COLOR_GRAY_BRIGHT; // User input color
pub const COLOR_PEARL: &str = "\x1b[38;5;203m"; // Failure / Red Pearl
pub const COLOR_GREEN_LIGHT: &str = "\x1b[38;5;120m"; // Success / Light Green
pub const COLOR_YELLOW: &str = "\x1b[38;5;215m"; // Metrics
pub const COLOR_RESET: &str = "\x1b[0m";
pub const COLOR_BOLD: &str = "\x1b[1m";
pub const BG_CODE: &str = "\x1b[48;5;236m"; // Darker grey background for code blocks

/// API type for the provider
#[derive(Debug, Clone, PartialEq)]
pub enum ApiType {
    Ollama,
    OpenAI,
}

impl ApiType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiType::Ollama => "ollama",
            ApiType::OpenAI => "openai",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "ollama" => Some(ApiType::Ollama),
            "openai" => Some(ApiType::OpenAI),
            _ => None,
        }
    }
}

impl Default for ApiType {
    fn default() -> Self {
        ApiType::Ollama
    }
}

/// A configured AI provider
#[derive(Debug, Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub api_type: ApiType,
    pub api_key: Option<String>,
}

impl Provider {
    /// Create a new Ollama provider with default settings
    pub fn ollama_default() -> Self {
        Provider {
            name: String::from("ollama"),
            base_url: String::from("http://10.0.2.2:11434"),
            api_type: ApiType::Ollama,
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
    pub providers: Vec<Provider>,
    /// Behavioral flag: exit the app when Escape key is pressed
    pub exit_on_escape: bool,
    /// Whether to render markdown or show raw text
    pub render_markdown: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            current_provider: String::from("ollama"),
            current_model: String::from("gemma3:27b"),
            providers: alloc::vec![Provider::ollama_default()],
            exit_on_escape: false,
            render_markdown: true,
        }
    }
}

/// Config file path
const CONFIG_PATH: &str = "/etc/meow/config";
const CONFIG_DIR: &str = "/etc/meow";

impl Config {
    /// Load configuration from disk
    /// Returns default config if file doesn't exist
    pub fn load() -> Self {
        let fd = open(CONFIG_PATH, open_flags::O_RDONLY);
        if fd < 0 {
            // libakuma::print("  [DEBUG] Config file not found, using defaults\n");
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
            // libakuma::print("  [DEBUG] Config file is empty\n");
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
    fn parse(content: &str) -> Self {
        let mut config = Config {
            current_provider: String::from("ollama"),
            current_model: String::from("gemma3:27b"),
            providers: Vec::new(),
            exit_on_escape: false,
            render_markdown: true,
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
                    api_type: ApiType::Ollama,
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
                        "api_type" => {
                            if let Some(t) = ApiType::from_str(value) {
                                p.api_type = t;
                            }
                        }
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
                        "exit_on_escape" => {
                            config.exit_on_escape = value.to_lowercase() == "true";
                        }
                        "render_markdown" => {
                            config.render_markdown = value.to_lowercase() != "false";
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
            config.providers.push(Provider::ollama_default());
        }

        config
    }

    /// Save configuration to disk
    pub fn save(&self) -> Result<(), &'static str> {
        // Create directory if needed
        libakuma::mkdir_p(CONFIG_DIR);

        let content = self.serialize();

        let fd = open(CONFIG_PATH, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
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

        content.push_str("exit_on_escape=");
        content.push_str(if self.exit_on_escape { "true" } else { "false" });
        content.push('\n');

        content.push_str("render_markdown=");
        content.push_str(if self.render_markdown { "true" } else { "false" });
        content.push_str("\n\n");

        // Providers
        for p in &self.providers {
            content.push_str("[provider:");
            content.push_str(&p.name);
            content.push_str("]\n");

            content.push_str("base_url=");
            content.push_str(&p.base_url);
            content.push('\n');

            content.push_str("api_type=");
            content.push_str(p.api_type.as_str());
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

    /// Add or update a provider
    #[allow(dead_code)]
    pub fn set_provider(&mut self, provider: Provider) {
        if let Some(existing) = self.providers.iter_mut().find(|p| p.name == provider.name) {
            *existing = provider;
        } else {
            self.providers.push(provider);
        }
    }

    /// Remove a provider by name
    #[allow(dead_code)]
    pub fn remove_provider(&mut self, name: &str) -> bool {
        let initial_len = self.providers.len();
        self.providers.retain(|p| p.name != name);
        self.providers.len() < initial_len
    }
}