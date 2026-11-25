//! A module that signs the local wire (CS:S #249), against an edge played here
//! with `css_wire_sign`'s own `Peer` -- the code the reference edge runs.
//!
//! The canonical bytes and the HMAC are css_wire's, tested in CS:S against
//! FIRMWARE.md's worked example and against a JavaScript implementation
//! written from that document; what is pinned here is this module's USE of
//! them: what it seals, what it believes, and what it sends again.
//!
//! WHAT THIS DOES NOT PROVE: that the reference edge seals what this module
//! believes. That is CS:S's e2e (`e2e/drivers/signedwire.mjs`), which plays a
//! module against a real edge; this plays an edge against the real module.

use css_wire::seal::Sealed;
use css_wire_sign::{seal_with, Peer, Refused};
use serde_json::{json, Value};
use spaces_device::wire::{
    topic, Lease, LocalAliveRequest, LocalHello, LocalPower, LocalScan, LocalScanReply, LocalStop,
    LocalUsage,
};
use spaces_device::{Action, Config, Module, Outcome, STOP_RESEND_MS};

const ME: &str = "dev-uuid-me";
const OTHER: &str = "dev-uuid-other";
const TOOL: &str = "laser-01";
const ALICE: &str = "ALICE-CARD";
const KEY: &str = "4f3c-this-devices-command-key";
const OTHER_KEY: &str = "another-devices-command-key";

/// Nonces as a counter, so each test can say which one a seal names.
fn counter(prefix: &'static str) -> impl FnMut() -> String + Send + 'static {
    let mut n = 0;
    move || {
        n += 1;
        format!("{prefix}-{n}")
    }
}

fn signing(config: Config) -> Module {
    Module::new(config).signing(KEY, counter("mod"))
}

fn reader() -> Module {
    let mut c = Config::new(ME, TOOL);
    c.gates_power = false;
    signing(c)
}

fn power() -> Module {
    signing(Config::new(ME, TOOL))
}

/// The edge, as far as one module is concerned.
struct Edge {
    peer: Peer,
    nonces: u32,
}

impl Edge {
    fn new(nonce: &str) -> Self {
        Edge {
            peer: Peer::new(nonce.to_string()),
            nonces: 0,
        }
    }

    /// Open one published message as the reference edge would, by its topic.
    fn hear(&mut self, topic: &str, payload: &str) -> Result<(), Refused> {
        fn open<M: Sealed + serde::de::DeserializeOwned>(
            e: &mut Edge,
            p: &str,
        ) -> Result<(), Refused> {
            let m: M = serde_json::from_str(p).expect("the module's JSON parses as its type");
            e.nonces += 1;
            let n = e.nonces;
            e.peer.open(KEY.as_bytes(), &m, || format!("edge-rot-{n}"))
        }
        match topic {
            t if t == topic::SCAN_REQUEST => open::<LocalScan>(self, payload),
            t if t == topic::STOP_REQUEST => open::<LocalStop>(self, payload),
            t if t == topic::USAGE_REQUEST => open::<LocalUsage>(self, payload),
            t if t == topic::POWER_REPORT => open::<LocalPower>(self, payload),
            t if t == topic::ALIVE_REQUEST => open::<LocalAliveRequest>(self, payload),
            other => panic!("a module sent on {other}"),
        }
    }

    /// Everything the module published, each opened in order.
    fn hear_all(&mut self, actions: &[Action]) -> Vec<(&'static str, Result<(), Refused>)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Publish { topic, payload } => Some((*topic, self.hear(topic, payload))),
                _ => None,
            })
            .collect()
    }

    fn sealed<M: Sealed + serde::Serialize>(&mut self, mut m: M) -> Vec<u8> {
        self.peer.seal(KEY.as_bytes(), &mut m);
        serde_json::to_vec(&m).unwrap()
    }

    fn hello(&mut self) -> Vec<u8> {
        self.sealed(LocalHello {
            device_id: ME.into(),
            seal: None,
        })
    }

    fn lease(&mut self, grant: bool, ttl_ms: i64) -> Vec<u8> {
        self.sealed(lease_for(ME, grant, ttl_ms))
    }

    fn reply(&mut self, authorized: bool) -> Vec<u8> {
        self.sealed(reply_for(Some(ME), authorized))
    }
}

fn lease_for(device: &str, grant: bool, ttl_ms: i64) -> Lease {
    Lease {
        resource: TOOL.into(),
        device_id: device.into(),
        grant,
        ttl_ms,
        reason: None,
        member: None,
        seal: None,
    }
}

fn reply_for(device: Option<&str>, authorized: bool) -> LocalScanReply {
    LocalScanReply {
        resource: TOOL.into(),
        authorized,
        reason: if authorized {
            "authorized"
        } else {
            "Training required"
        }
        .into(),
        strike: None,
        member: None,
        sign_in: None,
        device_id: device.map(str::to_string),
        seal: None,
    }
}

fn published(actions: &[Action], name: &str) -> Vec<Value> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Publish { topic, payload } if *topic == name => {
                Some(serde_json::from_str(payload).unwrap())
            }
            _ => None,
        })
        .collect()
}

fn notes(actions: &[Action]) -> Vec<&str> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Note(n) => Some(n.as_str()),
            _ => None,
        })
        .collect()
}

fn outcomes(actions: &[Action]) -> Vec<&Outcome> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Outcome(o) => Some(o),
            _ => None,
        })
        .collect()
}

/// Start the module and let it meet `edge`: its first heartbeat is stale, the
/// edge says hello, and the next heartbeat is believed. Returns the time.
fn meet(m: &mut Module, edge: &mut Edge) -> u64 {
    let first = edge.hear_all(&m.start(0));
    assert!(
        first.iter().all(|(_, r)| *r == Err(Refused::Stale)),
        "setup: a module's first messages name no edge nonce, so are stale: {first:?}"
    );
    m.message(topic::HELLO, &edge.hello(), 10);
    let next = edge.hear_all(&m.tick(1_000));
    assert!(
        !next.is_empty() && next.iter().all(|(_, r)| r.is_ok()),
        "setup: after the hello the heartbeat is believed: {next:?}"
    );
    1_000
}

// ── what it sends ─────────────────────────────────────────────────────────────

/// Every message a signing module sends carries a seal that checks out under
/// its key, and the requests that may name a device name it -- the edge finds
/// the key by that name.
#[test]
fn everything_a_signing_module_sends_is_sealed_with_its_key_and_names_it() {
    let mut m = power();
    let mut edge = Edge::new("edge-1");
    // Heard as it goes, as a real edge would: each answer is sealed to the
    // module nonce the edge has heard by then.
    let mut all = m.start(0);
    let mut heard = edge.hear_all(&all);
    m.message(topic::HELLO, &edge.hello(), 10);
    let mut step = |out: Vec<Action>, edge: &mut Edge| {
        heard.extend(edge.hear_all(&out));
        all.extend(out);
    };
    step(m.tick(1_000), &mut edge);
    step(m.swipe(ALICE, 1_100), &mut edge);
    let yes = edge.reply(true);
    step(m.message(topic::SCAN_RESPONSE, &yes, 1_200), &mut edge);
    let lease = edge.lease(true, 3_000);
    step(m.message(topic::LEASE, &lease, 1_300), &mut edge);
    step(m.tool_off(2_000), &mut edge);
    let topics: Vec<_> = heard.iter().map(|(t, _)| *t).collect();
    for want in [
        topic::POWER_REPORT,
        topic::ALIVE_REQUEST,
        topic::SCAN_REQUEST,
        topic::USAGE_REQUEST,
        topic::STOP_REQUEST,
    ] {
        assert!(
            topics.contains(&want),
            "nothing was sent on {want}: {topics:?}"
        );
    }
    for (t, r) in &heard {
        assert!(
            !matches!(r, Err(Refused::Unsigned) | Err(Refused::BadSignature)),
            "{t} was not sealed with the device's key: {r:?}"
        );
    }
    for t in [
        topic::SCAN_REQUEST,
        topic::USAGE_REQUEST,
        topic::STOP_REQUEST,
    ] {
        for p in published(&all, t) {
            assert_eq!(p["device_id"], ME, "{t} must name the device: {p}");
        }
    }
}

/// The control: a module that does not sign sends exactly what it always did.
#[test]
fn a_module_that_does_not_sign_sends_no_seal_and_names_no_device_on_a_scan() {
    let mut m = Module::new(Config::new(ME, TOOL));
    let mut all = m.start(0);
    all.extend(m.swipe(ALICE, 0));
    assert!(!m.signs());
    let scan = &published(&all, topic::SCAN_REQUEST)[0];
    assert_eq!(scan, &json!({"resource": TOOL, "card": ALICE}));
    for a in &all {
        if let Action::Publish { topic, payload } = a {
            assert!(
                !payload.contains("\"seal\""),
                "{topic} carries a seal: {payload}"
            );
        }
    }
}

/// The seal names the nonce the firmware's source drew, and the sequence
/// rises with every message.
#[test]
fn the_seal_names_the_drawn_nonce_and_a_rising_sequence() {
    let mut m = power();
    let all = m.start(0);
    let seals: Vec<Value> = all
        .iter()
        .filter_map(|a| match a {
            Action::Publish { payload, .. } => {
                Some(serde_json::from_str::<Value>(payload).unwrap()["seal"].clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(seals.len(), 2, "a power report and an alive: {seals:?}");
    assert!(seals.iter().all(|s| s["from"] == "mod-1"), "{seals:?}");
    assert!(
        seals.iter().all(|s| s["to"] == ""),
        "no edge heard yet: {seals:?}"
    );
    assert_eq!(
        (seals[0]["seq"].as_u64(), seals[1]["seq"].as_u64()),
        (Some(1), Some(2))
    );
}

// ── meeting the edge ──────────────────────────────────────────────────────────

/// The whole handshake: first messages stale, the hello teaches the edge's
/// nonce, and from then on the edge believes the module.
#[test]
fn a_hello_teaches_the_edges_nonce_and_the_module_is_then_believed() {
    let mut m = power();
    let mut edge = Edge::new("edge-1");
    meet(&mut m, &mut edge);
}

/// Without the hello, nothing the module sends is ever believed: the control
/// that shows the hello, and not time, is what did it.
#[test]
fn without_a_hello_the_module_is_never_believed() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    edge.hear_all(&m.start(0));
    let later = edge.hear_all(&m.tick(1_000));
    assert!(
        later.iter().all(|(_, r)| *r == Err(Refused::Stale)),
        "{later:?}"
    );
}

/// A hello for another device teaches nothing.
#[test]
fn a_hello_for_another_device_is_not_ours() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    edge.hear_all(&m.start(0));
    let mut theirs = LocalHello {
        device_id: OTHER.into(),
        seal: None,
    };
    seal_with(OTHER_KEY.as_bytes(), &mut theirs, "edge-1", "", 1);
    m.message(topic::HELLO, &serde_json::to_vec(&theirs).unwrap(), 10);
    let later = edge.hear_all(&m.tick(1_000));
    assert!(
        later.iter().all(|(_, r)| *r == Err(Refused::Stale)),
        "{later:?}"
    );
}

// ── what it believes: leases ──────────────────────────────────────────────────

/// A sealed lease energizes; the same lease unsealed does not.
#[test]
fn a_sealed_lease_energizes_and_an_unsigned_one_does_not() {
    let mut m = power();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    let unsigned = serde_json::to_vec(&lease_for(ME, true, 3_000)).unwrap();
    m.message(topic::LEASE, &unsigned, t + 20);
    assert!(
        !m.tool_is_on(),
        "an unsigned lease energized a signing module"
    );
    m.message(topic::LEASE, &edge.lease(true, 3_000), t + 30);
    assert!(m.tool_is_on(), "the sealed lease should energize");
}

/// A lease sealed under another device's key is not believed.
#[test]
fn a_lease_sealed_under_another_key_is_not_believed() {
    let mut m = power();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    let mut forged = lease_for(ME, true, 3_000);
    let to = edge.peer.theirs().unwrap().to_string();
    seal_with(OTHER_KEY.as_bytes(), &mut forged, "edge-1", &to, 99);
    m.message(topic::LEASE, &serde_json::to_vec(&forged).unwrap(), t + 20);
    assert!(!m.tool_is_on());
}

/// A recorded lease played again does not extend the session: the tool goes
/// off when the real one runs out.
#[test]
fn a_replayed_lease_does_not_keep_the_tool_on() {
    let mut m = power();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    let recorded = edge.lease(true, 3_000);
    m.message(topic::LEASE, &recorded, t + 100);
    assert!(m.tool_is_on());
    m.message(topic::LEASE, &recorded, t + 2_900);
    m.tick(t + 3_100);
    assert!(
        !m.tool_is_on(),
        "the replay kept the tool on past its lease"
    );
}

/// A forged withdrawal is a forgery too: an unsigned `grant: false` does not
/// cut a sealed lease, and a sealed one does.
#[test]
fn an_unsigned_withdrawal_does_not_cut_the_power_and_a_sealed_one_does() {
    let mut m = power();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    m.message(topic::LEASE, &edge.lease(true, 3_000), t + 20);
    let unsigned = serde_json::to_vec(&lease_for(ME, false, 0)).unwrap();
    m.message(topic::LEASE, &unsigned, t + 30);
    assert!(m.tool_is_on(), "an unsigned grant:false cut the power");
    m.message(topic::LEASE, &edge.lease(false, 0), t + 40);
    assert!(!m.tool_is_on(), "the sealed grant:false should cut it");
}

/// A plug, which owns no session: the lease is its whole authorization, so an
/// unsigned grant must not close its relay.
#[test]
fn a_plug_that_signs_is_energized_by_a_sealed_lease_only() {
    let mut c = Config::new(ME, TOOL);
    c.reads_cards = false;
    let mut m = signing(c);
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    let unsigned = serde_json::to_vec(&lease_for(ME, true, 3_000)).unwrap();
    m.message(topic::LEASE, &unsigned, t);
    assert!(
        !m.tool_is_on(),
        "an unsigned lease closed a signing plug's relay"
    );
    m.message(topic::LEASE, &edge.lease(true, 3_000), t + 10);
    assert!(m.tool_is_on());
}

/// A lease refused twice a second is said once, not twice a second.
#[test]
fn a_refusal_is_noted_once_until_something_is_believed() {
    let mut c = Config::new(ME, TOOL);
    c.reads_cards = false;
    let mut m = signing(c);
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    let unsigned = serde_json::to_vec(&lease_for(ME, true, 3_000)).unwrap();
    let first = m.message(topic::LEASE, &unsigned, t);
    let second = m.message(topic::LEASE, &unsigned, t + 500);
    assert_eq!(notes(&first).len(), 1, "{:?}", notes(&first));
    assert!(notes(&second).is_empty(), "{:?}", notes(&second));
    m.message(topic::LEASE, &edge.lease(false, 0), t + 600);
    let third = m.message(topic::LEASE, &unsigned, t + 700);
    assert_eq!(
        notes(&third).len(),
        1,
        "after a believed lease it is news again"
    );
}

// ── what it believes: scan answers ────────────────────────────────────────────

/// The answer sealed for this device is the answer.
#[test]
fn a_reply_sealed_for_this_device_is_the_answer() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    let out = m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    assert_eq!(outcomes(&out), vec![&Outcome::Authorized]);
}

/// The unsigned reply -- the one an edge also sends on a resource that
/// requires nothing, and the one a forger would send -- is no answer at all:
/// not a yes, and not a no either. The scan times out, and says why.
#[test]
fn an_unsigned_reply_is_no_answer_and_the_timeout_says_so() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    let yes = serde_json::to_vec(&reply_for(None, true)).unwrap();
    let out = m.message(topic::SCAN_RESPONSE, &yes, t + 10);
    assert!(outcomes(&out).is_empty(), "{out:?}");
    let no = serde_json::to_vec(&reply_for(None, false)).unwrap();
    let out = m.message(topic::SCAN_RESPONSE, &no, t + 20);
    assert!(
        outcomes(&out).is_empty(),
        "a forged refusal was taken as a no: {out:?}"
    );
    let out = m.tick(t + 10_000);
    assert_eq!(outcomes(&out), vec![&Outcome::NoAnswer]);
    assert!(
        notes(&out)
            .iter()
            .any(|n| n.contains("not sealed for this device")),
        "{:?}",
        notes(&out)
    );
}

/// The control for that note: a scan that simply went unanswered does not
/// blame the edge's sealing.
#[test]
fn a_scan_nobody_answered_does_not_blame_the_seal() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    let out = m.tick(t + 10_000);
    assert_eq!(outcomes(&out), vec![&Outcome::NoAnswer]);
    assert!(
        !notes(&out).iter().any(|n| n.contains("not sealed")),
        "{:?}",
        notes(&out)
    );
}

/// Another device's copy -- sealed for it, under its key -- is not ours.
#[test]
fn another_devices_sealed_copy_is_not_ours() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    let mut theirs = reply_for(Some(OTHER), true);
    seal_with(
        OTHER_KEY.as_bytes(),
        &mut theirs,
        "edge-1",
        "their-nonce",
        1,
    );
    let out = m.message(
        topic::SCAN_RESPONSE,
        &serde_json::to_vec(&theirs).unwrap(),
        t + 10,
    );
    assert!(outcomes(&out).is_empty(), "{out:?}");
}

/// A reply naming this device but carrying no seal is refused as unsigned.
#[test]
fn a_reply_naming_this_device_without_a_seal_is_not_believed() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    let bare = serde_json::to_vec(&reply_for(Some(ME), true)).unwrap();
    let out = m.message(topic::SCAN_RESPONSE, &bare, t + 10);
    assert!(outcomes(&out).is_empty(), "{out:?}");
}

/// Unsigned, an answer that does not parse is a no. Signed, it is nobody's
/// answer, so it is not even that.
#[test]
fn an_answer_that_does_not_parse_is_ignored_while_signing() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    let out = m.message(topic::SCAN_RESPONSE, b"{not json", t + 10);
    assert!(outcomes(&out).is_empty(), "{out:?}");
}

// ── what it sends again ───────────────────────────────────────────────────────

/// A card tapped before the edge was heard from: the scan is stale, the edge
/// says hello, and the module asks again -- this time believed.
#[test]
fn a_scan_the_edge_refused_as_stale_is_asked_again_after_the_hello() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    edge.hear_all(&m.start(0));
    let first = m.swipe(ALICE, 50);
    assert_eq!(
        edge.hear_all(&first),
        vec![(topic::SCAN_REQUEST, Err(Refused::Stale))]
    );
    let out = m.message(topic::HELLO, &edge.hello(), 60);
    let again = published(&out, topic::SCAN_REQUEST);
    assert_eq!(again.len(), 1, "the scan was not asked again: {out:?}");
    assert_eq!(again[0]["card"], ALICE);
    assert_eq!(edge.hear_all(&out), vec![(topic::SCAN_REQUEST, Ok(()))]);
}

/// An edge that restarted refuses the stop sent to its old nonce; the module
/// sends it again once it hears the new one, and the new edge believes it.
#[test]
fn a_stop_the_restarted_edge_refused_is_sent_again() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    let stopped = m.tool_off(t + 1_000);
    let mut restarted = Edge::new("edge-2");
    let heard = restarted.hear_all(&stopped);
    assert!(
        heard.contains(&(topic::STOP_REQUEST, Err(Refused::Stale))),
        "{heard:?}"
    );
    let out = m.message(topic::HELLO, &restarted.hello(), t + 2_000);
    let again = published(&out, topic::STOP_REQUEST);
    assert_eq!(again.len(), 1, "{out:?}");
    assert_eq!(again[0]["card"], ALICE);
    assert_eq!(
        restarted.hear_all(&out),
        vec![(topic::STOP_REQUEST, Ok(()))]
    );
    // Once: a second hello from the same edge sends nothing again.
    let twice = m.message(topic::HELLO, &restarted.hello(), t + 3_000);
    assert!(
        published(&twice, topic::STOP_REQUEST).is_empty(),
        "{twice:?}"
    );
}

/// Past the window, a stop is not sent again: the session is the server's.
#[test]
fn a_stop_older_than_the_window_is_not_sent_again() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    m.tool_off(t + 1_000);
    let mut restarted = Edge::new("edge-2");
    let late = t + 1_000 + STOP_RESEND_MS + 1;
    let out = m.message(topic::HELLO, &restarted.hello(), late);
    assert!(published(&out, topic::STOP_REQUEST).is_empty(), "{out:?}");
}

/// A stop the edge believed is not sent again when a hello from that same
/// edge arrives: only a NEW nonce means it was refused.
#[test]
fn a_stop_the_edge_believed_is_not_sent_again() {
    let mut m = reader();
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    let stopped = m.tool_off(t + 1_000);
    assert!(edge
        .hear_all(&stopped)
        .contains(&(topic::STOP_REQUEST, Ok(()))));
    let out = m.message(topic::HELLO, &edge.hello(), t + 2_000);
    assert!(published(&out, topic::STOP_REQUEST).is_empty(), "{out:?}");
}

/// A usage increment is not sent again: no acknowledgement says which reports
/// arrived, so a resend could bill twice.
#[test]
fn usage_is_not_sent_again() {
    let mut c = Config::new(ME, TOOL);
    c.usage_interval_ms = 1_000;
    let mut m = signing(c);
    let mut edge = Edge::new("edge-1");
    let t = meet(&mut m, &mut edge);
    m.swipe(ALICE, t);
    m.message(topic::SCAN_RESPONSE, &edge.reply(true), t + 10);
    m.message(topic::LEASE, &edge.lease(true, 10_000), t + 20);
    let reported = m.tick(t + 2_000);
    assert_eq!(
        published(&reported, topic::USAGE_REQUEST).len(),
        1,
        "setup: one increment"
    );
    let mut restarted = Edge::new("edge-2");
    let out = m.message(topic::HELLO, &restarted.hello(), t + 2_100);
    assert!(published(&out, topic::USAGE_REQUEST).is_empty(), "{out:?}");
}

// ── the fake edge's seals ─────────────────────────────────────────────────────

/// `tools/fake-edge` seals in Python, written from FIRMWARE.md rather than
/// from css_wire, so a bench module believes it. Its tests pin these vectors;
/// this pins the same ones against css_wire_sign, so the two implementations
/// are checked against each other and neither against itself. The lease is
/// FIRMWARE.md's own worked example.
#[test]
fn the_fake_edges_vectors_are_css_wires() {
    let key = b"0a0b0c0d";
    let mut lease = Lease {
        resource: "res-1".into(),
        device_id: "dev-1".into(),
        grant: true,
        ttl_ms: 3000,
        reason: None,
        member: Some(spaces_device::wire::LocalMember {
            id: "u-1".into(),
            name: "Cal".into(),
        }),
        seal: None,
    };
    seal_with(key, &mut lease, "n-edge", "n-mod", 7);
    assert_eq!(
        lease.seal.unwrap().sig,
        "7ae3d8164ad6034d6e8e3950a07adb684b52433806630c547da9d9ae8c56647f"
    );
    let mut reply = LocalScanReply {
        resource: "laser".into(),
        authorized: true,
        reason: "authorized".into(),
        strike: Some(spaces_device::wire::LocalStrike { pulse_ms: 4200 }),
        member: None,
        sign_in: None,
        device_id: Some("dev-1".into()),
        seal: None,
    };
    seal_with(key, &mut reply, "n-edge", "n-mod", 3);
    assert_eq!(
        reply.seal.unwrap().sig,
        "62d02add2e706ed477cbe1029395c766cdc9f970f3da1baec75b8b36b62a65bb"
    );
    let mut hello = LocalHello {
        device_id: "dev-1".into(),
        seal: None,
    };
    seal_with(key, &mut hello, "n-edge", "n-mod", 1);
    assert_eq!(
        hello.seal.unwrap().sig,
        "123b8574e1df9cbd6df884449596683324ad0a0c5094989d605252a89aad7920"
    );
}
