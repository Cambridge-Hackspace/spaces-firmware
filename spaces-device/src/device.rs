//! What any device is, to the loop that runs it (#8).
//!
//! A runtime -- `spaces-device-espidf::run` on an ESP32, a host loop in
//! spaces-iot-bridge -- owns the broker connection, the timer and the
//! update machinery, and it should not care what kind of device it is
//! driving. Before this trait it drove exactly one kind, [`Module`], through
//! an `Event` enum closed around the demo's buttons, so a badge reader that
//! wrapped a `Module` in more behaviour had to rebuild the whole loop for
//! itself, and missed what the loop did besides (spaces-toolguard-device #1,
//! #2, #5).
//!
//! So a device is anything that can:
//!
//! - start, take its own input, take a message, see the link change, and be
//!   ticked -- each answering with the actions it wants carried out;
//! - say whether a session is open, which is what holds an update back;
//! - say which of its actions are the loop's to carry out (a publish, a log
//!   line) and which are its own hardware's (a relay, a panel, a latch).
//!
//! The loop asks the device about the session rather than inferring it from
//! particular actions. Inference is how a device with no relay ended up with
//! no session guard at all: it never emitted the action the guard keyed on.

use crate::module::{Action, Activity, Module};
use crate::wire::topic;

/// The state of the device's link to its edge, as the runtime sees it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Link {
    /// Joined a network.
    pub wifi: bool,
    /// Connected to the local broker.
    pub broker: bool,
}

/// What the loop carries out on a device's behalf, whatever the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send on the local broker. Queued, never a blocking publish: the loop
    /// that sends is the loop that has to notice a lease running out.
    Publish {
        topic: &'static str,
        payload: String,
    },
    /// A line for the log.
    Note(String),
}

/// A device the loop can run.
///
/// Every method takes the time as monotonic milliseconds, as [`Module`] does,
/// so a device stays testable without a clock.
pub trait Device {
    /// What the device asks for. Some of it is the loop's ([`Device::effect`]),
    /// the rest goes to the device's own hardware.
    type Action;
    /// The device's own input: a card, a button, a sensor reading.
    type Input;

    /// The topics it listens on. Subscribed again on every connect.
    fn subscriptions(&self) -> &'static [&'static str];
    /// Once, before anything else.
    fn start(&mut self, now: u64) -> Vec<Self::Action>;
    /// Its own input arrived.
    fn input(&mut self, input: Self::Input, now: u64) -> Vec<Self::Action>;
    /// A message arrived on one of its topics.
    fn message(&mut self, topic: &str, payload: &[u8], now: u64) -> Vec<Self::Action>;
    /// The link changed. Never a reason to switch anything on, and not to be
    /// relied on to switch anything off: a lease runs out on silence, whether
    /// or not a disconnect was noticed.
    fn link(&mut self, link: Link, now: u64) -> Vec<Self::Action>;
    /// Time passed. Called often, whether or not anything happened.
    fn tick(&mut self, now: u64) -> Vec<Self::Action>;
    /// Whether a session is open, or about to be: an update waits until it is
    /// not, so that a restart never strands a member's session.
    fn in_session(&self) -> bool;
    /// The part of `action` the loop carries out, or the action back for the
    /// device's hardware.
    fn effect(action: Self::Action) -> Result<Effect, Self::Action>;
}

/// What a [`Module`] takes as its own input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleInput {
    /// A card was presented.
    Swipe(String),
    /// Stop pressed.
    ToolOff,
    /// The machine started or stopped working.
    Running(bool),
}

impl Device for Module {
    type Action = Action;
    type Input = ModuleInput;

    fn subscriptions(&self) -> &'static [&'static str] {
        topic::SUBSCRIPTIONS
    }

    fn start(&mut self, now: u64) -> Vec<Action> {
        Module::start(self, now)
    }

    fn input(&mut self, input: ModuleInput, now: u64) -> Vec<Action> {
        match input {
            ModuleInput::Swipe(card) => self.swipe(&card, now),
            ModuleInput::ToolOff => self.tool_off(now),
            ModuleInput::Running(on) => self.running(on, now),
        }
    }

    fn message(&mut self, topic: &str, payload: &[u8], now: u64) -> Vec<Action> {
        Module::message(self, topic, payload, now)
    }

    /// Nothing to do on purpose. With the link gone, leases stop arriving and
    /// the module switches off when the current one runs out; with it back,
    /// nothing switches on until a lease says so.
    fn link(&mut self, _link: Link, now: u64) -> Vec<Action> {
        Module::tick(self, now)
    }

    fn tick(&mut self, now: u64) -> Vec<Action> {
        Module::tick(self, now)
    }

    /// Asking, or in a session, or energized. A plug owns no session, so for
    /// it the relay is the nearest thing to one; a reader with no relay has a
    /// session and no relay. Asking counts: an update that restarts the box
    /// between a tap and its answer loses the answer.
    fn in_session(&self) -> bool {
        self.activity() != Activity::Idle || self.tool_is_on()
    }

    fn effect(action: Action) -> Result<Effect, Action> {
        match action {
            Action::Publish { topic, payload } => Ok(Effect::Publish { topic, payload }),
            Action::Note(text) => Ok(Effect::Note(text)),
            other => Err(other),
        }
    }
}
