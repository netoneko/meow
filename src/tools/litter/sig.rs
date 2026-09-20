//! Relay-plane cryptography (`docs/LITTER_RELAY_TOPOLOGY.md`'s envelope):
//! Ed25519 signatures over a canonical payload, so a relayed message
//! carries provable provenance — who originally said it (`ol`/`ot`/`sig`)
//! and who carried it here (`rl`/`rs`).
//!
//! Dedup is NOT a checksum over the body: the hub's seen-set keys on the
//! `(ol, ot, rl)` metadata alone (origin stamps are strictly monotonic per
//! hub, so origin + origin-ts identifies a message, and `rl == us` marks
//! our own echo returning). Hashing the body would buy nothing the pair
//! does not already give and would make the table carry payload.
//!
//! A key is an **agent's** identity, not a litter's: every meow scope has
//! its own `litter_key` and signs its own words with it. A litter is not a
//! signing party — `ol` names the swarm a message came from, `sig` names
//! the agent that said it, and `rs` names the agent that carried it.
//!
//! Key material comes from the config: `litter_key` (this agent's 32-byte
//! Ed25519 seed, hex) and `litter_peer_keys` (`name:<64-hex-pubkey>,...`),
//! which is a **guest list, not a name binding** — see `verify_payload`,
//! and note that leaving it unset accepts everyone. Both are set once at
//! startup; the sim swaps them per-litter (single-threaded by design —
//! same convention as the other litter statics).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

use litter_wire::Request;

static mut OUR_KEY: Option<SigningKey> = None;
static mut PEER_KEYS: Option<Vec<(String, VerifyingKey)>> = None;

/// Called once at startup from `Config::litter_key`.
pub fn set_our_key(hex_seed: Option<&str>) {
    let key = hex_seed.and_then(decode_hex).and_then(|b| {
        let bytes: [u8; 32] = b.try_into().ok()?;
        SigningKey::from_bytes(&bytes).into()
    });
    unsafe { *core::ptr::addr_of_mut!(OUR_KEY) = key };
}

/// Called once at startup from `Config::litter_peer_keys`
/// (`name:<64-hex-pubkey>,...` — unparsable entries are dropped, loudly).
pub fn set_peer_keys(spec: Option<&str>) {
    let mut keys = Vec::new();
    if let Some(spec) = spec {
        for entry in spec.split(',') {
            let entry = entry.trim();
            if let Some((name, hex)) = entry.split_once(':') {
                match decode_hex(hex).and_then(|b| {
                    let bytes: [u8; 32] = b.try_into().ok()?;
                    VerifyingKey::from_bytes(&bytes).ok()
                }) {
                    Some(vk) => keys.push((String::from(name.trim()), vk)),
                    None => libakuma::print(&format!("litter: dropping unparsable peer key for '{}'\n", name)),
                }
            }
        }
    }
    unsafe { *core::ptr::addr_of_mut!(PEER_KEYS) = Some(keys) };
}

pub fn our_key() -> Option<SigningKey> {
    unsafe { (*core::ptr::addr_of!(OUR_KEY)).clone() }
}

fn peer_keys() -> Option<&'static Vec<(String, VerifyingKey)>> {
    unsafe {
        let ptr = core::ptr::addr_of!(PEER_KEYS);
        (*ptr).as_ref()
    }
}

/// This litter's configured identity (envelope `ol`/`rl` values).
pub fn our_litter_name() -> Option<String> {
    super::litter_name()
}

/// Generate a fresh Ed25519 seed (32 bytes from the kernel RNG), hex-encoded
/// for the config. Used when `litter_key` is absent: the key is generated
/// once and persisted, not rotated per boot — peers pin our public key.
pub fn generate_seed() -> Option<String> {
    let mut bytes = [0u8; 32];
    match libakuma::getrandom(&mut bytes) {
        Ok(n) if n == 32 => Some(hex_encode(&bytes)),
        _ => None,
    }
}

/// The canonical bytes every relay signature commits to: origin litter,
/// bare sender, addressed target, body, origin ts. Both the origin
/// signature (`sig`) and the relayer signature (`rs`) sign exactly this —
/// a recipient rebuilds it from the wire fields and verifies both.
pub fn relay_payload(origin_litter: &str, from: &str, to: &str, body: &str, origin_ts: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"litter-relay-v3\x00");
    p.extend_from_slice(origin_litter.as_bytes());
    p.push(0);
    p.extend_from_slice(from.as_bytes());
    p.push(0);
    p.extend_from_slice(to.as_bytes());
    p.push(0);
    p.extend_from_slice(body.as_bytes());
    p.push(0);
    p.extend_from_slice(&origin_ts.to_be_bytes());
    p
}

/// Build the outbound `Send` for a locally-originated message, signed with
/// **this agent's** key. The signature says "this agent said this", and it
/// is the only signature that ever claims that: a hub relaying the message
/// later adds its own `rs` beside it and never touches `sig`.
///
/// `ot` is the sender's clock, not the hub's. The signed payload has to
/// contain the timestamp, so it must exist before the message reaches a
/// hub — the hub's own `stamp()` still orders its history, independently.
///
/// Falls back to an unsigned send when no litter identity or key is
/// configured: relay is off, and litter-local traffic never needed one.
pub fn signed_send(from: &str, to: &str, body: &str, round: i64) -> Request {
    let plain = || Request::send(String::from(from), String::from(to), String::from(body), round);
    let Some(litter) = our_litter_name() else { return plain() };
    let ot = crate::util::now_us();
    let payload = relay_payload(&litter, from, to, body, ot);
    match sign_payload(&payload) {
        Some(sig) => Request::signed(String::from(from), String::from(to), String::from(body), round, litter, ot, sig),
        None => plain(),
    }
}

/// Sign the canonical payload with this agent's key, hex-encoded for the
/// wire. `None` when no key is configured — relay is off.
pub fn sign_payload(payload: &[u8]) -> Option<String> {
    let key = our_key()?;
    Some(hex_encode(&key.sign(payload).to_bytes()))
}

/// Verify a wire signature against the configured peer keys.
///
/// Deliberately permissive — this ships to a LAN, not a hostile network:
///
/// - **No keys configured at all ⇒ accept any well-formed signature.** An
///   unset `litter_peer_keys` means "everyone is welcome", and this is the
///   default: two fresh litters join by pointing at each other and nothing
///   else. Note what this mode can and cannot do — Ed25519 verification
///   needs the signer's public key, and the envelope does not carry one,
///   so with an empty guest list there is simply nothing to check the
///   signature *against*. All that remains is the structural check (64
///   bytes of hex), which catches a truncated or corrupted frame — the
///   realistic LAN failure — and catches nothing else. The signature still
///   travels, so pinning a key later verifies traffic retroactively.
/// - **Keys configured ⇒ any of them may sign, for any name.** The key
///   set is a guest list, not a name-to-key binding: a pinned key that
///   verifies the payload is enough, whatever `litter` claims to be. Tying
///   a key to a name buys spoofing resistance we are not paying for yet.
pub fn verify_payload(_litter: &str, payload: &[u8], sig_hex: &str) -> bool {
    let Some(bytes) = decode_hex(sig_hex) else { return false };
    let Ok(arr) = <[u8; 64]>::try_from(bytes) else { return false };
    let sig = ed25519_dalek::Signature::from_bytes(&arr);
    match peer_keys() {
        // Unset (or never initialized): well-formed is all we can ask for.
        None => true,
        Some(keys) if keys.is_empty() => true,
        Some(keys) => keys.iter().any(|(_, vk)| vk.verify(payload, &sig).is_ok()),
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}
