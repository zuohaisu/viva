//! Content hashing for native memory facts (NFC + trim, SHA-256 hex lower).

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// SHA-256 over NFC-normalized, trim-normalized body text; lowercase hex.
pub fn content_hash(content: &str) -> String {
    let normalized: String = content.trim().nfc().collect();
    let digest = Sha256::digest(normalized.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
