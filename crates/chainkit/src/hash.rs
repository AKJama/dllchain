//! SHA-256, the way dllchain recognises a file under any name.

use sha2::{Digest, Sha256};
use std::path::Path;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The file's SHA-256, or `None` when it is not there (or not a file).
pub fn file_sha256(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    std::fs::read(path).ok().map(|b| sha256_hex(&b))
}

/// The first 12 hex digits, for messages.
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// A 64-digit lowercase hex string.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_value() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(is_sha256(&sha256_hex(b"")));
        assert!(!is_sha256("ABC"));
    }
}
