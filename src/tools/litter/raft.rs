//! Re-exports `litter-raft` (a sibling crate — see its `src/lib.rs`) rather
//! than defining the election state machine here. Extracted into its own
//! crate specifically so it can be `cargo test`-ed natively on macOS: it has
//! no libakuma dependency, unlike the rest of meow, which is `#![no_std]` end
//! to end and only runs under `aarch64-unknown-linux-musl` (in practice,
//! Docker's Linux VM). See `docs/LITTER_EXPERIMENT.md`.
//!
//! `run_tests` below is `meow test`'s in-binary suite for this module — a
//! condensed smoke check that the re-export actually wires up correctly, not
//! a restatement of `litter-raft`'s own (more exhaustive) native test suite.

pub use litter_raft::{ElectionState, Heartbeat, RequestVote, Role, VoteResponse};

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    use alloc::string::String;
    use alloc::format;
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- election tests ---\n");

    total += 1;
    {
        let mut s = ElectionState::new();
        s.start_election("a");
        let became_leader = s.on_vote_response(&VoteResponse { term: 1, voter: String::from("b"), granted: true }, 3);
        if became_leader && s.is_leader() && s.leader.as_deref() == Some("a") { passed += 1; }
        else { libakuma::print("  [!] 3-node majority election failed\n"); }
    }

    total += 1;
    {
        let mut s = ElectionState::new();
        s.start_election("a");
        s.on_vote_response(&VoteResponse { term: 1, voter: String::from("b"), granted: true }, 3);
        let was_leader = s.is_leader();
        let resp = s.on_request_vote(&RequestVote { term: 5, candidate: String::from("c") });
        if was_leader && s.role == Role::Follower && s.term == 5 && resp.granted { passed += 1; }
        else { libakuma::print("  [!] higher-term step-down failed\n"); }
    }

    total += 1;
    {
        let mut s = ElectionState::new();
        s.on_heartbeat(&Heartbeat { term: 1, leader: String::from("b") });
        if s.is_authorized("root-key", Some("root-key")) && s.is_authorized("b", None) && !s.is_authorized("c", None) {
            passed += 1;
        } else {
            libakuma::print("  [!] is_authorized (leader + root override) failed\n");
        }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}
