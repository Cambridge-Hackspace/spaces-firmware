//! A class 1 module: reads a card, asks the edge, and switches a tool.
//!
//! [`Module`] is a pure state machine. Things that happen — a card swiped, a
//! message from the broker, time passing — go in as method calls with the
//! current time, and what the firmware must do comes back as [`Action`]s. It
//! never touches hardware, the network or a clock, which is what lets every
//! rule below be tested on an ordinary computer.
//!
//! The rules, which are the point of this module:
//!
//! 1. The tool is energized only while there is an explicit yes for the
//!    session **and** an unexpired lease. Neither is enough alone.
//! 2. Anything other than an explicit yes is a no: a refusal, a reply with
//!    `status: "ok"` but no yes, a reply that does not parse, or no reply at
//!    all within the timeout.
//! 3. A lease is timed from its arrival, on the caller's monotonic clock, for
//!    the `ttl_ms` it carries. When it runs out the tool goes off by itself,
//!    without any message telling it to. `grant: false` turns it off at once.
//! 4. Losing the lease ends the session. A renewal arriving afterwards does not
//!    bring the tool back; a fresh swipe is needed. Auto-restarting a machine
//!    is the behaviour that latching interlocks exist to prevent.
//! 5. Every session that started is ended, however it ends: usage is reported
//!    and the tool is released, with the card that started the session.
//! 6. A power report carrying the device id goes out on a fixed interval. It is
//!    the heartbeat that keeps leases coming.
//! 7. Only the time the machine was actually running is reported as usage.

use crate::run_filter::RunFilter;
use crate::wire::{topic, Lease, PowerReport, ToolLogRequest, ToolOnReply, ToolRequest};

/// The two outputs a module drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// The tool's power: a relay in a real module, an LED in the demo.
    Tool,
    /// The machine actually working, e.g. a laser firing. Can only be on while
    /// the tool is.
    Running,
}

/// What the firmware must do. Apply them in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Publish {
        topic: &'static str,
        payload: String,
    },
    Set {
        output: Output,
        on: bool,
    },
    /// Something worth telling the person watching the serial console.
    Note(String),
}

/// What a module needs to know about itself.
#[derive(Debug, Clone)]
pub struct Config {
    /// From registration. Leases are recognised by it, and power reports carry
    /// it so the edge knows the module is alive.
    pub device_id: String,
    /// The tool, as named in requests. Usually its short external id.
    pub tool_id: String,
    /// How long to wait for an answer to a tool-on request.
    pub reply_timeout_ms: u64,
    /// How long, after a yes, to wait for the first lease before giving up.
    pub first_lease_timeout_ms: u64,
    /// How often to send a power report. Must be well inside the edge's
    /// offline threshold (`module_offline_ms`, 5 s in the reference edge).
    pub heartbeat_interval_ms: u64,
    /// Draw to report while energized and idle, and while running. Strings,
    /// as on the wire.
    pub draw_idle: String,
    pub draw_running: String,
    /// Conditioning for the running input: how long it must hold before usage
    /// starts, and how long usage persists after it drops. Both `0` is
    /// passthrough, which is exactly the behaviour that predates this field.
    /// See [`crate::run_filter`] -- on a laser `use_hold_ms` is what makes the
    /// difference between one engrave and three hundred fragments.
    pub use_assert_ms: u32,
    pub use_hold_ms: u32,
    /// Whether this module senses the machine working at all.
    ///
    /// Gates the `active_seconds` field on `tool-log`, and the distinction is
    /// not cosmetic: CS:S treats an **absent** figure as "this device cannot
    /// tell" and falls back to the billable one, while a **zero** means "the
    /// machine did not run" and bills nothing. A module with no sensor that
    /// reported `0` would make every session on a tool billed by working time
    /// cost nothing. Leave `false` unless something really is wired in.
    pub senses_running: bool,
    /// How often to publish a usage increment during a session. `0` reports
    /// once, at the end.
    ///
    /// Reports accumulate server-side in one atomic statement, so increments
    /// are safe and a reboot then costs one interval instead of the whole
    /// session. Not the default, because it changes how often a module talks.
    pub usage_interval_ms: u64,
}

impl Config {
    pub fn new(device_id: impl Into<String>, tool_id: impl Into<String>) -> Self {
        Config {
            device_id: device_id.into(),
            tool_id: tool_id.into(),
            reply_timeout_ms: 5_000,
            first_lease_timeout_ms: 5_000,
            heartbeat_interval_ms: 1_000,
            draw_idle: "0.3".into(),
            draw_running: "4.2".into(),
            // Passthrough, no sensing claimed, one report at the end: a module
            // built from `new` and nothing else behaves exactly as it did
            // before any of this existed.
            use_assert_ms: 0,
            use_hold_ms: 0,
            senses_running: false,
            usage_interval_ms: 0,
        }
    }
}

/// How long to remember a request that timed out, in case its answer arrives
/// late. See [`Phase::Idle`].
const LATE_REPLY_WINDOW_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    /// No session. `abandoned` is a request that got no answer in time. If a yes
    /// for it arrives later, the edge has opened a session the module is not
    /// using, and the tool would sit "in use" for everyone else until released.
    Idle { abandoned: Option<(String, u64)> },
    /// A tool-on request is out and its answer has not arrived. Only one at a
    /// time: answers carry nothing that says which request they belong to.
    Asking { card: String, since: u64 },
    /// The edge said yes for `card`.
    Session(Session),
}

#[derive(Debug, Clone, PartialEq)]
struct Session {
    card: String,
    authorized_at: u64,
    /// When the current lease runs out. `None` until the first lease arrives.
    lease_until: Option<u64>,
    /// Usage so far, and when the current run started, if running. This is the
    /// **held** figure -- gaps bridged by the run filter -- and it is what
    /// `tool-log`'s `seconds` carries.
    running_ms: u64,
    running_since: Option<u64>,
    /// The same clock against the **raw** signal, unbridged: `active_seconds`.
    /// On a laser this is beam-on time where `running_ms` is bench time, and
    /// the two differ by 30-50% on a raster.
    active_ms: u64,
    active_since: Option<u64>,
    /// How much of each figure has already been published. Reports are
    /// increments, so the next one carries the difference -- not the total, or
    /// the server's accumulation would count every interval twice.
    reported_ms: u64,
    reported_active_ms: u64,
    /// When the last usage report went out, for the interval.
    last_usage_at: Option<u64>,
}

impl Session {
    fn leased(&self, now: u64) -> bool {
        self.lease_until.is_some_and(|until| now < until)
    }

    /// Both figures as of `at`, including any run still open.
    ///
    /// Non-mutating on purpose: a reporter that folded the open interval into
    /// the accumulator to read it would double-count the moment it was called
    /// twice before the machine stopped.
    fn totals(&self, at: u64) -> (u64, u64) {
        let held = self.running_ms
            + self
                .running_since
                .map_or(0, |since| at.saturating_sub(since));
        let raw = self.active_ms
            + self
                .active_since
                .map_or(0, |since| at.saturating_sub(since));
        (held, raw)
    }
}

/// Why a session ended, for the console and the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    ToolOff,
    LeaseExpired,
    LeaseRefused,
    NoFirstLease,
}

pub struct Module {
    config: Config,
    phase: Phase,
    /// The running input after conditioning, and the raw reading behind it.
    /// Both are kept because they meter different figures: the filtered one is
    /// `seconds`, the raw one `active_seconds`.
    filter: RunFilter,
    /// The outputs as last set, so only changes are emitted.
    tool_on: bool,
    running_on: bool,
    last_report: Option<u64>,
}

impl Module {
    pub fn new(config: Config) -> Self {
        let filter = RunFilter::new(config.use_assert_ms, config.use_hold_ms);
        Module {
            config,
            phase: Phase::Idle { abandoned: None },
            filter,
            tool_on: false,
            running_on: false,
            last_report: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Whether the tool output is on, as last set.
    pub fn tool_is_on(&self) -> bool {
        self.tool_on
    }

    /// Whether the running output is on, as last set.
    pub fn running_is_on(&self) -> bool {
        self.running_on
    }

    /// Call once at start-up, after connecting. Puts both outputs in a known
    /// off state and sends the first heartbeat at once rather than a second
    /// later.
    pub fn start(&mut self, now: u64) -> Vec<Action> {
        let mut out = vec![
            Action::Set {
                output: Output::Tool,
                on: false,
            },
            Action::Set {
                output: Output::Running,
                on: false,
            },
        ];
        self.tool_on = false;
        self.running_on = false;
        self.heartbeat(now, &mut out);
        out
    }

    /// A card was presented.
    pub fn swipe(&mut self, card: &str, now: u64) -> Vec<Action> {
        let mut out = self.advance(now);
        match &self.phase {
            Phase::Idle { .. } => {
                out.push(Action::Note(format!("asking the edge about card {card}")));
                out.push(self.request(topic::TOOL_ON_REQUEST, card));
                self.phase = Phase::Asking {
                    card: card.to_string(),
                    since: now,
                };
            }
            Phase::Asking { card: waiting, .. } => {
                out.push(Action::Note(format!(
                    "ignoring card {card}: still waiting for the answer about {waiting}"
                )));
            }
            Phase::Session(session) => {
                out.push(Action::Note(format!(
                    "ignoring card {card}: this tool is in use by {}",
                    session.card
                )));
            }
        }
        out
    }

    /// The person pressed stop.
    pub fn tool_off(&mut self, now: u64) -> Vec<Action> {
        let mut out = self.advance(now);
        match self.phase {
            Phase::Session(_) => self.end(EndReason::ToolOff, now, &mut out),
            _ => out.push(Action::Note(
                "tool off pressed, but no session is open".into(),
            )),
        }
        out
    }

    /// The machine started or stopped working.
    ///
    /// `on` is **semantic**, not a pin level: the firmware reading the hardware
    /// applies the sense of its own wiring. Glitch rejection and gap bridging
    /// happen here, per [`Config::use_assert_ms`] and [`Config::use_hold_ms`],
    /// so a caller may pass every sample it takes.
    pub fn running(&mut self, on: bool, now: u64) -> Vec<Action> {
        let mut out = self.advance(now);
        self.filter.sample(on, now);
        self.update_outputs(now, &mut out);
        out
    }

    /// A message arrived on one of [`topic::SUBSCRIPTIONS`].
    pub fn message(&mut self, topic_name: &str, payload: &[u8], now: u64) -> Vec<Action> {
        let mut out = self.advance(now);
        match topic_name {
            topic::TOOL_ON_RESPONSE => self.on_reply(payload, now, &mut out),
            topic::LEASE => self.on_lease(payload, now, &mut out),
            // Acknowledgements. Not instructions, and never a reason to stay on.
            topic::TOOL_OFF_RESPONSE | topic::TOOL_LOG_RESPONSE | topic::POWER_RESPONSE => {}
            other => out.push(Action::Note(format!("ignoring message on {other}"))),
        }
        out
    }

    /// Time passed. Call often: a lease runs out only when this, or another
    /// call, notices. Every ~50 ms keeps the cut well inside a lease's TTL.
    pub fn tick(&mut self, now: u64) -> Vec<Action> {
        let mut out = self.advance(now);
        self.maybe_report_usage(now, &mut out);
        self.heartbeat(now, &mut out);
        out
    }

    // ---------------------------------------------------------------- internal

    /// Everything that happens just because time has passed. Run first in
    /// every entry point, so an expired lease is noticed before anything else
    /// is decided.
    fn advance(&mut self, now: u64) -> Vec<Action> {
        let mut out = Vec::new();
        // Decide first, then act: the decision reads the phase and the action
        // replaces it.
        enum Due {
            Nothing,
            ForgetAbandoned,
            GiveUpAsking(String),
            End(EndReason),
        }
        let due = match &self.phase {
            Phase::Idle {
                abandoned: Some((_, since)),
            } if now.saturating_sub(*since) >= LATE_REPLY_WINDOW_MS => Due::ForgetAbandoned,
            Phase::Asking { card, since }
                if now.saturating_sub(*since) >= self.config.reply_timeout_ms =>
            {
                Due::GiveUpAsking(card.clone())
            }
            Phase::Session(session) => match session.lease_until {
                Some(until) if now >= until => Due::End(EndReason::LeaseExpired),
                None if now.saturating_sub(session.authorized_at)
                    >= self.config.first_lease_timeout_ms =>
                {
                    Due::End(EndReason::NoFirstLease)
                }
                _ => Due::Nothing,
            },
            _ => Due::Nothing,
        };
        match due {
            Due::Nothing => {}
            Due::ForgetAbandoned => self.phase = Phase::Idle { abandoned: None },
            Due::GiveUpAsking(card) => {
                out.push(Action::Note(format!(
                    "no answer about card {card}: refused (a timeout is a no)"
                )));
                self.phase = Phase::Idle {
                    abandoned: Some((card, now)),
                };
            }
            Due::End(why) => self.end(why, now, &mut out),
        }
        // Before the outputs, so a hold that ran out is noticed by the clocks
        // in `update_outputs` on this pass rather than the next one. A machine
        // that stopped and is never sampled again has no edge to deliver, so
        // without this its usage would run until somebody touched the tool.
        self.filter.tick(now);
        self.update_outputs(now, &mut out);
        out
    }

    /// Publish a usage increment if one is due.
    ///
    /// Driven from [`Module::tick`] only, not from every entry point. Reporting
    /// is a time-driven concern and `tick` is the time-driven call -- already
    /// mandatory, since a lease expires only when something notices. Running it
    /// from `advance` instead meant a usage report could fall out of a swipe or
    /// a relay reading, landing on whichever call the interval happened to
    /// cross; correct, but surprising to a firmware author and awkward to
    /// reason about. A session ending still reports its remainder through
    /// [`Module::end`], so nothing is lost.
    ///
    /// **An increment, never a total.** The server accumulates reports, so a
    /// module that re-sent the running total would be billed for the sum of a
    /// series instead of its last term.
    fn maybe_report_usage(&mut self, now: u64, out: &mut Vec<Action>) {
        if self.config.usage_interval_ms == 0 {
            return;
        }
        let Phase::Session(session) = &self.phase else {
            return;
        };
        // From the last report, or from authorization for the first one -- not
        // from `None` meaning "due immediately", which would publish a
        // zero-length report the moment a session opened.
        let since = session.last_usage_at.unwrap_or(session.authorized_at);
        if now.saturating_sub(since) < self.config.usage_interval_ms {
            return;
        }
        let (held, raw) = session.totals(now);
        if held == session.reported_ms && raw == session.reported_active_ms {
            // Nothing accrued. The machine is authorized but idle, and an empty
            // report would be noise on the broker and a row in the audit log.
            return;
        }
        let card = session.card.clone();
        let (d_held, d_raw) = (
            held.saturating_sub(session.reported_ms),
            raw.saturating_sub(session.reported_active_ms),
        );
        out.push(self.usage_action(&card, d_held, d_raw));
        if let Phase::Session(session) = &mut self.phase {
            session.reported_ms = held;
            session.reported_active_ms = raw;
            session.last_usage_at = Some(now);
        }
    }

    /// One `tool-log`, carrying an increment of each figure.
    ///
    /// `active_seconds` is omitted unless this module actually senses the
    /// machine, because absent and zero mean different things to the server:
    /// absent falls back to the billable figure, zero bills nothing.
    fn usage_action(&self, card: &str, held_ms: u64, raw_ms: u64) -> Action {
        let log = ToolLogRequest {
            card: card.to_string(),
            tool_id: self.config.tool_id.clone(),
            seconds: held_ms as f64 / 1000.0,
            active_seconds: self.config.senses_running.then_some(raw_ms as f64 / 1000.0),
            temperature: None,
        };
        Action::Publish {
            topic: topic::TOOL_LOG_REQUEST,
            payload: serde_json::to_string(&log).expect("a ToolLogRequest always serializes"),
        }
    }

    fn on_reply(&mut self, payload: &[u8], now: u64, out: &mut Vec<Action>) {
        let reply: Option<ToolOnReply> = serde_json::from_slice(payload).ok();

        // IS IT MINE, BEFORE IS IT YES (CS:S #166). Every module on the local
        // broker receives every answer on this topic -- it is unnamespaced and
        // names no device -- so a neighbour's verdict arrives here looking
        // exactly like ours. Until the edge echoed `tool_id` the only defence
        // was the narrow window below; now a reply that names another tool is
        // dropped outright.
        //
        // THE PHASE DOES NOT CHANGE. Staying in `Asking` is the point: our own
        // answer may still be in flight, and treating a neighbour's reply as a
        // refusal would deny a member for someone else's swipe. If ours never
        // comes, the reply timeout already turns that into a no.
        if let Some(r) = &reply {
            if !r.is_for(&self.config.tool_id) {
                if matches!(self.phase, Phase::Asking { .. }) {
                    out.push(Action::Note(format!(
                        "ignoring a tool-on answer about {} -- this module asked about {}",
                        r.tool_id.as_deref().unwrap_or("?"),
                        self.config.tool_id
                    )));
                }
                return;
            }
        }

        match &self.phase {
            Phase::Asking { card, .. } => {
                let card = card.clone();
                match reply {
                    Some(reply) if reply.is_yes() => {
                        out.push(Action::Note(format!(
                            "card {card} authorized; waiting for a lease before switching on"
                        )));
                        self.phase = Phase::Session(Session {
                            card,
                            authorized_at: now,
                            lease_until: None,
                            running_ms: 0,
                            running_since: None,
                            active_ms: 0,
                            active_since: None,
                            reported_ms: 0,
                            reported_active_ms: 0,
                            last_usage_at: None,
                        });
                    }
                    Some(reply) => {
                        out.push(Action::Note(format!(
                            "card {card} refused: {}",
                            reply.explanation()
                        )));
                        self.phase = Phase::Idle { abandoned: None };
                    }
                    None => {
                        out.push(Action::Note(format!(
                            "card {card} refused: the answer did not parse"
                        )));
                        self.phase = Phase::Idle { abandoned: None };
                    }
                }
            }
            Phase::Idle {
                abandoned: Some((card, _)),
            } if reply.as_ref().is_some_and(|r| r.is_yes()) => {
                // The edge opened a session after we gave up waiting. Release
                // it, or the tool reads as in use until somebody reboots
                // something.
                let card = card.clone();
                out.push(Action::Note(format!(
                    "a late yes for card {card} arrived after giving up; releasing the tool"
                )));
                out.push(self.request(topic::TOOL_OFF_REQUEST, &card));
                self.phase = Phase::Idle { abandoned: None };
            }
            _ => {
                // Nothing outstanding, so this answer is not ours: with more
                // than one module on a broker, it is someone else's.
            }
        }
    }

    fn on_lease(&mut self, payload: &[u8], now: u64, out: &mut Vec<Action>) {
        let Ok(lease) = serde_json::from_slice::<Lease>(payload) else {
            // A lease that does not parse grants nothing. Silence is the
            // safe reading.
            return;
        };
        if lease.device_id != self.config.device_id {
            return; // every module on the broker sees every lease
        }
        if !matches!(self.phase, Phase::Session(_)) {
            // A lease is permission for a relay to stay closed, not permission
            // to close it. Without an authorized session there is nothing to
            // keep on.
            return;
        }
        if !lease.grant {
            self.end(EndReason::LeaseRefused, now, out);
        } else if let Phase::Session(session) = &mut self.phase {
            if session.lease_until.is_none() {
                out.push(Action::Note(format!(
                    "lease granted for {} ms; switching on",
                    lease.ttl_ms
                )));
            }
            session.lease_until = Some(now.saturating_add(lease.ttl_ms));
        }
        self.update_outputs(now, out);
    }

    /// End the session: outputs off first, then report the usage, then
    /// release the tool, with the card that started it. Usage before release,
    /// because on a metered tool the release settles the bill.
    fn end(&mut self, why: EndReason, now: u64, out: &mut Vec<Action>) {
        let Phase::Session(session) = &self.phase else {
            return;
        };
        // A lapsed lease cut the power when it ran out, which may be a little
        // before this call noticed. Bill to the cut, not to the noticing.
        let stopped_at = match (why, session.lease_until) {
            (EndReason::LeaseExpired, Some(until)) => until.min(now),
            _ => now,
        };
        // The remainder, not the total: whatever accrued since the last
        // periodic report. With `usage_interval_ms = 0` nothing has been
        // reported yet, so the remainder IS the total and this is the one
        // report of the session.
        let (held, raw) = session.totals(stopped_at);
        let d_held = held.saturating_sub(session.reported_ms);
        let d_raw = raw.saturating_sub(session.reported_active_ms);
        // Whether a periodic report already covered the session. A session that
        // reported nothing always gets a report, even a zero one -- rule 5 says
        // every session that started is reported, and a tool that was
        // authorized and never used is a fact worth recording.
        let reported_before = session.last_usage_at.is_some();
        let card = session.card.clone();
        self.phase = Phase::Idle { abandoned: None };
        self.update_outputs(now, out);

        out.push(Action::Note(match why {
            EndReason::ToolOff => "tool off".to_string(),
            EndReason::LeaseExpired => "the lease ran out; switching off".to_string(),
            EndReason::LeaseRefused => "the edge withdrew the lease; switching off".to_string(),
            EndReason::NoFirstLease => {
                "authorized, but no lease arrived; is this module bound in the power role?"
                    .to_string()
            }
        }));
        // Skipped only when a periodic report has already gone out AND there is
        // nothing left to add. Sending the zero anyway would be harmless to the
        // server's accumulation but is pure noise on the broker and a row in
        // the audit log for every session.
        if !(reported_before && d_held == 0 && d_raw == 0) {
            out.push(self.usage_action(&card, d_held, d_raw));
        }
        out.push(self.request(topic::TOOL_OFF_REQUEST, &card));
    }

    /// Bring the outputs, and the running clock, into line with the state.
    fn update_outputs(&mut self, now: u64, out: &mut Vec<Action>) {
        let tool = match &self.phase {
            Phase::Session(session) => session.leased(now),
            _ => false,
        };
        let running = tool && self.filter.is_on();
        // The raw signal runs its own clock for `active_seconds`. Gated on the
        // lease too, so neither figure can accrue time the member was not
        // authorized for.
        let active = tool && self.filter.asserted();

        // Both clocks resolve to whatever interval the caller polls at, since
        // an edge is only seen when something calls in. That is well below the
        // precision `use_hold_ms` already imposes on the held figure.
        if let Phase::Session(session) = &mut self.phase {
            match (session.running_since, running) {
                (None, true) => session.running_since = Some(now),
                (Some(since), false) => {
                    session.running_ms += now.saturating_sub(since);
                    session.running_since = None;
                }
                _ => {}
            }
            match (session.active_since, active) {
                (None, true) => session.active_since = Some(now),
                (Some(since), false) => {
                    session.active_ms += now.saturating_sub(since);
                    session.active_since = None;
                }
                _ => {}
            }
        }

        // Running goes off before the tool and comes on after it, so the two
        // never disagree about which is allowed.
        if !running && self.running_on {
            self.running_on = false;
            out.push(Action::Set {
                output: Output::Running,
                on: false,
            });
        }
        if tool != self.tool_on {
            self.tool_on = tool;
            out.push(Action::Set {
                output: Output::Tool,
                on: tool,
            });
        }
        if running && !self.running_on {
            self.running_on = true;
            out.push(Action::Set {
                output: Output::Running,
                on: true,
            });
        }
    }

    fn heartbeat(&mut self, now: u64, out: &mut Vec<Action>) {
        let due = self
            .last_report
            .is_none_or(|last| now.saturating_sub(last) >= self.config.heartbeat_interval_ms);
        if !due {
            return;
        }
        self.last_report = Some(now);
        let draw = if self.running_on {
            &self.config.draw_running
        } else if self.tool_on {
            &self.config.draw_idle
        } else {
            "0.0"
        };
        let report = PowerReport {
            tool_id: self.config.tool_id.clone(),
            device_id: Some(self.config.device_id.clone()),
            draw_now: Some(draw.to_string()),
            voltage_now: None,
            // This module knows its own output, so it says so. A device that
            // cannot tell would leave this out rather than claim false.
            relay_on: Some(self.tool_on),
            self_tripped: None,
        };
        out.push(Action::Publish {
            topic: topic::POWER_REPORT,
            payload: serde_json::to_string(&report).expect("a PowerReport always serializes"),
        });
    }

    fn request(&self, topic_name: &'static str, card: &str) -> Action {
        let request = ToolRequest {
            card: card.to_string(),
            tool_id: self.config.tool_id.clone(),
        };
        Action::Publish {
            topic: topic_name,
            payload: serde_json::to_string(&request).expect("a ToolRequest always serializes"),
        }
    }
}
