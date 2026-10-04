//! How long to wait before trying again.
//!
//! A configured module never gives up on its network. Giving up used to mean
//! falling back to setup mode, an open access point whose page sets the broker
//! address: after a long enough outage, anyone in range could point the module
//! at a broker of their own, and that broker would be the one granting leases.
//! So there is no "give up" here, only a wait that grows to a ceiling.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// The wait after the first failure.
    pub first: Duration,
    /// The wait never grows past this.
    pub ceiling: Duration,
}

impl Backoff {
    /// For joining Wi-Fi: 2 s, then 4, 8 and 16, then every 30 s for good.
    pub const WIFI: Backoff = Backoff {
        first: Duration::from_secs(2),
        ceiling: Duration::from_secs(30),
    };

    /// The wait after `failures` failures in a row. Doubles from
    /// [`first`](Self::first) and stops at [`ceiling`](Self::ceiling); zero
    /// failures means no wait.
    pub fn after(&self, failures: u32) -> Duration {
        if failures == 0 {
            return Duration::ZERO;
        }
        // Past 2^16 doublings everything is at the ceiling anyway, and this
        // keeps the shift from overflowing.
        let doublings = (failures - 1).min(16);
        self.first
            .checked_mul(1 << doublings)
            .map_or(self.ceiling, |wait| wait.min(self.ceiling))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn no_failures_no_wait() {
        assert_eq!(Backoff::WIFI.after(0), Duration::ZERO);
    }

    #[test]
    fn doubles_then_holds_at_the_ceiling() {
        let waits: Vec<_> = (1..=7).map(|n| Backoff::WIFI.after(n)).collect();
        assert_eq!(waits, [2, 4, 8, 16, 30, 30, 30].map(secs));
    }

    #[test]
    fn never_overflows_however_long_the_outage() {
        // A month of failures every 30 s, and then some.
        for failures in [100, 86_400, u32::MAX] {
            assert_eq!(Backoff::WIFI.after(failures), secs(30));
        }
    }

    #[test]
    fn a_huge_first_wait_is_still_capped() {
        let backoff = Backoff {
            first: Duration::MAX,
            ceiling: secs(5),
        };
        assert_eq!(backoff.after(1), secs(5));
        assert_eq!(backoff.after(3), secs(5));
    }
}
