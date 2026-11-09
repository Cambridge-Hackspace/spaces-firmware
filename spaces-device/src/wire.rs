//! The payloads a module exchanges with its edge on the local broker.
//!
//! **Defined in CS:S, not here** (#11). The topics and message shapes come from
//! the `css_wire` crate in the cooperative-systems-spaces repository, the one
//! definition both ends of the wire use (CS:S #219). This crate used to carry a
//! copy under other names, kept in step with CS:S by a contract test on each
//! side; a field now changes in one place and both ends see it.
//!
//! What stays here is the device's side of reading them: the rules a module
//! applies to a reply ([`ScanVerdict`]) and the topics it subscribes to. The
//! old names (`ScanRequest`, `topic::POWER_REPORT`, ...) remain as aliases of
//! the `css_wire` ones for this release, so dependants can move at their own
//! pace; new code should use the `css_wire` names.
//!
//! **One wire, resource-shaped** (CS:S #182). A door, a tool and a room are
//! the same thing on the local broker: a *resource* a badge is presented to,
//! whose bindings decide what follows a yes. Every request names the
//! `resource`, and nothing here says door or tool.

pub use css_wire::{
    local, LocalAliveRequest, LocalCondition, LocalPower, LocalScan, LocalScanReply, LocalStop,
    LocalStrike, LocalUsage, ResourceLeasePayload,
};

/// The local-broker topics a module uses, by the names this crate has always
/// given them. Each is the `css_wire::local` constant, not a second copy of
/// the string.
pub mod topic {
    use css_wire::local;

    pub const SCAN_REQUEST: &str = local::SCAN_REQUEST;
    pub const STOP_REQUEST: &str = local::STOP_REQUEST;
    pub const USAGE_REQUEST: &str = local::USAGE_REQUEST;
    /// `css_wire::local::POWER_REPORT_REQUEST`.
    pub const POWER_REPORT: &str = local::POWER_REPORT_REQUEST;
    /// `css_wire::local::RESOURCE_ALIVE_REQUEST`.
    pub const ALIVE_REQUEST: &str = local::RESOURCE_ALIVE_REQUEST;
    pub const CONDITION_REQUEST: &str = local::CONDITION_REQUEST;

    pub const SCAN_RESPONSE: &str = local::SCAN_RESPONSE;
    pub const STOP_RESPONSE: &str = local::STOP_RESPONSE;
    pub const USAGE_RESPONSE: &str = local::USAGE_RESPONSE;
    /// `css_wire::local::POWER_REPORT_RESPONSE`.
    pub const POWER_RESPONSE: &str = local::POWER_REPORT_RESPONSE;
    /// `css_wire::local::RESOURCE_LEASE`.
    pub const LEASE: &str = local::RESOURCE_LEASE;

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

/// The old name of [`LocalScan`]; for this release only.
pub type ScanRequest = LocalScan;
/// The old name of [`LocalStop`]; for this release only.
pub type StopRequest = LocalStop;
/// The old name of [`LocalUsage`]; for this release only.
pub type UsageReport = LocalUsage;
/// The old name of [`LocalAliveRequest`]; for this release only.
pub type AliveRequest = LocalAliveRequest;
/// The old name of [`LocalCondition`]; for this release only.
pub type ConditionReport = LocalCondition;
/// The old name of [`LocalPower`]; for this release only.
pub type PowerReport = LocalPower;
/// The old name of [`LocalStrike`]; for this release only.
pub type Strike = LocalStrike;
/// The old name of [`LocalScanReply`]; for this release only.
pub type ScanReply = LocalScanReply;
/// The old name of [`ResourceLeasePayload`]; for this release only.
pub type Lease = ResourceLeasePayload;

/// How a module reads the answer on `resource/response/scan`.
///
/// The reply's shape is CS:S's: `resource`, `authorized` and `reason` are all
/// required, so a reply missing any of them does not parse -- and a reply that
/// does not parse is a no. That includes the envelope `FIRMWARE.md` once
/// described (`status`, `tool_on`), which left the wire in CS:S #192 and is no
/// longer read at all: failing closed on it is the safe reading.
pub trait ScanVerdict {
    /// Whether this reply permits energizing, or releasing a latch. Only an
    /// explicit `authorized: true` is a yes; `reason` is for humans and is
    /// never consulted.
    fn is_yes(&self) -> bool;

    /// Whether this reply is an answer about `resource` -- ours, or a
    /// neighbour's.
    ///
    /// **Not a permission test.** It answers "is this mine", which has to be
    /// settled before `is_yes` is even asked: these topics are unnamespaced and
    /// carry no device id, so every module on the broker receives every answer,
    /// and without it a yes for the machine next door starts a session here
    /// (CS:S #166).
    fn is_for(&self, resource: &str) -> bool;

    /// Something to show a human.
    fn explanation(&self) -> &str;

    /// The latch pulse this yes carries, if any. Zero and negative are
    /// nothing: a latch released for no time is a latch not released.
    fn pulse_ms(&self) -> Option<u64>;
}

impl ScanVerdict for LocalScanReply {
    fn is_yes(&self) -> bool {
        self.authorized
    }

    fn is_for(&self, resource: &str) -> bool {
        self.resource == resource
    }

    fn explanation(&self) -> &str {
        if self.reason.is_empty() {
            "no reason given"
        } else {
            &self.reason
        }
    }

    fn pulse_ms(&self) -> Option<u64> {
        self.strike
            .as_ref()
            .filter(|s| s.pulse_ms > 0)
            .map(|s| s.pulse_ms as u64)
    }
}

/// A lease's lifetime in milliseconds, as a module counts it. The wire carries
/// it signed; a negative one grants no time at all.
pub fn lease_ttl_ms(lease: &ResourceLeasePayload) -> u64 {
    lease.ttl_ms.max(0) as u64
}
