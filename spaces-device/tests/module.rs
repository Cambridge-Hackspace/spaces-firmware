//! The rules a module must never get wrong, each pinned by a test.
//!
//! Time is whatever these tests say it is: every call takes the current
//! monotonic milliseconds, which is what makes a lease expiring testable.

use serde_json::{json, Value};
use spaces_device::wire::topic;
use spaces_device::{Action, Config, Module, Output};

const ME: &str = "dev-uuid-me";
const TOOL: &str = "laser-01";
const ALICE: &str = "ALICE-CARD";
const BOB: &str = "BOB-CARD";

fn module() -> Module {
    let mut m = Module::new(Config::new(ME, TOOL));
    m.start(0);
    m
}

fn published(actions: &[Action], name: &str) -> Vec<Value> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Publish { topic, payload } if *topic == name => {
                Some(serde_json::from_str(payload).expect("published JSON"))
            }
            _ => None,
        })
        .collect()
}

fn publishes_to(actions: &[Action]) -> Vec<&'static str> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Publish { topic, .. } => Some(*topic),
            _ => None,
        })
        .filter(|t| *t != topic::POWER_REPORT)
        .collect()
}

fn yes() -> Vec<u8> {
    json!({"authorized": true, "reason": "authorized"})
        .to_string()
        .into_bytes()
}

fn lease(device: &str, grant: bool, ttl_ms: u64) -> Vec<u8> {
    json!({"tool_id": "7c9e6679-uuid", "device_id": device, "grant": grant, "ttl_ms": ttl_ms})
        .to_string()
        .into_bytes()
}

/// Alice swipes at t=0, is authorized at t=100, leased at t=200 for 3 s.
fn alice_running_tool() -> Module {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::TOOL_ON_RESPONSE, &yes(), 100);
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(m.tool_is_on(), "setup: Alice's tool should be on");
    m
}

// ---------------------------------------------------------------- start-up

#[test]
fn start_switches_both_outputs_off_and_says_hello() {
    let mut m = Module::new(Config::new(ME, TOOL));
    let out = m.start(0);
    assert!(out.contains(&Action::Set {
        output: Output::Tool,
        on: false
    }));
    assert!(out.contains(&Action::Set {
        output: Output::Running,
        on: false
    }));
    let reports = published(&out, topic::POWER_REPORT);
    assert_eq!(reports.len(), 1, "the first heartbeat goes out at once");
    assert_eq!(reports[0]["device_id"], ME);
    assert_eq!(reports[0]["relay_on"], false);
}

// ------------------------------------------------- rule 1: yes AND a lease

#[test]
fn a_swipe_asks_the_edge_with_the_card_and_tool() {
    let mut m = module();
    let out = m.swipe(ALICE, 0);
    assert_eq!(
        published(&out, topic::TOOL_ON_REQUEST),
        vec![json!({"card": ALICE, "tool_id": TOOL})]
    );
    assert!(!m.tool_is_on());
}

#[test]
fn a_yes_alone_does_not_switch_on() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::TOOL_ON_RESPONSE, &yes(), 100);
    assert!(!m.tool_is_on(), "authorized but not yet leased");
}

#[test]
fn a_yes_and_a_lease_switch_on() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::TOOL_ON_RESPONSE, &yes(), 100);
    let out = m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(out.contains(&Action::Set {
        output: Output::Tool,
        on: true
    }));
    assert!(m.tool_is_on());
}

#[test]
fn a_lease_alone_does_not_switch_on() {
    // A lease is permission to stay on, not to come on.
    let mut m = module();
    m.message(topic::LEASE, &lease(ME, true, 3000), 0);
    assert!(!m.tool_is_on());
}

#[test]
fn a_yes_with_no_lease_gives_up_and_releases_the_tool() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::TOOL_ON_RESPONSE, &yes(), 100);
    let out = m.tick(100 + 5000);
    assert!(!m.tool_is_on());
    assert_eq!(
        publishes_to(&out),
        vec![topic::TOOL_LOG_REQUEST, topic::TOOL_OFF_REQUEST],
        "the session the edge opened is closed again"
    );
}

// ------------------------------------------- rule 2: only an explicit yes

#[test]
fn ok_without_a_yes_is_a_no() {
    // The fail-open reading FIRMWARE.md warns about.
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::TOOL_ON_RESPONSE, br#"{"status":"ok"}"#, 100);
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(!m.tool_is_on());
}

#[test]
fn an_explicit_refusal_is_a_no() {
    let mut m = module();
    m.swipe(BOB, 0);
    let out = m.message(
        topic::TOOL_ON_RESPONSE,
        br#"{"authorized":false,"reason":"Unknown card"}"#,
        100,
    );
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(!m.tool_is_on());
    assert!(out
        .iter()
        .any(|a| matches!(a, Action::Note(n) if n.contains("Unknown card"))));
}

#[test]
fn the_documented_envelope_is_understood_too() {
    // FIRMWARE.md describes the server's envelope on this topic; the edge sends
    // {authorized}. Either form's explicit yes works.
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(
        topic::TOOL_ON_RESPONSE,
        br#"{"status":"ok","tool_on":true}"#,
        100,
    );
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(m.tool_is_on());
}

#[test]
fn a_refusal_in_the_documented_envelope_is_a_no() {
    let mut m = module();
    m.swipe(BOB, 0);
    m.message(
        topic::TOOL_ON_RESPONSE,
        br#"{"status":"error","message":"Training required","tool_on":false}"#,
        100,
    );
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(!m.tool_is_on());
}

#[test]
fn any_explicit_no_beats_a_yes() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(
        topic::TOOL_ON_RESPONSE,
        br#"{"authorized":true,"tool_on":false}"#,
        100,
    );
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(!m.tool_is_on());
}

#[test]
fn an_answer_that_does_not_parse_is_a_no() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::TOOL_ON_RESPONSE, b"yes please", 100);
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(!m.tool_is_on());
}

#[test]
fn no_answer_is_a_no() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.tick(5000);
    m.message(topic::LEASE, &lease(ME, true, 3000), 5100);
    assert!(!m.tool_is_on());
}

#[test]
fn a_late_yes_after_giving_up_is_released_not_used() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.tick(5000); // gave up
    let out = m.message(topic::TOOL_ON_RESPONSE, &yes(), 6000);
    assert_eq!(
        published(&out, topic::TOOL_OFF_REQUEST),
        vec![json!({"card": ALICE, "tool_id": TOOL})]
    );
    m.message(topic::LEASE, &lease(ME, true, 3000), 6100);
    assert!(!m.tool_is_on());
}

#[test]
fn an_answer_nobody_asked_for_is_not_ours() {
    // With two modules on a broker, this is the other module's answer.
    let mut m = module();
    let out = m.message(topic::TOOL_ON_RESPONSE, &yes(), 100);
    assert!(publishes_to(&out).is_empty());
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(!m.tool_is_on());
}

// ------------------------------------------------- rule 3: the lease timer

#[test]
fn the_tool_goes_off_by_itself_when_the_lease_runs_out() {
    // No message arrives. Silence is the instruction.
    let mut m = alice_running_tool();
    m.tick(200 + 2999);
    assert!(m.tool_is_on(), "still inside the lease");
    let out = m.tick(200 + 3000);
    assert!(out.contains(&Action::Set {
        output: Output::Tool,
        on: false
    }));
    assert!(!m.tool_is_on());
}

#[test]
fn the_lease_is_timed_from_its_arrival() {
    let mut m = alice_running_tool();
    m.message(topic::LEASE, &lease(ME, true, 3000), 2000); // renewed
    m.tick(4999);
    assert!(m.tool_is_on(), "renewal moved the deadline to 5000");
    m.tick(5000);
    assert!(!m.tool_is_on());
}

#[test]
fn the_ttl_is_read_from_each_lease() {
    let mut m = alice_running_tool();
    m.message(topic::LEASE, &lease(ME, true, 500), 1000);
    m.tick(1500);
    assert!(
        !m.tool_is_on(),
        "a shorter TTL is honoured, not a hard-coded one"
    );
}

#[test]
fn grant_false_switches_off_at_once() {
    let mut m = alice_running_tool();
    let out = m.message(topic::LEASE, &lease(ME, false, 3000), 500);
    assert!(out.contains(&Action::Set {
        output: Output::Tool,
        on: false
    }));
    assert!(!m.tool_is_on());
}

#[test]
fn another_modules_lease_neither_starts_nor_keeps_this_one() {
    let mut m = alice_running_tool();
    m.message(topic::LEASE, &lease("someone-else", true, 60_000), 1000);
    m.tick(3200);
    assert!(!m.tool_is_on());
}

#[test]
fn a_lease_naming_the_tool_by_uuid_is_still_ours() {
    // The edge sends the tool's UUID, not the external id FIRMWARE.md shows.
    // Leases are matched on device_id, so either works.
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::TOOL_ON_RESPONSE, &yes(), 100);
    let uuid_lease = json!({"tool_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7", "device_id": ME,
               "grant": true, "ttl_ms": 3000});
    m.message(topic::LEASE, uuid_lease.to_string().as_bytes(), 200);
    assert!(m.tool_is_on());
}

#[test]
fn a_lease_that_does_not_parse_grants_nothing() {
    let mut m = alice_running_tool();
    // Missing ttl_ms: not a renewal.
    m.message(
        topic::LEASE,
        &json!({"device_id": ME, "grant": true})
            .to_string()
            .into_bytes(),
        1000,
    );
    m.tick(3200);
    assert!(!m.tool_is_on());
}

#[test]
fn a_zero_ttl_is_an_immediate_expiry() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::TOOL_ON_RESPONSE, &yes(), 100);
    m.message(topic::LEASE, &lease(ME, true, 0), 200);
    assert!(!m.tool_is_on());
}

// ---------------------------------------- rule 4: a lapse ends the session

#[test]
fn a_renewal_after_the_lease_lapsed_does_not_switch_back_on() {
    let mut m = alice_running_tool();
    m.tick(3200); // lapsed
    m.message(topic::LEASE, &lease(ME, true, 3000), 3300);
    assert!(!m.tool_is_on(), "a fresh swipe is needed");
}

#[test]
fn a_lapse_reports_and_releases_like_a_stop() {
    let mut m = alice_running_tool();
    let out = m.tick(3200);
    assert_eq!(
        publishes_to(&out),
        vec![topic::TOOL_LOG_REQUEST, topic::TOOL_OFF_REQUEST]
    );
    assert_eq!(published(&out, topic::TOOL_OFF_REQUEST)[0]["card"], ALICE);
}

// ---------------------------------------------- rule 5: every stop is clean

#[test]
fn tool_off_switches_off_then_reports_then_releases() {
    let mut m = alice_running_tool();
    let out = m.tool_off(1000);
    let first_publish = out
        .iter()
        .position(|a| matches!(a, Action::Publish { .. }))
        .unwrap();
    let switched_off = out
        .iter()
        .position(|a| {
            *a == Action::Set {
                output: Output::Tool,
                on: false,
            }
        })
        .expect("tool switched off");
    assert!(
        switched_off < first_publish,
        "outputs go off before anything is sent"
    );
    assert_eq!(
        publishes_to(&out),
        vec![topic::TOOL_LOG_REQUEST, topic::TOOL_OFF_REQUEST]
    );
}

#[test]
fn the_session_is_closed_with_the_card_that_opened_it() {
    let mut m = alice_running_tool();
    m.swipe(BOB, 500); // ignored: in use
    let out = m.tool_off(1000);
    assert_eq!(published(&out, topic::TOOL_LOG_REQUEST)[0]["card"], ALICE);
    assert_eq!(published(&out, topic::TOOL_OFF_REQUEST)[0]["card"], ALICE);
}

#[test]
fn a_second_swipe_during_a_session_sends_nothing() {
    // Its answer would be indistinguishable from any other on the broker.
    let mut m = alice_running_tool();
    let out = m.swipe(BOB, 500);
    assert!(publishes_to(&out).is_empty());
    assert!(m.tool_is_on(), "Alice's session carries on");
}

#[test]
fn a_second_swipe_while_waiting_sends_nothing() {
    let mut m = module();
    m.swipe(ALICE, 0);
    let out = m.swipe(BOB, 50);
    assert!(publishes_to(&out).is_empty());
}

#[test]
fn tool_off_with_no_session_sends_nothing() {
    let mut m = module();
    assert!(publishes_to(&m.tool_off(0)).is_empty());
}

#[test]
fn after_a_session_ends_the_next_person_can_swipe() {
    let mut m = alice_running_tool();
    m.tool_off(1000);
    let out = m.swipe(BOB, 1100);
    assert_eq!(published(&out, topic::TOOL_ON_REQUEST)[0]["card"], BOB);
}

// ------------------------------------------------- rule 6: the heartbeat

#[test]
fn a_power_report_goes_out_every_interval() {
    let mut m = module(); // reported at 0
    assert!(published(&m.tick(999), topic::POWER_REPORT).is_empty());
    assert_eq!(published(&m.tick(1000), topic::POWER_REPORT).len(), 1);
    assert!(published(&m.tick(1500), topic::POWER_REPORT).is_empty());
    assert_eq!(published(&m.tick(2000), topic::POWER_REPORT).len(), 1);
}

#[test]
fn the_power_report_tells_the_truth_about_the_relay_and_the_draw() {
    let mut m = alice_running_tool();
    let r = &published(&m.tick(1000), topic::POWER_REPORT)[0];
    assert_eq!(
        (r["relay_on"].clone(), r["draw_now"].clone()),
        (json!(true), json!("0.3"))
    );
    m.running(true, 1100);
    let r = &published(&m.tick(2000), topic::POWER_REPORT)[0];
    assert_eq!(r["draw_now"], "4.2");
    assert_eq!(r["device_id"], ME);
    assert_eq!(r["tool_id"], TOOL);
}

#[test]
fn acknowledgements_change_nothing() {
    // An ack is not approval to stay on.
    let mut m = alice_running_tool();
    m.message(topic::POWER_RESPONSE, br#"{"ok":true}"#, 3000);
    m.message(topic::TOOL_OFF_RESPONSE, br#"{"ok":true}"#, 3000);
    m.tick(3200);
    assert!(!m.tool_is_on());
}

// ------------------------------------------------- rule 7: metering

#[test]
fn running_needs_the_tool_on() {
    let mut m = module();
    m.running(true, 0);
    assert!(
        !m.running_is_on(),
        "a machine cannot run with its power off"
    );
}

#[test]
fn running_follows_the_switch_while_the_tool_is_on() {
    let mut m = alice_running_tool();
    m.running(true, 300);
    assert!(m.running_is_on());
    m.running(false, 400);
    assert!(!m.running_is_on());
}

#[test]
fn only_running_time_is_reported_as_usage() {
    let mut m = alice_running_tool(); // session open from 100, on from 200
    m.message(topic::LEASE, &lease(ME, true, 3000), 1000);
    m.running(true, 1000);
    m.running(false, 2500); // 1.5 s
    m.message(topic::LEASE, &lease(ME, true, 3000), 3000);
    m.running(true, 3000);
    m.running(false, 4000); // 1.0 s
    let out = m.tool_off(5000);
    assert_eq!(
        published(&out, topic::TOOL_LOG_REQUEST)[0]["seconds"],
        json!(2.5)
    );
}

#[test]
fn a_run_still_going_at_tool_off_is_counted() {
    let mut m = alice_running_tool();
    m.running(true, 1000);
    let out = m.tool_off(2000);
    assert_eq!(
        published(&out, topic::TOOL_LOG_REQUEST)[0]["seconds"],
        json!(1.0)
    );
}

#[test]
fn the_count_stops_the_moment_the_lease_lapses() {
    let mut m = alice_running_tool(); // leased until 3200
    m.running(true, 1200);
    let out = m.tick(9000); // noticed late; the cut was at 3200
    assert!(!m.running_is_on());
    assert_eq!(
        published(&out, topic::TOOL_LOG_REQUEST)[0]["seconds"],
        json!(2.0),
        "usage ends at the lease's end, not when the expiry was noticed"
    );
}

#[test]
fn running_goes_off_before_the_tool_and_on_after_it() {
    let mut m = alice_running_tool();
    m.running(true, 300);
    let out = m.tool_off(1000);
    let running_off = out
        .iter()
        .position(|a| {
            *a == Action::Set {
                output: Output::Running,
                on: false,
            }
        })
        .unwrap();
    let tool_off = out
        .iter()
        .position(|a| {
            *a == Action::Set {
                output: Output::Tool,
                on: false,
            }
        })
        .unwrap();
    assert!(running_off < tool_off);
}
