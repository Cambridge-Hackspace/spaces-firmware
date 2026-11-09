//! The Cooperative Systems: Spaces module protocol.
//!
//! Everything here is pure: no hardware, no network, no clock. That is what
//! lets the parts a module must never get wrong be tested on any machine.
//!
//! - [`wire`]: the messages a module exchanges with its edge, and their topics.
//! - [`module`]: the state machine that decides what to do with them.
//! - [`device`]: what any device is, to the loop that runs it.
//! - [`driver`]: the rules every such loop applies, written once.
//! - [`run_filter`]: conditioning a machine's "working" signal into something
//!   worth metering.
//! - [`registration`]: claiming an invite.
//! - [`backoff`]: how long to wait before trying again.
//! - [`status`]: what the status light shows.
//! - [`weblog`]: the log, kept for a web page.
//! - [`setup_page`] and [`form`]: a device's own setup page.

pub mod backoff;
pub mod device;
pub mod driver;
pub mod form;
pub mod module;
pub mod registration;
pub mod run_filter;
pub mod setup_page;
pub mod status;
pub mod weblog;
pub mod wire;

pub use device::{Device, Effect, Link, ModuleInput};
pub use driver::{Driver, Level, LinkEvent, Step};
pub use module::{Action, Activity, Config, EndReason, Module, Outcome, Output, Usage};
pub use run_filter::RunFilter;
