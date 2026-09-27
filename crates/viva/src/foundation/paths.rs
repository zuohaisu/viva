//! Resolution of the Viva persistent state root.
//!
//! All office state lives under one private per-user directory (`~/.viva` by
//! default, overridable via `VIVA_HOME` for tests and parallel lanes). Each
//! development lane must use its own `VIVA_HOME`, database and scratch repos.
//!
//! The historical `~/.ticket-autopilot` directory belongs to a retired product
//! (ADR 0008). Viva never reads, writes, migrates or deletes it.

use std::path::{Path, PathBuf};

/// Environment variable overriding the state root.
pub const VIVA_HOME_ENV: &str = "VIVA_HOME";
/// Default state root name under the user home directory.
pub const DEFAULT_HOME_NAME: &str = ".viva";
/// The SQLite database file inside [`viva_home`].
pub const DATABASE_FILE: &str = "office.db";

/// Resolve the Viva state root: explicit override > `VIVA_HOME` > `~/.viva`.
pub fn viva_home(override_dir: Option<&Path>) -> PathBuf {
    if let Some(explicit) = override_dir {
        return explicit.to_path_buf();
    }
    if let Ok(from_env) = std::env::var(VIVA_HOME_ENV) {
        if !from_env.is_empty() {
            return PathBuf::from(from_env);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(DEFAULT_HOME_NAME);
    }
    // No HOME (exotic service contexts): fall back to the process cwd so the
    // caller sees a deterministic path instead of panicking.
    PathBuf::from(DEFAULT_HOME_NAME)
}

/// Create `path` owner-only (0o700 on Unix) if missing and return it.
pub fn ensure_private_dir(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path.to_path_buf())
}

/// The office database path inside the state root.
pub fn database_path(home: &Path) -> PathBuf {
    home.join(DATABASE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_override_wins() {
        let chosen = viva_home(Some(Path::new("/tmp/viva-explicit")));
        assert_eq!(chosen, PathBuf::from("/tmp/viva-explicit"));
    }

    #[test]
    fn database_lives_inside_home() {
        let db = database_path(Path::new("/tmp/viva-home"));
        assert_eq!(db, PathBuf::from("/tmp/viva-home/office.db"));
    }
}
