//! Streaming redaction for Viva-written journals, logs and briefs
//! (V04, issue #13).
//!
//! Semantics:
//! - Configured secret values never land in Viva's own persisted output —
//!   including when a secret is split across output chunks. The streaming
//!   redactor carries a tail buffer across `push` calls so a chunk boundary
//!   cannot leak half a secret.
//! - Structured JSON is scrubbed value-wise before serialization.
//! - The redactor only ever transforms text that passes through it. It has
//!   no API that rewrites already-existing user files — original user
//!   material is never modified.

use serde_json::Value;

/// A streaming redactor over a configured secret set.
///
/// Typical use: `for chunk in stream { safe_out.push(redactor.push(chunk)) }`
/// then `safe_out.push(redactor.flush())`.
#[derive(Debug, Clone)]
pub struct Redactor {
    secrets: Vec<String>,
    replacement: String,
    /// Carried tail from the previous chunk: up to `max_secret_len - 1`
    /// bytes that might be the beginning of a secret split at the boundary.
    carry: String,
}

impl Redactor {
    /// Build a redactor over the secret values. Empty secrets are ignored;
    /// the default replacement keeps a shape hint without leaking content.
    pub fn new<I, S>(secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let secrets: Vec<String> = secrets
            .into_iter()
            .map(Into::into)
            .filter(|s| !s.is_empty())
            .collect();
        Self {
            secrets,
            replacement: "[REDACTED]".into(),
            carry: String::new(),
        }
    }

    pub fn with_replacement(mut self, replacement: impl Into<String>) -> Self {
        self.replacement = replacement.into();
        self
    }

    /// The configured replacement marker.
    pub fn marker(&self) -> &str {
        &self.replacement
    }

    /// Push one chunk through; returns text that is safe to emit. A secret
    /// split across chunks is still caught — the unemittable tail is kept
    /// until more text arrives or [`Redactor::flush`] is called.
    pub fn push(&mut self, chunk: &str) -> String {
        let mut working = std::mem::take(&mut self.carry);
        working.push_str(chunk);
        let redacted = self.redact_str(&working);
        // After redaction the only remaining risk is a *partial* secret at
        // the end of `working` that would complete with the next chunk.
        // Hold back the longest suffix that could be a secret prefix.
        let hold = self.partial_suffix_len(&redacted);
        let emit_at = redacted.len().saturating_sub(hold);
        // Never split a UTF-8 scalar point at the boundary.
        let mut emit_at = emit_at;
        while emit_at < redacted.len() && !redacted.is_char_boundary(emit_at) {
            emit_at -= 1;
        }
        self.carry = redacted[emit_at..].to_string();
        redacted[..emit_at].to_string()
    }

    /// End of stream: emit everything still carried.
    pub fn flush(&mut self) -> String {
        std::mem::take(&mut self.carry)
    }

    /// Scrub a structured JSON value: every string field that equals a
    /// configured secret is replaced; longer strings have occurrences
    /// replaced in place.
    pub fn scrub_json(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.redact_str(text)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.scrub_json(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.scrub_json(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// One-shot redaction of a complete string (no streaming semantics).
    pub fn redact_str(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            if secret.is_empty() {
                continue;
            }
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), &self.replacement);
            }
        }
        out
    }

    /// Length of the longest suffix of `text` that is a proper prefix of any
    /// configured secret (and could therefore complete with more input).
    fn partial_suffix_len(&self, text: &str) -> usize {
        let bytes = text.as_bytes();
        let mut best = 0;
        for secret in &self.secrets {
            let max = secret.len().saturating_sub(1).min(bytes.len());
            for take in (1..=max).rev() {
                if text.is_char_boundary(text.len() - take)
                    && secret.as_bytes().starts_with(&bytes[bytes.len() - take..])
                {
                    best = best.max(take);
                    break;
                }
            }
        }
        best
    }
}

/// A `std::io::Write` adapter that redacts before writing. Use it as the
/// sink for Viva-written journal/log/brief output; it appends through
/// itself and never rewrites an existing user file.
pub struct RedactingWriter<W: std::io::Write> {
    inner: W,
    redactor: Redactor,
}

/// Byte-level streaming redactor for raw terminal byte streams.
///
/// The terminal disk log must keep the byte-exact ANSI stream (dropping
/// escape bytes would fake the transcript), so redaction there happens at
/// the byte level: configured secrets are matched as byte patterns with a
/// carry buffer, so a secret split across read chunks is still replaced.
/// Everything else — including all escape sequences — passes through
/// byte-for-byte.
#[derive(Debug, Clone)]
pub struct ByteRedactor {
    secrets: Vec<Vec<u8>>,
    replacement: Vec<u8>,
    carry: Vec<u8>,
}

impl ByteRedactor {
    pub fn new<I, S>(secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Vec<u8>>,
    {
        let secrets: Vec<Vec<u8>> = secrets
            .into_iter()
            .map(Into::into)
            .filter(|s| !s.is_empty())
            .collect();
        Self {
            secrets,
            replacement: b"[REDACTED]".to_vec(),
            carry: Vec::new(),
        }
    }

    /// Longest secret length; the carry bound for split-pattern safety.
    fn max_secret_len(&self) -> usize {
        self.secrets.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Push raw bytes; returns bytes safe to append to the log.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut working = std::mem::take(&mut self.carry);
        working.extend_from_slice(chunk);
        let mut out = Vec::with_capacity(working.len());
        let mut pos = 0;
        'scan: while pos < working.len() {
            for secret in &self.secrets {
                if working[pos..].starts_with(secret) {
                    out.extend_from_slice(&self.replacement);
                    pos += secret.len();
                    continue 'scan;
                }
            }
            out.push(working[pos]);
            pos += 1;
        }
        // Hold back a tail that could be the start of a split secret.
        let max = self.max_secret_len().saturating_sub(1).min(out.len());
        let keep = max;
        self.carry = out[out.len().saturating_sub(keep)..].to_vec();
        out.truncate(out.len() - keep);
        out
    }

    /// End of stream: emit everything still carried.
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.carry)
    }
}

impl<W: std::io::Write> RedactingWriter<W> {
    pub fn new(inner: W, redactor: Redactor) -> Self {
        Self { inner, redactor }
    }

    pub fn into_inner(mut self) -> std::io::Result<W> {
        let tail = self.redactor.flush();
        if !tail.is_empty() {
            self.inner.write_all(tail.as_bytes())?;
        }
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: std::io::Write> std::io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = std::str::from_utf8(buf).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "non-utf8 log chunk")
        })?;
        let safe = self.redactor.push(text);
        if !safe.is_empty() {
            self.inner.write_all(safe.as_bytes())?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let tail = self.redactor.flush();
        if !tail.is_empty() {
            self.inner.write_all(tail.as_bytes())?;
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn simple_secret_is_replaced() {
        let mut redactor = Redactor::new(vec!["hunter2"]);
        let mut out = redactor.push("password is hunter2, ok?\n");
        out.push_str(&redactor.flush());
        assert!(!out.contains("hunter2"), "got: {out}");
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn secret_split_across_chunks_never_leaks() {
        let secret = "SUPERSECRET123";
        let mut redactor = Redactor::new(vec![secret]);
        let mut emitted = String::new();
        // Cut the secret at every possible boundary, one char at a time.
        for split in 0..=secret.len() {
            let first = &secret[..split];
            let second = &secret[split..];
            emitted.clear();
            emitted.push_str(&redactor.push(first));
            emitted.push_str(&redactor.push(second));
            emitted.push_str(&redactor.push(" tail"));
            emitted.push_str(&redactor.flush());
            assert!(
                !emitted.contains(secret),
                "split at {split} leaked the secret: {emitted}"
            );
            // The visible tail after the secret must survive.
            assert!(emitted.ends_with(" tail"), "split at {split}: {emitted}");
        }
    }

    #[test]
    fn multi_byte_secret_split_across_chunks_never_leaks() {
        // A multi-byte secret (Chinese) split at byte boundaries.
        let secret = "密钥令牌值";
        let bytes = secret.as_bytes();
        let mut redactor = Redactor::new(vec![secret]);
        let mut emitted = String::new();
        for split in 0..=bytes.len() {
            if !secret.is_char_boundary(split) {
                continue; // only legal char-boundary splits can be emitted
            }
            let (first, second) = (&bytes[..split], &bytes[split..]);
            let first = std::str::from_utf8(first).unwrap_or("");
            let second = std::str::from_utf8(second).unwrap_or("");
            emitted.clear();
            emitted.push_str(&redactor.push(first));
            emitted.push_str(&redactor.push(second));
            emitted.push_str(&redactor.flush());
            assert!(
                !emitted.contains(secret),
                "split at {split} leaked the secret: {emitted}"
            );
        }
    }

    #[test]
    fn redacting_writer_scrubs_persisted_output() {
        let redactor = Redactor::new(vec!["token-abc123"]);
        let mut sink: Vec<u8> = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut sink, redactor);
            writer
                .write_all(b"launching with token-abc")
                .expect("write 1");
            writer.write_all(b"123 completed\n").expect("write 2");
            writer.flush().expect("flush");
        }
        let text = String::from_utf8(sink).expect("utf8");
        assert!(!text.contains("token-abc123"), "leaked: {text}");
        assert!(text.contains("[REDACTED] completed"), "got: {text}");
    }

    #[test]
    fn structured_json_is_scrubbed_value_wise() {
        let redactor = Redactor::new(vec!["s3cr3t-value"]);
        let value = serde_json::json!({
            "api_key": "s3cr3t-value",
            "nested": {"token": "prefix-s3cr3t-value-suffix", "n": 3},
            "plain": "safe",
        });
        let scrubbed = redactor.scrub_json(&value);
        let text = serde_json::to_string(&scrubbed).expect("json");
        assert!(!text.contains("s3cr3t-value"), "leaked: {text}");
        assert_eq!(scrubbed["plain"], "safe");
        assert_eq!(scrubbed["nested"]["n"], 3);
    }
}
