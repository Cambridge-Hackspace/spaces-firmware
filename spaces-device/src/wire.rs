//! The payloads a module exchanges with its edge on the local broker.
//!
//! Every type here tolerates fields it does not know about, because the
//! platform promises to add fields without breaking older firmware. And every
//! optional field is an `Option`, because absent is not the same as `false`:
//! `relay_on` absent means "cannot report", while `false` means "I looked and
//! it is off", and the bypass detector needs to tell those apart.
//!
//! **One wire, resource-shaped** (CS:S #182). A door, a tool and a room are
//! the same thing on the local broker: a *resource* a badge is presented to,
//! whose bindings decide what follows a yes. Every request names the
//! `resource` -- its UUID or its short external id -- and nothing here says
//! door or tool. The `toolguard/*` and `door/*` topics this module spoke until
//! 0.3 are the edge's deprecated aliases for these, kept there for one release;
//! this module no longer speaks them.

use serde::{Deserialize, Serialize};

/// The local-broker topics a module uses. They are not namespaced and carry no
/// device id: the local broker is inside one building, and the payloads name
/// the resource and the card themselves.
pub mod topic {
    /// A badge presented to a resource.
    pub const SCAN_REQUEST: &str = "resource/request/scan";
    /// End the resource's session. A stop needs no badge (CS:S #179).
    pub const STOP_REQUEST: &str = "resource/request/stop";
    /// A usage increment.
    pub const USAGE_REQUEST: &str = "resource/request/usage";
    /// A power module's reading; also its heartbeat.
    pub const POWER_REPORT: &str = "resource/request/power";
    /// This device is alive. Every role, on the heartbeat timer; see
    /// [`super::AliveRequest`] for why it exists (CS:S #170).
    pub const ALIVE_REQUEST: &str = "resource/request/alive";

    /// The answer to a scan: `authorized`, and on a yes for a resource with a
    /// latch, the `strike` pulse.
    pub const SCAN_RESPONSE: &str = "resource/response/scan";
    pub const STOP_RESPONSE: &str = "resource/response/stop";
    pub const USAGE_RESPONSE: &str = "resource/response/usage";
    pub const POWER_RESPONSE: &str = "resource/response/power";
    /// Permission for one power module to stay energized, for `ttl_ms`.
    pub const LEASE: &str = "resource/lease";

    /// Everything a module subscribes to. With a clean session the broker
    /// forgets subscriptions when the link drops, so these must be subscribed
    /// again on every reconnect, not just the first.
    pub const SUBSCRIPTIONS: &[&str] = &[
        SCAN_RESPONSE,
        STOP_RESPONSE,
        USAGE_RESPONSE,
        POWER_RESPONSE,
        LEASE,
    ];
}

/// `resource/request/scan`: a badge presented to a resource. `card` always
/// names who is asking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRequest {
    pub resource: String,
    pub card: String,
}

/// `resource/request/stop`: end the resource's session.
///
/// `card` is **optional** (CS:S #179): a stop is a safety act and needs no
/// badge, so a device with no card -- a plug with a stop button -- names the
/// resource alone, and the server ends whatever session is open on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRequest {
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
}

/// `resource/request/usage`. `seconds` is what a metered tool bills by
/// default.
///
/// **`card` is optional** (CS:S #174). A device that identified the member
/// names their card, and the server checks it against the session. A device
/// that cannot -- a plug, which only ever receives a lease -- names the
/// resource alone, and the server attributes the usage to whoever is signed on
/// to it. The server already knows; the plug does not have to.
///
/// **Two durations, because a working signal gives two honest answers** (CS:S
/// #169). A laser's enable line drops on every scan-line return, so the same
/// engrave is either continuous bench time or only the intervals the beam was
/// on -- 30-50% apart on a raster.
///
/// - `seconds` is the usage this module calls billable: the **held** figure,
///   gaps bridged by [`crate::run_filter::RunFilter`].
/// - `active_seconds` is the **raw** figure, unbridged.
///
/// Which one bills is `tools.usage_basis` on the server, not the module's
/// choice. **Omit `active_seconds` entirely if nothing is wired in:** absent
/// means "this device cannot tell" and the server falls back to `seconds`,
/// while `0.0` means "the machine did not run" and bills nothing. Reporting a
/// zero from a module with no sensor would make every session on a
/// working-time-billed tool free.
///
/// **Reports accumulate.** The server adds each one to the open session in a
/// single atomic statement, so a module should send increments on a timer
/// rather than one total at the end -- a reboot then costs one interval instead
/// of the whole session. `temperature` is recorded and never billed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    pub seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// `resource/request/alive`: this device is alive (CS:S #170).
///
/// **Every device sends it, whatever its role**, on the heartbeat timer. It is
/// the liveness contract on the local broker: the edge credits every binding the
/// device holds, and forwards it so the server's `last_seen_at` -- what the
/// silence detector and the admin device list read -- is written too.
///
/// A power report counts as alive as well, but only a plug can send one the
/// server will accept: the relay is gated on the `power` binding. A reader or a
/// sensor behind an edge was therefore invisible to the server, and with the
/// default `fail_off` a reader denied its own tool a lease forever. This is the
/// message that fixes that. It carries the device id and nothing else: health
/// beyond "alive" is a different message with its own shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliveRequest {
    pub device_id: String,
}

/// `resource/request/power`. Also the module's heartbeat: the edge counts a
/// module as alive only while these keep arriving with its `device_id`, and
/// will not lease a module it has not heard from.
///
/// The decimal fields are numbers on this wire (they were strings on the
/// deprecated `toolguard/request/power`): the edge forwards them as decimal
/// strings to the server, which is where a value that gets billed or compared
/// against a limit is read, so nothing here is rounded through a float on its
/// way to a bill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerReport {
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draw_now: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voltage_now: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_on: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_tripped: Option<bool>,
}

/// The pulse a scan reply carries for a resource with a latch (CS:S #182):
/// how long to release it, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Strike {
    pub pulse_ms: i32,
}

/// The answer on `resource/response/scan`.
///
/// `authorized: true` is the only yes; `tool_on` is the same bit under the
/// deprecated alias's name and is read in case an older edge answers with it
/// -- see [`ScanReply::is_yes`] for how they combine. `resource` is the
/// resource this is an answer about, echoed by the edge, which is the only
/// thing that makes a reply addressable: these topics are unnamespaced and
/// carry no device id, so every module on the broker receives every answer.
/// See [`ScanReply::is_for`]. `strike` is present only on a yes for a resource
/// with a latch; a module that drives none ignores it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ScanReply {
    #[serde(default)]
    pub resource: Option<String>,
    /// The deprecated alias's name for `resource`; read, never written.
    #[serde(default)]
    pub tool_id: Option<String>,
    #[serde(default)]
    pub authorized: Option<bool>,
    /// The deprecated alias's name for `authorized`; read, never written.
    #[serde(default)]
    pub tool_on: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub strike: Option<Strike>,
}

impl ScanReply {
    /// Whether this reply permits energizing, or releasing a latch.
    ///
    /// Only an explicit `true` is a yes, and any explicit `false` is a no
    /// whatever else the reply says. `status: "ok"` on its own is not a yes,
    /// a reply arriving is not a yes, and `reason`/`message` are for humans
    /// and never consulted.
    pub fn is_yes(&self) -> bool {
        let said_no = self.authorized == Some(false) || self.tool_on == Some(false);
        let said_yes = self.authorized == Some(true) || self.tool_on == Some(true);
        said_yes && !said_no
    }

    /// Whether this reply is an answer about `resource` -- ours, or a
    /// neighbour's.
    ///
    /// **Not a permission test.** It answers "is this mine", which has to be
    /// settled before `is_yes` is even asked: without it, a yes for the machine
    /// next door starts a session here, and that is a real arrangement rather
    /// than a hypothetical -- one local broker, several readers, and a reply
    /// that named nothing until CS:S #166.
    ///
    /// A reply naming nothing returns `true`, which is the honest answer for an
    /// edge that does not send a name: nothing can be ruled out, so the module
    /// falls back to the weaker guard it has always had -- accept a reply only
    /// while exactly one request is outstanding, and only for a short window.
    /// That is why this is not sufficient on its own and the timeout stays.
    pub fn is_for(&self, resource: &str) -> bool {
        match self.named() {
            None => true,
            Some(named) => named == resource,
        }
    }

    /// The resource the reply names, under either spelling.
    pub fn named(&self) -> Option<&str> {
        self.resource.as_deref().or(self.tool_id.as_deref())
    }

    /// Something to show a human, from whichever field the sender filled in.
    pub fn explanation(&self) -> &str {
        self.message
            .as_deref()
            .or(self.reason.as_deref())
            .unwrap_or("no reason given")
    }

    /// The latch pulse this yes carries, if any. Zero and negative are
    /// nothing: a latch released for no time is a latch not released.
    pub fn pulse_ms(&self) -> Option<u64> {
        self.strike
            .filter(|s| s.pulse_ms > 0)
            .map(|s| s.pulse_ms as u64)
    }
}

/// `resource/lease`: permission for one power module to stay energized for
/// `ttl_ms` from the moment it arrives.
///
/// `device_id`, `grant` and `ttl_ms` are required: a lease missing any of them
/// fails to parse, and a lease that fails to parse grants nothing.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Lease {
    pub device_id: String,
    pub grant: bool,
    pub ttl_ms: u64,
    /// The resource's UUID as sent by the edge, not its short external id. A
    /// module identifies its leases by `device_id` and does not compare this.
    #[serde(default)]
    pub resource: Option<String>,
    /// The deprecated alias's name for `resource`; read, never written.
    #[serde(default)]
    pub tool_id: Option<String>,
    /// For logs. Never branched on: the vocabulary is not part of the contract.
    #[serde(default)]
    pub reason: Option<String>,
}
