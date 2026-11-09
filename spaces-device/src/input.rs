//! Buttons and switches, read from a pin that bounces.
//!
//! A contact closing does not go cleanly from open to closed: for a few
//! milliseconds it chatters, and a careless reader sees two presses. Worse, a
//! press can bounce *after* it has looked settled. On the demo board one press
//! of a button once became two tool-on requests 42 ms apart, with the old
//! "three samples alike" rule.
//!
//! So a [`Debounced`] input changes state only once the pin has read the new
//! state for [`settle`](Debounced::settle) milliseconds, and then holds that
//! state for at least [`hold`](Debounced::hold) milliseconds, whatever the pin
//! does meanwhile. No finger presses a button twice in a quarter of a second,
//! and no contact bounces for that long.

/// One input, with time passed in as monotonic milliseconds.
#[derive(Debug, Clone)]
pub struct Debounced {
    /// How long the pin must read a new state before it counts.
    pub settle: u64,
    /// How long a state, once counted, is kept regardless of the pin.
    pub hold: u64,
    active: bool,
    /// When the pin started reading something other than `active`.
    differs_since: Option<u64>,
    /// When `active` last changed.
    changed_at: Option<u64>,
}

impl Debounced {
    /// For push buttons: 30 ms to settle, then held for 250 ms.
    pub fn button() -> Self {
        Self::new(30, 250)
    }

    /// For a switch, which a person flips rather than taps: 30 ms to settle,
    /// held for 100 ms.
    pub fn switch() -> Self {
        Self::new(30, 100)
    }

    pub fn new(settle: u64, hold: u64) -> Self {
        Self {
            settle,
            hold,
            active: false,
            differs_since: None,
            changed_at: None,
        }
    }

    /// Whether the input currently counts as pressed or on.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Feed one reading taken at `now_ms`. Returns the new state if this
    /// reading changed it.
    pub fn sample(&mut self, now_ms: u64, reading: bool) -> Option<bool> {
        if reading == self.active {
            self.differs_since = None;
            return None;
        }
        let since = *self.differs_since.get_or_insert(now_ms);
        let settled = now_ms.saturating_sub(since) >= self.settle;
        let held = self
            .changed_at
            .is_none_or(|at| now_ms.saturating_sub(at) >= self.hold);
        if !(settled && held) {
            return None;
        }
        self.active = reading;
        self.differs_since = None;
        self.changed_at = Some(now_ms);
        Some(reading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `readings` at 10 ms intervals from `start`, as the demo samples,
    /// and return the times and states of every change.
    fn run(input: &mut Debounced, start: u64, readings: &[bool]) -> Vec<(u64, bool)> {
        readings
            .iter()
            .enumerate()
            .filter_map(|(i, &r)| {
                let t = start + 10 * i as u64;
                input.sample(t, r).map(|s| (t, s))
            })
            .collect()
    }

    fn presses(changes: &[(u64, bool)]) -> usize {
        changes.iter().filter(|(_, s)| *s).count()
    }

    const X: bool = true;
    const O: bool = false;

    #[test]
    fn a_clean_press_counts_once_after_settling() {
        let mut b = Debounced::button();
        let changes = run(&mut b, 0, &[O, X, X, X, X, X, X, O, O, O, O]);
        assert_eq!(changes.first(), Some(&(40, true)));
        assert_eq!(presses(&changes), 1);
    }

    #[test]
    fn chatter_shorter_than_settling_is_ignored() {
        let mut b = Debounced::button();
        let changes = run(&mut b, 0, &[X, O, X, X, O, X, O, O, O, O]);
        assert!(changes.is_empty());
        assert!(!b.active());
    }

    #[test]
    fn a_press_that_bounces_after_settling_counts_once() {
        // What the demo board did: pressed long enough to count, let go long
        // enough to count, and closed again, all within about 100 ms.
        let mut b = Debounced::button();
        let readings = [
            X, X, X, X, O, O, O, O, X, X, X, X, X, X, X, X, X, X, X, X, X, X, X, X, X, X, X, X, X,
            X, X, X, X, X, X, O, O, O, O,
        ];
        let changes = run(&mut b, 0, &readings);
        assert_eq!(presses(&changes), 1, "{changes:?}");
        // Without the hold, which is the rule the demo used to have, it is two.
        let mut old = Debounced::new(30, 0);
        assert_eq!(presses(&run(&mut old, 0, &readings)), 2);
    }

    #[test]
    fn two_deliberate_presses_count_twice() {
        let mut b = Debounced::button();
        let press = |b: &mut Debounced, t| run(b, t, &[X; 20]);
        let release = |b: &mut Debounced, t| run(b, t, &[O; 20]);
        let mut changes = press(&mut b, 0);
        changes.extend(release(&mut b, 200));
        changes.extend(press(&mut b, 400));
        changes.extend(release(&mut b, 600));
        assert_eq!(presses(&changes), 2, "{changes:?}");
        assert!(!b.active());
    }

    #[test]
    fn a_held_button_releases_once_the_hold_is_over() {
        let mut b = Debounced::button();
        // Pressed at 30 ms, released almost at once: the release still counts,
        // just no sooner than the hold allows.
        let changes = run(
            &mut b,
            0,
            &[
                X, X, X, X, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O, O,
                O, O,
            ],
        );
        assert_eq!(changes, [(30, true), (280, false)]);
    }

    #[test]
    fn a_switch_reports_both_directions() {
        let mut s = Debounced::switch();
        let mut changes = run(&mut s, 0, &[X; 20]);
        changes.extend(run(&mut s, 200, &[O; 20]));
        assert_eq!(changes, [(30, true), (230, false)]);
    }

    #[test]
    fn a_clock_that_starts_late_or_jumps_is_harmless() {
        let mut b = Debounced::button();
        assert_eq!(b.sample(u64::MAX - 100, true), None);
        assert_eq!(b.sample(u64::MAX, true), Some(true));
        // A reading from "before" the last change, as a wrapped or reset
        // clock might give, neither panics nor counts.
        assert_eq!(b.sample(5, false), None);
    }
}
