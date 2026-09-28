//! Viva — Personal AI Office host.
//!
//! `foundation` holds the V01/G0 contracts: typed ids, the SQLite store and
//! per-domain migration registry, the append-only office event log, the
//! control-request envelope, and the reference records (sessions, executions,
//! launch specs, terminal events, workbench contracts).
//!
//! Domain modules own their own migration namespaces and register them via
//! `<domain>::register_migrations`; composition into the binary lands with
//! V07 (lane A).

pub mod authority;
pub mod conversations;
pub mod foundation;
pub mod git;
pub mod harness;
pub mod knowledge;
pub mod maintenance;
pub mod members;
pub mod office;
pub mod projects;
pub mod redaction;
pub mod tasks;
pub mod terminal;
pub mod tools;
pub mod tui;
pub mod workflows;
pub mod workspaces;
