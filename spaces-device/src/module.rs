//! A class 1 module: reads a card, asks the edge, and — if it has one to
//! switch — switches a tool.
//!
//! [`Module`] is a pure state machine. Things that happen — a card swiped, a
//! message from the broker, time passing — go in as method calls with the
//! current time, and what the firmware must do comes back as [`Action`]s. It
//! never touches hardware, the network or a clock, which is what lets every
//! rule below be tested on an ordinary computer.
//!
//! **The roles are independent, and [`Config`] is where that lives.** CS:S
//! binds a device to a resource in a role — `reader`, `power`, `sensor` — and a
//! box may fill any combination: a badge reader that also switches the laser
//! and watches it work, a reader that only identifies the member and shows them
//! an answer, a reader that watches but cannot switch. The two that change the
//! protocol rather than just the wiring are [`Config::gates_power`], because
//! the edge leases only `power` bindings, and [`Config::senses_running`],
//! because absent and zero are different answers to "did the machine run".
//!
//! **A session and an energized output are not the same thing**, and conflating
//! them is what made this type unusable for a device with no relay: it would
//! wait `first_lease_timeout_ms` for a lease the edge was never going to offer
//! it, and release the session five seconds after every valid badge.
//!
//! The rules, which are the point of this module:
//!
//! 1. The tool is energized only while there is an explicit yes for the
//!    session **and** an unexpired lease. Neither is enough alone. A device
//!    with nothing to energize has no output to drive and is not leased, which
//!    does not weaken this rule — it is the rule being vacuous.
//! 2. Anything other than an explicit yes is a no: a refusal, a reply with
//!    `status: "ok"` but no yes, a reply that does not parse, or no reply at
//!    all within the timeout.
//! 3. A lease is timed from its arrival, on the caller's monotonic clock, for
//!    the `ttl_ms` it carries. When it runs out the tool goes off by itself,
//!    without any message telling it to. `grant: false` turns it off at once.
//! 4. Losing the lease ends the session, for a device that gates power: it can
//!    no longer guarantee the tool is safe. A renewal arriving afterwards does
//!    not bring the tool back; a fresh swipe is needed. Auto-restarting a
//!    machine is the behaviour that latching interlocks exist to prevent.
//! 5. Every session that started is ended, however it ends: usage is reported
//!    and the tool is released, with the card that started the session. A
//!    device with no lease to lapse ends its sessions by the policies in
//!    [`Config`] — a second tap of the same card, the machine falling idle, or
//!    a maximum session — because `tool-on` opens a session that something has
//!    to close and, with `tool_billing.enabled` off, the server sweeps none.
//! 6. A power report carrying the device id goes out on a fixed interval. It is
//!    the heartbeat that keeps leases coming, and the only thing on the local
//!    broker that credits a module with being alive — so every device sends it,
//!    including one with no relay, which reports no relay state rather than
//!    claiming `false`.
//! 7. Usage is the time the machine was working, reported as two figures: the
//!    held one that [`crate::run_filter`] bridges, and the raw asserted one.
//!    Which of them bills is the tool's setting on the server, not this
//!    module's choice.

use crate::run_filter::RunFilter;
use crate::wire::{
    topic, AliveRequest, Lease, PowerReport, ToolLogRequest, ToolOnReply, ToolRequest,
};

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
    /// Something worth telling the person standing at the machine.
    ///
    /// Separate from [`Action::Note`] on purpose. A note is prose for a log and
    /// its wording is not a contract; this is a decision a panel has to render,
    /// and a display that scraped a log line for the verdict would break the
    /// first time somebody improved the sentence.
    Outcome(Outcome),
}

/// What just happened, for a display.
///
/// Transient: each is emitted once, when it happens. What the module is *doing*
/// — resting, waiting, in a session — is [`Module::activity`] instead, because a
/// panel needs both and they answer different questions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// An explicit yes. The session is open; on a device that gates power the
    /// tool is not energized until a lease arrives.
    Authorized,
    /// A no, with the edge's own words for it. **Display this, do not branch on
    /// it** — the strings are not part of the contract.
    Refused(String),
    /// Nothing answered in time, which is a no that is nobody's fault. Worth
    /// distinguishing on a panel: a member told "access denied" checks their
    /// training, where one told "no answer" checks the network.
    NoAnswer,
    /// The session ended, and why.
    Ended(EndReason),
}

/// What the module is doing, for a display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// No session, nothing outstanding.
    Idle,
    /// A tool-on request is out and its answer has not arrived.
    Asking,
    /// A session is open. `energized` is whether the output is actually on,
    /// which is a different question: a device that gates power is authorized
    /// but dark until a lease arrives, and one that switches nothing is never
    /// energized at all.
    Session { energized: bool },
}

/// Usage so far in the open session, for a panel.
///
/// A member metered by the second should be able to see the figure while it
/// runs, and see it stop when the machine does: a panel that said only
/// "Signed in" over a running meter was asking to be trusted rather than read.
/// Only a device that senses the machine has one; see [`Module::usage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// Whether the machine counts as working right now. This is the **held**
    /// signal -- gaps bridged by the run filter -- so it does not flicker at
    /// scan-line rate on a laser.
    pub running: bool,
    /// The held figure so far, whole seconds: what `tool-log`'s `seconds`
    /// carries. Which figure the server bills is its setting, not this one.
    pub seconds: u64,
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
    /// Whether this device reports usage at all. Default `true`.
    ///
    /// `false` for a device that must not meter: a second plug on a tool another
    /// device already meters -- an extractor that follows the laser's lease --
    /// would otherwise report the same minutes again, and reports accumulate.
    /// Everything else is unchanged: the session, the lease, `tool-off`, the
    /// heartbeats. Only `tool-log` is withheld.
    pub reports_usage: bool,
    /// Whether this device switches the tool's power.
    ///
    /// **This is the difference between a session and an energized output, and
    /// they are not the same thing.** The safety rule is unchanged either way:
    /// an output is driven only on an explicit yes *and* an unexpired lease.
    /// What this decides is whether there is an output at all.
    ///
    /// `true` is a power module: it drives [`Output::Tool`], the edge leases it
    /// (`edge/src/modules.rs` offers leases only to `power` bindings), and
    /// losing the lease ends the session because the device can no longer
    /// guarantee the tool is safe.
    ///
    /// `false` is a device with nothing to energize -- a badge reader that only
    /// identifies the member and shows them an answer. It is never leased, so
    /// it must not wait for a lease it will never get: a reader-only device
    /// built as a power module ends every authorized scan five seconds later
    /// with `NoFirstLease`, and the member watches ACCESS GRANTED turn into a
    /// dead session. It also had no safety guarantee to lose, so a lease is not
    /// what ends its session -- see the policies below.
    pub gates_power: bool,
    /// Whether re-tapping the card that started the session ends it.
    ///
    /// The honest stop for a device that cannot sense the machine and cannot
    /// cut its power: the member says when they are done. Required for a
    /// reader-only device, because `tool-on` opens a session and nothing else
    /// here would ever close it -- and on the common configuration
    /// (`tool_billing.enabled = false`) the server has no backstop either, so
    /// the tool sticks `in_use` for the next member.
    pub stop_on_second_tap: bool,
    /// End the session after this long with the machine not working. `0` never.
    ///
    /// Needs [`Config::senses_running`] to mean anything: with no sensor the
    /// machine is never "working", and this would end every session as soon as
    /// it opened. Guarded rather than trusted -- see `Module::advance`.
    pub idle_end_ms: u64,
    /// A session is ended after this long regardless. `0` never.
    ///
    /// The backstop for a device whose other stops can all be missed: nobody
    /// re-taps, nothing senses the machine, and there is no lease to lapse.
    /// Without it a reader-only device can still strand a tool.
    pub max_session_ms: u64,
    /// Whether this device reads cards, and so owns the session.
    ///
    /// `true` is a device with a reader: a swipe asks the edge, the answer
    /// opens a session here, and this device reports and releases it.
    ///
    /// `false` is a plug. It has no reader, so it never learns a card and owns
    /// no session -- some other device bound to the same tool read the badge.
    /// **For it the lease is not permission to stay on, it is the whole
    /// authorization**, because the edge only ever leases a tool whose access
    /// chain it has already satisfied. That is #83's coordinator model: reader
    /// auth, edge decides, plug obeys.
    ///
    /// A plug publishes no `tool-on`, `tool-off` or `tool-log`, and **cannot**,
    /// even with a sensor wired in: every one of those names a card, and
    /// nothing the edge sends a power module carries one -- `toolguard/lease`
    /// has `tool_id`, `device_id`, `grant`, `ttl_ms` and `reason`, and no card.
    /// So usage on such a tool is the reader's to report. A plug's sensor is
    /// still worth having for the Running indicator and the draw figure.
    pub reads_cards: bool,
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
            reports_usage: true,
            // A power module, as every caller of this constructor was before
            // the flag existed. Behaviour is unchanged for all of them.
            gates_power: true,
            stop_on_second_tap: false,
            idle_end_ms: 0,
            max_session_ms: 0,
            reads_cards: true,
        }
    }

    /// A plug: switches the tool, reads no card, owns no session.
    ///
    /// Energized exactly while the edge's lease for it is unexpired, and off
    /// the moment it is not. Reports draw and relay state, and heartbeats.
    pub fn power_only(device_id: impl Into<String>, tool_id: impl Into<String>) -> Self {
        Config {
            reads_cards: false,
            ..Config::new(device_id, tool_id)
        }
    }

    /// A device that identifies the member and shows them an answer, and
    /// switches nothing.
    ///
    /// Not leased, so it waits for no lease; and since neither a lease nor a
    /// relay can end its session, the member's second tap does. Add
    /// [`Config::senses_running`] and [`Config::idle_end_ms`] if something is
    /// wired in to sense the machine, and set [`Config::max_session_ms`] as a
    /// backstop for a member who walks away.
    pub fn switches_nothing(device_id: impl Into<String>, tool_id: impl Into<String>) -> Self {
        Config {
            gates_power: false,
            stop_on_second_tap: true,
            ..Config::new(device_id, tool_id)
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

/// The two usage clocks and what has been reported of them.
///
/// One of these runs under a session on a device that reads cards, and one
/// runs while a plug is energized (CS:S #174): the same figures, reported the
/// same way, with or without a card to name.
#[derive(Debug, Clone, PartialEq)]
struct Meter {
    /// When this meter opened: the interval for the first report counts from
    /// here, so a zero-length report never falls out the moment it opens.
    opened_at: u64,
    /// Usage so far, and when the current run started, if running. This is the
    /// **held** figure -- gaps bridged by the run filter -- and it is what
    /// `tool-log`'s `seconds` carries.
    held_ms: u64,
    held_since: Option<u64>,
    /// The same clock against the **raw** signal, unbridged: `active_seconds`.
    /// On a laser this is beam-on time where `held_ms` is bench time, and the
    /// two differ by 30-50% on a raster.
    raw_ms: u64,
    raw_since: Option<u64>,
    /// How much of each figure has already been published. Reports are
    /// increments, so the next one carries the difference -- not the total, or
    /// the server's accumulation would count every interval twice.
    reported_held_ms: u64,
    reported_raw_ms: u64,
    /// When the last usage report went out, for the interval.
    last_report_at: Option<u64>,
}

impl Meter {
    fn open(now: u64) -> Self {
        Meter {
            opened_at: now,
            held_ms: 0,
            held_since: None,
            raw_ms: 0,
            raw_since: None,
            reported_held_ms: 0,
            reported_raw_ms: 0,
            last_report_at: None,
        }
    }

    /// Both figures as of `at`, including any run still open.
    ///
    /// Non-mutating on purpose: a reporter that folded the open interval into
    /// the accumulator to read it would double-count the moment it was called
    /// twice before the machine stopped.
    fn totals(&self, at: u64) -> (u64, u64) {
        let held = self.held_ms + self.held_since.map_or(0, |since| at.saturating_sub(since));
        let raw = self.raw_ms + self.raw_since.map_or(0, |since| at.saturating_sub(since));
        (held, raw)
    }

    /// What has accrued since the last report: the next increment.
    fn unreported(&self, at: u64) -> (u64, u64) {
        let (held, raw) = self.totals(at);
        (
            held.saturating_sub(self.reported_held_ms),
            raw.saturating_sub(self.reported_raw_ms),
        )
    }

    fn mark_reported(&mut self, at: u64) {
        let (held, raw) = self.totals(at);
        self.reported_held_ms = held;
        self.reported_raw_ms = raw;
        self.last_report_at = Some(at);
    }

    /// Whether a periodic report is due. From the last report, or from the
    /// opening for the first one -- not from `None` meaning "due immediately".
    fn report_due(&self, interval_ms: u64, now: u64) -> bool {
        interval_ms > 0
            && now.saturating_sub(self.last_report_at.unwrap_or(self.opened_at)) >= interval_ms
    }

    /// Advance both clocks to the current state of their signals.
    ///
    /// Both resolve to whatever interval the caller polls at, since an edge is
    /// only seen when something calls in. That is well below the precision
    /// `use_hold_ms` already imposes on the held figure.
    fn note(&mut self, held: bool, raw: bool, now: u64) {
        match (self.held_since, held) {
            (None, true) => self.held_since = Some(now),
            (Some(since), false) => {
                self.held_ms += now.saturating_sub(since);
                self.held_since = None;
            }
            _ => {}
        }
        match (self.raw_since, raw) {
            (None, true) => self.raw_since = Some(now),
            (Some(since), false) => {
                self.raw_ms += now.saturating_sub(since);
                self.raw_since = None;
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Session {
    card: String,
    authorized_at: u64,
    /// When the current lease runs out. `None` until the first lease arrives.
    lease_until: Option<u64>,
    meter: Meter,
    /// Since when the machine has not been working, for `idle_end_ms`. Starts
    /// at authorization, so a session where the machine never starts can still
    /// time out rather than sitting open until something else notices.
    idle_since: Option<u64>,
}

impl Session {
    fn leased(&self, now: u64) -> bool {
        self.lease_until.is_some_and(|until| now < until)
    }
}

/// Why a session ended, for the console and the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    ToolOff,
    LeaseExpired,
    LeaseRefused,
    NoFirstLease,
    /// The member re-tapped the card that started the session.
    SecondTap,
    /// The machine has not been working for [`Config::idle_end_ms`].
    Idle,
    /// [`Config::max_session_ms`] elapsed. The backstop, not a normal stop.
    MaxSession,
}

/// What time passing has made due. Decided first, then acted on, so the
/// decision can read the phase that the action then replaces.
enum Due {
    Nothing,
    ForgetAbandoned,
    GiveUpAsking(String),
    End(EndReason),
}

pub struct Module {
    config: Config,
    phase: Phase,
    /// The running input after conditioning, and the raw reading behind it.
    /// Both are kept because they meter different figures: the filtered one is
    /// `seconds`, the raw one `active_seconds`.
    filter: RunFilter,
    /// The lease held by a device that owns no session of its own, i.e. a plug
    /// ([`Config::reads_cards`] false). For a device with a reader the lease
    /// lives on its [`Session`] instead, because it is scoped to that session.
    standing_lease: Option<u64>,
    /// A plug's meter: open while it is energized, closed -- and reported,
    /// naming no card -- when the lease lapses (CS:S #174). A device with a
    /// reader meters under its [`Session`] instead.
    standing_meter: Option<Meter>,
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
            standing_lease: None,
            standing_meter: None,
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

    /// What this module is doing, for a panel to render.
    ///
    /// A plug reports [`Activity::Idle`] whatever its relay is doing: it owns
    /// no session, so it has no activity in this sense. Ask
    /// [`Module::tool_is_on`] about its relay.
    pub fn activity(&self) -> Activity {
        match &self.phase {
            Phase::Idle { .. } => Activity::Idle,
            Phase::Asking { .. } => Activity::Asking,
            Phase::Session(_) => Activity::Session {
                energized: self.tool_on,
            },
        }
    }

    /// How much the open session has used so far, as of `now`, for a panel.
    ///
    /// `None` outside a session, and `None` always on a device that does not
    /// sense the machine: it has no figure, and a panel must not invent one.
    /// Non-mutating, like [`Meter::totals`]; the caller is expected to have
    /// ticked to `now` first, since the hold releases only when something
    /// notices.
    pub fn usage(&self, now: u64) -> Option<Usage> {
        if !self.config.senses_running {
            return None;
        }
        match &self.phase {
            Phase::Session(session) => Some(Usage {
                running: session.meter.held_since.is_some(),
                seconds: session.meter.totals(now).0 / 1000,
            }),
            _ => None,
        }
    }

    /// Call once at start-up, after connecting. Puts both outputs in a known
    /// off state and sends the first heartbeat at once rather than a second
    /// later.
    pub fn start(&mut self, now: u64) -> Vec<Action> {
        // The Tool arm only for a device that has one, as in `update_outputs`.
        // Putting an output in a known-off state at boot is a safety measure
        // for a relay; for a device with no relay it is a claim about one.
        let mut out = Vec::new();
        if self.config.gates_power {
            out.push(Action::Set {
                output: Output::Tool,
                on: false,
            });
        }
        out.push(Action::Set {
            output: Output::Running,
            on: false,
        });
        self.tool_on = false;
        self.running_on = false;
        self.heartbeat(now, &mut out);
        out
    }

    /// A card was presented.
    pub fn swipe(&mut self, card: &str, now: u64) -> Vec<Action> {
        let mut out = self.advance(now);
        // A plug has no reader. Reported rather than ignored, because a card
        // arriving at a device configured not to have one means the config and
        // the hardware disagree, and the symptom otherwise is a tool that
        // never comes on with nothing anywhere saying why.
        if !self.config.reads_cards {
            out.push(Action::Note(format!(
                "ignoring card {card}: this device is configured with no reader"
            )));
            return out;
        }
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
            // The member's own card again ends their session, where configured.
            // This is the only stop a device has that neither switches the
            // tool nor senses it: there is no lease to lapse and no machine to
            // go quiet, and `tool-on` has already opened a session that
            // something must close -- on the default configuration
            // (`tool_billing.enabled = false`) the server sweeps nothing, so
            // "nobody closes it" means the tool sticks `in_use` for the next
            // member.
            //
            // Scoped to the card that started it. Anyone else's card is still
            // refused, because ending a stranger's session is not a courtesy.
            Phase::Session(session) if self.config.stop_on_second_tap && session.card == card => {
                out.push(Action::Note(format!(
                    "card {card} again: ending the session"
                )));
                self.end(EndReason::SecondTap, now, &mut out);
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
    /// What should end an open session, and why.
    ///
    /// The order is the priority. `max_session_ms` is first because it is the
    /// backstop: it must fire whatever else is or is not true of the session.
    ///
    /// **The lease arms are reached only when this device gates power.** A
    /// device that switches nothing is never leased
    /// (`edge/src/modules.rs` offers leases only to `power` bindings), so
    /// asking it to wait for one ends every authorized scan at
    /// `first_lease_timeout_ms` with `NoFirstLease`. It also never held the
    /// safety guarantee that losing a lease represents, so a lapsed lease is
    /// not what should end its session.
    fn session_due(&self, session: &Session, now: u64) -> Due {
        let open_for = now.saturating_sub(session.authorized_at);
        if self.config.max_session_ms > 0 && open_for >= self.config.max_session_ms {
            return Due::End(EndReason::MaxSession);
        }
        if self.config.gates_power {
            match session.lease_until {
                Some(until) if now >= until => return Due::End(EndReason::LeaseExpired),
                None if open_for >= self.config.first_lease_timeout_ms => {
                    return Due::End(EndReason::NoFirstLease)
                }
                _ => {}
            }
        }
        // Gated on `senses_running`, not merely on the interval being set:
        // with no sensor the machine is never working, so an idle timeout
        // would end every session the moment it opened.
        if self.config.senses_running && self.config.idle_end_ms > 0 {
            if let Some(since) = session.idle_since {
                if now.saturating_sub(since) >= self.config.idle_end_ms {
                    return Due::End(EndReason::Idle);
                }
            }
        }
        Due::Nothing
    }

    fn advance(&mut self, now: u64) -> Vec<Action> {
        let mut out = Vec::new();
        let due = match &self.phase {
            Phase::Idle {
                abandoned: Some((_, since)),
            } if now.saturating_sub(*since) >= LATE_REPLY_WINDOW_MS => Due::ForgetAbandoned,
            Phase::Asking { card, since }
                if now.saturating_sub(*since) >= self.config.reply_timeout_ms =>
            {
                Due::GiveUpAsking(card.clone())
            }
            Phase::Session(session) => self.session_due(session, now),
            _ => Due::Nothing,
        };
        match due {
            Due::Nothing => {}
            Due::ForgetAbandoned => self.phase = Phase::Idle { abandoned: None },
            Due::GiveUpAsking(card) => {
                out.push(Action::Note(format!(
                    "no answer about card {card}: refused (a timeout is a no)"
                )));
                out.push(Action::Outcome(Outcome::NoAnswer));
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
        let interval = self.config.usage_interval_ms;
        // Nothing accrued means no report: the machine is authorized but idle,
        // and an empty report would be noise on the broker and a row in the
        // audit log. Decided here, with the meter borrowed, and published
        // after, with it released.
        let due = match (&mut self.phase, &mut self.standing_meter) {
            (Phase::Session(session), _) if session.meter.report_due(interval, now) => {
                let (d_held, d_raw) = session.meter.unreported(now);
                if d_held == 0 && d_raw == 0 {
                    return;
                }
                session.meter.mark_reported(now);
                Some((Some(session.card.clone()), d_held, d_raw))
            }
            (_, Some(meter)) if meter.report_due(interval, now) => {
                let (d_held, d_raw) = meter.unreported(now);
                if d_held == 0 && d_raw == 0 {
                    return;
                }
                meter.mark_reported(now);
                Some((None, d_held, d_raw))
            }
            _ => None,
        };
        if let Some((card, d_held, d_raw)) = due {
            self.report_usage(out, card.as_deref(), d_held, d_raw);
        }
    }

    /// Publish a usage increment -- unless this device is configured not to
    /// meter ([`Config::reports_usage`]), in which case nothing goes out and
    /// the clocks are simply never read. The one gate for all three callers:
    /// the periodic report, a session's end, a plug's meter closing.
    fn report_usage(&self, out: &mut Vec<Action>, card: Option<&str>, held_ms: u64, raw_ms: u64) {
        if !self.config.reports_usage {
            return;
        }
        out.push(self.usage_action(card, held_ms, raw_ms));
    }

    /// One `tool-log`, carrying an increment of each figure.
    ///
    /// `active_seconds` is omitted unless this module actually senses the
    /// machine, because absent and zero mean different things to the server:
    /// absent falls back to the billable figure, zero bills nothing. `card` is
    /// omitted by a plug, which never learns one: the server attributes the
    /// usage to whoever is signed on to the tool (CS:S #174).
    fn usage_action(&self, card: Option<&str>, held_ms: u64, raw_ms: u64) -> Action {
        let log = ToolLogRequest {
            card: card.map(str::to_string),
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
                        out.push(Action::Outcome(Outcome::Authorized));
                        self.phase = Phase::Session(Session {
                            card,
                            authorized_at: now,
                            lease_until: None,
                            meter: Meter::open(now),
                            idle_since: Some(now),
                        });
                    }
                    Some(reply) => {
                        out.push(Action::Note(format!(
                            "card {card} refused: {}",
                            reply.explanation()
                        )));
                        out.push(Action::Outcome(Outcome::Refused(
                            reply.explanation().to_string(),
                        )));
                        self.phase = Phase::Idle { abandoned: None };
                    }
                    None => {
                        out.push(Action::Note(format!(
                            "card {card} refused: the answer did not parse"
                        )));
                        // Our own words: there is no reply to quote. Still a
                        // refusal, per rule 2 -- an answer that does not parse
                        // is not a yes.
                        out.push(Action::Outcome(Outcome::Refused(
                            "unreadable answer".to_string(),
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
        // A plug has no session, and for it the lease is the whole
        // authorization: the edge only leases a tool whose access chain it has
        // already satisfied, having consulted whatever read the badge.
        if !self.config.reads_cards {
            self.standing_lease = lease.grant.then(|| now.saturating_add(lease.ttl_ms));
            if !lease.grant {
                out.push(Action::Note("lease withdrawn; switching off".into()));
            }
            self.update_outputs(now, out);
            return;
        }
        if !matches!(self.phase, Phase::Session(_)) {
            // For a device that reads cards, a lease is permission for a relay
            // to stay closed, not permission to close it. Without an authorized
            // session of its own there is nothing to keep on.
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
        let (d_held, d_raw) = session.meter.unreported(stopped_at);
        // Whether a periodic report already covered the session. A session that
        // reported nothing always gets a report, even a zero one -- rule 5 says
        // every session that started is reported, and a tool that was
        // authorized and never used is a fact worth recording.
        let reported_before = session.meter.last_report_at.is_some();
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
            EndReason::SecondTap => "the same card again: the member is done".to_string(),
            EndReason::Idle => "the machine has not been working; releasing the tool".to_string(),
            EndReason::MaxSession => {
                "the maximum session time elapsed; releasing the tool".to_string()
            }
        }));
        // Skipped only when a periodic report has already gone out AND there is
        // nothing left to add. Sending the zero anyway would be harmless to the
        // server's accumulation but is pure noise on the broker and a row in
        // the audit log for every session.
        if !(reported_before && d_held == 0 && d_raw == 0) {
            self.report_usage(out, Some(&card), d_held, d_raw);
        }
        out.push(self.request(topic::TOOL_OFF_REQUEST, &card));
        // After the release, not before: the panel saying "session ended" while
        // the tool is still energized is the one ordering a member could act on
        // and be wrong about.
        out.push(Action::Outcome(Outcome::Ended(why)));
    }

    /// Bring the outputs, and the running clock, into line with the state.
    fn update_outputs(&mut self, now: u64, out: &mut Vec<Action>) {
        // Whether usage may accrue, which is NOT the same question as whether
        // an output is energized.
        //
        // For a power module it is the same: it cannot be running if it is not
        // energized, and energizing needs the lease. For a device that switches
        // nothing there is no lease and never will be, so the only thing to
        // gate on is the session -- and gating on a lease it cannot get would
        // mean it never recorded a second of the usage it was installed to
        // measure.
        let live = if self.config.reads_cards {
            match &self.phase {
                Phase::Session(session) => !self.config.gates_power || session.leased(now),
                _ => false,
            }
        } else {
            // A plug: no session of its own, so the lease is the authorization
            // and its expiry is the instruction to stop. Read here rather than
            // tracked as an event, so it lapses on whatever call comes next --
            // nobody sends a message when a lease runs out, by design.
            self.standing_lease.is_some_and(|until| now < until)
        };
        // The output. Only a device that gates power has one, and the safety
        // rule for it is unchanged: an explicit yes AND an unexpired lease.
        let tool = self.config.gates_power && live;
        let running = live && self.filter.is_on();
        // The billable figure is the usage this device can honestly measure
        // (CS:S #174). With a sensor, the machine working -- the held signal.
        // Without one, if it switches the power, the time the power was on:
        // "when the power is on, the bill goes to the tool". A bare reader has
        // neither and reports no time; it was never installed to measure any.
        let occupancy = live
            && if self.config.senses_running {
                self.filter.is_on()
            } else {
                self.config.gates_power
            };
        // The raw signal runs its own clock for `active_seconds`. Gated the
        // same way, so neither figure can accrue time outside a session.
        let active = live && self.filter.asserted();

        if let Phase::Session(session) = &mut self.phase {
            session.meter.note(occupancy, active, now);
            // Idle is measured against the HELD signal, not the raw one, or a
            // laser's scan-line returns would each reset the idle clock and a
            // job would look like a machine starting and stopping forever.
            match (session.idle_since, running) {
                (Some(_), true) => session.idle_since = None,
                (None, false) => session.idle_since = Some(now),
                _ => {}
            }
        }

        // A plug's meter opens when it energizes and closes when it does not,
        // reporting what accrued with no card to name (CS:S #174): the server
        // attributes it to whoever is signed on to the tool. A lapsed lease cut
        // the power when it ran out, which may be a little before this call
        // noticed; bill to the cut, not to the noticing.
        if !self.config.reads_cards {
            match (self.standing_meter.as_mut(), live) {
                (None, true) => {
                    // Noted on the pass that opens it, or the first interval
                    // would be lost to whenever the next call happened to come.
                    let mut meter = Meter::open(now);
                    meter.note(occupancy, active, now);
                    self.standing_meter = Some(meter);
                }
                (Some(meter), true) => meter.note(occupancy, active, now),
                (Some(meter), false) => {
                    let stopped_at = self.standing_lease.map_or(now, |until| until.min(now));
                    meter.note(false, false, stopped_at);
                    let (d_held, d_raw) = meter.unreported(stopped_at);
                    let reported_before = meter.last_report_at.is_some();
                    self.standing_meter = None;
                    // The same rule as a session's end: skipped only when a
                    // periodic report already went out AND nothing is left.
                    if !(reported_before && d_held == 0 && d_raw == 0) {
                        self.report_usage(out, None, d_held, d_raw);
                    }
                }
                (None, false) => {}
            }
        }

        // `Output::Running` is an indicator -- "the machine is working" -- and
        // any device may have one, including a reader with a sensor and a lamp.
        // `Output::Tool` is the actuator, and only a device that gates power
        // has one: emitting it otherwise would have a firmware author wire up
        // an `Outputs` impl for a relay it has not got, which is how a device
        // comes to report a relay state the bypass detector then believes
        // (CS:S #84).
        //
        // Running goes off before the tool and comes on after it, so the two
        // never disagree about which is allowed.
        if !running && self.running_on {
            self.running_on = false;
            out.push(Action::Set {
                output: Output::Running,
                on: false,
            });
        }
        if self.config.gates_power && tool != self.tool_on {
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
            // A device that switches nothing has no draw to report and no
            // relay to report on. `None` is "cannot tell", which the bypass
            // detector treats as unknown -- whereas a `"0.0"` and a `false`
            // from a device with no relay are two positive claims about a
            // circuit it cannot see, and the detector would believe both.
            //
            // The report still goes out: it is the plug's telemetry, and it
            // counts as being alive too for edges that predate the pulse. The
            // pulse below is the liveness contract proper (CS:S #170).
            draw_now: self.config.gates_power.then(|| draw.to_string()),
            voltage_now: None,
            relay_on: self.config.gates_power.then_some(self.tool_on),
            self_tripped: None,
        };
        out.push(Action::Publish {
            topic: topic::POWER_REPORT,
            payload: serde_json::to_string(&report).expect("a PowerReport always serializes"),
        });

        // The pulse (CS:S #170). Every role, every heartbeat. A reader or a
        // sensor has no power to report, and the server refuses a relayed power
        // report for anything not bound as `power` -- so without this they were
        // invisible to the server and, under the default `fail_off`, a reader
        // denied its own tool a lease forever.
        let alive = AliveRequest {
            device_id: self.config.device_id.clone(),
        };
        out.push(Action::Publish {
            topic: topic::ALIVE_REQUEST,
            payload: serde_json::to_string(&alive).expect("an AliveRequest always serializes"),
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
