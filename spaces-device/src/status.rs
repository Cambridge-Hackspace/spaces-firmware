//! What the board's status light shows.
//!
//! A pure function from the module's state and the time to a colour, so the
//! patterns can be checked here and the firmware only has to put the result on
//! an LED. The light is for whoever is standing at the machine: it says whether
//! the module is set up, on its way, working, or in trouble. It never says
//! whether the tool is on; the tool's own output does that.

/// The states worth telling apart at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// Serving the setup page: join its access point to configure it.
    Setup,
    /// Joining Wi-Fi, registering, or reaching the edge's broker for the
    /// first time.
    Connecting,
    /// On the edge's broker: everything working.
    Online,
    /// Was on the broker and lost it. Leases stop with it, so the tool will
    /// be off.
    BrokerLost,
    /// Stopped by an error, about to restart.
    Failed,
}

impl Status {
    /// Every status, for code that stores one as a number.
    pub const ALL: [Status; 5] = [
        Status::Setup,
        Status::Connecting,
        Status::Online,
        Status::BrokerLost,
        Status::Failed,
    ];

    /// The inverse of `status as u8`.
    pub fn from_u8(n: u8) -> Option<Status> {
        Status::ALL.get(usize::from(n)).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const OFF: Rgb = Rgb { r: 0, g: 0, b: 0 };
}

/// The brightest any channel gets. These LEDs are painful at full power, and
/// an eighth is plenty to read across a room.
pub const MAX: u8 = 32;

const BLUE: Rgb = Rgb { r: 0, g: 0, b: MAX };
const AMBER: Rgb = Rgb {
    r: MAX,
    g: MAX * 3 / 8,
    b: 0,
};
const GREEN: Rgb = Rgb { r: 0, g: MAX, b: 0 };
const RED: Rgb = Rgb { r: MAX, g: 0, b: 0 };

/// One pulse of the setup light, dark to bright to dark.
pub const SETUP_PERIOD_MS: u64 = 2000;
/// One on-and-off of the connecting blink.
pub const BLINK_PERIOD_MS: u64 = 1000;
/// How often the online light flashes, and for how long.
pub const ONLINE_PERIOD_MS: u64 = 3000;
pub const ONLINE_FLASH_MS: u64 = 100;

/// The colour to show in `status` at `now_ms`, on any monotonic clock.
///
/// - Setup: blue, pulsing slowly.
/// - Connecting: amber, blinking.
/// - Online: a brief green flash every few seconds, dark otherwise, so a
///   working module does not light up a room.
/// - Broker lost: amber, steady.
/// - Failed: red, steady.
pub fn colour(status: Status, now_ms: u64) -> Rgb {
    match status {
        Status::Setup => {
            let phase = now_ms % SETUP_PERIOD_MS;
            let half = SETUP_PERIOD_MS / 2;
            // A triangle: up for the first half, down for the second.
            let level = if phase < half {
                phase
            } else {
                SETUP_PERIOD_MS - phase
            };
            scale(BLUE, level, half)
        }
        Status::Connecting => {
            if now_ms % BLINK_PERIOD_MS < BLINK_PERIOD_MS / 2 {
                AMBER
            } else {
                Rgb::OFF
            }
        }
        Status::Online => {
            if now_ms % ONLINE_PERIOD_MS < ONLINE_FLASH_MS {
                GREEN
            } else {
                Rgb::OFF
            }
        }
        Status::BrokerLost => AMBER,
        Status::Failed => RED,
    }
}

/// `colour` at `level` out of `full`.
fn scale(colour: Rgb, level: u64, full: u64) -> Rgb {
    let channel = |c: u8| (u64::from(c) * level.min(full) / full) as u8;
    Rgb {
        r: channel(colour.r),
        g: channel(colour.g),
        b: channel(colour.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_round_trip() {
        for status in Status::ALL {
            assert_eq!(Status::from_u8(status as u8), Some(status));
        }
        assert_eq!(Status::from_u8(Status::ALL.len() as u8), None);
    }

    #[test]
    fn nothing_is_ever_brighter_than_max() {
        for status in Status::ALL {
            for t in (0..10_000).step_by(7) {
                let c = colour(status, t);
                assert!(
                    c.r <= MAX && c.g <= MAX && c.b <= MAX,
                    "{status:?} at {t}: {c:?}"
                );
            }
        }
    }

    #[test]
    fn setup_pulses_blue_from_dark_to_full_and_back() {
        assert_eq!(colour(Status::Setup, 0), Rgb::OFF);
        assert_eq!(colour(Status::Setup, 1000), BLUE);
        assert_eq!(colour(Status::Setup, 2000), Rgb::OFF);
        // Half way up is half as bright, and the way down mirrors the way up.
        assert_eq!(colour(Status::Setup, 500).b, MAX / 2);
        assert_eq!(colour(Status::Setup, 300), colour(Status::Setup, 1700));
        // Only ever blue.
        for t in (0..2000).step_by(10) {
            let c = colour(Status::Setup, t);
            assert_eq!((c.r, c.g), (0, 0));
        }
    }

    #[test]
    fn setup_brightens_steadily_without_jumps() {
        let mut last = 0;
        for t in 0..=1000 {
            let b = colour(Status::Setup, t).b;
            assert!(b >= last && b - last <= 1, "jump at {t}: {last} to {b}");
            last = b;
        }
    }

    #[test]
    fn connecting_blinks_amber_half_the_time() {
        assert_eq!(colour(Status::Connecting, 0), AMBER);
        assert_eq!(colour(Status::Connecting, 499), AMBER);
        assert_eq!(colour(Status::Connecting, 500), Rgb::OFF);
        assert_eq!(colour(Status::Connecting, 999), Rgb::OFF);
        assert_eq!(colour(Status::Connecting, 1000), AMBER);
    }

    #[test]
    fn online_is_mostly_dark_with_a_brief_green_flash() {
        assert_eq!(colour(Status::Online, 0), GREEN);
        assert_eq!(colour(Status::Online, 99), GREEN);
        assert_eq!(colour(Status::Online, 100), Rgb::OFF);
        assert_eq!(colour(Status::Online, 2999), Rgb::OFF);
        assert_eq!(colour(Status::Online, 3000), GREEN);
        let lit = (0..ONLINE_PERIOD_MS)
            .filter(|&t| colour(Status::Online, t) != Rgb::OFF)
            .count() as u64;
        assert_eq!(lit, ONLINE_FLASH_MS);
    }

    #[test]
    fn trouble_is_steady_and_unmistakable() {
        for t in [0, 1, 499, 500, 1234, 99_999] {
            assert_eq!(colour(Status::BrokerLost, t), AMBER);
            assert_eq!(colour(Status::Failed, t), RED);
        }
        // Lost-broker amber is the same colour as connecting, held steady:
        // "trying to get there" against "was there, now is not".
        assert_eq!(colour(Status::Connecting, 0), colour(Status::BrokerLost, 0));
    }

    #[test]
    fn long_uptimes_keep_their_patterns() {
        let t = 49 * 24 * 3600 * 1000; // past where a 32-bit millisecond clock wraps
        assert_eq!(
            colour(Status::Online, t),
            colour(Status::Online, t % ONLINE_PERIOD_MS)
        );
        assert_eq!(
            colour(Status::Setup, t + 1000).b,
            colour(Status::Setup, (t + 1000) % SETUP_PERIOD_MS).b
        );
    }
}
