//! The loop's rules, written once (#7).
//!
//! Every runtime that connects a device to its edge's broker has to get the
//! same few things right, and each used to get them right on its own:
//! `spaces-device-espidf::run`, a copy of it in spaces-toolguard-device, and
//! a host loop in spaces-iot-bridge. The rules:
//!
//! - **Subscribe again on every connect.** The session is clean, so the broker
//!   forgets the subscriptions when the link drops. A client that subscribes
//!   once is connected and deaf after the first reconnect, and FIRMWARE.md
//!   names that trap.
//! - **A disconnect switches nothing.** With the link gone, leases stop
//!   arriving and the device switches off when the current one runs out. A
//!   disconnect is not trusted to be noticed, and a reconnect is not a reason
//!   to switch anything on.
//! - **A fragmented message is dropped, loudly.** Nothing on a device's topics
//!   is anywhere near large enough to arrive in pieces; for a lease, dropping
//!   reads as silence, which is the safe reading.
//! - **An error is a log line, not an event.** The device's view of its link
//!   comes from connected and disconnected; a transient error between them is
//!   noise.
//! - **Send without blocking.** [`Step::Publish`] is to be queued by the
//!   runtime, never sent with a call that waits on the network: the loop that
//!   sends is the loop that has to notice a lease running out.
//! - **An update waits for the session.** The driver tells the runtime when a
//!   session opens and closes, from [`Device::in_session`].
//!
//! [`Driver`] holds a [`Device`] and applies these to it. It is pure: what it
//! returns is a list of [`Step`]s for the runtime to carry out with whatever
//! MQTT client, logger, status light and updater it has. So the rules are
//! tested here, on a host, once, and a runtime is only the glue between its
//! client's events and these steps.

use crate::device::{Device, Effect, Link};
use crate::wire::topic;

/// What happened on the device's link, as a runtime's MQTT client reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// Connected to the local broker.
    Connected,
    /// The broker connection dropped.
    Disconnected,
    /// A whole message.
    Message { topic: String, payload: Vec<u8> },
    /// A message that arrived in pieces.
    Fragment,
    /// An error from the client, between connects.
    Error(String),
    /// The network came up or went away.
    Wifi(bool),
}

/// How loud a [`Step::Log`] line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
}

/// What the runtime does next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step<A> {
    /// Subscribe to a topic.
    Subscribe(&'static str),
    /// Queue a message on the local broker. Never a blocking publish.
    Publish {
        topic: &'static str,
        payload: String,
    },
    /// A line for the log.
    Log(Level, String),
    /// A session opened (`true`) or closed: tell the updater.
    Session(bool),
    /// The broker was reached (`true`) or lost: the update milestone, and the
    /// status light.
    Broker(bool),
    /// For the device's own hardware.
    Device(A),
}

/// Topics heard or said every second, which would drown the log.
const QUIET: &[&str] = &[
    topic::LEASE,
    topic::POWER_RESPONSE,
    topic::POWER_REPORT,
    topic::ALIVE_REQUEST,
];

/// A device, and the rules every loop applies to it.
pub struct Driver<D: Device> {
    device: D,
    link: Link,
    session: bool,
}

impl<D: Device> Driver<D> {
    pub fn new(device: D) -> Self {
        Driver {
            device,
            link: Link::default(),
            session: false,
        }
    }

    /// The device, to read its state (a panel, a status page).
    pub fn device(&self) -> &D {
        &self.device
    }

    /// The link as the driver last heard it.
    pub fn link(&self) -> Link {
        self.link
    }

    /// Once, before anything else.
    pub fn start(&mut self, now: u64) -> Vec<Step<D::Action>> {
        let actions = self.device.start(now);
        self.carry(actions)
    }

    /// The device's own input.
    pub fn input(&mut self, input: D::Input, now: u64) -> Vec<Step<D::Action>> {
        let actions = self.device.input(input, now);
        self.carry(actions)
    }

    /// Time passed.
    pub fn tick(&mut self, now: u64) -> Vec<Step<D::Action>> {
        let actions = self.device.tick(now);
        self.carry(actions)
    }

    /// Something happened on the link. The rules are here.
    pub fn on(&mut self, event: LinkEvent, now: u64) -> Vec<Step<D::Action>> {
        let mut steps = Vec::new();
        let actions = match event {
            LinkEvent::Connected => {
                self.link.broker = true;
                steps.push(Step::Broker(true));
                steps.push(Step::Log(
                    Level::Info,
                    "connected to the local broker; subscribing".into(),
                ));
                // Every connect, not only the first: the session is clean.
                for t in self.device.subscriptions() {
                    steps.push(Step::Subscribe(t));
                }
                self.device.link(self.link, now)
            }
            LinkEvent::Disconnected => {
                if !self.link.broker {
                    return steps;
                }
                self.link.broker = false;
                steps.push(Step::Broker(false));
                steps.push(Step::Log(
                    Level::Warn,
                    "lost the local broker; reconnecting".into(),
                ));
                self.device.link(self.link, now)
            }
            LinkEvent::Message { topic, payload } => {
                if !QUIET.contains(&topic.as_str()) {
                    steps.push(Step::Log(
                        Level::Info,
                        format!("<< {topic} {}", String::from_utf8_lossy(&payload)),
                    ));
                }
                self.device.message(&topic, &payload, now)
            }
            LinkEvent::Fragment => {
                steps.push(Step::Log(
                    Level::Warn,
                    "ignoring a message too large to arrive in one piece".into(),
                ));
                return steps;
            }
            LinkEvent::Error(e) => {
                steps.push(Step::Log(Level::Warn, format!("broker connection: {e}")));
                return steps;
            }
            LinkEvent::Wifi(up) => {
                if up == self.link.wifi {
                    return steps;
                }
                self.link.wifi = up;
                steps.push(Step::Log(
                    if up { Level::Info } else { Level::Warn },
                    if up { "Wi-Fi is up" } else { "Wi-Fi is down" }.into(),
                ));
                self.device.link(self.link, now)
            }
        };
        steps.extend(self.carry(actions));
        steps
    }

    /// The device's actions as steps, and a session change if there was one.
    fn carry(&mut self, actions: Vec<D::Action>) -> Vec<Step<D::Action>> {
        let mut steps = Vec::new();
        for action in actions {
            match D::effect(action) {
                Ok(Effect::Publish { topic, payload }) => {
                    if !QUIET.contains(&topic) {
                        steps.push(Step::Log(Level::Info, format!(">> {topic} {payload}")));
                    }
                    steps.push(Step::Publish { topic, payload });
                }
                Ok(Effect::Note(text)) => steps.push(Step::Log(Level::Info, text)),
                Err(own) => steps.push(Step::Device(own)),
            }
        }
        let session = self.device.in_session();
        if session != self.session {
            self.session = session;
            steps.push(Step::Session(session));
        }
        steps
    }
}
