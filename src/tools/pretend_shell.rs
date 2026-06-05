//! A tiny in-process "pretend shell".
//!
//! meow's kernel `spawn` only hands back the child's stdout fd — there is no
//! `dup2` to wire a child's stdout into an arbitrary fd before exec. So we can't
//! build a *real* shell that plumbs file descriptors. Instead we *pretend*: meow
//! parses the operators itself, runs each command, captures its stdout (which
//! `spawn` already gives us), and re-writes that output to a redirect backend —
//! a file or a socket. No busybox required.
//!
//! Supported grammar (deliberately minimal):
//!   - `cmd1 && cmd2`  run cmd2 only if cmd1 exited 0
//!   - `cmd1 || cmd2`  run cmd2 only if cmd1 exited non-zero
//!   - `cmd > target`  truncate-write cmd's stdout to target
//!   - `cmd >> target` append cmd's stdout to target
//!
//! A redirect `target` is one of:
//!   - a file path (resolved within the meow sandbox), or
//!   - `tcp:HOST:PORT` — open a TCP socket and stream stdout to it.
//!
//! `&&`/`||` are left-associative with equal precedence, matching POSIX sh for
//! these two operators. There is no piping, no `;`, no globbing, no `$VARS` —
//! by design. This is "basic commands, nothing fancy".

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

use libakuma::{open, close, write_fd, open_flags, socket, connect, send, shutdown, socket_const, SocketAddrV4};

use super::context::resolve_path;
use super::mod_types::ToolResult;
use super::shell::{resolve_binary, spawn_and_collect, drain_child};

/// Connector that precedes a command in the chain.
#[derive(Clone, Copy, PartialEq)]
enum Connector {
    /// First command in the line — always runs.
    First,
    /// `&&` — run only if the previous command succeeded (exit 0).
    And,
    /// `||` — run only if the previous command failed (exit != 0).
    Or,
}

/// A single command plus an optional output redirect.
struct Command {
    connector: Connector,
    argv: Vec<String>,
    redirect: Option<Redirect>,
}

struct Redirect {
    target: String,
    append: bool,
}

/// One lexical token of the command line.
enum Tok {
    Word(String),
    And,
    Or,
    /// `>` (append = false) or `>>` (append = true).
    Redir(bool),
}

/// Entry point: lex, parse, then execute the chain.
pub fn run(line: &str) -> ToolResult {
    let toks = lex(line);
    if toks.is_empty() {
        return ToolResult::err("Empty command");
    }
    let commands = match parse(toks) {
        Ok(c) => c,
        Err(e) => return ToolResult::err(&e),
    };
    if commands.is_empty() {
        return ToolResult::err("Empty command");
    }
    execute(commands)
}

/// Quote-aware lexer that splits words and recognizes the `&&`, `||`, `>`, `>>`
/// operators even when they are adjacent to words (e.g. `echo hi>out`).
fn lex(line: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    let mut chars = line.chars().peekable();
    let flush = |cur: &mut String, toks: &mut Vec<Tok>| {
        if !cur.is_empty() {
            toks.push(Tok::Word(core::mem::take(cur)));
        }
    };

    while let Some(c) = chars.next() {
        if escape {
            cur.push(c);
            escape = false;
            continue;
        }
        if in_single {
            if c == '\'' { in_single = false; } else { cur.push(c); }
            continue;
        }
        if in_double {
            if c == '"' { in_double = false; } else { cur.push(c); }
            continue;
        }

        match c {
            '\\' => escape = true,
            '\'' => in_single = true,
            '"' => in_double = true,
            ' ' | '\t' => flush(&mut cur, &mut toks),
            '&' => {
                flush(&mut cur, &mut toks);
                if chars.peek() == Some(&'&') {
                    chars.next();
                    toks.push(Tok::And);
                }
                // A lone `&` (background) is unsupported; drop it silently.
            }
            '|' => {
                flush(&mut cur, &mut toks);
                if chars.peek() == Some(&'|') {
                    chars.next();
                    toks.push(Tok::Or);
                }
                // A lone `|` (pipe) is unsupported; drop it silently.
            }
            '>' => {
                flush(&mut cur, &mut toks);
                if chars.peek() == Some(&'>') {
                    chars.next();
                    toks.push(Tok::Redir(true));
                } else {
                    toks.push(Tok::Redir(false));
                }
            }
            _ => cur.push(c),
        }
    }
    flush(&mut cur, &mut toks);
    toks
}

/// Turn a token stream into a chain of `Command`s.
fn parse(toks: Vec<Tok>) -> Result<Vec<Command>, String> {
    let mut commands = Vec::new();
    let mut connector = Connector::First;
    let mut argv: Vec<String> = Vec::new();
    let mut redirect: Option<Redirect> = None;
    // When we just saw `>`/`>>`, the next Word is the redirect target.
    let mut pending_redir: Option<bool> = None;

    // Close out the command currently being built.
    fn finish(
        commands: &mut Vec<Command>,
        connector: Connector,
        argv: &mut Vec<String>,
        redirect: &mut Option<Redirect>,
    ) -> Result<(), String> {
        if argv.is_empty() {
            return Err(String::from("Syntax error: empty command near operator"));
        }
        commands.push(Command {
            connector,
            argv: core::mem::take(argv),
            redirect: redirect.take(),
        });
        Ok(())
    }

    for tok in toks {
        match tok {
            Tok::Word(w) => {
                if let Some(append) = pending_redir.take() {
                    redirect = Some(Redirect { target: w, append });
                } else {
                    argv.push(w);
                }
            }
            Tok::Redir(append) => {
                if pending_redir.is_some() {
                    return Err(String::from("Syntax error: redirect without target"));
                }
                pending_redir = Some(append);
            }
            Tok::And | Tok::Or => {
                if pending_redir.is_some() {
                    return Err(String::from("Syntax error: redirect without target"));
                }
                finish(&mut commands, connector, &mut argv, &mut redirect)?;
                connector = if matches!(tok, Tok::And) { Connector::And } else { Connector::Or };
            }
        }
    }
    if pending_redir.is_some() {
        return Err(String::from("Syntax error: redirect without target"));
    }
    // Trailing command (only if non-empty — a bare line yields nothing).
    if !argv.is_empty() {
        finish(&mut commands, connector, &mut argv, &mut redirect)?;
    } else if !commands.is_empty() {
        return Err(String::from("Syntax error: trailing operator"));
    }
    Ok(commands)
}

/// Run the parsed chain with short-circuit semantics, collecting a report.
fn execute(commands: Vec<Command>) -> ToolResult {
    let mut report = String::new();
    let mut last_exit: i32 = 0;
    let mut any_ran = false;

    for cmd in &commands {
        let should_run = match cmd.connector {
            Connector::First => true,
            Connector::And => last_exit == 0,
            Connector::Or => last_exit != 0,
        };
        if !should_run {
            continue;
        }
        any_ran = true;

        let binary = resolve_binary(&cmd.argv[0]);
        let args: Vec<&str> = cmd.argv[1..].iter().map(|s| s.as_str()).collect();

        match &cmd.redirect {
            // Redirected: open the sink up-front (POSIX truncates `>` before the
            // command runs) and stream the child's stdout straight into it — the
            // child's pipe is read in TOOL_BUFFER_SIZE chunks and written to the
            // file/socket fd, so nothing is buffered whole in meow's heap.
            Some(r) => {
                let mut sink = match Sink::open(&r.target, r.append) {
                    Ok(s) => s,
                    Err(e) => {
                        last_exit = 1;
                        report.push_str(&format!("[{} > {}] {}\n", cmd.argv[0], r.target, e));
                        continue;
                    }
                };
                let mut written = 0usize;
                let res = drain_child(&binary, &args, |chunk| {
                    sink.write_all(chunk).map(|n| written += n)
                });
                sink.close();
                match res {
                    Ok(exit_code) => {
                        last_exit = exit_code;
                        report.push_str(&format!(
                            "[{} {}> {}] piped {} bytes (exit {})\n",
                            cmd.argv[0], if r.append { ">" } else { "" }, r.target, written, exit_code
                        ));
                    }
                    Err(e) => {
                        last_exit = 1;
                        report.push_str(&format!("[{} > {}] {}\n", cmd.argv[0], r.target, e));
                    }
                }
            }
            // No redirect: collect stdout to return to the model.
            None => match spawn_and_collect(&binary, &args) {
                Ok((output, exit_code)) => {
                    last_exit = exit_code;
                    let s = core::str::from_utf8(&output).unwrap_or("<binary output>");
                    if !s.is_empty() {
                        report.push_str(s);
                        if !s.ends_with('\n') {
                            report.push('\n');
                        }
                    }
                }
                Err(e) => {
                    last_exit = 127;
                    report.push_str(&format!("[{}] {}\n", cmd.argv[0], e));
                }
            },
        }
    }

    if !any_ran {
        return ToolResult::ok(String::from("(no command ran — short-circuited)\nExit code: 0"));
    }

    let mut out = String::new();
    if report.is_empty() {
        out.push_str("(No output)\n");
    } else {
        out.push_str("stdout:\n```\n");
        out.push_str(&report);
        out.push_str("```\n");
    }
    out.push_str(&format!("Exit code: {}", last_exit));

    if last_exit == 0 {
        ToolResult::ok(out)
    } else {
        ToolResult { success: false, output: out }
    }
}

/// A streaming redirect backend: a TCP socket (`tcp:HOST:PORT`) or a file path
/// (resolved within the sandbox). Opened once, fed chunk-by-chunk as the child
/// produces output, then closed.
enum Sink {
    File(i32),
    Socket(i32),
}

impl Sink {
    /// Open the sink for `target`. For files this truncates (`>`) or seeks to
    /// end (`>>`) before any data is written — matching POSIX redirect order.
    fn open(target: &str, append: bool) -> Result<Sink, String> {
        if let Some(addr_str) = target.strip_prefix("tcp:") {
            let addr = SocketAddrV4::parse(addr_str)
                .ok_or_else(|| format!("invalid tcp address '{}' (want HOST:PORT)", addr_str))?;
            let fd = socket(socket_const::AF_INET, socket_const::SOCK_STREAM, 0);
            if fd < 0 {
                return Err(String::from("socket() failed"));
            }
            if connect(fd, &addr) < 0 {
                close(fd);
                return Err(format!("connect to {} failed", addr_str));
            }
            return Ok(Sink::Socket(fd));
        }

        let resolved = resolve_path(target)
            .ok_or_else(|| format!("'{}' is outside the sandbox", target))?;
        let flags = if append {
            open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_APPEND
        } else {
            open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC
        };
        let fd = open(&resolved, flags);
        if fd < 0 {
            return Err(format!("cannot open '{}' for writing", target));
        }
        Ok(Sink::File(fd))
    }

    /// Write a full chunk, looping until every byte is accepted. Returns the
    /// number of bytes written (== `data.len()` on success).
    fn write_all(&mut self, data: &[u8]) -> Result<usize, String> {
        let mut off = 0usize;
        while off < data.len() {
            let n = match self {
                Sink::File(fd) => write_fd(*fd, &data[off..]),
                Sink::Socket(fd) => send(*fd, &data[off..], 0),
            };
            if n <= 0 {
                return Err(format!("redirect write failed after {} bytes", off));
            }
            off += n as usize;
        }
        Ok(off)
    }

    fn close(self) {
        match self {
            Sink::File(fd) => { close(fd); }
            Sink::Socket(fd) => {
                let _ = shutdown(fd, socket_const::SHUT_WR);
                close(fd);
            }
        }
    }
}
