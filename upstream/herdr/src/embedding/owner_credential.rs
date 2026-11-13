//! Viva #191: shared CLI/TUI snapshot/private-file incarnation resolution.
use std::{
    fmt, io,
    time::{Duration, Instant},
};

/// Only a fully validated capability belonging to another host may request a
/// live-identity recheck. Storage/format failures must never use this marker.
#[derive(Debug)]
pub struct OwnerAnchorHostMismatch;

impl fmt::Display for OwnerAnchorHostMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("stale_owner_anchor: runtime capability belongs to another host incarnation")
    }
}

impl std::error::Error for OwnerAnchorHostMismatch {}

pub(super) fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "native client authentication readiness timed out",
            )
        })
}

/// Retry a validated host mismatch only after the live endpoint confirms the
/// identity changed. The caller's original deadline also bounds every snapshot
/// request; neither a new incarnation nor a retry creates a new budget.
pub fn owner_credential_for_host(
    deadline: Instant,
    running_host: impl FnMut(Duration) -> io::Result<String>,
    load: impl FnMut(&str) -> io::Result<String>,
) -> io::Result<String> {
    resolve(|| remaining(deadline), running_host, load)
}

fn resolve(
    mut budget: impl FnMut() -> io::Result<Duration>,
    mut running_host: impl FnMut(Duration) -> io::Result<String>,
    mut load: impl FnMut(&str) -> io::Result<String>,
) -> io::Result<String> {
    let mut host = running_host(budget()?)?;
    loop {
        budget()?;
        match load(&host) {
            Ok(anchor) => return Ok(anchor),
            Err(error)
                if error.kind() == io::ErrorKind::PermissionDenied
                    && error
                        .get_ref()
                        .is_some_and(|cause| cause.is::<OwnerAnchorHostMismatch>()) =>
            {
                let latest = running_host(budget()?)?;
                if latest == host {
                    return Err(error);
                }
                host = latest;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn repeated_rotations_keep_the_original_deadline_and_snapshot_budget() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(3);
        let now = Cell::new(start);
        let snapshots = Cell::new(0);
        let mut timeouts = Vec::new();
        let mut loads = 0;
        let error = resolve(
            || {
                deadline
                    .checked_duration_since(now.get())
                    .filter(|duration| !duration.is_zero())
                    .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
            },
            |timeout| {
                timeouts.push(timeout);
                snapshots.set(snapshots.get() + 1);
                now.set(now.get() + Duration::from_secs(1));
                Ok(format!("native-{}", snapshots.get()))
            },
            |_| {
                loads += 1;
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    OwnerAnchorHostMismatch,
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(snapshots.get(), 3);
        assert_eq!(loads, 2, "do not read a capability after the deadline");
        assert_eq!(
            timeouts,
            [
                Duration::from_secs(3),
                Duration::from_secs(2),
                Duration::from_secs(1)
            ]
        );
    }

    #[test]
    fn expired_deadline_does_not_query_or_read() {
        let error = owner_credential_for_host(
            Instant::now(),
            |_| panic!("expired deadline queried a host"),
            |_| panic!("expired deadline read a capability"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_permission_error_with_stale_text_is_not_a_rotation_contract() {
        let mut snapshots = 0;
        let error = owner_credential_for_host(
            Instant::now() + Duration::from_secs(1),
            |_| {
                snapshots += 1;
                Ok("native-a".into())
            },
            |_| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    OwnerAnchorHostMismatch.to_string(),
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(snapshots, 1);
    }
}
