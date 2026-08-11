//! Reading JSON, over the `picojson` pull parser.
//!
//! Every hand-rolled JSON scanner meow had before this searched for `"key"`
//! as a substring and returned whatever came after it. That is a substring
//! search wearing a parser's name: it matches a key at *any* depth (a chat
//! chunk's top-level `"id"` and a tool call's nested `"id"` are the same
//! lookup to `str::find`), and a brace inside a string value can end an
//! object early. `picojson` is a real tokenizer — `no_std`, no allocation of
//! its own, no recursion — and everything here is a path-addressed view on
//! top of it. Same design as `userspace/box/src/json.rs`; not shared as a
//! crate because meow and box are built as separate standalone workspaces
//! with no dependency between them today.
//!
//! Values are addressed by the path that leads to them, so
//! `["choices", "0", "delta", "content"]` reaches only the first choice's
//! delta text, never a same-named field nested somewhere else:
//!
//! ```text
//! {"choices": [{"delta": {"content": "hi"}}]}   →  path ["choices", 0, "delta", "content"] = "hi"
//! ```
//!
//! The parser needs a scratch buffer only to un-escape strings; this module
//! allocates one as large as the document, which is always enough, since an
//! un-escaped string is never longer than its escaped form.

use alloc::string::String;
use alloc::vec::Vec;
use picojson::{Event, PullParser, SliceParser};

pub use picojson::ParseError;

/// One step of the path to a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    /// An object member name.
    Key(String),
    /// A position in an array.
    Index(usize),
}

impl Seg {
    /// Whether a pattern segment selects this one. `*` selects any segment; a
    /// decimal pattern selects that array position; anything else is a literal
    /// key.
    fn matched_by(&self, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        match self {
            Self::Key(k) => k == pattern,
            Self::Index(i) => pattern.parse::<usize>().is_ok_and(|p| p == *i),
        }
    }
}

/// Where the value currently being visited sits in the document, outermost
/// first. Empty for a value at the document root.
pub struct Path {
    segs: Vec<Seg>,
}

impl Path {
    /// Whether this path is exactly `pattern` — same length, segment by
    /// segment.
    pub fn matches(&self, pattern: &[&str]) -> bool {
        self.segs.len() == pattern.len()
            && self.segs.iter().zip(pattern).all(|(s, p)| s.matched_by(p))
    }

    pub fn segments(&self) -> &[Seg] {
        &self.segs
    }
}

/// A JSON value, as seen by a visitor.
///
/// Numbers are integers only: the parser is built with float support off
/// (`float-skip`), which reports any float it meets as [`Value::Other`]
/// rather than failing the whole document.
#[derive(Debug, PartialEq)]
pub enum Value<'a> {
    Str(&'a str),
    Int(i64),
    Bool(bool),
    Null,
    /// The `{` of an object or the `[` of an array. Reported so a caller can
    /// tell an empty array apart from a missing one.
    StartObject,
    StartArray,
    /// A number that is not an integer this build can represent.
    Other,
}

/// Which container the walk is currently inside, and whether it has already
/// pushed a segment onto the path for the member being read.
struct Frame {
    array: bool,
    next_index: usize,
    seg_pushed: bool,
}

/// A value inside an array takes the next position; one inside an object was
/// already named by its key, and one at the document root has no segment.
fn enter_array_element(path: &mut Path, stack: &mut [Frame]) {
    if let Some(frame) = stack.last_mut() {
        if frame.array {
            if frame.seg_pushed {
                path.segs.pop();
            }
            path.segs.push(Seg::Index(frame.next_index));
            frame.next_index += 1;
            frame.seg_pushed = true;
        }
    }
}

/// Walk `doc`, calling `visit` for every value with the path that leads to it.
///
/// One pass, in document order, no allocation beyond the path and the parser's
/// scratch buffer. Callers accumulate what they need as it goes by.
pub fn walk<F>(doc: &str, mut visit: F) -> Result<(), ParseError>
where
    F: FnMut(&Path, Value<'_>),
{
    // Only escaped strings are copied here; the parser borrows from `doc`
    // otherwise. Sizing it to the document makes `ScratchBufferFull`
    // unreachable.
    let mut scratch = alloc::vec![0u8; doc.len() + 16];
    let mut parser = SliceParser::with_buffer(doc, &mut scratch);

    let mut path = Path { segs: Vec::new() };
    let mut stack: Vec<Frame> = Vec::new();

    loop {
        let event = parser.next_event()?;

        match event {
            Event::EndDocument => break,

            Event::Key(k) => {
                if let Some(frame) = stack.last_mut() {
                    if frame.seg_pushed {
                        path.segs.pop();
                    }
                    frame.seg_pushed = true;
                }
                path.segs.push(Seg::Key(String::from(k.as_str())));
            }

            Event::StartObject | Event::StartArray => {
                let array = event == Event::StartArray;
                enter_array_element(&mut path, &mut stack);
                visit(
                    &path,
                    if array {
                        Value::StartArray
                    } else {
                        Value::StartObject
                    },
                );
                stack.push(Frame {
                    array,
                    next_index: 0,
                    seg_pushed: false,
                });
            }

            Event::EndObject | Event::EndArray => {
                if let Some(frame) = stack.pop() {
                    if frame.seg_pushed {
                        path.segs.pop();
                    }
                }
            }

            Event::String(s) => {
                enter_array_element(&mut path, &mut stack);
                visit(&path, Value::Str(s.as_str()));
            }
            Event::Number(n) => {
                let value = n.as_int().map_or(Value::Other, Value::Int);
                enter_array_element(&mut path, &mut stack);
                visit(&path, value);
            }
            Event::Bool(b) => {
                enter_array_element(&mut path, &mut stack);
                visit(&path, Value::Bool(b));
            }
            Event::Null => {
                enter_array_element(&mut path, &mut stack);
                visit(&path, Value::Null);
            }
        }
    }

    Ok(())
}

/// The string at `pattern`, or `None` if it is absent or not a string. The
/// first match wins.
pub fn string_at(doc: &str, pattern: &[&str]) -> Option<String> {
    let mut found = None;
    let _ = walk(doc, |path, value| {
        if found.is_none() {
            if let Value::Str(s) = value {
                if path.matches(pattern) {
                    found = Some(String::from(s));
                }
            }
        }
    });
    found
}

/// Every string matching `pattern`, in document order — `["Cmd", "*"]` for an
/// array of strings, `["data", "*", "id"]` across array elements.
pub fn strings_at(doc: &str, pattern: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let _ = walk(doc, |path, value| {
        if let Value::Str(s) = value {
            if path.matches(pattern) {
                found.push(String::from(s));
            }
        }
    });
    found
}

/// The integer at `pattern`.
pub fn number_at(doc: &str, pattern: &[&str]) -> Option<i64> {
    let mut found = None;
    let _ = walk(doc, |path, value| {
        if found.is_none() {
            if let Value::Int(i) = value {
                if path.matches(pattern) {
                    found = Some(i);
                }
            }
        }
    });
    found
}

/// Whether anything at all sits at `pattern` — including an empty array or
/// object, which the `*_at` accessors cannot report.
pub fn exists(doc: &str, pattern: &[&str]) -> bool {
    let mut seen = false;
    let _ = walk(doc, |path, _| {
        if path.matches(pattern) {
            seen = true;
        }
    });
    seen
}

// meow has no host-testable lib half (unlike `box`/`sshd`, see CLAUDE.md §
// Testing), so these run in-binary via `meow test` like every other module's
// `run_tests`, not as `#[cfg(test)]` host unit tests.
#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    use alloc::format;
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- json tests ---\n");

    macro_rules! check {
        ($desc:expr, $got:expr, $want:expr) => {
            total += 1;
            let got = $got;
            let want = $want;
            if got == want { passed += 1; }
            else { libakuma::print(&format!("  [!] {}: got {:?} want {:?}\n", $desc, got, want)); }
        };
    }

    check!(
        "a key only matches at its own depth",
        string_at(
            r#"{"container": {"id": "wrong"}, "choices": [{"delta": {"content": "right"}}]}"#,
            &["choices", "0", "delta", "content"]
        ),
        Some(String::from("right"))
    );
    check!(
        "unrooted key does not match",
        string_at(r#"{"choices": [{"delta": {"content": "right"}}]}"#, &["content"]),
        None
    );
    check!(
        "braces/brackets inside strings are not structure",
        string_at(r#"{"choices": [{"delta": {"content": "echo }{ ][" }}]}"#, &["choices", "0", "delta", "content"]),
        Some(String::from("echo }{ ]["))
    );
    check!(
        "unescapes strings including unicode",
        string_at(r#"{"summary": "he said \"hié\""}"#, &["summary"]),
        Some(String::from("he said \"hié\""))
    );
    check!(
        "strings_at collects every match in document order",
        strings_at(r#"{"data": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}"#, &["data", "*", "id"]),
        alloc::vec![String::from("a"), String::from("b"), String::from("c")]
    );
    check!(
        "exists sees an empty array",
        exists(r#"{"data": []}"#, &["data"]),
        true
    );
    check!(
        "exists is false for an absent path",
        exists(r#"{"other": 1}"#, &["data"]),
        false
    );
    check!(
        "number_at rejects the wrong type",
        number_at(r#"{"start": 5, "name": "x"}"#, &["name"]),
        None
    );
    check!(
        "number_at reads an integer",
        number_at(r#"{"start": 5, "name": "x"}"#, &["start"]),
        Some(5)
    );
    check!(
        "malformed documents are reported, not panicked on",
        walk(r#"{"a": }"#, |_, _| {}).is_err(),
        true
    );

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}
