//! The Cooperative Systems: Spaces module protocol.
//!
//! Everything here is pure: no hardware, no network, no clock. That is what
//! lets the parts a module must never get wrong be tested on any machine.
//!
//! - [`wire`]: the messages a module exchanges with its edge, and their topics.
//! - [`module`]: the state machine that decides what to do with them.

pub mod module;
pub mod wire;

pub use module::{Action, Config, Module, Output};
