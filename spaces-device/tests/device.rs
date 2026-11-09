//! A `Module` as a [`Device`]: what the loop that runs it relies on (#8).
//!
//! The loop asks the device whether a session is open, and which of its
//! actions are the loop's to carry out. These pin both answers for the one
//! device this crate ships, so a runtime built on the trait gets the same
//! behaviour the demo's hand-written loop had.

use serde_json::json;
use spaces_device::wire::topic;
use spaces_device::{Action, Config, Device, Effect, Link, Module, ModuleInput, Output};

const ME: &str = "dev-uuid-me";
const TOOL: &str = "laser-01";

fn started(config: Config) -> Module {
    let mut m = Module::new(config);
    Device::start(&mut m, 0);
    m
}

fn yes() -> Vec<u8> {
    json!({"authorized": true, "reason": "authorized"})
        .to_string()
        .into_bytes()
}

fn lease(grant: bool) -> Vec<u8> {
    json!({"resource": "7c9e6679-uuid", "device_id": ME, "grant": grant, "ttl_ms": 3_000})
        .to_string()
        .into_bytes()
}

#[test]
fn a_reader_is_in_session_from_the_tap_until_it_ends() {
    // From the tap, not from the yes: an update that restarts the box between
    // a tap and its answer loses the answer, and with it the member's session.
    let mut m = started(Config::switches_nothing(ME, TOOL));
    assert!(!m.in_session(), "idle is not a session");
    m.input(ModuleInput::Swipe("CARD".into()), 10);
    assert!(m.in_session(), "asking already counts");
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    assert!(m.in_session(), "signed in");
    m.input(ModuleInput::ToolOff, 200);
    assert!(!m.in_session(), "the stop ended it");
}

#[test]
fn a_reader_that_is_refused_is_not_in_session() {
    let mut m = started(Config::switches_nothing(ME, TOOL));
    m.input(ModuleInput::Swipe("CARD".into()), 10);
    let no = json!({"authorized": false, "reason": "no"}).to_string();
    m.message(topic::SCAN_RESPONSE, no.as_bytes(), 100);
    assert!(!m.in_session());
}

#[test]
fn a_plug_is_in_session_while_its_relay_is_on() {
    // A plug owns no session; its relay is the nearest thing to one. The
    // mutant is asking only `activity()`, which is Idle for a plug throughout.
    let mut m = started(Config::power_only(ME, TOOL));
    assert!(!m.in_session());
    m.message(topic::LEASE, &lease(true), 100);
    assert!(m.tool_is_on());
    assert!(m.in_session(), "an energized plug must hold an update back");
    m.message(topic::LEASE, &lease(false), 200);
    assert!(!m.in_session());
}

#[test]
fn publishes_and_notes_are_the_loops_and_the_rest_is_the_hardwares() {
    let publish = Action::Publish {
        topic: topic::SCAN_REQUEST,
        payload: "{}".into(),
    };
    assert_eq!(
        Module::effect(publish),
        Ok(Effect::Publish {
            topic: topic::SCAN_REQUEST,
            payload: "{}".into()
        })
    );
    assert_eq!(
        Module::effect(Action::Note("hello".into())),
        Ok(Effect::Note("hello".into()))
    );
    let set = Action::Set {
        output: Output::Tool,
        on: true,
    };
    assert_eq!(Module::effect(set.clone()), Err(set));
}

#[test]
fn a_link_coming_up_switches_nothing_on() {
    // Only a lease energizes. A reconnect, or a lease that arrived before the
    // link was known to be up, is not a reason to.
    let mut m = started(Config::power_only(ME, TOOL));
    let out = m.link(
        Link {
            wifi: true,
            broker: true,
        },
        50,
    );
    assert!(!out.contains(&Action::Set {
        output: Output::Tool,
        on: true
    }));
    assert!(!m.tool_is_on());
}

#[test]
fn a_swipe_input_asks_the_edge() {
    let mut m = started(Config::new(ME, TOOL));
    let out = m.input(ModuleInput::Swipe("CARD".into()), 10);
    assert!(out
        .iter()
        .any(|a| matches!(a, Action::Publish { topic, .. } if *topic == topic::SCAN_REQUEST)));
}

#[test]
fn a_module_listens_on_every_topic_it_answers_to() {
    let m = started(Config::new(ME, TOOL));
    for t in [topic::SCAN_RESPONSE, topic::LEASE] {
        assert!(m.subscriptions().contains(&t), "{t} missing");
    }
}
