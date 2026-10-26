//! The rules a module must never get wrong, each pinned by a test.
//!
//! Time is whatever these tests say it is: every call takes the current
//! monotonic milliseconds, which is what makes a lease expiring testable.

use serde_json::{json, Value};
use spaces_device::wire::topic;
use spaces_device::{Action, Activity, Config, EndReason, Module, Outcome, Output, Usage};

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
        // Both heartbeats are filtered out, so assertions about the protocol's
        // decisions are not drowned by the two messages every tick sends. The
        // heartbeats have their own arms below.
        .filter(|t| *t != topic::POWER_REPORT && *t != topic::ALIVE_REQUEST)
        .collect()
}

fn yes() -> Vec<u8> {
    json!({"authorized": true, "reason": "authorized"})
        .to_string()
        .into_bytes()
}

fn lease(device: &str, grant: bool, ttl_ms: u64) -> Vec<u8> {
    json!({"resource": "7c9e6679-uuid", "device_id": device, "grant": grant, "ttl_ms": ttl_ms})
        .to_string()
        .into_bytes()
}

/// Alice swipes at t=0, is authorized at t=100, leased at t=200 for 3 s.
fn alice_running_tool() -> Module {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(m.tool_is_on(), "setup: Alice's tool should be on");
    m
}

/// The same, on a box that declares a sensor. The metering arms drive a
/// running input, which only a device with a sensor has; on one without, the
/// billable figure is the time the power was on (CS:S #174), not the input.
fn alice_running_sensed_tool() -> Module {
    let mut c = Config::new(ME, TOOL);
    c.senses_running = true;
    let mut m = Module::new(c);
    m.start(0);
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
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
        published(&out, topic::SCAN_REQUEST),
        vec![json!({"resource": TOOL, "card": ALICE})]
    );
    assert!(!m.tool_is_on());
}

#[test]
fn a_yes_alone_does_not_switch_on() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    assert!(!m.tool_is_on(), "authorized but not yet leased");
}

#[test]
fn a_yes_and_a_lease_switch_on() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
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
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    let out = m.tick(100 + 5000);
    assert!(!m.tool_is_on());
    assert_eq!(
        publishes_to(&out),
        vec![topic::USAGE_REQUEST, topic::STOP_REQUEST],
        "the session the edge opened is closed again"
    );
}

// ------------------------------------------- rule 2: only an explicit yes

#[test]
fn ok_without_a_yes_is_a_no() {
    // The fail-open reading FIRMWARE.md warns about.
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, br#"{"status":"ok"}"#, 100);
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(!m.tool_is_on());
}

#[test]
fn an_explicit_refusal_is_a_no() {
    let mut m = module();
    m.swipe(BOB, 0);
    let out = m.message(
        topic::SCAN_RESPONSE,
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
        topic::SCAN_RESPONSE,
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
        topic::SCAN_RESPONSE,
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
        topic::SCAN_RESPONSE,
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
    m.message(topic::SCAN_RESPONSE, b"yes please", 100);
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
    let out = m.message(topic::SCAN_RESPONSE, &yes(), 6000);
    assert_eq!(
        published(&out, topic::STOP_REQUEST),
        vec![json!({"resource": TOOL, "card": ALICE})]
    );
    m.message(topic::LEASE, &lease(ME, true, 3000), 6100);
    assert!(!m.tool_is_on());
}

#[test]
fn an_answer_nobody_asked_for_is_not_ours() {
    // With two modules on a broker, this is the other module's answer.
    let mut m = module();
    let out = m.message(topic::SCAN_RESPONSE, &yes(), 100);
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
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    let uuid_lease = json!({"resource": "7c9e6679-7425-40de-944b-e07fc1f90ae7", "device_id": ME,
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
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
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
        vec![topic::USAGE_REQUEST, topic::STOP_REQUEST]
    );
    assert_eq!(published(&out, topic::STOP_REQUEST)[0]["card"], ALICE);
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
        vec![topic::USAGE_REQUEST, topic::STOP_REQUEST]
    );
}

#[test]
fn the_session_is_closed_with_the_card_that_opened_it() {
    let mut m = alice_running_tool();
    m.swipe(BOB, 500); // ignored: in use
    let out = m.tool_off(1000);
    assert_eq!(published(&out, topic::USAGE_REQUEST)[0]["card"], ALICE);
    assert_eq!(published(&out, topic::STOP_REQUEST)[0]["card"], ALICE);
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
fn tool_off_with_no_session_still_releases_the_resource_without_a_card() {
    // A stop is a safety act (CS:S #179, #182): whatever device the button is
    // on, and whether or not this device holds a session, the stop goes out
    // naming the resource alone, and the edge and the server end whatever
    // session is open on it -- a door box's stop button is how the room it
    // opened is closed. A stop that lands on nothing is harmless. Until 0.4 a
    // reader with no session of its own sent nothing.
    let mut m = module();
    let out = m.tool_off(0);
    assert_eq!(publishes_to(&out), vec![topic::STOP_REQUEST]);
    let stop = &published(&out, topic::STOP_REQUEST)[0];
    assert_eq!(stop["resource"], TOOL);
    assert!(
        stop.get("card").is_none(),
        "named a card it does not have: {stop}"
    );
}

#[test]
fn after_a_session_ends_the_next_person_can_swipe() {
    let mut m = alice_running_tool();
    m.tool_off(1000);
    let out = m.swipe(BOB, 1100);
    assert_eq!(published(&out, topic::SCAN_REQUEST)[0]["card"], BOB);
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
        (json!(true), json!(0.3))
    );
    m.running(true, 1100);
    let r = &published(&m.tick(2000), topic::POWER_REPORT)[0];
    assert_eq!(r["draw_now"], 4.2);
    assert_eq!(r["device_id"], ME);
    assert_eq!(r["resource"], TOOL);
}

#[test]
fn acknowledgements_change_nothing() {
    // An ack is not approval to stay on.
    let mut m = alice_running_tool();
    m.message(topic::POWER_RESPONSE, br#"{"ok":true}"#, 3000);
    m.message(topic::STOP_RESPONSE, br#"{"ok":true}"#, 3000);
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
    let mut m = alice_running_sensed_tool(); // session open from 100, on from 200
    m.message(topic::LEASE, &lease(ME, true, 3000), 1000);
    m.running(true, 1000);
    m.running(false, 2500); // 1.5 s
    m.message(topic::LEASE, &lease(ME, true, 3000), 3000);
    m.running(true, 3000);
    m.running(false, 4000); // 1.0 s
    let out = m.tool_off(5000);
    assert_eq!(
        published(&out, topic::USAGE_REQUEST)[0]["seconds"],
        json!(2.5)
    );
}

#[test]
fn a_run_still_going_at_tool_off_is_counted() {
    let mut m = alice_running_sensed_tool();
    m.running(true, 1000);
    let out = m.tool_off(2000);
    assert_eq!(
        published(&out, topic::USAGE_REQUEST)[0]["seconds"],
        json!(1.0)
    );
}

#[test]
fn the_count_stops_the_moment_the_lease_lapses() {
    let mut m = alice_running_sensed_tool(); // leased until 3200
    m.running(true, 1200);
    let out = m.tick(9000); // noticed late; the cut was at 3200
    assert!(!m.running_is_on());
    assert_eq!(
        published(&out, topic::USAGE_REQUEST)[0]["seconds"],
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

// ------------------------------------------- whose answer is this? (CS:S #166)
//
// `toolguard/response/tool-on` is unnamespaced and names no device, so every
// module on a local broker receives every answer. Before the edge echoed
// `tool_id` there was nothing in the payload to match on, and the only defence
// was the reply window: accept an answer while exactly one request is
// outstanding. That narrows the race and cannot close it -- which is why
// README.md said "one module per broker, for now".
//
// These four arms are the two halves of the filter and its two fallbacks.

/// A yes that names a tool explicitly.
fn yes_for(tool: &str) -> Vec<u8> {
    json!({"resource": tool, "authorized": true, "reason": "authorized"})
        .to_string()
        .into_bytes()
}

#[test]
fn a_yes_for_another_tool_does_not_start_a_session_here() {
    let mut m = module();
    m.swipe(ALICE, 0);

    m.message(topic::SCAN_RESPONSE, &yes_for("drill-07"), 100);

    // THE LEASE IS WHAT MAKES THIS ARM DISCRIMINATE, and leaving it out made the
    // test a tautology -- caught by disabling the filter and watching it still
    // pass. `tool_is_on()` is false after a bare yes whatever the filter does,
    // because a session waits for a lease before energizing. So the stray yes
    // has to be followed by a lease: with the filter there is no session for the
    // lease to apply to, and without one the tool comes on for a swipe that
    // happened at another machine.
    let actions = m.message(topic::LEASE, &lease(ME, true, 3000), 200);

    assert!(
        !m.tool_is_on(),
        "a yes addressed to drill-07, plus a lease, switched this module's tool on \
         -- this is the defect: one broker, several readers, and a neighbour's \
         verdict energizing the wrong machine"
    );
    assert!(
        publishes_to(&actions).is_empty(),
        "a lease with no session of our own should publish nothing, got {:?}",
        publishes_to(&actions)
    );
}

#[test]
fn and_our_own_answer_still_arrives_afterwards() {
    // The control for the arm above, and the reason the phase must not change:
    // a neighbour's reply must not be treated as a refusal either, or their
    // swipe denies our member.
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes_for("drill-07"), 100);

    m.message(topic::SCAN_RESPONSE, &yes_for(TOOL), 200);
    m.message(topic::LEASE, &lease(ME, true, 3000), 300);

    assert!(
        m.tool_is_on(),
        "after ignoring a reply for another tool, our own yes was not honoured -- \
         the filter dropped the request instead of the stray answer"
    );
}

#[test]
fn a_yes_naming_our_tool_is_accepted() {
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes_for(TOOL), 100);
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(
        m.tool_is_on(),
        "a yes naming this module's own tool was refused"
    );
}

#[test]
fn a_yes_naming_no_tool_is_still_accepted() {
    // An edge predating CS:S #166 sends no `tool_id`. Refusing those would
    // brick every module against an edge that has not been updated, so the
    // absent case falls back to the reply window. Stated as a test because it
    // is a deliberate hole, not an oversight: see ToolOnReply::is_for.
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(
        m.tool_is_on(),
        "a reply with no tool_id was dropped, which would break every module \
         talking to an edge from before the echo existed"
    );
}

// ------------------------------------------------- the two usage figures (#169)
//
// A module that senses its machine reports two durations: the HELD figure, with
// gaps bridged, as `seconds`; and the RAW asserted time as `active_seconds`. The
// case they exist for is a laser, whose enable line drops on every scan-line
// return -- so on one engrave the two differ by 30-50% and only the server knows
// which one should bill.

/// A module that senses the machine, with the filter and reporter configured.
fn sensing(assert_ms: u32, hold_ms: u32, interval_ms: u64) -> Module {
    let mut c = Config::new(ME, TOOL);
    c.use_assert_ms = assert_ms;
    c.use_hold_ms = hold_ms;
    c.senses_running = true;
    c.usage_interval_ms = interval_ms;
    let mut m = Module::new(c);
    m.start(0);
    m
}

/// Alice authorized at 100 and leased at 200 for ten minutes, so the lease is
/// never what ends these tests.
fn alice_on(mut m: Module) -> Module {
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    m.message(topic::LEASE, &lease(ME, true, 600_000), 200);
    assert!(m.tool_is_on(), "setup: the tool should be on");
    m
}

fn logs(actions: &[Action]) -> Vec<Value> {
    published(actions, topic::USAGE_REQUEST)
}

#[test]
fn a_module_that_senses_nothing_omits_the_active_figure() {
    // The default. Absent and zero mean different things to the server: absent
    // falls back to the billable figure, zero bills nothing. A module with no
    // sensor reporting 0.0 would make every session on a working-time-billed
    // tool free, which is why this is a config flag and not an accumulator
    // that happens to be empty.
    let mut m = alice_running_tool();
    m.running(true, 1000);
    let out = m.tool_off(2000);
    let log = &logs(&out)[0];
    // Its billable figure is the time the power was on -- energized from 200
    // -- not the input it declares it has not got (CS:S #174).
    assert_eq!(log["seconds"], json!(1.8));
    assert!(
        log.get("active_seconds").is_none(),
        "a module with no sensor claimed to have measured the machine: {log}"
    );
}

#[test]
fn a_box_that_switches_power_but_senses_nothing_bills_the_time_the_power_was_on() {
    // "When the power is on, the bill goes to the tool" (CS:S #174). Before
    // this, a reader+relay box with no sensor reported 0.0 for every session
    // and a per-minute tool behind it was free.
    let mut m = alice_running_tool(); // energized from 200
    m.message(topic::LEASE, &lease(ME, true, 60_000), 1_000);
    let out = m.tool_off(5_200);
    let log = &logs(&out)[0];
    assert_eq!(log["seconds"], json!(5.0));
    assert!(log.get("active_seconds").is_none(), "{log}");
}

#[test]
fn a_bare_reader_with_no_sensor_still_reports_no_time() {
    // It neither switches the power nor senses the machine, so it has nothing
    // to measure; signed-in time is not usage.
    let mut m = alice_reading(no_power());
    let out = m.swipe(ALICE, 30_000); // second tap ends it
    assert_eq!(logs(&out)[0]["seconds"], json!(0.0));
}

#[test]
fn a_sensing_module_reports_both_figures() {
    let mut m = alice_on(sensing(0, 0, 0));
    m.running(true, 1000);
    let out = m.tool_off(2000);
    let log = &logs(&out)[0];
    assert_eq!(log["seconds"], json!(1.0));
    assert_eq!(
        log["active_seconds"],
        json!(1.0),
        "with no hold the two figures are the same, and both must be sent"
    );
}

/// The defect the whole feature exists for, as a waveform: four scan-line
/// passes with the beam off between them. The held figure is the job; the raw
/// figure is the beam.
#[test]
fn the_held_figure_bridges_the_scan_line_gaps_and_the_raw_one_does_not() {
    let mut m = alice_on(sensing(0, 5_000, 0));
    m.running(true, 1_000);
    m.running(false, 1_400); // pass: 400 ms
    m.running(true, 1_600); // gap: 200 ms, bridged
    m.running(false, 2_000); // pass: 400 ms
    let out = m.tool_off(3_000);
    let log = &logs(&out)[0];
    assert_eq!(
        log["seconds"],
        json!(2.0),
        "the held figure should span 1000..3000 as one job"
    );
    assert_eq!(
        log["active_seconds"],
        json!(0.8),
        "the raw figure should be the two 400 ms passes and nothing else"
    );
}

/// Without this the Running output strobes at scan-line rate and the accumulator
/// records a job as hundreds of fragments. Asserted on the output, because that
/// is the half a person at the machine sees.
#[test]
fn a_bridged_gap_does_not_strobe_the_running_output() {
    let mut m = alice_on(sensing(0, 5_000, 0));
    m.running(true, 1_000);
    let out = m.running(false, 1_400);
    assert!(
        !out.iter().any(|a| matches!(
            a,
            Action::Set {
                output: Output::Running,
                on: false
            }
        )),
        "the Running output was cut during a scan-line return: {out:?}"
    );
    assert!(m.running_is_on());
}

#[test]
fn a_hold_that_expires_with_no_further_sample_ends_usage() {
    // Nothing reports the end of a job: the signal simply stops. Without the
    // filter being ticked on the way through `advance`, usage would run until
    // somebody next touched the tool.
    let mut m = alice_on(sensing(0, 1_000, 0));
    m.running(true, 1_000);
    m.running(false, 2_000);
    m.tick(2_999);
    assert!(m.running_is_on(), "the hold ended early");
    m.tick(3_000);
    assert!(!m.running_is_on(), "the hold never expired");
    let out = m.tool_off(4_000);
    assert_eq!(
        logs(&out)[0]["seconds"],
        json!(2.0),
        "usage should end when the hold did, not when the tool was released"
    );
}

#[test]
fn periodic_reports_are_increments_that_sum_to_the_total_exactly_once() {
    // Reports accumulate server-side, so a module that re-sent the running
    // total would be billed the sum of a series instead of its last term.
    let mut m = alice_on(sensing(0, 0, 1_000));
    let mut all: Vec<Value> = Vec::new();
    m.running(true, 1_000);
    for t in [2_000, 3_000] {
        all.extend(logs(&m.tick(t)));
    }
    m.running(false, 3_500);
    all.extend(logs(&m.tool_off(4_000)));

    assert_eq!(
        all.len(),
        3,
        "expected two periodic reports and a final one"
    );
    let sum: f64 = all.iter().map(|l| l["seconds"].as_f64().unwrap()).sum();
    assert!(
        (sum - 2.5).abs() < 1e-9,
        "the increments sum to {sum}, not the 2.5 s the machine ran: {all:?}"
    );
    let active: f64 = all
        .iter()
        .map(|l| l["active_seconds"].as_f64().unwrap())
        .sum();
    assert!(
        (active - 2.5).abs() < 1e-9,
        "the active increments sum to {active}, not 2.5: {all:?}"
    );
}

#[test]
fn the_final_report_carries_only_what_the_periodic_ones_did_not() {
    // The other side of the same claim: proving the sum is right does not prove
    // the last report is an increment rather than a coincidence.
    //
    // The timings matter. `running(false)` and `tool_off` both run `advance`,
    // so either can publish a due increment -- these are chosen so the interval
    // is NOT due at those calls and the remainder can only come from `end`.
    let mut m = alice_on(sensing(0, 0, 1_000));
    m.running(true, 1_000);
    let periodic = logs(&m.tick(2_000));
    assert_eq!(periodic[0]["seconds"], json!(1.0), "{periodic:?}");
    m.running(false, 2_500); // 1.5 s run, 0.5 s of it unreported
    let out = m.tool_off(2_600);
    assert_eq!(
        logs(&out)[0]["seconds"],
        json!(0.5),
        "the final report resent time the periodic one had already reported"
    );
}

#[test]
fn a_session_that_reported_everything_already_does_not_add_a_zero() {
    // With a periodic report landing exactly at the end there is nothing left
    // to say. Sending the zero would be harmless to the server's accumulation
    // and pure noise on the broker, so it is suppressed -- but only because
    // something was reported. See the test below for the other case.
    let mut m = alice_on(sensing(0, 0, 1_000));
    m.running(true, 1_000);
    m.running(false, 2_000);
    let reported = logs(&m.tick(3_000));
    assert_eq!(reported.len(), 1, "setup: expected one periodic report");
    let out = m.tool_off(3_100);
    assert!(
        logs(&out).is_empty(),
        "a zero-increment report was appended after the periodic one: {:?}",
        logs(&out)
    );
    assert_eq!(
        publishes_to(&out),
        vec![topic::STOP_REQUEST],
        "the tool must still be released"
    );
}

#[test]
fn a_session_that_never_ran_still_reports_once() {
    // Rule 5: every session that started is reported. A tool authorized and
    // never used is a fact worth recording, so the zero here is the report --
    // not a suppressed duplicate.
    let mut m = alice_on(sensing(0, 0, 1_000));
    let out = m.tool_off(500);
    assert_eq!(
        publishes_to(&out),
        vec![topic::USAGE_REQUEST, topic::STOP_REQUEST]
    );
    assert_eq!(logs(&out)[0]["seconds"], json!(0.0));
}

#[test]
fn an_authorized_but_idle_tool_reports_no_usage_at_all() {
    // The interval is due repeatedly, and there is nothing to say. An empty
    // report is noise on the broker and a row in the audit log.
    let mut m = alice_on(sensing(0, 0, 1_000));
    let mut all: Vec<Value> = Vec::new();
    for t in [2_000, 3_000, 4_000] {
        all.extend(logs(&m.tick(t)));
    }
    assert!(
        all.is_empty(),
        "an idle tool published usage it never accrued: {all:?}"
    );
}

#[test]
fn a_glitch_on_the_sense_line_is_not_billed() {
    // 50 ms of noise against a 200 ms allowance. The HV supply next to a
    // laser's enable line is not quiet.
    let mut m = alice_on(sensing(200, 0, 0));
    m.running(true, 1_000);
    m.running(false, 1_050);
    let out = m.tool_off(2_000);
    let log = &logs(&out)[0];
    assert_eq!(log["seconds"], json!(0.0));
    assert_eq!(log["active_seconds"], json!(0.0));
}

// ------------------------------------ a device that switches nothing (roles)
//
// CS:S binds a device to a resource in a role, and the roles are independent:
// a box may read cards, switch power, sense the machine, show a panel, or any
// combination. `gates_power` is the one that changes the protocol, because the
// edge offers leases only to `power` bindings -- so a device that switches
// nothing is never leased and must not wait for one.

fn no_power() -> Module {
    let mut m = Module::new(Config::switches_nothing(ME, TOOL));
    m.start(0);
    m
}

/// Alice is authorized at t=100. No lease, because none is ever coming.
fn alice_reading(mut m: Module) -> Module {
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    m
}

/// THE DEFECT THIS EXISTS FOR. Built as a power module, a badge reader ends
/// every authorized scan `first_lease_timeout_ms` later with `NoFirstLease`:
/// the member watches ACCESS GRANTED turn into a released session, for a lease
/// that was never going to arrive.
#[test]
fn a_device_that_switches_nothing_does_not_wait_for_a_lease() {
    let mut m = alice_reading(no_power());
    let out = m.tick(100 + 60_000); // twelve times the lease timeout
    assert!(
        publishes_to(&out).is_empty(),
        "the session was released waiting for a lease it can never get: {out:?}"
    );
}

#[test]
fn a_power_module_still_gives_up_when_no_lease_arrives() {
    // The other side of the same claim: the arm above must not have been
    // removed, only scoped. A power module with no lease is a module that
    // cannot guarantee the tool is safe.
    let mut m = module();
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    let out = m.tick(100 + 5_000);
    assert_eq!(
        publishes_to(&out),
        vec![topic::USAGE_REQUEST, topic::STOP_REQUEST]
    );
}

#[test]
fn a_device_that_switches_nothing_drives_no_tool_output() {
    let mut m = Module::new(Config::switches_nothing(ME, TOOL));
    let boot = m.start(0);
    assert!(
        !boot.iter().any(|a| matches!(
            a,
            Action::Set {
                output: Output::Tool,
                ..
            }
        )),
        "a device with no relay claimed one at boot: {boot:?}"
    );
    let out = alice_reading(m).tick(200);
    assert!(
        !out.iter().any(|a| matches!(
            a,
            Action::Set {
                output: Output::Tool,
                ..
            }
        )),
        "a device with no relay tried to energize a tool: {out:?}"
    );
}

#[test]
fn a_device_that_switches_nothing_reports_no_relay_state_and_no_draw() {
    // `None` is "cannot tell". A `false` and a `"0.0"` from a device with no
    // relay are two positive claims about a circuit it cannot see, and the
    // bypass detector believes both (CS:S #84).
    let mut m = Module::new(Config::switches_nothing(ME, TOOL));
    let out = m.start(0);
    let report = &published(&out, topic::POWER_REPORT)[0];
    assert_eq!(
        report["device_id"], ME,
        "the heartbeat must still identify it"
    );
    assert!(
        report.get("relay_on").is_none(),
        "claimed a relay state: {report}"
    );
    assert!(report.get("draw_now").is_none(), "claimed a draw: {report}");
}

#[test]
fn usage_accrues_without_a_lease_when_there_is_nothing_to_energize() {
    // A reader with a sensor is installed to measure the machine. Gating its
    // accumulator on a lease it can never get would record nothing, ever.
    let mut c = Config::switches_nothing(ME, TOOL);
    c.senses_running = true;
    let mut m = Module::new(c);
    m.start(0);
    let mut m = alice_reading(m);
    m.running(true, 1_000);
    m.running(false, 3_000);
    let out = m.swipe(ALICE, 4_000); // second tap ends it
    let log = &published(&out, topic::USAGE_REQUEST)[0];
    assert_eq!(log["seconds"], json!(2.0));
    assert_eq!(log["active_seconds"], json!(2.0));
}

// ------------------------------------------------- stops a reader can reach

#[test]
fn the_same_card_again_ends_the_session() {
    // The only stop available to a device that neither switches the tool nor
    // senses it. Without it `tool-on` opens a session nothing ever closes, and
    // with billing off the server sweeps nothing either.
    let mut m = alice_reading(no_power());
    let out = m.swipe(ALICE, 5_000);
    assert_eq!(
        publishes_to(&out),
        vec![topic::USAGE_REQUEST, topic::STOP_REQUEST],
        "a second tap did not release the tool"
    );
    assert_eq!(
        published(&out, topic::STOP_REQUEST)[0]["card"],
        ALICE,
        "released with the wrong card"
    );
}

#[test]
fn somebody_elses_card_does_not_end_the_session() {
    // Ending a stranger's session is not a courtesy. Note the asymmetry with
    // `tool-off`, which CS:S deliberately accepts from any card so anyone can
    // cut power for safety -- that is a different act from signing off.
    let mut m = alice_reading(no_power());
    let out = m.swipe(BOB, 5_000);
    assert!(
        publishes_to(&out).is_empty(),
        "Bob's tap ended Alice's session: {out:?}"
    );
}

#[test]
fn a_second_tap_is_ignored_where_it_is_not_configured() {
    // A tool with a physical stop button does not want a re-tap to mean "done"
    // -- it means "somebody swiped again", which is what the old note says.
    let mut c = Config::switches_nothing(ME, TOOL);
    c.stop_on_second_tap = false;
    let mut m = Module::new(c);
    m.start(0);
    let mut m = alice_reading(m);
    let out = m.swipe(ALICE, 5_000);
    assert!(publishes_to(&out).is_empty(), "{out:?}");
}

#[test]
fn a_machine_left_idle_ends_the_session() {
    let mut c = Config::switches_nothing(ME, TOOL);
    c.senses_running = true;
    c.idle_end_ms = 10_000;
    let mut m = Module::new(c);
    m.start(0);
    let mut m = alice_reading(m);
    m.running(true, 1_000);
    m.running(false, 2_000);
    assert!(
        publishes_to(&m.tick(11_999)).is_empty(),
        "ended before the idle timeout"
    );
    let out = m.tick(12_000);
    assert_eq!(
        publishes_to(&out),
        vec![topic::USAGE_REQUEST, topic::STOP_REQUEST],
        "the idle timeout never fired"
    );
    assert_eq!(
        published(&out, topic::USAGE_REQUEST)[0]["seconds"],
        json!(1.0),
        "the idle time was billed as usage"
    );
}

#[test]
fn an_idle_timeout_is_ignored_when_nothing_senses_the_machine() {
    // With no sensor the machine is never "working", so an idle timeout would
    // end every session the moment it opened -- a tool nobody could use. The
    // guard is on `senses_running`, not merely on the interval being set.
    let mut c = Config::switches_nothing(ME, TOOL);
    c.idle_end_ms = 10_000;
    c.senses_running = false;
    let mut m = Module::new(c);
    m.start(0);
    let mut m = alice_reading(m);
    let out = m.tick(60_000);
    assert!(
        publishes_to(&out).is_empty(),
        "a device with no sensor idled its own session out: {out:?}"
    );
}

#[test]
fn the_maximum_session_is_a_backstop_whatever_else_is_true() {
    // Nobody re-tapped, nothing sensed the machine, no lease lapsed. Without
    // this a reader-only device can still strand a tool indefinitely.
    let mut c = Config::switches_nothing(ME, TOOL);
    c.max_session_ms = 30_000;
    let mut m = Module::new(c);
    m.start(0);
    let mut m = alice_reading(m);
    assert!(publishes_to(&m.tick(30_099)).is_empty(), "ended early");
    let out = m.tick(30_100);
    assert_eq!(
        publishes_to(&out),
        vec![topic::USAGE_REQUEST, topic::STOP_REQUEST]
    );
}

// ------------------------------------------------- a plug with no reader

fn plug() -> Module {
    let mut m = Module::new(Config::power_only(ME, TOOL));
    m.start(0);
    m
}

/// For a plug the lease IS the authorization, not merely permission to stay on.
/// The edge leases a tool only once its access chain is satisfied, having
/// consulted whatever read the badge -- #83's coordinator model.
#[test]
fn a_plug_energizes_on_a_lease_alone() {
    let mut m = plug();
    let out = m.message(topic::LEASE, &lease(ME, true, 3_000), 100);
    assert!(out.contains(&Action::Set {
        output: Output::Tool,
        on: true
    }));
    assert!(m.tool_is_on());
}

#[test]
fn a_device_with_a_reader_still_refuses_a_lease_without_a_session() {
    // The other side: for a device that owns its session a lease is permission
    // to STAY closed, not to close. This arm must be scoped, not deleted.
    let mut m = module();
    m.message(topic::LEASE, &lease(ME, true, 3_000), 0);
    assert!(!m.tool_is_on());
}

#[test]
fn a_plug_switches_off_when_its_lease_runs_out() {
    // Nobody sends a message when a lease expires, by design -- the renewals
    // stopping IS the instruction.
    let mut m = plug();
    m.message(topic::LEASE, &lease(ME, true, 3_000), 100);
    assert!(m.tool_is_on());
    let out = m.tick(3_100);
    assert!(out.contains(&Action::Set {
        output: Output::Tool,
        on: false
    }));
    assert!(!m.tool_is_on());
}

#[test]
fn a_plug_switches_off_on_a_withdrawn_lease() {
    let mut m = plug();
    m.message(topic::LEASE, &lease(ME, true, 60_000), 100);
    let out = m.message(topic::LEASE, &lease(ME, false, 0), 200);
    assert!(out.contains(&Action::Set {
        output: Output::Tool,
        on: false
    }));
    assert!(!m.tool_is_on());
}

#[test]
fn a_plug_ignores_a_lease_for_another_device() {
    let mut m = plug();
    m.message(topic::LEASE, &lease("some-other-plug", true, 60_000), 100);
    assert!(!m.tool_is_on(), "took a lease addressed to another device");
}

#[test]
fn a_plug_publishes_nothing_about_a_card_it_cannot_read() {
    // It names no card and cannot: `toolguard/lease` carries tool_id,
    // device_id, grant, ttl_ms and reason -- no card. So tool-on, tool-off and
    // tool-log are all impossible, and usage on such a tool is the reader's to
    // report.
    let mut m = plug();
    let out = m.swipe(ALICE, 100);
    assert!(
        publishes_to(&out).is_empty(),
        "a plug asked the edge about a card: {out:?}"
    );
    assert!(
        out.iter()
            .any(|a| matches!(a, Action::Note(n) if n.contains("no reader"))),
        "a card arriving at a device with no reader should say so: {out:?}"
    );
    // And the stray card must not have energized anything.
    assert!(!m.tool_is_on());
}

#[test]
fn a_plug_still_heartbeats_with_its_relay_state() {
    // It has a relay, so unlike a reader it reports one -- and the report is
    // what credits it with being alive.
    let mut m = Module::new(Config::power_only(ME, TOOL));
    let out = m.start(0);
    let report = &published(&out, topic::POWER_REPORT)[0];
    assert_eq!(report["device_id"], ME);
    assert_eq!(report["relay_on"], false);
}

// ------------------------------------------- what a panel is told (Outcome)
//
// A display needs a decision it can render, not prose for a log. These are
// separate from `Action::Note` so that improving a log sentence cannot change
// what a member sees.

fn outcomes(actions: &[Action]) -> Vec<Outcome> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Outcome(o) => Some(o.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_yes_tells_the_panel_the_member_is_authorized() {
    let mut m = module();
    m.swipe(ALICE, 0);
    let out = m.message(topic::SCAN_RESPONSE, &yes(), 100);
    assert_eq!(outcomes(&out), vec![Outcome::Authorized]);
}

#[test]
fn a_refusal_carries_the_edges_own_words_to_the_panel() {
    let mut m = module();
    m.swipe(BOB, 0);
    let out = m.message(
        topic::SCAN_RESPONSE,
        br#"{"authorized":false,"reason":"Training required"}"#,
        100,
    );
    assert_eq!(
        outcomes(&out),
        vec![Outcome::Refused("Training required".to_string())],
        "the panel must be able to show the reason without parsing a log line"
    );
}

#[test]
fn an_unreadable_answer_is_a_refusal_and_not_a_silence() {
    // Rule 2: an answer that does not parse is a no. It must not read as
    // NoAnswer, because the two send a member to different places -- a refusal
    // to their training record, a silence to the network.
    let mut m = module();
    m.swipe(ALICE, 0);
    let out = m.message(topic::SCAN_RESPONSE, b"yes please", 100);
    assert_eq!(
        outcomes(&out),
        vec![Outcome::Refused("unreadable answer".to_string())]
    );
}

#[test]
fn nothing_answering_is_reported_as_a_silence_not_a_refusal() {
    let mut m = module();
    m.swipe(ALICE, 0);
    let out = m.tick(5_000);
    assert_eq!(outcomes(&out), vec![Outcome::NoAnswer]);
}

#[test]
fn the_end_of_a_session_is_announced_after_the_tool_is_released() {
    // Order matters: a panel saying "session ended" while the tool is still
    // energized is the one instruction a member could act on and be hurt by.
    let mut m = alice_running_tool();
    let out = m.tool_off(1_000);
    assert_eq!(outcomes(&out), vec![Outcome::Ended(EndReason::ToolOff)]);
    let last_publish = out
        .iter()
        .rposition(|a| matches!(a, Action::Publish { .. }))
        .expect("a release should have been published");
    let announced = out
        .iter()
        .position(|a| matches!(a, Action::Outcome(Outcome::Ended(_))))
        .expect("the end should have been announced");
    assert!(
        announced > last_publish,
        "the panel was told before the tool was released: {out:?}"
    );
}

#[test]
fn the_end_reason_reaches_the_panel_so_it_can_say_which_stop_it_was() {
    let mut c = Config::switches_nothing(ME, TOOL);
    c.max_session_ms = 30_000;
    let mut m = Module::new(c);
    m.start(0);
    let mut m = alice_reading(m);
    let out = m.tick(30_100);
    assert_eq!(outcomes(&out), vec![Outcome::Ended(EndReason::MaxSession)]);
}

#[test]
fn activity_distinguishes_resting_asking_and_in_session() {
    let mut m = module();
    assert_eq!(m.activity(), Activity::Idle);
    m.swipe(ALICE, 0);
    assert_eq!(m.activity(), Activity::Asking);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    assert_eq!(
        m.activity(),
        Activity::Session { energized: false },
        "authorized is not energized: a power module waits for a lease"
    );
    m.message(topic::LEASE, &lease(ME, true, 3_000), 200);
    assert_eq!(m.activity(), Activity::Session { energized: true });
}

#[test]
fn a_device_that_switches_nothing_is_in_session_but_never_energized() {
    let m = alice_reading(no_power());
    assert_eq!(
        m.activity(),
        Activity::Session { energized: false },
        "a device with no relay reported an energized tool"
    );
}

// ------------------------------------------------- a plug meters (CS:S #174)
//
// The plug is the device that knows whether the power is on and, with a
// sensor, whether the machine is working -- and it never learns a card. So it
// reports usage naming the tool alone, and the server attributes it to whoever
// is signed on to that tool.

fn plug_with(interval_ms: u64, senses: bool) -> Module {
    let mut c = Config::power_only(ME, TOOL);
    c.usage_interval_ms = interval_ms;
    c.senses_running = senses;
    let mut m = Module::new(c);
    m.start(0);
    m
}

#[test]
fn a_plug_meters_the_time_it_is_energized_and_reports_it_with_no_card() {
    let mut m = plug();
    m.message(topic::LEASE, &lease(ME, true, 5_000), 100); // energized 100..5100
    assert!(
        logs(&m.tick(3_000)).is_empty(),
        "reported mid-lease with no interval set"
    );
    let out = m.tick(5_100);
    assert!(!m.tool_is_on());
    assert_eq!(
        publishes_to(&out),
        vec![topic::USAGE_REQUEST],
        "a plug reports usage and nothing else -- it has no card to release with"
    );
    let log = &logs(&out)[0];
    assert!(
        log.get("card").is_none(),
        "a plug named a card it cannot know: {log}"
    );
    assert_eq!(log["resource"], TOOL);
    assert_eq!(log["seconds"], json!(5.0));
    assert!(
        log.get("active_seconds").is_none(),
        "no sensor, yet a working figure: {log}"
    );
}

#[test]
fn a_plugs_count_stops_at_the_cut_not_at_the_noticing() {
    let mut m = plug();
    m.message(topic::LEASE, &lease(ME, true, 3_000), 100); // cut at 3100
    let out = m.tick(9_000);
    assert_eq!(logs(&out)[0]["seconds"], json!(3.0));
}

#[test]
fn a_withdrawn_lease_closes_the_plugs_meter() {
    let mut m = plug();
    m.message(topic::LEASE, &lease(ME, true, 60_000), 100);
    let out = m.message(topic::LEASE, &lease(ME, false, 0), 2_100);
    assert_eq!(logs(&out)[0]["seconds"], json!(2.0));
}

#[test]
fn a_plug_reports_increments_that_sum_to_the_total_exactly_once() {
    let mut m = plug_with(1_000, false);
    m.message(topic::LEASE, &lease(ME, true, 60_000), 0);
    let mut all: Vec<Value> = Vec::new();
    for t in [1_000, 2_000, 2_500, 3_000] {
        all.extend(logs(&m.tick(t)));
    }
    all.extend(logs(&m.message(topic::LEASE, &lease(ME, false, 0), 3_500)));
    assert_eq!(all.len(), 4, "{all:?}");
    let sum: f64 = all.iter().map(|l| l["seconds"].as_f64().unwrap()).sum();
    assert!(
        (sum - 3.5).abs() < 1e-9,
        "increments summed to {sum}, not 3.5"
    );
    assert!(all.iter().all(|l| l.get("card").is_none()), "{all:?}");
}

#[test]
fn a_plug_with_a_sensor_reports_both_figures_with_no_card() {
    let mut m = plug_with(0, true);
    m.message(topic::LEASE, &lease(ME, true, 60_000), 0);
    m.running(true, 1_000);
    m.running(false, 3_000);
    let out = m.message(topic::LEASE, &lease(ME, false, 0), 4_000);
    let log = &logs(&out)[0];
    assert!(log.get("card").is_none(), "{log}");
    assert_eq!(
        log["seconds"],
        json!(2.0),
        "with a sensor the figure is the machine working"
    );
    assert_eq!(log["active_seconds"], json!(2.0));
}

#[test]
fn a_plug_never_energized_reports_nothing() {
    let mut m = plug_with(1_000, false);
    m.message(topic::LEASE, &lease(ME, false, 0), 100);
    let mut all: Vec<Value> = Vec::new();
    for t in [1_000, 2_000, 5_000] {
        all.extend(logs(&m.tick(t)));
    }
    assert!(
        all.is_empty(),
        "a plug that was never on reported usage: {all:?}"
    );
}

#[test]
fn a_plugs_stop_releases_the_tool_without_a_card() {
    // A stop is a safety act and needs no badge (CS:S #179). A plug has no
    // session and no card; its stop names the tool alone, and the server ends
    // whatever session is open there. The plug does not switch itself off --
    // the lease that follows is withdrawn, and that is what cuts it.
    let mut m = plug();
    m.message(topic::LEASE, &lease(ME, true, 60_000), 100);
    let out = m.tool_off(1_000);
    assert_eq!(publishes_to(&out), vec![topic::STOP_REQUEST], "{out:?}");
    let req = &published(&out, topic::STOP_REQUEST)[0];
    assert!(
        req.get("card").is_none(),
        "a plug named a card it cannot know: {req}"
    );
    assert_eq!(req["resource"], TOOL);
    assert!(
        m.tool_is_on(),
        "a plug must not cut itself on a stop; the lease decides"
    );
}

#[test]
fn a_plug_has_no_activity_of_its_own() {
    // It owns no session; its relay is `tool_is_on`. A panel on a plug would
    // be showing the tool's state, not a member's session.
    let mut m = plug();
    m.message(topic::LEASE, &lease(ME, true, 3_000), 100);
    assert!(m.tool_is_on());
    assert_eq!(m.activity(), Activity::Idle);
}

// ------------------------------------------------- a device that does not meter
//
// A second plug on a tool another device already meters -- an extractor that
// follows the laser's lease -- must switch, pulse, and release exactly as any
// other, and never send a tool-log: reports accumulate, and it would count the
// same minutes twice.

#[test]
fn a_device_configured_not_to_report_usage_still_stops_cleanly_and_sends_no_log() {
    let mut c = Config::new(ME, TOOL);
    c.senses_running = true;
    c.usage_interval_ms = 1_000;
    c.reports_usage = false;
    let mut m = Module::new(c);
    m.start(0);
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    m.message(topic::LEASE, &lease(ME, true, 60_000), 200);
    m.running(true, 1_000);
    let mut periodic: Vec<Value> = Vec::new();
    for t in [2_000, 3_000, 4_000] {
        periodic.extend(logs(&m.tick(t)));
    }
    assert!(
        periodic.is_empty(),
        "a periodic report went out: {periodic:?}"
    );
    let out = m.tool_off(5_000);
    assert_eq!(
        publishes_to(&out),
        vec![topic::STOP_REQUEST],
        "the stop must still release the tool, and only that: {out:?}"
    );
    assert!(!m.tool_is_on(), "the relay stayed closed");
}

#[test]
fn a_plug_configured_not_to_report_usage_follows_its_lease_in_silence() {
    // The extractor-plug case, and the point is that nothing else about it
    // moves.
    let mut c = Config::power_only(ME, TOOL);
    c.usage_interval_ms = 1_000;
    c.reports_usage = false;
    let mut m = Module::new(c);
    m.start(0);
    m.message(topic::LEASE, &lease(ME, true, 3_000), 100);
    assert!(
        m.tool_is_on(),
        "a plug that does not meter must still energize"
    );
    let mut all: Vec<Value> = Vec::new();
    for t in [1_200, 2_200] {
        all.extend(logs(&m.tick(t)));
    }
    let out = m.tick(3_100);
    all.extend(logs(&out));
    assert!(!m.tool_is_on(), "and still drop at the cut");
    assert!(
        all.is_empty(),
        "a plug that does not meter reported: {all:?}"
    );
    // The heartbeats are untouched: this device is still alive to the edge.
    assert!(
        !published(&m.tick(4_100), topic::ALIVE_REQUEST).is_empty(),
        "the pulse stopped with the metering"
    );
}

// ------------------------------------------------- usage, for a panel
//
// A member metered by the second should be able to watch the figure. These
// arms pin what a panel may show: a figure only where something measures one,
// the HELD figure (what bills by default), and a running flag that follows the
// bridged signal rather than the raw one.

#[test]
fn only_a_device_that_senses_the_machine_offers_a_usage_figure() {
    // No sensor: no figure, in a session or out of one. A panel that showed
    // "0:00:00" over a machine nothing is measuring would be inventing it.
    let m = alice_reading(module());
    assert_eq!(
        m.usage(500),
        None,
        "a device with no sensor offered a figure"
    );

    // Sensing, but no session: nothing to count.
    let mut m = sensing(0, 0, 0);
    assert_eq!(m.usage(0), None);
    m.swipe(ALICE, 0);
    assert_eq!(m.usage(50), None, "a figure before the yes");
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    assert_eq!(
        m.usage(100),
        Some(Usage {
            running: false,
            seconds: 0
        })
    );
}

#[test]
fn the_usage_figure_is_the_held_one_and_follows_the_bridged_signal() {
    // hold 2 s: the machine pausing for less than that is still "running",
    // on the panel exactly as in the figure that bills.
    let mut m = sensing(0, 2_000, 0);
    m.swipe(ALICE, 0);
    m.message(topic::SCAN_RESPONSE, &yes(), 100);
    m.message(topic::LEASE, &lease(ME, true, 60_000), 200);

    m.running(true, 1_000);
    m.tick(4_000);
    assert_eq!(
        m.usage(4_000),
        Some(Usage {
            running: true,
            seconds: 3
        })
    );

    // The raw signal drops; inside the hold the panel still says running and
    // the figure still climbs.
    m.running(false, 4_000);
    m.tick(5_000);
    assert_eq!(
        m.usage(5_000),
        Some(Usage {
            running: true,
            seconds: 4
        }),
        "the panel flickered on a pause the bill bridges"
    );

    // Past the hold: stopped, and the figure holds where the bridge ended
    // (1 s -> 6 s = 5 s), not where the raw signal dropped. The release is
    // noticed on a tick, so the figure resolves to the poll interval -- a box
    // polls every 75 ms; this test polls at the boundary.
    m.tick(6_000);
    m.tick(7_000);
    assert_eq!(
        m.usage(7_000),
        Some(Usage {
            running: false,
            seconds: 5
        })
    );
    m.tick(9_000);
    assert_eq!(
        m.usage(9_000).map(|u| u.seconds),
        Some(5),
        "idle time counted"
    );

    // Reading is not reporting: asking twice changes nothing.
    assert_eq!(m.usage(9_000), m.usage(9_000));
}

// ------------------------------------------------- the pulse (CS:S #170)
//
// Every device says it is alive, whatever its role. This is what lets a reader
// or a sensor -- which has no power to report -- be heard from at all, at the
// edge and at the server.

fn pulses(actions: &[Action]) -> Vec<Value> {
    published(actions, topic::ALIVE_REQUEST)
}

#[test]
fn every_role_pulses_on_start_and_on_every_heartbeat() {
    for (label, config) in [
        ("reader+power", Config::new(ME, TOOL)),
        ("reader only", Config::switches_nothing(ME, TOOL)),
        ("plug", Config::power_only(ME, TOOL)),
    ] {
        let mut m = Module::new(config);
        let boot = m.start(0);
        assert_eq!(
            pulses(&boot).len(),
            1,
            "{label}: the first pulse goes out at start, with the first power report"
        );
        // Inside the interval: nothing. On it: one. The pulse rides the same
        // timer as the power report, so the two cannot drift apart.
        assert!(pulses(&m.tick(999)).is_empty(), "{label}: pulsed early");
        assert_eq!(
            pulses(&m.tick(1_000)).len(),
            1,
            "{label}: no pulse on the heartbeat"
        );
    }
}

#[test]
fn the_pulse_names_this_device_and_nothing_else() {
    // The device id is what the edge resolves to bindings. Nothing else: health
    // beyond "alive" is a different message, and a pulse that grew fields would
    // be the expensive-every-second kind.
    let mut m = Module::new(Config::switches_nothing(ME, TOOL));
    let out = m.start(0);
    let pulse = &pulses(&out)[0];
    assert_eq!(pulse["device_id"], ME);
    assert_eq!(
        pulse.as_object().map(|o| o.len()),
        Some(1),
        "the pulse carries more than the device id: {pulse}"
    );
}

#[test]
fn a_device_that_switches_nothing_still_sends_a_power_report_alongside() {
    // Not instead of. Edges that predate the pulse credit only the power
    // report, so dropping it from a reader would reintroduce the silence this
    // fixes against them.
    let mut m = Module::new(Config::switches_nothing(ME, TOOL));
    let out = m.start(0);
    assert_eq!(published(&out, topic::POWER_REPORT).len(), 1);
    assert_eq!(pulses(&out).len(), 1);
}

// ------------------------------------------------------- a latch (CS:S #182)
//
// A door, a tool and a room are the same resource with different bindings. A
// device bound in the `strike` role drives a latch: on a yes whose reply
// carries `strike.pulse_ms`, it releases the latch for exactly that long. The
// pulse is the edge's to name -- it is the resource's strike binding's
// profile -- and the latch is never leased and never held for a session.

fn door_box() -> Module {
    let mut m = Module::new(Config::drives_a_latch(ME, "front-door"));
    m.start(0);
    m
}

fn yes_with_strike(pulse_ms: i64) -> Vec<u8> {
    json!({"resource": "front-door", "authorized": true, "reason": "authorized",
           "strike": {"pulse_ms": pulse_ms}})
    .to_string()
    .into_bytes()
}

fn sets(actions: &[Action], output: Output) -> Vec<bool> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Set { output: o, on } if *o == output => Some(*on),
            _ => None,
        })
        .collect()
}

#[test]
fn the_wire_is_resource_shaped() {
    // The topics and the field a request names the resource by, pinned as
    // literals: this crate copies the wire rather than depending on css_lib,
    // so nothing but this test holds it to FIRMWARE.md's local-broker tables.
    assert_eq!(topic::SCAN_REQUEST, "resource/request/scan");
    assert_eq!(topic::STOP_REQUEST, "resource/request/stop");
    assert_eq!(topic::USAGE_REQUEST, "resource/request/usage");
    assert_eq!(topic::POWER_REPORT, "resource/request/power");
    assert_eq!(topic::ALIVE_REQUEST, "resource/request/alive");
    assert_eq!(topic::SCAN_RESPONSE, "resource/response/scan");
    assert_eq!(topic::LEASE, "resource/lease");
    let mut m = module();
    let scan = &published(&m.swipe(ALICE, 0), topic::SCAN_REQUEST)[0];
    assert_eq!(scan["resource"], TOOL);
    assert!(
        scan.get("tool_id").is_none(),
        "spoke the alias's name: {scan}"
    );
    let report = &published(&m.tick(1000), topic::POWER_REPORT)[0];
    assert_eq!(report["resource"], TOOL);
    assert!(
        report["draw_now"].is_number(),
        "draw is a number on this wire: {report}"
    );
}

#[test]
fn a_yes_with_a_strike_releases_the_latch_for_exactly_the_pulse() {
    let mut m = door_box();
    let start = m.start(0);
    assert_eq!(
        sets(&start, Output::Strike),
        vec![false],
        "known-off at boot"
    );
    assert!(
        sets(&start, Output::Tool).is_empty(),
        "a door box has no relay"
    );
    m.swipe(ALICE, 0);
    let out = m.message(topic::SCAN_RESPONSE, &yes_with_strike(4200), 100);
    assert_eq!(sets(&out, Output::Strike), vec![true]);
    assert!(m.strike_is_on());
    assert!(out.contains(&Action::Outcome(Outcome::Authorized)));
    assert_eq!(m.activity(), Activity::Session { energized: false });
    // Still released just inside the pulse, closed again at its end -- and
    // the session was the pulse: over, with nothing sent for it.
    assert!(sets(&m.tick(4299), Output::Strike).is_empty());
    assert!(m.strike_is_on());
    let end = m.tick(4300);
    assert_eq!(sets(&end, Output::Strike), vec![false]);
    assert!(!m.strike_is_on());
    assert!(end.contains(&Action::Outcome(Outcome::Ended(EndReason::StrikePulsed))));
    assert!(
        publishes_to(&end).is_empty(),
        "a pulse ending is not a stop and not usage: {end:?}"
    );
    assert_eq!(m.activity(), Activity::Idle);
    // And the next badge is asked about at once.
    let next = m.swipe(BOB, 4400);
    assert_eq!(published(&next, topic::SCAN_REQUEST)[0]["card"], BOB);
}

#[test]
fn a_yes_that_names_no_pulse_releases_nothing_on_a_door_box() {
    // The pulse is the resource's strike binding's; a device told none has
    // nothing to guess a duration from, and says so instead.
    let mut m = door_box();
    m.swipe(ALICE, 0);
    let out = m.message(topic::SCAN_RESPONSE, &yes(), 100);
    assert!(sets(&out, Output::Strike).is_empty());
    assert!(!m.strike_is_on());
    assert!(out
        .iter()
        .any(|a| matches!(a, Action::Note(n) if n.contains("no pulse"))));
    // The session was the pulse, and there was none: over at once.
    let end = m.tick(150);
    assert!(end.contains(&Action::Outcome(Outcome::Ended(EndReason::StrikePulsed))));
    assert_eq!(m.activity(), Activity::Idle);
}

#[test]
fn a_zero_or_negative_pulse_is_no_pulse() {
    let mut m = door_box();
    m.swipe(ALICE, 0);
    let out = m.message(topic::SCAN_RESPONSE, &yes_with_strike(0), 100);
    assert!(sets(&out, Output::Strike).is_empty());
    let mut m = door_box();
    m.swipe(ALICE, 0);
    let out = m.message(topic::SCAN_RESPONSE, &yes_with_strike(-5), 100);
    assert!(sets(&out, Output::Strike).is_empty());
}

#[test]
fn a_no_never_releases_the_latch_whatever_it_carries() {
    let mut m = door_box();
    m.swipe(ALICE, 0);
    let no = json!({"resource": "front-door", "authorized": false, "reason": "Card not authorized",
                    "strike": {"pulse_ms": 4200}})
    .to_string()
    .into_bytes();
    let out = m.message(topic::SCAN_RESPONSE, &no, 100);
    assert!(sets(&out, Output::Strike).is_empty());
    assert!(!m.strike_is_on());
    assert!(out.contains(&Action::Outcome(Outcome::Refused(
        "Card not authorized".into()
    ))));
}

#[test]
fn a_device_that_drives_no_latch_ignores_the_strike() {
    // A reader beside a tool that happens to have a strike binding elsewhere
    // gets the pulse in its reply too; with no latch of its own it has nothing
    // to release, and its session follows the tool's rules as before.
    let mut m = module();
    m.swipe(ALICE, 0);
    let yes = json!({"resource": TOOL, "authorized": true, "strike": {"pulse_ms": 4200}})
        .to_string()
        .into_bytes();
    let out = m.message(topic::SCAN_RESPONSE, &yes, 100);
    assert!(sets(&out, Output::Strike).is_empty());
    assert!(!m.strike_is_on());
    assert_eq!(m.activity(), Activity::Session { energized: false });
}

#[test]
fn a_room_in_one_box_pulses_the_latch_and_keeps_the_tools_session_rules() {
    // Reads cards, gates power AND drives a latch: the latch pulses on the
    // yes, the relay waits for its lease, and the session is the tool's --
    // it ends on the lease and the stop, not on the pulse.
    let mut c = Config::new(ME, "wood-shop");
    c.drives_latch = true;
    let mut m = Module::new(c);
    m.start(0);
    m.swipe(ALICE, 0);
    let yes = json!({"resource": "wood-shop", "authorized": true, "strike": {"pulse_ms": 1000}})
        .to_string()
        .into_bytes();
    let out = m.message(topic::SCAN_RESPONSE, &yes, 100);
    assert_eq!(sets(&out, Output::Strike), vec![true]);
    assert!(!m.tool_is_on(), "the relay waits for its lease");
    m.message(topic::LEASE, &lease(ME, true, 3000), 200);
    assert!(m.tool_is_on());
    let later = m.tick(1100);
    assert_eq!(sets(&later, Output::Strike), vec![false], "the pulse ended");
    assert!(m.tool_is_on(), "the session did not end with the pulse");
    assert_eq!(m.activity(), Activity::Session { energized: true });
    let stop = m.tool_off(2000);
    assert!(stop.contains(&Action::Outcome(Outcome::Ended(EndReason::ToolOff))));
    assert_eq!(published(&stop, topic::STOP_REQUEST)[0]["card"], ALICE);
}
