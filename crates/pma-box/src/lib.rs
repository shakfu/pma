//! Run a coding agent in a disposable container, with the API key held on the host.
//!
//! Copied from sanduk-rs at `031bb4e` (2026-11-04) and maintained here apart from it; see
//! `docs/dev/pma-box.md`. Containers, networks, images and directories are named `pma-`, so
//! `sanduk ps` and `sanduk clean` never touch them.
//!
//! ```no_run
//! use pma_box::run::{Mode, Options};
//! # fn main() -> pma_box::error::Result<()> {
//! let sanduk = pma_box::Sanduk::new(pma_box::util::Dirs::default());
//! let plan = sanduk.plan(Options {
//!     task: Some("Summarise this repository.".into()),
//!     workdir: "./repo".into(),
//!     mode: Mode::KeySafe,
//!     ..Options::default()
//! })?;
//! let mut live = plan.start()?;
//! let status = live.command().status()?;
//! let outcome = live.finish(status);
//! # let _ = outcome; Ok(()) }
//! ```
//!
//! The container engines are [`crate::container`]; the per-process sandbox is
//! [`sanduk_sandbox`](https://docs.rs/sanduk-sandbox). This crate holds the providers, the relay,
//! the agents, recipes and kits, and the run. See [`run`] for its phases.

use std::sync::Arc;

pub mod agent;
pub mod catalog;
mod check;
pub mod clone;
pub mod container;
pub mod error;
pub mod http;
pub mod kits;
pub mod observe;
pub mod preflight;
pub mod providers;
pub mod recipes;
pub mod relay;
pub mod resources;
pub mod run;
pub mod sections;
pub mod util;

/// Runs agents with one set of directories and one observer. Holds no process-wide state: two in
/// one process may differ in both.
#[derive(Clone)]
pub struct Sanduk {
    dirs: util::Dirs,
    obs: Arc<dyn observe::Observer>,
    engine: Option<crate::container::Engine>,
}

impl Sanduk {
    /// Notes discarded until [`observer`](Sanduk::observer) is set.
    pub fn new(dirs: util::Dirs) -> Self {
        Sanduk {
            dirs,
            obs: Arc::new(()),
            engine: None,
        }
    }

    /// Where this instance's runs send their notes, and ask whether to stop.
    pub fn observer(mut self, obs: Arc<dyn observe::Observer>) -> Self {
        self.obs = obs;
        self
    }

    /// Use `engine` rather than the one a run's `runtime` names: for tests, with
    /// `crate::container::testing::Fake` (feature `test-util`).
    pub fn engine(mut self, engine: crate::container::Engine) -> Self {
        self.engine = Some(engine);
        self
    }

    pub fn dirs(&self) -> &util::Dirs {
        &self.dirs
    }

    /// Checks `opts` and acquires nothing. See [`run::Plan`].
    pub fn plan(&self, opts: run::Options) -> error::Result<run::Plan> {
        run::Plan::new(&self.dirs, self.obs.clone(), self.engine.clone(), opts)
    }
}
