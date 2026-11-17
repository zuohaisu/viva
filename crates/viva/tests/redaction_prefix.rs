//! Regression: a configured prefix must not expose the recoverable suffix of
//! a longer generated credential, including every streaming split boundary.
use viva::redaction::{ByteRedactor, Redactor};
#[test]
fn overlapping_secret_prefix_does_not_shorten_a_credential() {
    let secret = "cred-private-fixture-uuid";
    for split in 0..=secret.len() {
        let mut redactor = ByteRedactor::new([b"cred-".to_vec(), secret.as_bytes().to_vec()]);
        let mut out = redactor.push(&secret.as_bytes()[..split]);
        out.extend(redactor.push(&secret.as_bytes()[split..]));
        out.extend(redactor.flush());
        assert_eq!(out, b"[REDACTED]", "byte split {split}");
        let mut redactor = Redactor::new(["cred-", secret]);
        let mut out = redactor.push(&secret[..split]);
        out.push_str(&redactor.push(&secret[split..]));
        out.push_str(&redactor.flush());
        assert_eq!(out, "[REDACTED]", "text split {split}");
    }
}
