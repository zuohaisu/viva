//! Viva — local-first personal AI collaboration system.
//!
//! `foundation` holds the V01/G0 contracts: typed ids, the SQLite store and
//! per-domain migration registry, the append-only event log, the
//! control-request envelope, and the reference records (sessions, executions,
//! launch specs, terminal events, workbench contracts).
//!
//! Domain modules own their own migration namespaces and register them via
//! `<domain>::register_migrations`; composition into the binary lands with
//! V07 (lane A).

pub mod agents;
pub mod authority;
pub mod conversations;
pub mod experience;
pub mod foundation;
pub mod git;
pub mod harness;
pub mod knowledge;
pub mod machines;
pub mod maintenance;
pub mod members;
pub mod memory;
pub mod plugins;
pub mod projects;
pub mod redaction;
pub mod server;
pub mod skills;
pub mod tasks;
pub mod terminal;
pub mod tools;
pub mod tui;
pub mod update;
pub mod workflows;
pub mod workspaces;

/// Workbench scene layout persisted per worktree (#75). S4 agent locate uses
/// [`SceneBook::locate`] / [`TerminalLocation`].
pub use tui::workbench::scenes::{SceneBook, TerminalLocation};
