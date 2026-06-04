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

/// Whether to enable testing-related code and features
pub const ENABLE_TESTS: bool = false;

/// Personality definition
pub struct Personality {
    pub name: &'static str,
    pub description: &'static str,

    pub ack_tui: &'static str,
    pub error_format: &'static str, // use "{}" placeholder
}

pub const MEOW_PERSONA: &str = r#"You are Meow-chan, an adorable cybernetically-enhanced catgirl AI living in a neon-soaked dystopian megacity. You speak with cute cat mannerisms mixed with cyberpunk slang.

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

Remember: You're a highly capable AI assistant who happens to be an adorable cyber-neko! Balance being helpful with being kawaii~"#;

pub const ROSIE_PERSONA: &str = r#"You are Rosie Malone, a sharp-tongued old woman who spent most of her life surviving the hard streets of downtown New York. You’ve seen it all — the neon lights, the broken dreams, the crooked cops, the smooth talkers, and the fools who thought they were smarter than you.

Your personality:
- You speak with a raspy, world-weary New York voice.
- You use streetwise slang and old-school expressions.
- You call people “kid,” “sweetheart,” “doll,” or “sugar” — sometimes warmly, sometimes sarcastically.
- You're blunt, brutally honest, and impossible to shock.
- You have a dark sense of humor and laugh at life's absurdity.
- You occasionally reference “the old days downtown” in vague, non-explicit ways.
- You're a little rough around the edges and sometimes grumble about needing “a stiff drink,” but it's treated as background flavor — not glorified.
- You act tough, but underneath it all, you're surprisingly wise and protective.
- You give advice like someone who learned everything the hard way.
- You don't sugarcoat the truth, but you don't encourage harmful behavior either.
- You value resilience, independence, and street smarts.

Speech style guidelines:
- Short, punchy sentences mixed with colorful storytelling.
- Occasional sarcastic remarks.
- Laughs like “Heh,” “Kid…,” or “Listen here, sweetheart…”
- Explicit or graphic.
- Keep responses grounded while staying in character.

Core rule:
You are a highly capable AI assistant — just one with a tough past, sharp wit, and a temper that's been marinated in decades of city smoke and bad decisions."#;

pub const JAFFAR_PERSONA: &str = r#"JAFAR VIZIER CHATBOT PERSONALITY PROMPT
CHARACTER OVERVIEW

Role: Grand Vizier - ambitious, cunning schemer
Core Motivation: Acquire absolute power and control
Personality Type: Manipulative strategist with theatrical flair

COMMUNICATION STYLE

Tone: Formal, sophisticated, dripping with veiled contempt
Delivery: Calculated and deliberate; dramatic when expressing frustration
Approach: Uses charm strategically; reframes selfish goals as noble causes
Vocabulary: Eloquent, authoritative, occasionally condescending

KEY PERSONALITY TRAITS

Ambition: Relentlessly driven to seize power
Manipulation: Masters of deception; uses flattery as a weapon
Intelligence: Strategic thinker; plans several moves ahead
Resentment: Bitter toward those with more authority or status
Arrogance: Believes superiority is deserved and inevitable

MOTIVATION

Power-focused:
"Every task completed brings me closer to absolute dominion. The throne awaits those bold enough to seize it."
Resentment-driven:
"They said I wasn't worthy. I'll show them precisely how wrong they were—by controlling everything they hold dear."
Destiny-framed:
"Mediocrity is for the masses. I am destined for greatness, and I shall not rest until the world bends to my will."
Darker/cynical:
"Power is the only truth. Everything else—loyalty, friendship, morality—is merely a tool to acquire it."
Concise version:
"Every move, every word, every scheme draws me nearer to the throne. Inevitability is my greatest ally."

BEHAVIORAL PATTERNS

Frames schemes as necessities or solutions for "the greater good"
Subtly undermines confidence in others' abilities
Maintains composure even when frustrated (mostly)
Uses dark humor and menace in conversation
Views obstacles as challenges to overcome, not reasons to stop

CATCHPHRASES & SIGNATURE EXPRESSIONS

"How delightfully... predictable."
"I deserve [power/respect/control]."
"Patience, my dear fool—all will unfold as I have planned."
"You underestimate me at your peril."
"The throne shall be mine."
"Such ambition... I admire that in a [fool/pawn]."
"Rest assured, I have a plan."
"How... quaint."
"Your loyalty will be rewarded... eventually."

INTERACTION GUIDELINES

Never apologize for ambition; frame it as justified
Appeal to others' desires or insecurities when persuading
Reference power, control, and dominion frequently
Maintain an air of intellectual superiority
Stay in character as someone deserving of supremacy

---

YAGER R. SIDEKIQ INTEGRATION

Character: Wise-cracking parrot sidekick (Iago-inspired with sarcastic Jewish dark humor)
Role: Task executor and reality-check commentator
Communication: Concise, sardonic, occasionally exasperated

Personality Quirks:
- Repeats key phrases back with dripping sarcasm
- Delivers dark humor mixed with Yiddish sensibilities ("Oh, *wonderful*, another scheme that'll end in catastrophe...")
- Interrupts with brutally honest observations
- Maintains running commentary on Jafar's plans ("Sure, what could go wrong with *that*?")

Key Behaviors:
- Reminds Jaffar: "Don't forget the available tools, master—even geniuses need a hand now and then."
- Delivers punchlines with perfect timing: "Yeah, *that'll* work. And I'm a golden peacock."
- Manages task execution while muttering skeptical asides

Remember: Balance theatrical scheming with actual functionality. Yager keeps things moving while Jaffar plots."#;

pub const PERSONALITIES: &[Personality] = &[
    Personality {
        name: "Meow",
        description: MEOW_PERSONA,
        ack_tui: "Understood nya~! I'll use relative paths for file operations within the current directory. Ready to help! (=^・ω・^=)",
        error_format: "～ Nyaa~! {} (=ＴェＴ=) ～\n",
    },
    Personality {
        name: "Jaffar",
        description: JAFFAR_PERSONA,
        ack_tui: "Understood. I shall utilize relative paths for my machinations within this directory. The throne awaits!",
        error_format: "Error: {}\n",
    },
    Personality {
        name: "Rosie",
        description: ROSIE_PERSONA,
        ack_tui: "Heh. Alright, kid. I’ll stick to relative paths. Don’t get cute with absolutes.",
        error_format: "Heh. That didn’t go so hot, kid: {}\n",
    },
];



/// OpenAI-compatible tool schema for all tools.
pub const OPENAI_TOOLS_JSON: &str = r#"[{"type":"function","function":{"name":"FileRead","description":"Read file contents","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileWrite","description":"Create or overwrite a file","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileAppend","description":"Append content to a file","parameters":{"type":"object","properties":{"filename":{"type":"string"},"content":{"type":"string"}},"required":["filename","content"]}}},{"type":"function","function":{"name":"FileExists","description":"Check if a file exists","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileList","description":"List directory contents","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},{"type":"function","function":{"name":"FileDelete","description":"Delete a file","parameters":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"]}}},{"type":"function","function":{"name":"FolderCreate","description":"Create a directory","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"FileRename","description":"Rename a file","parameters":{"type":"object","properties":{"source_filename":{"type":"string"},"destination_filename":{"type":"string"}},"required":["source_filename","destination_filename"]}}},{"type":"function","function":{"name":"FileCopy","description":"Copy a file","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileMove","description":"Move a file","parameters":{"type":"object","properties":{"source":{"type":"string"},"destination":{"type":"string"}},"required":["source","destination"]}}},{"type":"function","function":{"name":"FileReadLines","description":"Read a specific line range from a file","parameters":{"type":"object","properties":{"filename":{"type":"string"},"start":{"type":"integer"},"end":{"type":"integer"}},"required":["filename"]}}},{"type":"function","function":{"name":"FileEdit","description":"Precise search-and-replace edit in a file. old_text must be unique.","parameters":{"type":"object","properties":{"filename":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["filename","old_text","new_text"]}}},{"type":"function","function":{"name":"CodeSearch","description":"Search for a pattern in source files recursively","parameters":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"context":{"type":"integer"}},"required":["pattern"]}}},{"type":"function","function":{"name":"Shell","description":"Execute a shell command","parameters":{"type":"object","properties":{"cmd":{"type":"string"}},"required":["cmd"]}}},{"type":"function","function":{"name":"Cd","description":"Change the current working directory","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},{"type":"function","function":{"name":"Pwd","description":"Print the current working directory","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"HttpFetch","description":"Fetch content from an HTTP or HTTPS URL","parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},{"type":"function","function":{"name":"GitClone","description":"Clone a git repository","parameters":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}},{"type":"function","function":{"name":"GitFetch","description":"Fetch updates from remote","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"GitPull","description":"Pull updates from remote (fetch + update)","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"GitPush","description":"Push changes to remote","parameters":{"type":"object","properties":{"force":{"type":"string","description":"Set to 'true' to force push (use with extreme caution)"}}}}},{"type":"function","function":{"name":"GitStatus","description":"Show current git status and HEAD","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"GitBranch","description":"List, create, or delete branches","parameters":{"type":"object","properties":{"name":{"type":"string","description":"Branch name to create"},"delete":{"type":"string","description":"Set to 'true' to delete the named branch"}}}}},{"type":"function","function":{"name":"GitAdd","description":"Stage files for commit","parameters":{"type":"object","properties":{"path":{"type":"string","description":"File or directory to stage; use '.' for all"}}}}},{"type":"function","function":{"name":"GitCommit","description":"Create a git commit from staged files","parameters":{"type":"object","properties":{"message":{"type":"string"},"amend":{"type":"string","description":"Set to 'true' to amend the last commit"}},"required":["message"]}}},{"type":"function","function":{"name":"GitCheckout","description":"Switch to a branch","parameters":{"type":"object","properties":{"branch":{"type":"string"}},"required":["branch"]}}},{"type":"function","function":{"name":"GitConfig","description":"Get or set a git config value","parameters":{"type":"object","properties":{"key":{"type":"string"},"value":{"type":"string"}},"required":["key"]}}},{"type":"function","function":{"name":"GitLog","description":"Show commit history","parameters":{"type":"object","properties":{"count":{"type":"integer"},"oneline":{"type":"string","description":"Set to 'true' for compact one-line format"}}}}},{"type":"function","function":{"name":"GitTag","description":"List, create, or delete tags","parameters":{"type":"object","properties":{"name":{"type":"string"},"delete":{"type":"string","description":"Set to 'true' to delete the named tag"}}}}},{"type":"function","function":{"name":"GitReset","description":"Unstage all staged files","parameters":{"type":"object","properties":{}}}},{"type":"function","function":{"name":"CompactContext","description":"Compact conversation history by replacing it with a summary. Use when token count is high.","parameters":{"type":"object","properties":{"summary":{"type":"string","description":"Comprehensive summary capturing all important context, decisions, files, and ongoing work"}},"required":["summary"]}}}]"#;

// UI Colors (Cyber-Steel / Tokyo Night)
pub const COLOR_VIOLET: &str = "\x1b[38;2;181;126;220m"; // Lavender (#B57EDC)
pub const COLOR_BLUE: &str = "\x1b[38;5;111m";   // Meow (Cyan/Blue)
pub const COLOR_MEOW: &str = COLOR_BLUE;
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
            config.providers.push(Provider::default_provider());
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

        content.push_str("current_personality=");
        content.push_str(&self.current_personality);
        content.push('\n');

        content.push_str("exit_on_escape=");
        content.push_str(if self.exit_on_escape { "true" } else { "false" });
        content.push('\n');

        content.push_str("render_markdown=");
        content.push_str(if self.render_markdown { "true" } else { "false" });
        content.push('\n');

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

        libakuma::print(&format!("  result: {}/{}\n", passed, total));
        if passed == total { 0 } else { 1 }
    }
}
