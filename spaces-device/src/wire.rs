//! The payloads a module exchanges with its edge on the local broker.
//!
//! Every type here tolerates fields it does not know about, because the
//! platform promises to add fields without breaking older firmware. And every
//! optional field is an `Option`, because absent is not the same as `false`:
//! `relay_on` absent means "cannot report", while `false` means "I looked and
//! it is off", and the bypass detector needs to tell those apart.

use serde::{Deserialize, Serialize};

/// The local-broker topics a module uses. They are not namespaced and carry no
/// device id: the local broker is inside one building, and the payloads name
/// the tool and the card themselves.
pub mod topic {
    pub const TOOL_ON_REQUEST: &str = "toolguard/request/tool-on";
    pub const TOOL_OFF_REQUEST: &str = "toolguard/request/tool-off";
    pub const TOOL_LOG_REQUEST: &str = "toolguard/request/tool-log";
    pub const POWER_REPORT: &str = "toolguard/request/power";

    pub const TOOL_ON_RESPONSE: &str = "toolguard/response/tool-on";
    pub const TOOL_OFF_RESPONSE: &str = "toolguard/response/tool-off";
    pub const TOOL_LOG_RESPONSE: &str = "toolguard/response/tool-log";
    pub const POWER_RESPONSE: &str = "toolguard/response/power";
    pub const LEASE: &str = "toolguard/lease";

    /// Everything a module subscribes to. With a clean session the broker
    /// forgets subscriptions when the link drops, so these must be subscribed
    /// again on every reconnect, not just the first.
    pub const SUBSCRIPTIONS: &[&str] = &[
        TOOL_ON_RESPONSE,
        TOOL_OFF_RESPONSE,
        TOOL_LOG_RESPONSE,
        POWER_RESPONSE,
        LEASE,
    ];
}

/// `toolguard/request/tool-on` and `toolguard/request/tool-off`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRequest {
    pub card: String,
    pub tool_id: String,
}

/// `toolguard/request/tool-log`. `seconds` is what a metered tool bills by
/// default.
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
pub struct ToolLogRequest {
    pub card: String,
    pub tool_id: String,
    pub seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// `toolguard/request/power`. Also the module's heartbeat: the edge counts a
/// module as alive only while these keep arriving with its `device_id`, and
/// will not lease a module it has not heard from.
///
/// The decimal fields are strings, so a value that gets billed or compared
/// against a limit is never rounded through a binary float.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerReport {
    pub tool_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draw_now: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voltage_now: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_on: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_tripped: Option<bool>,
}

/// The answer on `toolguard/response/tool-on`.
///
/// FIRMWARE.md says this carries the server's envelope, with `tool_on: true`
/// as the only yes. The edge actually sends `{"authorized": …, "reason": …}`.
/// Both fields are read, so this works with the edge as built and with the
/// document as written; see [`ToolOnReply::is_yes`] for how they combine.
///
/// Since CS:S #166 the edge also echoes `tool_id`, which is the only thing that
/// makes a reply addressable: these topics are unnamespaced and carry no device
/// id, so every module on the broker receives every answer. See
/// [`ToolOnReply::is_for`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ToolOnReply {
    /// The tool this is an answer about, echoed by the edge (CS:S #166).
    ///
    /// `None` from an edge predating that, where there is nothing to match on.
    #[serde(default)]
    pub tool_id: Option<String>,
    #[serde(default)]
    pub authorized: Option<bool>,
    #[serde(default)]
    pub tool_on: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

impl ToolOnReply {
    /// Whether this reply permits energizing.
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

    /// Whether this reply is an answer about `tool_id` — ours, or a neighbour's.
    ///
    /// **Not a permission test.** It answers "is this mine", which has to be
    /// settled before `is_yes` is even asked: without it, a yes for the machine
    /// next door starts a session here, and that is a real arrangement rather
    /// than a hypothetical — one local broker, several readers, and a reply that
    /// named nothing until CS:S #166.
    ///
    /// An absent `tool_id` returns `true`, which is the honest answer for an
    /// edge that does not send one: nothing can be ruled out, so the module
    /// falls back to the weaker guard it has always had — accept a reply only
    /// while exactly one request is outstanding, and only for a short window.
    /// That is why this is not sufficient on its own and the timeout stays.
    pub fn is_for(&self, tool_id: &str) -> bool {
        match self.tool_id.as_deref() {
            None => true,
            Some(named) => named == tool_id,
        }
    }

    /// Something to show a human, from whichever field the sender filled in.
    pub fn explanation(&self) -> &str {
        self.message
            .as_deref()
            .or(self.reason.as_deref())
            .unwrap_or("no reason given")
    }
}

/// `toolguard/lease`: permission for one power module to stay energized for
/// `ttl_ms` from the moment it arrives.
///
/// `device_id`, `grant` and `ttl_ms` are required: a lease missing any of them
/// fails to parse, and a lease that fails to parse grants nothing.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Lease {
    pub device_id: String,
    pub grant: bool,
    pub ttl_ms: u64,
    /// The tool's UUID as sent by the edge, not its short external id (the
    /// example in FIRMWARE.md shows the latter). A module identifies its
    /// leases by `device_id` and does not compare this.
    #[serde(default)]
    pub tool_id: Option<String>,
    /// For logs. Never branched on: the vocabulary is not part of the contract.
    #[serde(default)]
    pub reason: Option<String>,
}
