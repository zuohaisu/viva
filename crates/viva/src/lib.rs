//! Viva — Personal AI Office host.
//!
//! `foundation` holds the V01/G0 contracts: typed ids, the SQLite store and
//! per-domain migration registry, the append-only office event log, the
//! control-request envelope, and the reference records (sessions, executions,
//! launch specs, terminal events, workbench contracts).

pub mod foundation;
