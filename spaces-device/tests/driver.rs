//! The rules every loop applies to a device, each pinned (#7).
//!
//! Driven with a fake device that records what reached it, so a rule is
//! tested on its own terms -- "a fragment never reaches the device" -- rather
//! than through whatever a real Module happens to do with the result.

use spaces_device::wire::topic;
use spaces_device::{Device, Driver, Effect, Level, Link, LinkEvent, Step};

/// A device that records its calls, and whose session and actions the test
/// sets.
#[derive(Default)]
struct Fake {
    links: Vec<Link>,
    messages: Vec<String>,
    session: bool,
    /// What the next call answers with.
    next: Vec<Act>,
}

#[derive(Debug, Clone, PartialEq)]
enum Act {
    Send(&'static str),
    Say(&'static str),
    Relay(bool),
}

impl Fake {
    fn answer(&mut self) -> Vec<Act> {
        std::mem::take(&mut self.next)
    }
}

impl Device for Fake {
    type Action = Act;
    type Input = Act;

    fn subscriptions(&self) -> &'static [&'static str] {
        &["a/topic", "b/topic"]
    }
    fn start(&mut self, _now: u64) -> Vec<Act> {
        self.answer()
    }
    fn input(&mut self, input: Act, _now: u64) -> Vec<Act> {
        vec![input]
    }
    fn message(&mut self, topic: &str, _payload: &[u8], _now: u64) -> Vec<Act> {
        self.messages.push(topic.to_string());
        self.answer()
    }
    fn link(&mut self, link: Link, _now: u64) -> Vec<Act> {
        self.links.push(link);
        self.answer()
    }
    fn tick(&mut self, _now: u64) -> Vec<Act> {
        self.answer()
    }
    fn in_session(&self) -> bool {
        self.session
    }
    fn effect(action: Act) -> Result<Effect, Act> {
        match action {
            Act::Send(t) => Ok(Effect::Publish {
                topic: t,
                payload: "{}".into(),
            }),
            Act::Say(text) => Ok(Effect::Note(text.into())),
            other => Err(other),
        }
    }
}

fn subscribes<A>(steps: &[Step<A>]) -> Vec<&'static str> {
    steps
        .iter()
        .filter_map(|s| match s {
            Step::Subscribe(t) => Some(*t),
            _ => None,
        })
        .collect()
}

fn warns<A>(steps: &[Step<A>]) -> usize {
    steps
        .iter()
        .filter(|s| matches!(s, Step::Log(Level::Warn, _)))
        .count()
}

#[test]
fn every_connect_subscribes_again_not_only_the_first() {
    // The session is clean: the broker forgets subscriptions on every drop.
    // The mutant is subscribing once, which is connected and deaf after the
    // first reconnect.
    let mut d = Driver::new(Fake::default());
    let first = d.on(LinkEvent::Connected, 0);
    assert_eq!(subscribes(&first), ["a/topic", "b/topic"]);
    d.on(LinkEvent::Disconnected, 10);
    let again = d.on(LinkEvent::Connected, 20);
    assert_eq!(
        subscribes(&again),
        ["a/topic", "b/topic"],
        "a reconnect must subscribe again"
    );
}

#[test]
fn a_connect_is_the_broker_milestone_and_tells_the_device() {
    let mut d = Driver::new(Fake::default());
    let steps = d.on(LinkEvent::Connected, 0);
    assert!(steps.contains(&Step::Broker(true)));
    assert!(d.device().links.last().unwrap().broker);
    assert!(d.link().broker);
}

#[test]
fn a_disconnect_is_reported_once_and_subscribes_nothing() {
    let mut d = Driver::new(Fake::default());
    d.on(LinkEvent::Connected, 0);
    let lost = d.on(LinkEvent::Disconnected, 10);
    assert!(lost.contains(&Step::Broker(false)));
    assert!(subscribes(&lost).is_empty());
    assert!(!d.device().links.last().unwrap().broker);
    // A second disconnect with nothing between is not news.
    let links = d.device().links.len();
    assert!(d.on(LinkEvent::Disconnected, 20).is_empty());
    assert_eq!(d.device().links.len(), links);
}

#[test]
fn a_fragment_is_dropped_loudly_and_never_reaches_the_device() {
    let mut d = Driver::new(Fake::default());
    d.on(LinkEvent::Connected, 0);
    let steps = d.on(LinkEvent::Fragment, 10);
    assert_eq!(warns(&steps), 1, "dropped silently: {steps:?}");
    assert!(d.device().messages.is_empty());
}

#[test]
fn an_error_is_a_log_line_and_not_an_event() {
    let mut d = Driver::new(Fake::default());
    d.on(LinkEvent::Connected, 0);
    let links = d.device().links.len();
    let steps = d.on(LinkEvent::Error("timeout".into()), 10);
    assert_eq!(steps.len(), 1);
    assert_eq!(warns(&steps), 1);
    assert_eq!(
        d.device().links.len(),
        links,
        "the device heard about an error"
    );
    assert!(d.link().broker, "an error is not a disconnect");
}

#[test]
fn wifi_changes_are_reported_once() {
    let mut d = Driver::new(Fake::default());
    d.on(LinkEvent::Wifi(true), 0);
    assert!(d.device().links.last().unwrap().wifi);
    assert!(
        d.on(LinkEvent::Wifi(true), 10).is_empty(),
        "repeated, not news"
    );
    d.on(LinkEvent::Wifi(false), 20);
    assert!(!d.device().links.last().unwrap().wifi);
    assert_eq!(d.device().links.len(), 2);
}

#[test]
fn a_message_reaches_the_device_and_its_answer_is_carried_out() {
    let fake = Fake {
        next: vec![Act::Send(topic::SCAN_REQUEST), Act::Relay(true)],
        ..Fake::default()
    };
    let mut d = Driver::new(fake);
    let steps = d.on(
        LinkEvent::Message {
            topic: topic::SCAN_RESPONSE.into(),
            payload: b"{}".to_vec(),
        },
        0,
    );
    assert_eq!(d.device().messages, [topic::SCAN_RESPONSE]);
    assert!(steps.contains(&Step::Publish {
        topic: topic::SCAN_REQUEST,
        payload: "{}".into()
    }));
    assert!(steps.contains(&Step::Device(Act::Relay(true))));
}

#[test]
fn a_note_is_a_log_line_not_a_hardware_action() {
    let mut d = Driver::new(Fake::default());
    let steps = d.input(Act::Say("hello"), 0);
    assert_eq!(steps, [Step::Log(Level::Info, "hello".into())]);
}

#[test]
fn the_session_is_told_to_the_updater_when_it_changes_and_only_then() {
    // An update waits for the session to close. The mutant is never telling
    // the updater, which lets an OTA restart the box mid-session.
    let mut d = Driver::new(Fake::default());
    assert!(
        !d.tick(0).contains(&Step::Session(false)),
        "nothing changed"
    );
    let fake = Fake {
        session: true,
        ..Fake::default()
    };
    let mut d = Driver::new(fake);
    assert!(d.tick(0).contains(&Step::Session(true)));
    assert!(
        !d.tick(10).iter().any(|s| matches!(s, Step::Session(_))),
        "repeated, not news"
    );
}

#[test]
fn the_leases_and_heartbeats_are_not_narrated() {
    // They arrive and go every second; narrating them drowns the log.
    let fake = Fake {
        next: vec![Act::Send(topic::POWER_REPORT)],
        ..Fake::default()
    };
    let mut d = Driver::new(fake);
    let lease = d.on(
        LinkEvent::Message {
            topic: topic::LEASE.into(),
            payload: b"{}".to_vec(),
        },
        0,
    );
    assert!(
        !lease.iter().any(|s| matches!(s, Step::Log(..))),
        "{lease:?}"
    );
    let scan = d.on(
        LinkEvent::Message {
            topic: topic::SCAN_RESPONSE.into(),
            payload: b"{}".to_vec(),
        },
        10,
    );
    assert!(scan.iter().any(|s| matches!(s, Step::Log(Level::Info, _))));
}

#[test]
fn a_module_runs_under_the_driver_as_it_did_by_hand() {
    // The one device this crate ships: a plug leased through the driver is
    // switched on, and its session reaches the updater.
    use serde_json::json;
    use spaces_device::{Action, Config, Module, Output};
    let mut d = Driver::new(Module::new(Config::power_only("me", "laser-01")));
    d.start(0);
    let lease =
        json!({"resource": "r", "device_id": "me", "grant": true, "ttl_ms": 3_000}).to_string();
    let steps = d.on(
        LinkEvent::Message {
            topic: topic::LEASE.into(),
            payload: lease.into_bytes(),
        },
        100,
    );
    assert!(steps.contains(&Step::Device(Action::Set {
        output: Output::Tool,
        on: true
    })));
    assert!(steps.contains(&Step::Session(true)));
}
