//! Conditioning a machine's "working" signal into something worth metering.
//!
//! A module that senses whether its machine is actually working rarely gets a
//! clean signal. The case this exists for is the Hackspace laser: its Ruida
//! controller offers one line, `L-ON1`, and that line **drops on every
//! scan-line return** -- low for each pass, high while the head overshoots and
//! steps in Y. Fed straight into a usage accumulator it reports a 20-minute
//! engrave as a few hundred fragments, and strobes the Running output at
//! scan-line rate while it does.
//!
//! So the raw signal needs two independent allowances, and they are
//! deliberately asymmetric:
//!
//! - **`assert_ms`** -- how long the signal must hold *true* before usage
//!   starts. A glitch must not start a session's clock. Nothing on a laser's
//!   enable line is quiet: it sits next to a switching HV supply.
//! - **`hold_ms`** -- how long usage persists after the signal goes *false*. A
//!   gap must not end it.
//!
//! One must not imply the other. A filter with a single symmetric debounce
//! either rejects real work or counts a glitch, depending on which way it is
//! tuned, and there is no setting of it that does both jobs.
//!
//! **`hold_ms` is also the knob that chooses what "in use" means**, which is
//! why it is configuration and not a constant:
//!
//! ```text
//! hold_ms = 0       the raw asserted intervals -- beam-on time, tube wear
//! hold_ms = 10_000  the whole job as one span  -- bench time, machine hours
//! ```
//!
//! CS:S wants both figures and decides per tool which one bills
//! (`tools.usage_basis`, CS:S #169). A module reports the held figure as
//! `seconds` and the raw one as `active_seconds`, so this filter is what
//! produces the first from the second rather than a choice between them.
//!
//! **What is NOT here: polarity.** [`crate::module::Module::running`] takes a
//! *semantic* bool -- "the machine started or stopped working" -- not a pin
//! level, and the firmware reading the pin already applies the sense of its own
//! wiring (`examples/buttons` reads `is_low()` because its switch is wired to
//! ground). A laser's enable line is active-low by default but that is a
//! machine-level vendor setting, so it genuinely varies per installation; it
//! belongs with the code that reads the pin, where exactly one inversion can
//! happen. Putting it here too would make a double inversion a one-character
//! mistake.

/// Turns a noisy "the machine is working" signal into a held one.
///
/// Holds its own state, so one lives per module rather than per sample. The
/// default is [`RunFilter::passthrough`]: output follows input instantly, which
/// is bit-identical to having no filter at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFilter {
    assert_ms: u32,
    hold_ms: u32,
    /// The last raw sample, and when it became that value. The hold is measured
    /// from the *deassertion*, not from when a later call noticed it, so a
    /// filter ticked late does not extend the hold it was given.
    raw: bool,
    raw_since: u64,
    /// Stage one: the signal with noise rejected. `active_seconds`' source.
    clean: bool,
    /// When `clean` last changed. The hold runs from here and not from
    /// `raw_since`, so a glitch arriving *during* a hold cannot extend it.
    clean_since: u64,
    /// Stage two: `clean` with gaps bridged. `seconds`' source.
    on: bool,
}

impl RunFilter {
    pub fn new(assert_ms: u32, hold_ms: u32) -> Self {
        RunFilter {
            assert_ms,
            hold_ms,
            raw: false,
            raw_since: 0,
            clean: false,
            clean_since: 0,
            on: false,
        }
    }

    /// No glitch rejection and no hold: the output follows the input exactly.
    ///
    /// The default, so adding this filter to a module that had none changes
    /// nothing about what it reports.
    pub fn passthrough() -> Self {
        Self::new(0, 0)
    }

    pub fn assert_ms(&self) -> u32 {
        self.assert_ms
    }

    pub fn hold_ms(&self) -> u32 {
        self.hold_ms
    }

    /// The signal with noise rejected but **no** gap bridging: the time the
    /// machine's own signal was genuinely asserted. `active_seconds`' source.
    ///
    /// Not the bare pin. `assert_ms` rejects noise from *both* figures, because
    /// a glitch is interference and not the machine working -- on a tool billed
    /// by working time, a raw figure that counted glitches would bill them.
    pub fn asserted(&self) -> bool {
        self.clean
    }

    /// The asserted signal with gaps bridged. `seconds`' source.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// A new reading from the hardware. Returns the conditioned value.
    ///
    /// Only a *change* moves `raw_since`. Without that guard, polling firmware
    /// would push the hold's deadline forward on every sample and a stopped
    /// machine would accrue usage forever -- see
    /// `repeated_false_samples_do_not_refresh_the_hold`.
    pub fn sample(&mut self, raw: bool, now: u64) -> bool {
        if raw != self.raw {
            self.raw = raw;
            self.raw_since = now;
        }
        self.evaluate(now)
    }

    /// Time passed with no new reading. Returns the conditioned value.
    ///
    /// Needed because a hold expires on its own: without this, a machine that
    /// stopped and was never sampled again would accrue usage until the next
    /// edge, which on a tool left idle is until somebody touches it.
    pub fn tick(&mut self, now: u64) -> bool {
        self.evaluate(now)
    }

    /// Two stages, each measured from the instant its own input last
    /// *changed* rather than from the instant a call noticed. That is what
    /// makes the filter independent of how often it is polled: a late `tick`
    /// gets no extra hold, and repeated samples of an unchanged signal move
    /// nothing.
    ///
    /// Stage two runs from `clean_since`, not `raw_since`, and the difference
    /// is a real case: a glitch arriving midway through a bridged gap moves
    /// `raw_since` but not `clean_since`, so it cannot push the hold's deadline
    /// out. Measuring from `raw_since` would let noise extend a measurement.
    ///
    /// There was also a `hold_until` field here, set once per gap so repeated
    /// false samples could not keep pushing the deadline out. Mutation testing
    /// deleted it and no test noticed, which was correct: `sample` only touches
    /// `raw_since` on a change, so recomputing the deadline is already
    /// idempotent and the extra state guarded nothing.
    fn evaluate(&mut self, now: u64) -> bool {
        // Stage one: noise rejection. A pure function of the raw signal's age,
        // so it needs no state of its own -- once the signal has been held long
        // enough it stays clean for as long as it stays raw.
        let clean = self.raw && now.saturating_sub(self.raw_since) >= u64::from(self.assert_ms);
        if clean != self.clean {
            self.clean = clean;
            self.clean_since = now;
        }

        // Stage two: gap bridging.
        if self.clean {
            self.on = true;
        } else if self.on && now.saturating_sub(self.clean_since) >= u64::from(self.hold_ms) {
            self.on = false;
        }
        self.on
    }
}

impl Default for RunFilter {
    fn default() -> Self {
        Self::passthrough()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default must be invisible: a module that gains this filter and
    /// configures nothing reports exactly what it reported before.
    #[test]
    fn passthrough_follows_the_input_with_no_delay_either_way() {
        let mut f = RunFilter::passthrough();
        assert!(!f.is_on());
        assert!(f.sample(true, 100));
        assert!(f.is_on());
        assert!(!f.sample(false, 101));
        assert!(!f.is_on());
    }

    #[test]
    fn a_glitch_shorter_than_assert_never_starts_usage() {
        let mut f = RunFilter::new(100, 0);
        // Up at 1000, down at 1050: 50 ms, half the allowance.
        assert!(!f.sample(true, 1000), "a glitch started the clock");
        assert!(!f.sample(false, 1050));
        assert!(!f.is_on());
    }

    #[test]
    fn a_signal_held_past_assert_starts_usage_and_not_before() {
        let mut f = RunFilter::new(100, 0);
        assert!(!f.sample(true, 1000));
        // The boundary, from both sides. An off-by-one here is a filter that
        // either never asserts or asserts immediately.
        assert!(!f.tick(1099));
        assert!(f.tick(1100));
    }

    /// The defect this exists for. A laser's enable line drops on every
    /// scan-line return, so without a hold the engrave below is six fragments
    /// and the Running output strobes.
    #[test]
    fn a_gap_shorter_than_hold_is_bridged() {
        let mut f = RunFilter::new(0, 5_000);
        assert!(f.sample(true, 0));
        for pass in 0..3 {
            let t = pass * 1_000;
            assert!(f.sample(false, t + 400), "the gap ended usage");
            assert!(f.is_on(), "usage ended during a scan-line return");
            assert!(f.sample(true, t + 600));
        }
        assert!(f.is_on());
    }

    #[test]
    fn a_gap_longer_than_hold_ends_usage() {
        let mut f = RunFilter::new(0, 5_000);
        assert!(f.sample(true, 0));
        assert!(f.sample(false, 1_000));
        assert!(
            f.tick(5_999),
            "ended early: the hold runs from the deassertion"
        );
        assert!(!f.tick(6_000));
        assert!(!f.is_on());
    }

    /// The hold is measured from the deassertion, not from the call that
    /// notices it. A filter ticked late must not get a longer hold than one
    /// ticked promptly -- otherwise usage depends on the poll interval.
    #[test]
    fn a_late_tick_does_not_extend_the_hold() {
        let mut f = RunFilter::new(0, 1_000);
        assert!(f.sample(true, 0));
        assert!(f.sample(false, 500));
        // Nothing looks at it for a long time; the hold still ended at 1500.
        assert!(!f.tick(9_000));
    }

    /// And repeated samples of an unchanged signal must not refresh it. The
    /// guard is the `raw != self.raw` check in [`RunFilter::sample`]: without
    /// it, every poll while the machine sat idle would move `raw_since` forward
    /// and the hold would never expire -- a tool that never stops accruing
    /// usage, on firmware that polls, which is all of it.
    #[test]
    fn repeated_false_samples_do_not_refresh_the_hold() {
        let mut f = RunFilter::new(0, 1_000);
        assert!(f.sample(true, 0));
        assert!(f.sample(false, 100));
        for t in [200, 400, 600, 800, 1_000] {
            assert!(f.sample(false, t), "the hold ended early at {t}");
        }
        assert!(!f.sample(false, 1_100));
    }

    /// Bridging must not re-arm the assert allowance. A laser whose passes are
    /// shorter than `assert_ms` would otherwise never resume after the first
    /// gap, and the job would meter as one pass.
    #[test]
    fn a_bridged_gap_does_not_re_apply_the_assert_allowance() {
        let mut f = RunFilter::new(1_000, 5_000);
        assert!(!f.sample(true, 0));
        assert!(f.tick(1_000), "did not assert after the allowance");
        // A pass far shorter than assert_ms, inside the hold.
        assert!(f.sample(false, 1_100));
        assert!(f.sample(true, 1_200));
        assert!(f.is_on(), "a 100 ms pass inside the hold dropped usage");
    }

    /// After a hold genuinely expires, the allowance applies again -- the
    /// machine is starting, not continuing.
    #[test]
    fn the_assert_allowance_applies_again_after_the_hold_expires() {
        let mut f = RunFilter::new(1_000, 1_000);
        assert!(!f.sample(true, 0));
        assert!(f.tick(1_000));
        assert!(f.sample(false, 2_000));
        assert!(!f.tick(3_000), "the hold did not expire");
        // Starting again: it must earn the allowance a second time.
        assert!(!f.sample(true, 4_000));
        assert!(f.tick(5_000));
    }

    #[test]
    fn the_asserted_signal_is_reported_separately_from_the_held_one() {
        // This is what keeps the two usage figures distinguishable:
        // `asserted()` feeds active_seconds, `is_on()` feeds seconds.
        let mut f = RunFilter::new(0, 5_000);
        f.sample(true, 0);
        f.sample(false, 100);
        assert!(
            !f.asserted(),
            "the asserted signal must follow the hardware, not the hold"
        );
        assert!(f.is_on(), "the held signal must still be on");
    }

    /// `assert_ms` must clean **both** figures. A glitch is interference, not
    /// the machine working -- and on a tool billed by working time, an
    /// `active_seconds` that counted glitches would bill them. Found by a
    /// module-level test asserting a 50 ms glitch billed 0.0 and getting 0.05.
    #[test]
    fn a_glitch_never_reaches_the_asserted_signal_either() {
        let mut f = RunFilter::new(200, 0);
        f.sample(true, 1_000);
        assert!(
            !f.asserted(),
            "a signal 0 ms old was already treated as asserted"
        );
        f.sample(false, 1_050);
        assert!(!f.asserted());
        assert!(!f.is_on());
    }

    /// A glitch arriving midway through a bridged gap must not extend the hold.
    /// This is why stage two runs from `clean_since` and not `raw_since`: the
    /// glitch moves the latter, and measuring from it would let noise lengthen
    /// a billed measurement.
    #[test]
    fn a_glitch_during_a_gap_does_not_extend_the_hold() {
        let mut f = RunFilter::new(200, 5_000);
        f.sample(true, 0);
        assert!(f.tick(200), "setup: should have asserted by now");
        f.sample(false, 1_000); // the hold runs 1000..6000
        f.sample(true, 3_000); // noise, far too short to assert
        f.sample(false, 3_050);
        assert!(f.tick(5_999), "the hold ended early");
        assert!(
            !f.tick(6_000),
            "the glitch pushed the hold's deadline past its true end"
        );
    }
}
