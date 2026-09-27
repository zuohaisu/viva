//! V01/G0 foundation: the contracts every other lane codes against.
//!
//! Layout map (each module states its G0 semantics):
//! - [`ids`]: typed identifiers and UTC timestamps
//! - [`paths`]: `VIVA_HOME` resolution and private directory rules
//! - [`error`]: the office error taxonomy
//! - [`store`]: SQLite open/WAL, schema metadata, per-domain migration registry
//! - [`events`]: the append-only office event log
//! - [`envelope`]: control requests/results — caller binding, idempotency,
//!   rejection reasons
//! - [`records`]: session kinds, attribution snapshots, executions, launch
//!   specs, terminal events, workbench query/action contracts

pub mod envelope;
pub mod error;
pub mod events;
pub mod ids;
pub mod paths;
pub mod records;
pub mod store;

pub use error::{OfficeError, OfficeResult};
pub use store::{Domain, FrozenMigrations, MigrationRegistry};

use std::sync::OnceLock;

/// The migrations V01 owns: the `foundation` domain creates the schema
/// metadata, event log, session/execution references, launch specs, terminal
/// events and control-request ledger. Later domains register their own
/// namespaces; they reference foundation tables but never alter them.
pub fn foundation_migrations() -> &'static FrozenMigrations {
    static FROZEN: OnceLock<FrozenMigrations> = OnceLock::new();
    FROZEN.get_or_init(|| {
        MigrationRegistry::new()
            .register(
                store::DOMAIN_FOUNDATION,
                1,
                "foundation reference tables",
                store::FOUNDATION_V1_SQL,
            )
            .freeze()
            .expect("foundation v1 registry is well-formed")
    })
}
