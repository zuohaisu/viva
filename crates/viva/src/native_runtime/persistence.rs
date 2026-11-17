//! Native persistence reuses Viva's configured secret list and redactors.
//! Raw PTY bytes and original user files never pass through this adapter.
use super::*;
use crate::redaction::{ByteRedactor, Redactor};

pub(super) struct Persistence {
    state: Mutex<State>,
}
struct State {
    secrets: Vec<String>,
    stream: ByteRedactor,
}
impl Persistence {
    pub(super) fn new(home: &std::path::Path) -> io::Result<Self> {
        let configured = crate::redaction::configured_secrets(home)?;
        let policy = Self {
            state: Mutex::new(State {
                stream: ByteRedactor::new(Vec::<Vec<u8>>::new()),
                secrets: Vec::new(),
            }),
        };
        for secret in configured {
            policy.register(&secret)?;
        }
        Ok(policy)
    }
    pub(super) fn register(&self, secret: &str) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("persistence redactor poisoned"))?;
        // Tracing may Debug/JSON-escape a string before it reaches Write.
        // Protect both canonical encodings as well as the raw value.
        let json = serde_json::to_string(secret)?;
        let debug = format!("{secret:?}");
        for value in [secret, &json[1..json.len() - 1], &debug[1..debug.len() - 1]] {
            if !value.is_empty() && !state.secrets.iter().any(|known| known == value) {
                state.secrets.push(value.to_owned());
                state.stream.add_secret(value.as_bytes().to_vec());
            }
        }
        Ok(())
    }
    pub(super) fn json(&self, value: &serde_json::Value) -> io::Result<serde_json::Value> {
        let state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("persistence redactor poisoned"))?;
        Ok(Redactor::new(state.secrets.clone()).scrub_json(value))
    }
    pub(super) fn text(&self, value: &str) -> io::Result<String> {
        self.json(&serde_json::json!(value))?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("invalid redactor result"))
    }
    pub(super) fn chunk(&self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| io::Error::other("persistence redactor poisoned"))?
            .stream
            .push(bytes))
    }
    pub(super) fn flush(&self) -> io::Result<Vec<u8>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| io::Error::other("persistence redactor poisoned"))?
            .stream
            .flush())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaped_trace_values_do_not_reveal_a_configured_secret() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Persistence::new(dir.path()).unwrap();
        let secret = "credential-with-quote-\"-and-backslash-\\";
        policy.register(secret).unwrap();
        let debug = format!("{secret:?}");
        let mut safe = policy.chunk(debug.as_bytes()).unwrap();
        safe.extend(policy.flush().unwrap());
        assert_eq!(safe, b"\"[REDACTED]\"");
    }

    #[test]
    fn configured_and_rotated_credentials_scrub_structures_and_split_log_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("redaction.secrets"),
            "# comment\nconfigured-secret-123\n",
        )
        .unwrap();
        let policy = Persistence::new(dir.path()).unwrap();
        policy.register("new-worker-credential").unwrap();
        let value = policy.json(&serde_json::json!({"error": "configured-secret-123", "env": ["new-worker-credential"]})).unwrap();
        assert!(!value.to_string().contains("configured-secret-123"));
        assert!(!value.to_string().contains("new-worker-credential"));
        let mut output = policy.chunk(b"prefix configured-").unwrap();
        output.extend(policy.chunk(b"secret-123 new-worker-").unwrap());
        output.extend(policy.chunk(b"credential suffix").unwrap());
        output.extend(policy.flush().unwrap());
        assert_eq!(output, b"prefix [REDACTED] [REDACTED] suffix");
    }
}
