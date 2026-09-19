//! `meow litter observe`: a read-only, human-facing transcript view across
//! every participant's inbox — not a tool the LLM calls, a CLI convenience
//! for the operator watching a debate happen. Rendered with a small colored
//! ASCII avatar per message so a scroll of the whole litter's back-and-forth
//! stays readable at a glance: one ANSI color per sender, reused every time
//! that sender speaks again.
//!
//! `AVATAR` is a local copy of the top-level `src/akuma_20.txt` — the
//! smallest of the four sizes there (`akuma_{20,40,79,120}.txt`), chosen
//! specifically because a per-message avatar needs to stay small next to the
//! actual text; `akuma_40.txt` (already vendored here, unused before this)
//! is the "banner" size `sshd`/`amd64` use for a one-time login/boot splash,
//! not something you want repeated once per chat message. Kept as a local
//! copy rather than reaching across the source tree, same reasoning as
//! `sshd/src/protocol.rs`'s own `akuma_40.txt` comment.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const AVATAR: &str = include_str!("../../akuma_20.txt");

/// A fixed, small palette of standard ANSI foreground colors, bright variants
/// first — the avatar art is mostly `%`/`#`/`*`/`+` glyphs, which read badly
/// in a dim color on a dark terminal. Cycles once every 8 distinct senders; a
/// litter bigger than that repeats colors rather than failing.
const COLORS: [u8; 8] = [91, 92, 93, 94, 95, 96, 31, 34];

/// Assigns each sender name the same color every time it's seen, in
/// first-seen order — deterministic, so re-running `observe` against an
/// unchanged mailbox prints identically rather than reshuffling colors.
pub struct ColorAssigner {
    seen: Vec<String>,
}

impl ColorAssigner {
    pub fn new() -> Self {
        Self { seen: Vec::new() }
    }

    pub fn color_for(&mut self, name: &str) -> u8 {
        if let Some(i) = self.seen.iter().position(|n| n == name) {
            return COLORS[i % COLORS.len()];
        }
        self.seen.push(String::from(name));
        COLORS[(self.seen.len() - 1) % COLORS.len()]
    }
}

impl Default for ColorAssigner {
    fn default() -> Self {
        Self::new()
    }
}

/// One message as `observe` merges and prints it — transport-agnostic,
/// built from either the filesystem mailbox's `LitterMessage` or the hub's
/// `litter_wire::Message` (dropping the hub's `ts`: see `merge_chronological`
/// for why round order is what this command promises, not wall-clock order).
pub struct Entry {
    pub from: String,
    pub round: i64,
    pub body: String,
}

/// Stable sort by `round` (the debate's own unit of progress, present in
/// both transports) and then by sender name. Neither transport gives every
/// message a wall clock that's comparable across *all* senders — the
/// filesystem mailbox has none at all, and the hub's `ts` is hub-local but
/// dropped here anyway so both transports produce the same order for the
/// same conversation, which matters more for a human-facing view than a
/// truer-but-transport-dependent ordering would.
pub fn merge_chronological(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by(|a, b| a.round.cmp(&b.round).then_with(|| a.from.cmp(&b.from)));
    entries
}

/// Renders one entry: the colored avatar art, then `from [round N]: body`.
pub fn render(entry: &Entry, color: u8) -> String {
    let mut out = String::new();
    for line in AVATAR.lines() {
        out.push_str(&format!("\x1b[{}m{}\x1b[0m\n", color, line));
    }
    out.push_str(&format!("\x1b[{}m{}\x1b[0m [round {}]: {}\n\n", color, entry.from, entry.round, entry.body));
    out
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter observe tests ---\n");

    // ColorAssigner: stable per name, distinct across different names.
    total += 1;
    {
        let mut c = ColorAssigner::new();
        let a1 = c.color_for("sherlock");
        let b1 = c.color_for("hercules");
        let a2 = c.color_for("sherlock");
        if a1 == a2 && a1 != b1 {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] color assignment not stable/distinct: a1={} b1={} a2={}\n", a1, b1, a2));
        }
    }

    // ColorAssigner: cycles rather than panicking past the palette size.
    total += 1;
    {
        let mut c = ColorAssigner::new();
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
        let colors: Vec<u8> = names.iter().map(|n| c.color_for(n)).collect();
        if colors[0] == colors[8] && colors[1] == colors[9] {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] color cycling wrong: {:?}\n", colors));
        }
    }

    // merge_chronological: sorts by round, then by sender name within a round.
    total += 1;
    {
        let entries = alloc::vec![
            Entry { from: String::from("hercules"), round: 1, body: String::from("b") },
            Entry { from: String::from("sherlock"), round: 0, body: String::from("a") },
            Entry { from: String::from("ressler"), round: 1, body: String::from("c") },
        ];
        let sorted = merge_chronological(entries);
        let order: Vec<&str> = sorted.iter().map(|e| e.from.as_str()).collect();
        if order == ["sherlock", "hercules", "ressler"] {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] merge order wrong: {:?}\n", order));
        }
    }

    // render: includes the avatar art, the color codes, and the message text.
    total += 1;
    {
        let entry = Entry { from: String::from("sherlock"), round: 2, body: String::from("the game is afoot") };
        let out = render(&entry, 91);
        if out.contains("\x1b[91m") && out.contains("\x1b[0m") && out.contains("[round 2]: the game is afoot") {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] render missing expected pieces: {:?}\n", out));
        }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}
