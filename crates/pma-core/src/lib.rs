//! pma's core: the store, scanning, dispatch and workflows, and the
//! `api` module its front ends share. It prints nothing and depends on no
//! UI crate. Design: `docs/dev/design.md`, `docs/dev/frontends.md`.

// Process groups, `sh` and mode bits are assumed throughout. See TODO.md.
#[cfg(not(unix))]
compile_error!("pma supports Linux and macOS only");

pub mod accept;
pub mod agent;
pub mod api;
pub mod catalog;
pub mod class;
pub mod complexity;
pub mod config;
pub mod dates;
pub mod deps;
pub mod dispatch;
pub mod herdr;
pub mod history;
pub mod jobs;
pub mod keys;
pub mod next;
pub mod pass;
pub mod progress;
pub mod projects;
pub mod publish;
pub mod rank;
pub mod report;
pub mod report_runs;
pub mod route;
pub mod scan;
pub mod screen;
pub mod script;
pub mod settings;
pub mod store;
pub mod sync;
pub mod todo;
pub mod watch;
pub mod worker;
pub mod workflow;
