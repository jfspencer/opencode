//! Identifiers (packages/schema/src/identifier.ts) and session slugs (packages/core/src/util/slug.ts).
//!
//! An identifier is 12 hex digits of `timestamp * 0x1000 + counter` (bitwise-negated for descending IDs, so newer
//! sorts first), then 14 random base-62 characters. The counter restarts every millisecond.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const CHARS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const RANDOM: usize = 14;

static LAST: Mutex<(u64, u64)> = Mutex::new((0, 0));

/// Milliseconds since the Unix epoch (`Date.now()`).
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn ascending() -> String {
    create(false, now() as u64)
}

pub fn descending() -> String {
    create(true, now() as u64)
}

fn create(descending: bool, timestamp: u64) -> String {
    let counter = {
        let mut last = LAST.lock().unwrap();
        if last.0 != timestamp {
            *last = (timestamp, 0);
        }
        last.1 += 1;
        last.1
    };
    encode(descending, timestamp, counter)
}

/// The identifier of one timestamp and counter, with fresh random characters.
fn encode(descending: bool, timestamp: u64, counter: u64) -> String {
    // JS BigInt arithmetic: the 48 low bits of the (possibly negated) value are what is printed.
    let current = (timestamp as u128) * 0x1000 + counter as u128;
    let value = if descending { !current } else { current };
    let mut out = String::with_capacity(12 + RANDOM);
    for index in 0..6 {
        let byte = (value >> (40 - 8 * index)) & 0xff;
        out.push_str(&format!("{byte:02x}"));
    }
    let mut bytes = [0u8; RANDOM];
    random(&mut bytes);
    out.extend(bytes.iter().map(|b| CHARS[(*b % 62) as usize] as char));
    out
}

pub fn random(buf: &mut [u8]) {
    getrandom::fill(buf).expect("system random source");
}

/// A random index below `n` (`Math.floor(Math.random() * n)`).
pub fn random_below(n: usize) -> usize {
    let mut bytes = [0u8; 8];
    random(&mut bytes);
    (u64::from_le_bytes(bytes) % n as u64) as usize
}

pub fn session() -> String {
    format!("ses_{}", descending())
}

pub fn event() -> String {
    format!("evt_{}", ascending())
}

const ADJECTIVES: &[&str] = &[
    "brave", "calm", "clever", "cosmic", "crisp", "curious", "eager", "gentle", "glowing", "happy", "hidden", "jolly",
    "kind", "lucky", "mighty", "misty", "neon", "nimble", "playful", "proud", "quick", "quiet", "shiny", "silent",
    "stellar", "sunny", "swift", "tidy", "witty",
];
const NOUNS: &[&str] = &[
    "cabin", "cactus", "canyon", "circuit", "comet", "eagle", "engine", "falcon", "forest", "garden", "harbor", "island",
    "knight", "lagoon", "meadow", "moon", "mountain", "nebula", "orchid", "otter", "panda", "pixel", "planet", "river",
    "rocket", "sailor", "squid", "star", "tiger", "wizard", "wolf",
];

pub fn slug() -> String {
    format!("{}-{}", ADJECTIVES[random_below(ADJECTIVES.len())], NOUNS[random_below(NOUNS.len())])
}

/// `crypto.randomUUID()`: a version 4 UUID.
pub fn uuid() -> String {
    let mut bytes = [0u8; 16];
    random(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

#[cfg(test)]
mod tests {
    #[test]
    fn layout() {
        // `encode`, not `create`: tests running in parallel share `create`'s per-millisecond counter.
        let a = super::encode(false, 1_767_225_600_000, 1);
        let b = super::encode(false, 1_767_225_600_000, 2);
        assert_eq!(a.len(), 26);
        assert!(a[..12] < b[..12]);
        let d = super::encode(true, 1_767_225_600_000, 1);
        let e = super::encode(true, 1_767_225_600_001, 1);
        assert!(e[..12] < d[..12]);
    }
}
