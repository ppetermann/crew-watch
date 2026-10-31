//! crew-watch's data layer: a single-pass process snapshot, agent-runtime
//! detection with subtree aggregation, and read-only parsing of a firstmate
//! home's fleet records, so another program can show what the `crew-watch`
//! TUI shows.
//!
//! [`fleet::Sampler`] is the entry point: point it at a firstmate home and
//! call [`fleet::Sampler::sample`] once per refresh to get a serializable
//! [`fleet::FleetSample`]. The lower-level modules stay public for callers
//! that want one piece (detection, the `.status` parser, ...).
//!
//! ```no_run
//! use crew_watch::fleet::Sampler;
//!
//! // Any firstmate home: the library has no default path.
//! let mut sampler = Sampler::new("/home/me/agents/firstmate".into());
//! loop {
//!     let sample = sampler.sample(); // serde-serializable FleetSample
//!     for agent in &sample.agents {
//!         println!("{} {:?} {:.1}% {}", agent.pid, agent.activity, agent.cpu_percent, agent.title);
//!     }
//!     std::thread::sleep(std::time::Duration::from_secs(2));
//! }
//! ```
//!
//! The terminal UI lives in the `crew-watch` binary behind the default `tui`
//! feature; depend with `default-features = false` to build only this
//! library.

pub mod activity;
pub mod config;
pub mod detect;
pub mod fleet;
pub mod format_util;
pub mod gate;
pub mod ledger;
mod macos;
pub mod meta;
pub mod model;
pub mod procfs;
pub mod project;
pub mod prpoll;
pub mod quota;
pub mod taskinfo;
pub mod titles;
