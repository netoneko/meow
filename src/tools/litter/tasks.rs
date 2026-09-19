//! Task memory with lease semantics — the coordinator's shared, on-disk task
//! table (see `live`). One file per task under `/litter/tasks/<id>.task`:
//!
//!     holder: sherlock      (or `-` while unassigned)
//!     until: 1730000000000  (lease expiry, µs; `0` = no lease)
//!     ---
//!     the actual task text
//!
//! This file table IS the replicated task state: it lives outside any
//! process, so a leadership takeover (or an operator inspecting the yard)
//! sees the exact same queue the old leader saw — pending tasks stay
//! pending, and leases whose worker died simply expire and get requeued.
//!
//! Completion protocol: the assigned agent writes `<id>.done` (its result
//! summary) with its ordinary FileWrite tool. The coordinator compacts each
//! done pair into `snapshot.log` and deletes both files, so the queue never
//! grows unbounded and a (re)joining agent streams one compact snapshot
//! instead of the full history.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libakuma::{open, close, read_fd, fstat, open_flags, read_dir};

/// How long an assignment may go without the task being finished. Generous
/// on purpose (an LLM turn can take minutes): a lease that expires while the
/// worker is alive just means the same task gets re-offered later — assume
/// good will, accept duplicate work on the margin.
pub const LEASE_US: u64 = 900 * 1_000_000;

#[derive(Debug, PartialEq)]
pub struct Task {
    /// File stem — the task id used in messages and the `.done` marker.
    pub id: String,
    pub holder: Option<String>,
    /// Lease expiry in µs since the epoch; `0` = unassigned/unleased.
    pub until: u64,
    pub body: String,
}

pub fn parse(id: &str, text: &str) -> Option<Task> {
    // Canonical form first; an operator-dropped bare text file (yard.sh
    // task) is a pending task with no header.
    let text = text.strip_prefix("\u{feff}").unwrap_or(text);
    if let Some(rest) = text.strip_prefix("holder: ") {
        let mut lines = rest.splitn(3, '\n');
        let holder_line = lines.next()?;
        let until_line = lines.next()?;
        let body = lines.next()?;
        let holder = holder_line.trim();
        let until = until_line.trim().strip_prefix("until: ")?.trim();
        return Some(Task {
            id: String::from(id),
            holder: if holder == "-" { None } else { Some(String::from(holder)) },
            until: until.parse::<u64>().unwrap_or(0),
            body: String::from(body),
        });
    }
    Some(Task { id: String::from(id), holder: None, until: 0, body: String::from(text) })
}

pub fn serialize(task: &Task) -> String {
    let holder = task.holder.as_deref().unwrap_or("-");
    format!("holder: {}\nuntil: {}\n---\n{}", holder, task.until, task.body)
}

pub fn is_leased(task: &Task, now_us: u64) -> bool {
    task.holder.is_some() && task.until > now_us
}

/// Everything the coordinator needs from one `read_dir` of the task table:
/// live tasks (parsed), plus the ids whose `.done` marker exists.
pub fn scan(dir: &str, now_us: u64) -> (Vec<Task>, Vec<String>) {
    let mut tasks = Vec::new();
    let mut done = Vec::new();
    let entries = match read_dir(dir) {
        Some(e) => e,
        None => return (tasks, done),
    };
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        if let Some(id) = entry.name.strip_suffix(".task") {
            if let Some(text) = read_small_file(&format!("{}/{}", dir, entry.name)) {
                if let Some(t) = parse(id, &text) {
                    tasks.push(t);
                }
            }
        } else if let Some(id) = entry.name.strip_suffix(".done") {
            done.push(String::from(id));
        }
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    done.sort();
    let _ = now_us;
    (tasks, done)
}

/// Compact one completed task into a snapshot line — the whole history a
/// (re)joining agent needs, not the full transcript.
pub fn snapshot_line(id: &str, summary: &str) -> String {
    let mut line = String::from(summary.trim());
    if line.len() > 160 {
        let mut cut = 160;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push('…');
    }
    format!("[done: {}] {}\n", id, line)
}

fn read_small_file(path: &str) -> Option<String> {
    let fd = open(path, open_flags::O_RDONLY);
    if fd < 0 {
        return None;
    }
    let stat = match fstat(fd) {
        Ok(s) => s,
        Err(_) => {
            close(fd);
            return None;
        }
    };
    let size = stat.st_size as usize;
    if size == 0 || size > 32 * 1024 {
        close(fd);
        return None;
    }
    let mut buf = alloc::vec![0u8; size];
    let n = read_fd(fd, &mut buf);
    close(fd);
    if n <= 0 {
        return None;
    }
    String::from_utf8(buf[..n as usize].to_vec()).ok()
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter tasks tests ---\n");

    // canonical file round-trip
    total += 1;
    {
        let t = Task { id: String::from("t1"), holder: Some(String::from("sherlock")), until: 42, body: String::from("audit main.rs") };
        let back = parse("t1", &serialize(&t));
        let ok = matches!(&back, Some(b) if *b == t);
        if ok { passed += 1; } else { libakuma::print(&format!("  [!] round-trip: {:?}\n", back)); }
    }

    // operator-dropped bare text parses as an unassigned pending task
    total += 1;
    {
        let t = parse("t2", "just do the thing\nwith two lines");
        let ok = matches!(&t, Some(b) if b.holder.is_none() && b.until == 0 && b.body.contains("two lines"));
        if ok { passed += 1; } else { libakuma::print(&format!("  [!] bare text: {:?}\n", t)); }
    }

    // lease expiry arithmetic
    total += 1;
    {
        let leased = Task { id: String::from("t"), holder: Some(String::from("a")), until: 100, body: String::new() };
        let expired = Task { id: String::from("t"), holder: Some(String::from("a")), until: 50, body: String::new() };
        let unassigned = Task { id: String::from("t"), holder: None, until: 100, body: String::new() };
        if is_leased(&leased, 99) && !is_leased(&expired, 50) && !is_leased(&unassigned, 1) { passed += 1; }
        else { libakuma::print("  [!] lease expiry logic wrong\n"); }
    }

    // snapshot lines are bounded and newline-terminated
    total += 1;
    {
        let long = snapshot_line("t3", &"x".repeat(400));
        if long.starts_with("[done: t3] ") && long.ends_with("…\n") && long.len() < 200 { passed += 1; }
        else { libakuma::print(&format!("  [!] snapshot_line: {}\n", long)); }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}
